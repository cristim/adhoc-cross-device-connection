//! Safe `NSKeyedArchiver` binary-plist decoder for `UASharedPasteboardInfoWrapper`.
//!
//! Decodes Apple Universal Clipboard's `rActPayload` (`UASharedPasteboardInfoWrapper`)
//! using a strict `$classname` allowlist, active-stack cycle detection, a total
//! node-expansion budget, and checked `offset + size` end-exclusive slicing.

use anyhow::{bail, ensure, Context, Result};
use plist::{Dictionary, Uid, Value};
use std::collections::HashSet;
use std::io::Cursor;

use crate::companion_client::{Pasteboard, PasteboardItem};

const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 4096;
const MAX_ARCHIVE_BYTES: usize = 4 * 1024 * 1024;

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
            Value::Dictionary(d) => {
                if let Some(u) = d.get("NS.integer").and_then(Value::as_unsigned_integer) {
                    Some(usize::try_from(u).context("integer overflows usize")?)
                } else {
                    None
                }
            }
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
    let root_val = Value::from_reader(Cursor::new(bytes)).context("parsing binary plist")?;
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

fn decode_root(ar: &mut Archive<'_>, root_uid: &Uid) -> Result<Pasteboard> {
    let root_dict = ar
        .resolve_raw(root_uid, 0)?
        .as_dictionary()
        .context("NSKeyedArchiver root object is not a dictionary")?;

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
                            blob.get(offset..end)
                                .with_context(|| {
                                    format!(
                                        "type slice {offset}..{end} out of bounds for dataFile ({} bytes)",
                                        blob.len()
                                    )
                                })?
                                .to_vec()
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

/// Encode a [`Pasteboard`] as a synthetic binary `NSKeyedArchiver`
/// `UASharedPasteboardInfoWrapper` plist for loopback testing.
pub fn encode_wrapper_for_test(pb: &Pasteboard) -> Result<Vec<u8>> {
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

    let mut blob = Vec::new();
    let mut type_dict_keys = Vec::new();
    let mut type_dict_objs = Vec::new();

    for it in &pb.items {
        let offset = blob.len() as u64;
        blob.extend_from_slice(&it.data);
        let size = it.data.len() as u64;
        let uti_uid = push_obj(&mut objects, Value::String(it.uti.clone()));
        let mut t_info = Dictionary::new();
        t_info.insert("$class".into(), Value::Uid(cls_type));
        t_info.insert("type".into(), Value::Uid(uti_uid));
        t_info.insert("offset".into(), Value::Integer(offset.into()));
        t_info.insert("dataSize".into(), Value::Integer(size.into()));
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
    item_dict.insert("types".into(), Value::Uid(types_uid));
    let item_uid = push_obj(&mut objects, Value::Dictionary(item_dict));

    let mut items_nsarray = Dictionary::new();
    items_nsarray.insert("$class".into(), Value::Uid(cls_array));
    items_nsarray.insert(
        "NS.objects".into(),
        Value::Array(vec![Value::Uid(item_uid)]),
    );
    let items_uid = push_obj(&mut objects, Value::Dictionary(items_nsarray));

    let mut pbinfo = Dictionary::new();
    pbinfo.insert("$class".into(), Value::Uid(cls_info));
    pbinfo.insert("items".into(), Value::Uid(items_uid));
    let pbinfo_uid = push_obj(&mut objects, Value::Dictionary(pbinfo));

    let data_uid = push_obj(&mut objects, Value::Data(blob));
    let mut wrapper = Dictionary::new();
    wrapper.insert("$class".into(), Value::Uid(cls_wrapper));
    wrapper.insert("pbinfo".into(), Value::Uid(pbinfo_uid));
    wrapper.insert("dataFile".into(), Value::Uid(data_uid));
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
}
