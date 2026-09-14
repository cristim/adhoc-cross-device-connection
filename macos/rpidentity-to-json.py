#!/usr/bin/env python3
"""Assemble ac-dc's rpidentity.json from an RPIdentity keychain dump.

Companion-link Pair-Verify (HAP-style, see ../src/companion.rs) authenticates
each side with a long-term Ed25519 keypair. Apple devices trust one another
because every same-account device's PUBLIC identity is synced in the iCloud
keychain under service `RPIdentity-SameAccountDevice`: an OPACK blob carrying

    edPK   <data>   32-byte Ed25519 public key  (the device's long-term key)
    dIRK   <data>   16-byte device IRK           (BLE identity-resolving key)

(see seemoo-lab/handoff-authentication-swift MacKeychainController.swift and its
rp-identities.plist sample.) NOTE the synced item holds only the PUBLIC key;
the matching PRIVATE signing key (edSK) is NOT here — it lives elsewhere and is
very likely SEP-protected (see macos/RPIDENTITY.md).

This tool builds the `rpidentity.json` that `companion.rs::PairingIdentity::load`
expects, whose EXACT schema is:

    {
      "ed_sk": "<64 hex chars>",          # OUR 32-byte Ed25519 seed (private)
      "dirk":  "<32 hex chars>",          # OUR 16-byte device IRK (optional)
      "peers": [                          # every OTHER same-account device
        { "label": "iPhone", "edpk": "<64 hex chars>" },   # 32-byte edPK
        ...
      ]
    }

`ed_sk` is the 32-byte Ed25519 *seed* (ed25519-dalek `SigningKey::from_bytes`),
NOT the 64-byte libsodium secret key. If an edSK ever turns up in a dump it is
64 bytes (seed32 || pub32) and we keep the first 32.

Two ways to populate `ed_sk`:

  (b) GENERATE our own keypair (recommended). `inject-rpidentity.swift` mints a
      CryptoKit Ed25519 key, injects its public half into the iCloud keychain as
      a new RPIdentity-SameAccountDevice item, and writes the private seed to a
      small `identity-self.json`. Pass that file with `--self`.

  (a) EXPORT an existing device's edSK and impersonate it. Pass `--from-self-dump`
      pointing at a dump that (improbably) contains an edSK. Marked UNVERIFIED;
      almost certainly blocked by the Secure Enclave — see the probe in
      macos/RPIDENTITY.md.

Either way, the PEER public keys come from the same RPIdentity dump.

Usage:
    # approach (b): merge our generated identity with the peers in the dump
    python3 rpidentity-to-json.py --self identity-self.json --dump dump.json -o rpidentity.json

    # peers only (e.g. to inspect what would be trusted)
    python3 rpidentity-to-json.py --dump dump.json -o rpidentity.json

    # approach (a): try to lift an edSK out of a dump (UNVERIFIED)
    python3 rpidentity-to-json.py --from-self-dump dump.json --dump dump.json -o rpidentity.json
"""

import argparse
import base64
import binascii
import json
import plistlib
import sys

RP_SERVICE = "RPIdentity-SameAccountDevice"


# --------------------------------------------------------------------------
# Minimal OPACK decoder (port of the subset in ../src/opack.rs). The keychain
# value data for an RPIdentity item is an OPACK-encoded dict {edPK, dIRK}. Some
# dump paths hand us that dict already decoded (e.g. a plist `v_Data` dict), so
# we only need OPACK when the value arrives as raw bytes.
# --------------------------------------------------------------------------
class _OpackDecoder:
    def __init__(self, data):
        self.d = data
        self.i = 0

    def _take(self, n):
        if self.i + n > len(self.d):
            raise ValueError("OPACK: unexpected end of data")
        s = self.d[self.i:self.i + n]
        self.i += n
        return s

    def _be(self, n):
        v = 0
        for b in self._take(n):
            v = (v << 8) | b
        return v

    def parse(self):
        t = self._take(1)[0]
        if t == 0x01:
            return True
        if t == 0x02:
            return False
        if 0x08 <= t <= 0x2F:
            return t - 0x08
        if 0x30 <= t <= 0x33:
            return self._be(t - 0x30 + 1)
        if 0x40 <= t <= 0x60:
            return self._take(t - 0x40).decode("utf-8", "replace")
        if 0x61 <= t <= 0x64:
            ln = self._be(t - 0x60)
            return self._take(ln).decode("utf-8", "replace")
        if 0x70 <= t <= 0x90:
            return bytes(self._take(t - 0x70))
        if 0x91 <= t <= 0x94:
            ln = self._be(t - 0x90)
            return bytes(self._take(ln))
        if 0xD0 <= t <= 0xDE:
            return [self.parse() for _ in range(t - 0xD0)]
        if t == 0xDF:
            out = []
            while self.d[self.i] != 0x03:
                out.append(self.parse())
            self.i += 1
            return out
        if 0xE0 <= t <= 0xEE:
            return {self._key(self.parse()): self.parse() for _ in range(t - 0xE0)}
        if t == 0xEF:
            out = {}
            while self.d[self.i] != 0x03:
                k = self._key(self.parse())
                out[k] = self.parse()
            self.i += 1
            return out
        raise ValueError(f"OPACK: unknown type byte {t:#04x}")

    @staticmethod
    def _key(k):
        return k if isinstance(k, str) else repr(k)


def opack_decode(data):
    return _OpackDecoder(data).parse()


# --------------------------------------------------------------------------
# byte coercion (shared shape with dump-to-keys.py)
# --------------------------------------------------------------------------
def as_bytes(value):
    """Coerce a value (bytes, plist Data, 0x-hex, hex, base64) to bytes."""
    if isinstance(value, (bytes, bytearray)):
        return bytes(value)
    if isinstance(value, str):
        s = value.strip().replace(" ", "").replace("\n", "")
        # <aa bb cc> plist-style hex sometimes copied verbatim
        if s.startswith("<") and s.endswith(">"):
            s = s[1:-1]
        if s.lower().startswith("0x"):
            return binascii.unhexlify(s[2:])
        # try hex first, then base64
        try:
            if len(s) % 2 == 0 and all(c in "0123456789abcdefABCDEF" for c in s):
                return binascii.unhexlify(s)
        except binascii.Error:
            pass
        return base64.b64decode(s)
    raise ValueError(f"cannot interpret value of type {type(value).__name__}")


def value_data_to_edpk_dirk(v_data):
    """From an RPIdentity item's value data (a dict, or OPACK bytes/str),
    return (edpk_bytes, dirk_bytes_or_None), or None if it doesn't parse."""
    d = None
    if isinstance(v_data, dict):
        d = v_data
    else:
        try:
            raw = as_bytes(v_data)
        except (binascii.Error, ValueError):
            return None
        # A binary/xml plist dict?
        if raw[:6] == b"bplist" or raw.lstrip()[:5] == b"<?xml":
            try:
                d = plistlib.loads(raw)
            except Exception:
                d = None
        if d is None:
            try:
                d = opack_decode(raw)
            except (ValueError, IndexError):
                return None
    if not isinstance(d, dict):
        return None
    edpk = d.get("edPK") or d.get("edpk")
    dirk = d.get("dIRK") or d.get("dirk")
    if edpk is None:
        return None
    try:
        edpk_b = as_bytes(edpk)
    except (binascii.Error, ValueError):
        return None
    dirk_b = None
    if dirk is not None:
        try:
            dirk_b = as_bytes(dirk)
        except (binascii.Error, ValueError):
            dirk_b = None
    if len(edpk_b) != 32:
        return None
    return edpk_b, dirk_b


def iter_rp_items(node):
    """Yield dicts that look like an RPIdentity-SameAccountDevice keychain item,
    from anywhere in a dump (plist, Frida JSON, nested lists/dicts)."""
    if isinstance(node, dict):
        svce = node.get("svce") or node.get("service") or node.get("kSecAttrService")
        if svce == RP_SERVICE or any(
            (isinstance(v, str) and v == RP_SERVICE) for v in node.values()
        ):
            yield node
        for v in node.values():
            yield from iter_rp_items(v)
    elif isinstance(node, list):
        for v in node:
            yield from iter_rp_items(v)


def item_value_data(item):
    for k in ("v_Data", "vdata", "keyData", "kSecValueData", "data", "value"):
        if k in item:
            return item[k]
    return None


def item_label(item):
    for k in ("labl", "label", "kSecAttrLabel", "acct", "account", "kSecAttrAccount"):
        v = item.get(k)
        if isinstance(v, str) and v:
            return v
    return ""


def find_self_edsk(node):
    """(Approach a) Yield any edSK-like private signing key found in a dump.
    libsodium edSK is 64 bytes (seed32 || pub32); we keep the first 32 (seed)."""
    if isinstance(node, dict):
        for k in ("edSK", "edsk", "edSecretKey", "signingSecretKey"):
            if k in node:
                try:
                    b = as_bytes(node[k])
                except (binascii.Error, ValueError):
                    b = None
                if b and len(b) in (32, 64):
                    yield b[:32], item_label(node)
        for v in node.values():
            yield from find_self_edsk(v)
    elif isinstance(node, list):
        for v in node:
            yield from find_self_edsk(v)


def load_dump(path):
    with open(path, "rb") as f:
        raw = f.read()
    if raw.lstrip()[:1] in (b"{", b"["):
        return json.loads(raw)
    if raw[:6] == b"bplist" or raw.lstrip()[:5] == b"<?xml":
        return plistlib.loads(raw)
    # last resort: try JSON, then plist
    try:
        return json.loads(raw)
    except json.JSONDecodeError:
        return plistlib.loads(raw)


def main():
    ap = argparse.ArgumentParser(description="Build ac-dc rpidentity.json from an RPIdentity keychain dump")
    ap.add_argument("--dump", help="keychain dump (Frida JSON, plist, or security-CLI blob) with RPIdentity items")
    ap.add_argument("--self", dest="self_file",
                    help="identity-self.json from inject-rpidentity.swift (approach b): {ed_sk, edpk, dirk}")
    ap.add_argument("--from-self-dump",
                    help="(approach a, UNVERIFIED) dump that may contain our own edSK to impersonate a device")
    ap.add_argument("-o", "--output", default="rpidentity.json", help="output file (default: rpidentity.json)")
    args = ap.parse_args()

    ed_sk = None
    dirk = None
    our_edpk = None

    # ---- our own identity -------------------------------------------------
    if args.self_file:
        with open(args.self_file) as f:
            s = json.load(f)
        ed_sk = s["ed_sk"].strip()
        dirk = (s.get("dirk") or "").strip() or None
        our_edpk = (s.get("edpk") or "").strip().lower() or None
        if len(bytes.fromhex(ed_sk)) != 32:
            sys.exit("error: --self ed_sk must be a 32-byte (64 hex char) Ed25519 seed")
    elif args.from_self_dump:
        cands = list(find_self_edsk(load_dump(args.from_self_dump)))
        if not cands:
            sys.exit(
                "error: no edSK found in --from-self-dump.\n"
                "This is EXPECTED: the RPIdentity synced item holds only the public\n"
                "edPK, and the private signing key is almost certainly SEP-protected.\n"
                "Use approach (b) instead (generate + inject); see macos/RPIDENTITY.md."
            )
        seed, lbl = cands[0]
        ed_sk = seed.hex()
        sys.stderr.write(f"WARNING (approach a, UNVERIFIED): using exported edSK from '{lbl or '?'}'.\n")

    # ---- peers ------------------------------------------------------------
    peers = []
    seen = set()
    if args.dump:
        dump = load_dump(args.dump)
        for item in iter_rp_items(dump):
            v = item_value_data(item)
            if v is None:
                continue
            parsed = value_data_to_edpk_dirk(v)
            if parsed is None:
                continue
            edpk_b, dirk_b = parsed
            edpk_hex = edpk_b.hex()
            if edpk_hex in seen:
                continue
            # Skip our own injected item so we don't list ourselves as a peer.
            if our_edpk and edpk_hex == our_edpk:
                continue
            seen.add(edpk_hex)
            peers.append({"label": item_label(item), "edpk": edpk_hex})

    if ed_sk is None:
        sys.stderr.write(
            "NOTE: no --self / --from-self-dump given, so ed_sk is a placeholder of\n"
            "all-zeros. Pair-Verify will NOT work until you provide our real seed\n"
            "(run inject-rpidentity.swift, then pass --self identity-self.json).\n"
        )
        ed_sk = "00" * 32

    out = {"ed_sk": ed_sk, "peers": peers}
    if dirk:
        out["dirk"] = dirk

    with open(args.output, "w") as f:
        json.dump(out, f, indent=2)
        f.write("\n")

    print(f"Wrote {args.output}: ed_sk set={'yes' if ed_sk != '00'*32 else 'PLACEHOLDER'}, "
          f"dirk={'yes' if dirk else 'no'}, peers={len(peers)}")
    for p in peers:
        print(f"  peer label={p['label'] or '(none)'} edpk={p['edpk'][:16]}...")
    if not peers:
        sys.stderr.write(
            "\nWARNING: no RPIdentity-SameAccountDevice peers found in the dump.\n"
            "  * Path A (plain `security`) often returns nothing for synchronizable\n"
            "    items; use the Frida path (see export-rpidentity.sh) to capture all.\n"
            "  * Confirm the dump has items with svce 'RPIdentity-SameAccountDevice'.\n"
        )


if __name__ == "__main__":
    main()
