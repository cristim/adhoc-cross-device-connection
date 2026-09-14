#!/usr/bin/env python3
"""Convert a seemoo-lab keychain_access.py dump into ac-dc's keys.json.

The Frida tool (apple-continuity-tools/keychain_access) hooks
SecItemCopyMatching in rapportd and writes every accessed keychain item to a
JSON file. For the Continuity BLE keys, the returned item data is a *binary
plist* containing at least:

    keyData        <data>    the AES key we need
    keyIdentifier  <string>  UUID naming the key
    isWrappedKey   <bool>    True => still wrapped, NOT directly usable
    lastUsedCounter <int>

The exact JSON shape of the dump varies slightly by macOS version (the returned
data may appear as a "0x…" hex string, base64, or an already-decoded dict), so
this script does NOT assume fixed field names. It walks the whole dump, finds
anything that decodes to a plist dict carrying `keyData`, and emits:

    { "keys": [ { "id": "<keyIdentifier>", "key": "<keyData hex>" }, ... ] }

Usage:
    python3 dump-to-keys.py dump.json -o keys.json
"""

import argparse
import base64
import binascii
import json
import plistlib
import sys


def iter_blob_candidates(node):
    """Yield raw byte strings that might be a serialized plist, from anywhere
    in the dump. Handles 0x-hex strings, plain hex, and base64."""
    if isinstance(node, dict):
        for v in node.values():
            yield from iter_blob_candidates(v)
    elif isinstance(node, list):
        for v in node:
            yield from iter_blob_candidates(v)
    elif isinstance(node, str):
        s = node.strip()
        # 0x-prefixed hex (the format keychain_access.py uses for returned data)
        if s.lower().startswith("0x"):
            try:
                yield binascii.unhexlify(s[2:])
            except binascii.Error:
                pass
            return
        # bare hex
        if len(s) >= 16 and len(s) % 2 == 0 and all(c in "0123456789abcdefABCDEF" for c in s):
            try:
                yield binascii.unhexlify(s)
            except binascii.Error:
                pass
        # base64 (bplist00 encodes as "YnBsaXN0MDА…")
        if s.startswith("YnBsaXN0"):
            try:
                yield base64.b64decode(s)
            except (binascii.Error, ValueError):
                pass


def iter_decoded_dicts(node):
    """Yield dicts that already contain a `keyData`-like field, in case a macOS
    version's tooling decoded the plist into JSON for us."""
    if isinstance(node, dict):
        if any(k in node for k in ("keyData", "keydata", "v_Data")):
            yield node
        for v in node.values():
            yield from iter_decoded_dicts(v)
    elif isinstance(node, list):
        for v in node:
            yield from iter_decoded_dicts(v)


def as_bytes(value):
    """Coerce a keyData value (bytes, plist Data, hex str, base64 str) to bytes."""
    if isinstance(value, (bytes, bytearray)):
        return bytes(value)
    if isinstance(value, str):
        s = value.strip()
        if s.lower().startswith("0x"):
            return binascii.unhexlify(s[2:])
        try:
            return binascii.unhexlify(s)
        except binascii.Error:
            return base64.b64decode(s)
    raise ValueError(f"cannot interpret keyData of type {type(value).__name__}")


def extract_from_plist_dict(d):
    """Return (id, key_bytes, wrapped) from a decoded continuity key dict, or None."""
    key_field = None
    for k in ("keyData", "keydata", "v_Data"):
        if k in d:
            key_field = d[k]
            break
    if key_field is None:
        return None
    try:
        key = as_bytes(key_field)
    except (binascii.Error, ValueError):
        return None
    kid = d.get("keyIdentifier") or d.get("keyidentifier") or d.get("labl") or ""
    if not isinstance(kid, str):
        kid = str(kid)
    wrapped = bool(d.get("isWrappedKey", d.get("iswrappedkey", False)))
    return kid, key, wrapped


def main():
    ap = argparse.ArgumentParser(description="Convert a keychain_access dump to ac-dc keys.json")
    ap.add_argument("dump", help="dump.json from keychain_access.py")
    ap.add_argument("-o", "--output", default="keys.json", help="output keys.json (default: keys.json)")
    ap.add_argument("--include-wrapped", action="store_true",
                    help="also emit wrapped keys (NOT usable for BLE decrypt; for inspection only)")
    args = ap.parse_args()

    with open(args.dump, "rb") as f:
        raw = f.read()
    try:
        dump = json.loads(raw)
    except json.JSONDecodeError as e:
        sys.exit(f"error: {args.dump} is not valid JSON: {e}")

    found = {}   # keyIdentifier (or key-hex) -> (id, key_bytes, wrapped)

    # Path 1: blobs that decode to a binary plist with keyData.
    for blob in iter_blob_candidates(dump):
        if not blob.startswith(b"bplist") and not blob.lstrip().startswith(b"<?xml"):
            continue
        try:
            obj = plistlib.loads(blob)
        except Exception:
            continue
        dicts = obj if isinstance(obj, list) else [obj]
        for d in dicts:
            if not isinstance(d, dict):
                continue
            res = extract_from_plist_dict(d)
            if res:
                kid, key, wrapped = res
                found[kid or key.hex()] = (kid, key, wrapped)

    # Path 2: dicts already decoded to JSON.
    for d in iter_decoded_dicts(dump):
        res = extract_from_plist_dict(d)
        if res:
            kid, key, wrapped = res
            found.setdefault(kid or key.hex(), (kid, key, wrapped))

    keys = []
    skipped_wrapped = 0
    for kid, key, wrapped in found.values():
        if wrapped and not args.include_wrapped:
            skipped_wrapped += 1
            continue
        entry = {"id": kid, "key": key.hex()}
        if wrapped:
            entry["wrapped"] = True
        if len(key) != 16:
            entry["note"] = f"unexpected key length {len(key)} (BLE key is AES-128 = 16 bytes)"
        keys.append(entry)

    if not keys:
        sys.stderr.write(
            "No usable Continuity keys found in the dump.\n"
            "Hints:\n"
            "  * Make sure you toggled Handoff OFF then ON while keychain_access.py\n"
            "    was attached to rapportd, so the keys are re-read and captured.\n"
            "  * Try --include-wrapped to see whether only wrapped keys were seen.\n"
            "  * Inspect the dump: the item you want has service\n"
            "    'com.apple.continuity.encryption' and a keyData field.\n"
        )
        sys.exit(1)

    with open(args.output, "w") as f:
        json.dump({"keys": keys}, f, indent=2)
        f.write("\n")

    print(f"Wrote {len(keys)} key(s) to {args.output}"
          + (f" ({skipped_wrapped} wrapped key(s) skipped; use --include-wrapped to inspect)"
             if skipped_wrapped else ""))
    for k in keys:
        flags = " [WRAPPED]" if k.get("wrapped") else ""
        print(f"  id={k['id'] or '(none)'} len={len(k['key'])//2}B{flags}")


if __name__ == "__main__":
    main()
