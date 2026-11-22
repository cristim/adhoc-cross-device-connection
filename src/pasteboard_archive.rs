//! Safe `NSKeyedArchiver` binary-plist decoder for `UASharedPasteboardInfoWrapper`.
//!
//! Decodes Apple Universal Clipboard's `rActPayload` (`UASharedPasteboardInfoWrapper`)
//! using a strict `$classname` allowlist, active-stack cycle detection, a total
//! node-expansion budget, and checked `offset + size` end-exclusive slicing.

use anyhow::{bail, ensure, Context, Result};
use plist::stream::{BinaryReader, Event, OwnedEvent};
use plist::{Dictionary, Uid, Value};
use std::collections::HashSet;
use std::io::Cursor;

use crate::companion_client::{Pasteboard, PasteboardItem};

const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 4096;
const MAX_ARCHIVE_BYTES: usize = 4 * 1024 * 1024;
const MAX_DECODED_BYTES: usize = 16 * 1024 * 1024;

/// Builds a [`Value`] tree from one already-consumed stream `event`, pulling
/// further events (for arrays/dictionaries) from `events`.
///
/// `plist` 1.10.1's binary reader only rejects a reference back to an object
/// still on its own resolution stack (a true cycle); a later, separate
/// reference to an already-resolved object is parsed again from scratch. A
/// small, acyclic but deeply reference-sharing plist can therefore expand
/// exponentially once fully materialized by `Value::from_reader`, even though
/// its wire size is bounded by `MAX_ARCHIVE_BYTES`. Walking the event stream
/// ourselves and charging `node_budget`/depth here, before any `Value` tree
/// exists, bounds that expansion the same way `Archive` already bounds the
/// NSKeyedArchiver `$objects` graph built on top of it.
fn value_from_event(
    event: OwnedEvent,
    events: &mut impl Iterator<Item = Result<OwnedEvent, plist::Error>>,
    depth: usize,
    node_budget: &mut usize,
) -> Result<Value> {
    *node_budget = node_budget
        .checked_sub(1)
        .with_context(|| format!("plist node budget ({MAX_NODES}) exceeded while parsing"))?;
    Ok(match event {
        Event::StartArray(_) => {
            ensure!(depth < MAX_DEPTH, "plist nesting exceeds {MAX_DEPTH}");
            let mut items = Vec::new();
            loop {
                let next = events
                    .next()
                    .context("plist event stream ended inside an array")??;
                if matches!(next, Event::EndCollection) {
                    break;
                }
                items.push(value_from_event(next, events, depth + 1, node_budget)?);
            }
            Value::Array(items)
        }
        Event::StartDictionary(_) => {
            ensure!(depth < MAX_DEPTH, "plist nesting exceeds {MAX_DEPTH}");
            let mut dict = Dictionary::new();
            loop {
                let key_event = events
                    .next()
                    .context("plist event stream ended inside a dictionary")??;
                let key = match key_event {
                    Event::EndCollection => break,
                    Event::String(s) => s.into_owned(),
                    other => bail!("non-string plist dictionary key: {other:?}"),
                };
                let value_event = events
                    .next()
                    .context("plist dictionary key missing its value")??;
                ensure!(
                    !matches!(value_event, Event::EndCollection),
                    "plist dictionary key missing its value"
                );
                let value = value_from_event(value_event, events, depth + 1, node_budget)?;
                dict.insert(key, value);
            }
            Value::Dictionary(dict)
        }
        Event::Boolean(b) => Value::Boolean(b),
        Event::Data(d) => Value::Data(d.into_owned()),
        Event::Date(d) => Value::Date(d),
        Event::Integer(i) => Value::Integer(i),
        Event::Real(r) => Value::Real(r),
        Event::String(s) => Value::String(s.into_owned()),
        Event::Uid(u) => Value::Uid(u),
        other => bail!("unsupported plist event: {other:?}"),
    })
}

/// Parses a binary plist into a [`Value`] with `MAX_NODES`/`MAX_DEPTH` charged
/// during parsing rather than after, per [`value_from_event`].
fn decode_plist_bounded(bytes: &[u8]) -> Result<Value> {
    let mut events = BinaryReader::new(Cursor::new(bytes));
    let root = events
        .next()
        .context("empty plist event stream")?
        .context("parsing binary plist")?;
    let mut node_budget = MAX_NODES;
    let value = value_from_event(root, &mut events, 0, &mut node_budget)?;
    ensure!(
        events.next().is_none(),
        "plist event stream has trailing data after its root value"
    );
    Ok(value)
}

const ALLOWED_CLASSES: &[&str] = &[
    "UASharedPasteboardInfoWrapper",
    "UASharedPasteboardInfo",
    "UASharedPasteboardItemInfo",
    "UASharedPasteboardTypeInfo",
    "NSUUID",
    "NSData",
    "NSMutableData",
    "NSArray",
    "NSMutableArray",
    "NSDictionary",
    "NSMutableDictionary",
    "NSString",
    "NSMutableString",
    "NSNumber",
];

struct Archive<'a> {
    objects: &'a [Value],
    visiting: HashSet<u64>,
    expanded: usize,
}

impl<'a> Archive<'a> {
    fn new(objects: &'a [Value]) -> Self {
        Self {
            objects,
            visiting: HashSet::new(),
            expanded: 0,
        }
    }

    fn resolve_raw(&mut self, uid: &Uid, depth: usize) -> Result<&'a Value> {
        ensure!(
            depth <= MAX_DEPTH,
            "NSKeyedArchiver nesting exceeds {MAX_DEPTH}"
        );
        self.expanded += 1;
        ensure!(
            self.expanded <= MAX_NODES,
            "NSKeyedArchiver node budget ({MAX_NODES}) exceeded"
        );
        let idx = uid.get();
        let val = self
            .objects
            .get(idx as usize)
            .with_context(|| format!("NSKeyedArchiver UID {idx} out of bounds"))?;
        if let Value::Dictionary(dict) = val {
            self.check_class(dict, depth)?;
        }
        Ok(val)
    }

    fn enter(&mut self, uid: &Uid) -> Result<()> {
        let idx = uid.get();
        ensure!(
            self.visiting.insert(idx),
            "NSKeyedArchiver cyclic UID reference at {idx}"
        );
        Ok(())
    }

    fn leave(&mut self, uid: &Uid) {
        self.visiting.remove(&uid.get());
    }

    fn check_class(&mut self, dict: &Dictionary, depth: usize) -> Result<Option<String>> {
        let Some(class_val) = dict.get("$class") else {
            return Ok(None);
        };
        let class_uid = class_val
            .as_uid()
            .context("NSKeyedArchiver $class is not a UID")?;
        let idx = class_uid.get();
        ensure!(depth < MAX_DEPTH, "NSKeyedArchiver $class depth exceeded");
        self.expanded += 1;
        ensure!(
            self.expanded <= MAX_NODES,
            "NSKeyedArchiver node budget ({MAX_NODES}) exceeded"
        );
        let class_obj = self
            .objects
            .get(idx as usize)
            .and_then(Value::as_dictionary)
            .with_context(|| format!("NSKeyedArchiver $class UID {idx} invalid"))?;
        let name = class_obj
            .get("$classname")
            .and_then(Value::as_string)
            .context("NSKeyedArchiver class object missing $classname")?;
        ensure!(
            ALLOWED_CLASSES.contains(&name),
            "disallowed NSKeyedArchiver class: {name}"
        );
        Ok(Some(name.to_string()))
    }

    fn resolve_deref(&mut self, val: &'a Value, depth: usize) -> Result<(&'a Value, Option<Uid>)> {
        if let Some(uid) = val.as_uid() {
            self.enter(uid)?;
            let resolved = self.resolve_raw(uid, depth + 1);
            match resolved {
                Ok(v) => Ok((v, Some(*uid))),
                Err(e) => {
                    self.leave(uid);
                    Err(e)
                }
            }
        } else {
            if let Value::Dictionary(dict) = val {
                self.check_class(dict, depth)?;
            }
            Ok((val, None))
        }
    }

    fn as_string(&mut self, val: &'a Value, depth: usize) -> Result<Option<String>> {
        let (v, guard) = self.resolve_deref(val, depth)?;
        let out = match v {
            Value::String(s) if s == "$null" => None,
            Value::String(s) => Some(s.clone()),
            Value::Dictionary(d) => d
                .get("NS.string")
                .and_then(Value::as_string)
                .map(ToString::to_string),
            _ => None,
        };
        if let Some(uid) = guard {
            self.leave(&uid);
        }
        Ok(out)
    }

    fn as_data(&mut self, val: &'a Value, depth: usize) -> Result<Option<Vec<u8>>> {
        let (v, guard) = self.resolve_deref(val, depth)?;
        let out = match v {
            Value::String(s) if s == "$null" => None,
            Value::Data(b) => Some(b.clone()),
            Value::Dictionary(d) => d
                .get("NS.data")
                .and_then(Value::as_data)
                .map(|b| b.to_vec()),
            _ => None,
        };
        if let Some(uid) = guard {
            self.leave(&uid);
        }
        Ok(out)
    }

    fn as_usize(&mut self, val: &'a Value, depth: usize) -> Result<Option<usize>> {
        let (v, guard) = self.resolve_deref(val, depth)?;
        let out = match v {
            Value::String(s) if s == "$null" => None,
            Value::Integer(n) => {
                let u = n
                    .as_unsigned()
                    .context("negative integer in NSKeyedArchiver offset/size")?;
                Some(usize::try_from(u).context("integer overflows usize")?)
            }
            Value::Dictionary(d) => match d.get("NS.integer") {
                Some(n) => {
                    let u = n.as_unsigned_integer().context(
                        "negative or non-integer NS.integer in NSKeyedArchiver offset/size",
                    )?;
                    Some(usize::try_from(u).context("integer overflows usize")?)
                }
                None => None,
            },
            _ => None,
        };
        if let Some(uid) = guard {
            self.leave(&uid);
        }
        Ok(out)
    }

    fn as_array_uids(&mut self, val: &'a Value, depth: usize) -> Result<Vec<&'a Value>> {
        let (v, guard) = self.resolve_deref(val, depth)?;
        let res = match v {
            Value::Array(arr) => Ok(arr.iter().collect()),
            Value::Dictionary(d) => {
                if let Some(objs) = d.get("NS.objects").and_then(Value::as_array) {
                    Ok(objs.iter().collect())
                } else {
                    bail!("expected NSArray/NS.objects in NSKeyedArchiver")
                }
            }
            _ => bail!("expected array in NSKeyedArchiver"),
        };
        if let Some(uid) = guard {
            self.leave(&uid);
        }
        res
    }

    fn dict_values(
        &mut self,
        val: &'a Value,
        depth: usize,
    ) -> Result<Vec<(Option<String>, &'a Value)>> {
        let (v, guard) = self.resolve_deref(val, depth)?;
        let res = match v {
            Value::Dictionary(d) => {
                if let (Some(keys), Some(objs)) = (
                    d.get("NS.keys").and_then(Value::as_array),
                    d.get("NS.objects").and_then(Value::as_array),
                ) {
                    ensure!(
                        keys.len() == objs.len(),
                        "NSDictionary NS.keys/NS.objects length mismatch"
                    );
                    let mut pairs = Vec::with_capacity(keys.len());
                    for (k, o) in keys.iter().zip(objs.iter()) {
                        let key_str = self.as_string(k, depth + 1)?;
                        pairs.push((key_str, o));
                    }
                    Ok(pairs)
                } else {
                    Ok(d.iter()
                        .filter(|(k, _)| *k != "$class")
                        .map(|(k, v)| (Some(k.clone()), v))
                        .collect())
                }
            }
            Value::Array(arr) => Ok(arr.iter().map(|v| (None, v)).collect()),
            _ => bail!("expected NSDictionary or NSArray for item types"),
        };
        if let Some(uid) = guard {
            self.leave(&uid);
        }
        res
    }
}

/// Decode a binary plist `NSKeyedArchiver` payload (`UASharedPasteboardInfoWrapper`
/// or `UASharedPasteboardInfo`) into a [`Pasteboard`].
pub fn decode_wrapper(bytes: &[u8]) -> Result<Pasteboard> {
    ensure!(
        bytes.len() <= MAX_ARCHIVE_BYTES,
        "pasteboard archive exceeds {MAX_ARCHIVE_BYTES} bytes"
    );
    let root_val = decode_plist_bounded(bytes)?;
    let root_dict = root_val
        .as_dictionary()
        .context("NSKeyedArchiver root is not a dictionary")?;
    let archiver = root_dict
        .get("$archiver")
        .and_then(Value::as_string)
        .context("plist missing $archiver")?;
    ensure!(
        archiver == "NSKeyedArchiver",
        "unexpected $archiver: {archiver}"
    );
    let objects = root_dict
        .get("$objects")
        .and_then(Value::as_array)
        .context("NSKeyedArchiver missing $objects")?;
    let top = root_dict
        .get("$top")
        .and_then(Value::as_dictionary)
        .context("NSKeyedArchiver missing $top")?;
    let root_uid = top
        .get("root")
        .and_then(Value::as_uid)
        .context("NSKeyedArchiver $top missing root UID")?;

    let mut ar = Archive::new(objects);
    ar.enter(root_uid)?;
    let res = decode_root(&mut ar, root_uid);
    ar.leave(root_uid);
    res
}

fn get_any<'a>(dict: &'a Dictionary, keys: &[&str]) -> Option<&'a Value> {
    keys.iter().find_map(|k| dict.get(k))
}

/// Type name only, never contents: for error messages about unexpected payloads.
fn kind(v: &Value) -> &'static str {
    match v {
        Value::Dictionary(_) => "dictionary",
        Value::Array(_) => "array",
        Value::String(s) if s == "$null" => "$null",
        Value::String(_) => "string",
        Value::Data(_) => "data",
        Value::Integer(_) => "integer",
        Value::Real(_) => "real",
        Value::Boolean(_) => "boolean",
        Value::Date(_) => "date",
        Value::Uid(_) => "uid",
        _ => "other",
    }
}

fn decode_root(ar: &mut Archive<'_>, root_uid: &Uid) -> Result<Pasteboard> {
    let root = ar.resolve_raw(root_uid, 0)?;
    let root_dict = root.as_dictionary().with_context(|| {
        format!(
            "NSKeyedArchiver root object is a {}, not a dictionary",
            kind(root)
        )
    })?;

    let wrapper_data_file = match get_any(
        root_dict,
        &[
            "dataFile",
            "UASharedPasteboardInfoWrapperExtraDataKey",
            "UASharedPasteboardInfoWrapperDataFileKey",
            "UASharedPasteboardInfoDataFileKey",
        ],
    ) {
        Some(v) => ar.as_data(v, 1)?,
        None => None,
    };

    let (pbinfo_dict, pbinfo_guard) = if let Some(pbinfo_val) = get_any(
        root_dict,
        &[
            "pbinfo",
            "UASharedPasteboardInfoWrapperPBInfoKey",
            "UASharedPasteboardInfoKey",
        ],
    ) {
        let (v, g) = ar.resolve_deref(pbinfo_val, 1)?;
        let d = v.as_dictionary().context("pbinfo is not a dictionary")?;
        (d, g)
    } else {
        (root_dict, None)
    };

    let res = decode_pbinfo(ar, pbinfo_dict, wrapper_data_file);
    if let Some(uid) = pbinfo_guard {
        ar.leave(&uid);
    }
    res
}

fn decode_pbinfo<'a>(
    ar: &mut Archive<'a>,
    pbinfo: &'a Dictionary,
    wrapper_data_file: Option<Vec<u8>>,
) -> Result<Pasteboard> {
    let data_file = match wrapper_data_file {
        Some(b) => Some(b),
        None => match get_any(pbinfo, &["dataFile", "UASharedPasteboardInfoDataFileKey"]) {
            Some(v) => ar.as_data(v, 2)?,
            None => None,
        },
    };

    let Some(items_val) = get_any(pbinfo, &["items", "UASharedPasteboardInfoItemsKey"]) else {
        return Ok(Pasteboard::default());
    };
    let item_refs = ar.as_array_uids(items_val, 2)?;
    let mut out_items = Vec::new();
    let mut decoded_bytes = 0usize;

    for item_ref in item_refs {
        let (item_val, item_guard) = ar.resolve_deref(item_ref, 3)?;
        let res = (|| -> Result<()> {
            let item_dict = item_val
                .as_dictionary()
                .context("UASharedPasteboardItemInfo is not a dictionary")?;
            let Some(types_val) =
                get_any(item_dict, &["types", "UASharedPasteboardItemInfoTypesKey"])
            else {
                return Ok(());
            };
            let type_entries = ar.dict_values(types_val, 4)?;
            for (key_uti, type_ref) in type_entries {
                let (t_val, t_guard) = ar.resolve_deref(type_ref, 5)?;
                let t_res = (|| -> Result<()> {
                    let t_dict = t_val
                        .as_dictionary()
                        .context("UASharedPasteboardTypeInfo is not a dictionary")?;
                    let uti = match get_any(t_dict, &["type", "UASharedPasteboardTypeInfoTypeKey"])
                    {
                        Some(v) => ar.as_string(v, 6)?,
                        None => None,
                    }
                    .or(key_uti)
                    .context("UASharedPasteboardTypeInfo missing UTI `type`")?;

                    let inline =
                        match get_any(t_dict, &["data", "UASharedPasteboardTypeInfoDataFileKey"]) {
                            Some(v) => ar.as_data(v, 6)?,
                            None => None,
                        };
                    let data = if let Some(inline_bytes) = inline {
                        charge_decoded(&mut decoded_bytes, inline_bytes.len())?;
                        inline_bytes
                    } else {
                        let size = match get_any(
                            t_dict,
                            &["dataSize", "size", "UASharedPasteboardTypeInfoSizeKey"],
                        ) {
                            Some(v) => ar.as_usize(v, 6)?,
                            None => None,
                        }
                        .unwrap_or(0);
                        if size == 0 {
                            Vec::new()
                        } else {
                            let offset = match get_any(
                                t_dict,
                                &["offset", "UASharedPasteboardTypeInfoOffsetKey"],
                            ) {
                                Some(v) => ar.as_usize(v, 6)?,
                                None => None,
                            }
                            .unwrap_or(0);
                            let blob = data_file
                                .as_deref()
                                .context("type has dataSize > 0 but archive has no dataFile")?;
                            let end = offset
                                .checked_add(size)
                                .context("type offset + dataSize overflows usize")?;
                            let slice = blob.get(offset..end).with_context(|| {
                                format!(
                                    "type slice {offset}..{end} out of bounds for dataFile ({} bytes)",
                                    blob.len()
                                )
                            })?;
                            charge_decoded(&mut decoded_bytes, slice.len())?;
                            slice.to_vec()
                        }
                    };
                    out_items.push(PasteboardItem { uti, data });
                    Ok(())
                })();
                if let Some(uid) = t_guard {
                    ar.leave(&uid);
                }
                t_res?;
            }
            Ok(())
        })();
        if let Some(uid) = item_guard {
            ar.leave(&uid);
        }
        res?;
    }

    Ok(Pasteboard { items: out_items })
}

fn charge_decoded(total: &mut usize, n: usize) -> Result<()> {
    *total = total.saturating_add(n);
    ensure!(
        *total <= MAX_DECODED_BYTES,
        "decoded pasteboard exceeds {MAX_DECODED_BYTES} bytes"
    );
    Ok(())
}

/// Encode a [`Pasteboard`] as a binary `NSKeyedArchiver`
/// `UASharedPasteboardInfoWrapper` plist matching `macOS 26.1`'s schema.
pub fn encode_wrapper(pb: &Pasteboard) -> Result<Vec<u8>> {
    let mut objects: Vec<Value> = vec![Value::String("$null".into())];
    fn push_obj(objects: &mut Vec<Value>, v: Value) -> Uid {
        let idx = objects.len() as u64;
        objects.push(v);
        Uid::new(idx)
    }
    fn push_class(objects: &mut Vec<Value>, name: &str) -> Uid {
        let mut d = Dictionary::new();
        d.insert("$classname".into(), Value::String(name.into()));
        push_obj(objects, Value::Dictionary(d))
    }

    let cls_wrapper = push_class(&mut objects, "UASharedPasteboardInfoWrapper");
    let cls_info = push_class(&mut objects, "UASharedPasteboardInfo");
    let cls_item = push_class(&mut objects, "UASharedPasteboardItemInfo");
    let cls_type = push_class(&mut objects, "UASharedPasteboardTypeInfo");
    let cls_array = push_class(&mut objects, "NSArray");
    let cls_dict = push_class(&mut objects, "NSDictionary");
    let cls_uuid = push_class(&mut objects, "NSUUID");

    let mut uuid_dict = Dictionary::new();
    uuid_dict.insert("$class".into(), Value::Uid(cls_uuid));
    uuid_dict.insert(
        "NS.uuidbytes".into(),
        Value::Data(uuid::Uuid::new_v4().as_bytes().to_vec()),
    );
    let uuid_uid = push_obj(&mut objects, Value::Dictionary(uuid_dict));

    let mut blob = Vec::new();
    let mut type_dict_keys = Vec::new();
    let mut type_dict_objs = Vec::new();

    for (idx, it) in pb.items.iter().enumerate() {
        let offset = blob.len() as u64;
        blob.extend_from_slice(&it.data);
        let size = it.data.len() as u64;
        let uti_uid = push_obj(&mut objects, Value::String(it.uti.clone()));
        let mut t_info = Dictionary::new();
        t_info.insert("$class".into(), Value::Uid(cls_type));
        t_info.insert(
            "UASharedPasteboardTypeInfoTypeKey".into(),
            Value::Uid(uti_uid),
        );
        t_info.insert(
            "UASharedPasteboardTypeInfoOffsetKey".into(),
            Value::Integer(offset.into()),
        );
        t_info.insert(
            "UASharedPasteboardTypeInfoSizeKey".into(),
            Value::Integer(size.into()),
        );
        t_info.insert(
            "UASharedPasteboardTypeInfoIndex".into(),
            Value::Integer((idx as u64).into()),
        );
        t_info.insert(
            "UASharedPasteboardTypeInfoPrefRepKey".into(),
            Value::Boolean(idx == 0),
        );
        t_info.insert(
            "UASharedPasteboardTypeInfoUUIDKey".into(),
            Value::Uid(uuid_uid),
        );
        let t_uid = push_obj(&mut objects, Value::Dictionary(t_info));
        type_dict_keys.push(Value::Uid(uti_uid));
        type_dict_objs.push(Value::Uid(t_uid));
    }

    let mut types_nsdict = Dictionary::new();
    types_nsdict.insert("$class".into(), Value::Uid(cls_dict));
    types_nsdict.insert("NS.keys".into(), Value::Array(type_dict_keys));
    types_nsdict.insert("NS.objects".into(), Value::Array(type_dict_objs));
    let types_uid = push_obj(&mut objects, Value::Dictionary(types_nsdict));

    let mut item_dict = Dictionary::new();
    item_dict.insert("$class".into(), Value::Uid(cls_item));
    item_dict.insert(
        "UASharedPasteboardItemInfoTypesKey".into(),
        Value::Uid(types_uid),
    );
    let item_uid = push_obj(&mut objects, Value::Dictionary(item_dict));

    let mut items_nsarray = Dictionary::new();
    items_nsarray.insert("$class".into(), Value::Uid(cls_array));
    items_nsarray.insert(
        "NS.objects".into(),
        Value::Array(vec![Value::Uid(item_uid)]),
    );
    let items_uid = push_obj(&mut objects, Value::Dictionary(items_nsarray));

    let total_size = blob.len() as u64;
    let mut pbinfo = Dictionary::new();
    pbinfo.insert("$class".into(), Value::Uid(cls_info));
    pbinfo.insert(
        "UASharedPasteboardInfoItemsKey".into(),
        Value::Uid(items_uid),
    );
    pbinfo.insert(
        "UASharedPasteboardInfoDataSizeKey".into(),
        Value::Integer(total_size.into()),
    );
    let pbinfo_uid = push_obj(&mut objects, Value::Dictionary(pbinfo));

    let data_uid = push_obj(&mut objects, Value::Data(blob));
    let mut wrapper = Dictionary::new();
    wrapper.insert("$class".into(), Value::Uid(cls_wrapper));
    wrapper.insert(
        "UASharedPasteboardInfoWrapperPBInfoKey".into(),
        Value::Uid(pbinfo_uid),
    );
    wrapper.insert(
        "UASharedPasteboardInfoWrapperExtraDataKey".into(),
        Value::Uid(data_uid),
    );
    wrapper.insert(
        "UASharedPasteboardInfoWrapperExtraTypeKey".into(),
        Value::Integer(2.into()),
    );
    wrapper.insert(
        "UASharedPasteboardInfoWrapperProtocolVersionKey".into(),
        Value::Integer(0.into()),
    );
    let root_uid = push_obj(&mut objects, Value::Dictionary(wrapper));

    let mut top = Dictionary::new();
    top.insert("root".into(), Value::Uid(root_uid));

    let mut root = Dictionary::new();
    root.insert("$version".into(), Value::Integer(100_000.into()));
    root.insert("$archiver".into(), Value::String("NSKeyedArchiver".into()));
    root.insert("$top".into(), Value::Dictionary(top));
    root.insert("$objects".into(), Value::Array(objects));

    let mut out = Vec::new();
    Value::Dictionary(root)
        .to_writer_binary(&mut out)
        .context("encoding binary NSKeyedArchiver plist")?;
    Ok(out)
}

/// Encode a [`Pasteboard`] as a synthetic binary `NSKeyedArchiver`
/// `UASharedPasteboardInfoWrapper` plist for loopback testing.
pub fn encode_wrapper_for_test(pb: &Pasteboard) -> Result<Vec<u8>> {
    encode_wrapper(pb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_macos26_full_key_names() {
        let mut objects: Vec<Value> = vec![Value::String("$null".into())];
        let mut t = Dictionary::new();
        t.insert(
            "UASharedPasteboardTypeInfoTypeKey".into(),
            Value::String("public.utf8-plain-text".into()),
        );
        t.insert(
            "UASharedPasteboardTypeInfoOffsetKey".into(),
            Value::Integer(0.into()),
        );
        t.insert(
            "UASharedPasteboardTypeInfoSizeKey".into(),
            Value::Integer(5.into()),
        );
        objects.push(Value::Dictionary(t));
        let mut types = Dictionary::new();
        types.insert(
            "NS.keys".into(),
            Value::Array(vec![Value::String("public.utf8-plain-text".into())]),
        );
        types.insert(
            "NS.objects".into(),
            Value::Array(vec![Value::Uid(Uid::new(1))]),
        );
        objects.push(Value::Dictionary(types));
        let mut item = Dictionary::new();
        item.insert(
            "UASharedPasteboardItemInfoTypesKey".into(),
            Value::Uid(Uid::new(2)),
        );
        objects.push(Value::Dictionary(item));
        let mut info = Dictionary::new();
        info.insert(
            "UASharedPasteboardInfoItemsKey".into(),
            Value::Array(vec![Value::Uid(Uid::new(3))]),
        );
        objects.push(Value::Dictionary(info));
        let mut wrapper = Dictionary::new();
        wrapper.insert(
            "UASharedPasteboardInfoWrapperPBInfoKey".into(),
            Value::Uid(Uid::new(4)),
        );
        wrapper.insert(
            "UASharedPasteboardInfoWrapperExtraDataKey".into(),
            Value::Data(b"hello".to_vec()),
        );
        objects.push(Value::Dictionary(wrapper));
        let mut top = Dictionary::new();
        top.insert("root".into(), Value::Uid(Uid::new(5)));
        let mut root = Dictionary::new();
        root.insert("$archiver".into(), Value::String("NSKeyedArchiver".into()));
        root.insert("$top".into(), Value::Dictionary(top));
        root.insert("$objects".into(), Value::Array(objects));
        let mut bytes = Vec::new();
        Value::Dictionary(root)
            .to_writer_binary(&mut bytes)
            .unwrap();
        let decoded = decode_wrapper(&bytes).unwrap();
        assert_eq!(
            decoded.items,
            vec![PasteboardItem {
                uti: "public.utf8-plain-text".into(),
                data: b"hello".to_vec(),
            }]
        );
    }

    #[test]
    fn roundtrips_multi_representation_archive_with_shared_uids() {
        let pb = Pasteboard {
            items: vec![
                PasteboardItem {
                    uti: "public.utf8-plain-text".into(),
                    data: b"hello universal clipboard".to_vec(),
                },
                PasteboardItem {
                    uti: "public.html".into(),
                    data: b"<b>hello</b>".to_vec(),
                },
            ],
        };
        let encoded = encode_wrapper_for_test(&pb).unwrap();
        let decoded = decode_wrapper(&encoded).unwrap();
        assert_eq!(decoded, pb);
    }

    #[test]
    fn empty_data_size_without_datafile_returns_empty_bytes() {
        let mut objects: Vec<Value> = vec![Value::String("$null".into())];
        let mut t = Dictionary::new();
        t.insert(
            "type".into(),
            Value::String("public.utf8-plain-text".into()),
        );
        t.insert("dataSize".into(), Value::Integer(0.into()));
        objects.push(Value::Dictionary(t));
        let mut types = Dictionary::new();
        types.insert(
            "NS.keys".into(),
            Value::Array(vec![Value::String("public.utf8-plain-text".into())]),
        );
        types.insert(
            "NS.objects".into(),
            Value::Array(vec![Value::Uid(Uid::new(1))]),
        );
        objects.push(Value::Dictionary(types));
        let mut item = Dictionary::new();
        item.insert("types".into(), Value::Uid(Uid::new(2)));
        objects.push(Value::Dictionary(item));
        let mut info = Dictionary::new();
        info.insert("items".into(), Value::Array(vec![Value::Uid(Uid::new(3))]));
        objects.push(Value::Dictionary(info));
        let mut top = Dictionary::new();
        top.insert("root".into(), Value::Uid(Uid::new(4)));
        let mut root = Dictionary::new();
        root.insert("$archiver".into(), Value::String("NSKeyedArchiver".into()));
        root.insert("$top".into(), Value::Dictionary(top));
        root.insert("$objects".into(), Value::Array(objects));
        let mut bytes = Vec::new();
        Value::Dictionary(root)
            .to_writer_binary(&mut bytes)
            .unwrap();
        let decoded = decode_wrapper(&bytes).unwrap();
        assert_eq!(decoded.items.len(), 1);
        assert!(decoded.items[0].data.is_empty());
    }

    #[test]
    fn rejects_disallowed_class() {
        let mut objects: Vec<Value> = vec![Value::String("$null".into())];
        let mut evil_cls = Dictionary::new();
        evil_cls.insert("$classname".into(), Value::String("NSExpression".into()));
        objects.push(Value::Dictionary(evil_cls));
        let mut obj = Dictionary::new();
        obj.insert("$class".into(), Value::Uid(Uid::new(1)));
        objects.push(Value::Dictionary(obj));
        let mut top = Dictionary::new();
        top.insert("root".into(), Value::Uid(Uid::new(2)));
        let mut root = Dictionary::new();
        root.insert("$archiver".into(), Value::String("NSKeyedArchiver".into()));
        root.insert("$top".into(), Value::Dictionary(top));
        root.insert("$objects".into(), Value::Array(objects));
        let mut bytes = Vec::new();
        Value::Dictionary(root)
            .to_writer_binary(&mut bytes)
            .unwrap();
        let err = decode_wrapper(&bytes).unwrap_err().to_string();
        assert!(
            err.contains("disallowed NSKeyedArchiver class"),
            "got: {err}"
        );
    }

    #[test]
    fn rejects_negative_or_oob_offset_size() {
        let mut objects: Vec<Value> = vec![Value::String("$null".into())];
        let mut t = Dictionary::new();
        t.insert(
            "type".into(),
            Value::String("public.utf8-plain-text".into()),
        );
        t.insert("offset".into(), Value::Integer(10.into()));
        t.insert("dataSize".into(), Value::Integer(5.into()));
        objects.push(Value::Dictionary(t));
        let mut types = Dictionary::new();
        types.insert(
            "NS.keys".into(),
            Value::Array(vec![Value::String("public.utf8-plain-text".into())]),
        );
        types.insert(
            "NS.objects".into(),
            Value::Array(vec![Value::Uid(Uid::new(1))]),
        );
        objects.push(Value::Dictionary(types));
        let mut item = Dictionary::new();
        item.insert("types".into(), Value::Uid(Uid::new(2)));
        objects.push(Value::Dictionary(item));
        let mut info = Dictionary::new();
        info.insert("items".into(), Value::Array(vec![Value::Uid(Uid::new(3))]));
        info.insert("dataFile".into(), Value::Data(vec![1, 2, 3, 4]));
        objects.push(Value::Dictionary(info));
        let mut top = Dictionary::new();
        top.insert("root".into(), Value::Uid(Uid::new(4)));
        let mut root = Dictionary::new();
        root.insert("$archiver".into(), Value::String("NSKeyedArchiver".into()));
        root.insert("$top".into(), Value::Dictionary(top));
        root.insert("$objects".into(), Value::Array(objects.clone()));
        let mut bytes = Vec::new();
        Value::Dictionary(root.clone())
            .to_writer_binary(&mut bytes)
            .unwrap();
        assert!(decode_wrapper(&bytes).is_err());

        // Also test negative dataSize.
        if let Value::Dictionary(ref mut d) = objects[1] {
            d.insert("offset".into(), Value::Integer(0.into()));
            d.insert("dataSize".into(), Value::Integer((-1i64).into()));
        }
        root.insert("$objects".into(), Value::Array(objects));
        let mut neg_bytes = Vec::new();
        Value::Dictionary(root)
            .to_writer_binary(&mut neg_bytes)
            .unwrap();
        assert!(decode_wrapper(&neg_bytes).is_err());
    }

    fn archive_with_types(types: Vec<Dictionary>, data_file: Vec<u8>) -> Vec<u8> {
        let mut objects: Vec<Value> = vec![Value::String("$null".into())];
        let n = types.len() as u64;
        let mut keys = Vec::new();
        let mut uids = Vec::new();
        for (i, t) in types.into_iter().enumerate() {
            objects.push(Value::Dictionary(t));
            keys.push(Value::String(format!("public.t{i}")));
            uids.push(Value::Uid(Uid::new(i as u64 + 1)));
        }
        let mut types_dict = Dictionary::new();
        types_dict.insert("NS.keys".into(), Value::Array(keys));
        types_dict.insert("NS.objects".into(), Value::Array(uids));
        objects.push(Value::Dictionary(types_dict));
        let mut item = Dictionary::new();
        item.insert("types".into(), Value::Uid(Uid::new(n + 1)));
        objects.push(Value::Dictionary(item));
        let mut info = Dictionary::new();
        info.insert(
            "items".into(),
            Value::Array(vec![Value::Uid(Uid::new(n + 2))]),
        );
        info.insert("dataFile".into(), Value::Data(data_file));
        objects.push(Value::Dictionary(info));
        let mut top = Dictionary::new();
        top.insert("root".into(), Value::Uid(Uid::new(n + 3)));
        let mut root = Dictionary::new();
        root.insert("$archiver".into(), Value::String("NSKeyedArchiver".into()));
        root.insert("$top".into(), Value::Dictionary(top));
        root.insert("$objects".into(), Value::Array(objects));
        let mut bytes = Vec::new();
        Value::Dictionary(root)
            .to_writer_binary(&mut bytes)
            .unwrap();
        bytes
    }

    fn type_range(offset: Value, size: i64) -> Dictionary {
        let mut t = Dictionary::new();
        t.insert("offset".into(), offset);
        t.insert("dataSize".into(), Value::Integer(size.into()));
        t
    }

    #[test]
    fn rejects_negative_wrapped_offset() {
        let mut wrapped = Dictionary::new();
        wrapped.insert("NS.integer".into(), Value::Integer((-1i64).into()));
        let bytes = archive_with_types(
            vec![type_range(Value::Dictionary(wrapped), 2)],
            vec![1, 2, 3, 4],
        );
        let err = decode_wrapper(&bytes).unwrap_err();
        assert!(format!("{err:#}").contains("negative"), "got: {err:#}");
    }

    #[test]
    fn rejects_many_types_sharing_one_large_range() {
        // Each type is within bounds, but together they would expand ~20 MiB.
        let mib = 1024 * 1024;
        let types = (0..20)
            .map(|_| type_range(Value::Integer(0.into()), mib as i64))
            .collect();
        let bytes = archive_with_types(types, vec![7u8; mib]);
        let Err(err) = decode_wrapper(&bytes) else {
            panic!("budget must reject");
        };
        assert!(format!("{err:#}").contains("decoded"), "got: {err:#}");
    }

    #[test]
    fn non_dictionary_root_error_names_its_kind() {
        let mut top = Dictionary::new();
        top.insert("root".into(), Value::Uid(Uid::new(1)));
        let mut root = Dictionary::new();
        root.insert("$archiver".into(), Value::String("NSKeyedArchiver".into()));
        root.insert("$top".into(), Value::Dictionary(top));
        root.insert(
            "$objects".into(),
            Value::Array(vec![
                Value::String("$null".into()),
                Value::String("some text".into()),
            ]),
        );
        let mut bytes = Vec::new();
        Value::Dictionary(root)
            .to_writer_binary(&mut bytes)
            .unwrap();
        let err = decode_wrapper(&bytes).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("root object is a string"), "got: {msg}");
        assert!(!msg.contains("some text"), "contents must not leak: {msg}");
    }

    #[test]
    fn rejects_cyclic_uid_and_truncated_plist() {
        let mut objects: Vec<Value> = vec![Value::String("$null".into())];
        let mut info = Dictionary::new();
        info.insert("pbinfo".into(), Value::Uid(Uid::new(1)));
        objects.push(Value::Dictionary(info));
        let mut top = Dictionary::new();
        top.insert("root".into(), Value::Uid(Uid::new(1)));
        let mut root = Dictionary::new();
        root.insert("$archiver".into(), Value::String("NSKeyedArchiver".into()));
        root.insert("$top".into(), Value::Dictionary(top));
        root.insert("$objects".into(), Value::Array(objects));
        let mut bytes = Vec::new();
        Value::Dictionary(root)
            .to_writer_binary(&mut bytes)
            .unwrap();
        assert!(decode_wrapper(&bytes).is_err());
        assert!(decode_wrapper(b"bplist00truncated").is_err());
    }

    // `plist` 1.10.1's binary reader has no API to force an actual shared
    // offset-table entry from a `Value` tree (its writer has no notion of
    // object identity), so the exact "revisited shared object" expansion
    // CodeRabbit flagged cannot be constructed through the available crate
    // API. This instead proves the mechanism the fix adds: `decode_wrapper`
    // now charges a node budget while building the `Value` tree itself
    // (`decode_plist_bounded`), before `Archive` ever runs, so a flat plist
    // that is small on the wire but exceeds that budget is rejected at parse
    // time with a distinct message from `Archive`'s own (post-parse) one.
    #[test]
    fn rejects_a_plist_whose_raw_node_count_exceeds_the_parse_time_budget() {
        let filler: Vec<Value> = (0..MAX_NODES + 100)
            .map(|i| Value::String(format!("filler-{i}")))
            .collect();
        let mut bytes = Vec::new();
        Value::Array(filler).to_writer_binary(&mut bytes).unwrap();
        assert!(
            bytes.len() <= MAX_ARCHIVE_BYTES,
            "fixture must stay under the wire-size cap to prove this is a \
             count limit, not the pre-existing byte-size one"
        );

        let err = decode_wrapper(&bytes).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("node budget") && msg.contains("while parsing"),
            "expected a parse-time node budget error, got: {msg}"
        );
    }
}
