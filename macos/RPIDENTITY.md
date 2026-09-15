# RPIdentity export — the long-term Pair-Verify identity

`macos/export-keys.sh` exports only the **BLE Continuity AES keys** (keychain
service `com.apple.continuity.encryption`). Those decrypt the BLE advert that
says "a copy is available". They do **not** get us the clipboard *content*.

The content travels over **companion-link** (`_companion-link._tcp`, over
`awdl0`), and companion-link runs a **HAP-style Pair-Verify** handshake first
(see `../src/companion.rs`). Pair-Verify authenticates each side with a
**long-term Ed25519 keypair** — a completely different key from the BLE AES key.
This document covers exporting/producing that identity.

## Why Apple devices trust each other (the "trusted device" question)

Every device signed into an Apple ID publishes its **public** long-term identity
into the **iCloud keychain**, synced to all same-account devices, under the
generic-password service:

```
service:  RPIdentity-SameAccountDevice
value:    OPACK({ "edPK": <32-byte Ed25519 public key>,
                  "dIRK": <16-byte device IRK> })
attrs:    kSecClassGenericPassword, synchronizable=true,
          account=<UUID>, label=<device name>, access group com.apple.rapport
```

(Confirmed from seemoo-lab `handoff-authentication-swift`
`MacKeychainController.swift` + `rp-identities.plist`.)

During Pair-Verify each side signs `ownEphemeralPub || peerEphemeralPub` with its
long-term Ed25519 **secret** key; the other side verifies with the corresponding
**edPK it already holds in its RPIdentity list**. So:

- To **verify a peer**, we need that peer's `edPK`. Those are exactly the synced
  RPIdentity items — we read them out and put them in `peers[]`.
- To **be accepted by a peer**, *our* `edPK` must already be in *the peer's*
  RPIdentity list. Because the list is iCloud-synced across the account, adding a
  `RPIdentity-SameAccountDevice` item locally propagates our public key to every
  same-account device, which then trusts our signatures.

The crucial asymmetry: **the synced item contains only the PUBLIC key** (`edPK`).
The matching **private** signing key (`edSK`) is *not* in that item — it lives
elsewhere on each device and is what the probe below investigates.

## Two approaches

### (a) Export an existing device's own `edSK` and impersonate it
Dump some real device's long-term signing **private** key and load it as our
`ed_sk`. Then we *are* that device to the peer, and no injection is needed.

**Problem:** the RPIdentity synced item never carries `edSK`. The private key is
held separately by `rapportd`, and on modern hardware (T2 / Apple Silicon) an
identity key like this is a prime candidate for **Secure-Enclave protection**:
generated in the SEP, usable only via `SecKeyCreateSignature`, and
**non-exportable** — `SecKeyCopyExternalRepresentation` returns `NULL` for a SEP
key. If that is the case (very likely), approach (a) is a dead end. The probe
below is exactly how you confirm this on your Mac before betting on (a).

### (b) Generate our own keypair and inject its public key  — **RECOMMENDED**
Mint a fresh Ed25519 keypair, keep the **private** seed locally (in
`rpidentity.json`), and `SecItemAdd` its **public** key as a new
`RPIdentity-SameAccountDevice` item so all same-account devices accept us. This
is precisely what seemoo `MacKeychainController.createNewRPIdentityItem` does,
and it sidesteps the SEP problem entirely because we never need any existing
device's private key — we own ours in software.

**Recommendation: (b).** It does not depend on the (probably unexportable) `edSK`,
it matches `PairingIdentity::load` cleanly (we control `ed_sk`), and it has a
working reference implementation. Approach (a) is retained in the tooling only as
an opportunistic `--from-self-dump` path, clearly marked UNVERIFIED, in case a
given macOS version does leave an `edSK` readable.

## `rpidentity.json` schema (EXACT — matches `PairingIdentity::load`)

`src/companion.rs::PairingIdentity::load` deserializes this shape (hex strings):

```json
{
  "ed_sk": "<64 hex chars>",
  "dirk":  "<32 hex chars>",
  "peers": [
    { "label": "iPhone", "edpk": "<64 hex chars>" }
  ]
}
```

| field         | bytes | meaning                                                        | required |
|---------------|-------|----------------------------------------------------------------|----------|
| `ed_sk`       | 32    | **our** Ed25519 *seed* (ed25519-dalek `SigningKey::from_bytes`) | yes      |
| `dirk`        | 16    | **our** device IRK; defaults to all-zero if absent             | no       |
| `peers[].edpk`| 32    | a peer device's `edPK` (its long-term public key)              | 0+       |
| `peers[].label`| —    | free-form device name, for logging only                        | no (defaults "") |

Watch the size mismatch: a **libsodium `edSK` is 64 bytes** (`seed32 || pub32`).
`ed_sk` here is the **32-byte seed only** — the first half. The tooling handles
this (`--seed` accepts 32 or 64 and keeps 32; `--from-self-dump` keeps the first
32 of any 64-byte edSK it finds).

## The tooling

Three files, mirroring the BLE exporter's mechanics (secure dir under
`~/Library/Application Support/ac-dc`, mode 600, Time Machine exclusion,
one-shot auto-wipe LaunchAgent):

- **`export-rpidentity.sh`** — orchestrator. Reads peer edPKs (Path A: plain
  `security`; Path B: Frida on `rapportd`), runs the injector, assembles
  `rpidentity.json`, arms the auto-wipe.
- **`inject-rpidentity.swift`** — CryptoKit keygen + OPACK encode + `SecItemAdd`
  of the synchronizable `RPIdentity-SameAccountDevice` item; writes our private
  seed to `identity-self.json`. (The plain `security` CLI cannot create a
  synchronizable binary item with the extra `pdmn`/`tomb`/syncViewHint attrs,
  hence a small Security.framework helper.) Supports `--dry-run` (mint without
  touching the keychain) and `--no-access-group` (fallback if the
  `com.apple.rapport` access group is refused).
- **`rpidentity-to-json.py`** — parses the peer dump (plist / Frida-JSON /
  raw-OPACK / security blob), merges our `identity-self.json`, emits
  `rpidentity.json`. Pure stdlib, includes a small OPACK decoder.

Typical run (approach b):

```bash
# 1. capture peers via Frida (Path B), producing rp-dump.json  (see script output)
# 2. generate + inject + assemble:
./macos/export-rpidentity.sh --from-dump "$HOME/Library/Application Support/ac-dc/rp-dump.json"
# On Linux: use the exported rpidentity.json from a secure transfer or read-only macOS mount.
ac-dc <companion-link subcommand> --identity rpidentity.json     # once the socket driver lands
```

## PROBE: is the local device's `edSK` exportable, or SEP-protected?

This answers the #1 unknown (do we even *have* an option (a)?). Run on the Mac.

**Step 1 — confirm the synced RPIdentity item is public-only.** It should show
`edPK` + `dIRK` and no private key:

```bash
# Try the plain CLI (usually empty for synchronizable items — that's expected):
security find-generic-password -s "RPIdentity-SameAccountDevice" -g 2>&1 | head
# Reliable view: Frida-dump rapportd (see export-rpidentity.sh Path B) and grep:
python3 - <<'PY'
import json; d=json.load(open("rp-dump.json"))
print("edSK present anywhere:", "edSK" in json.dumps(d))
PY
```

If `edSK present anywhere: False`, the private key is **not** in the synced
keychain — expected, and already reason enough to prefer (b).

**Step 2 — hunt for a private signing key and test exportability.** `rapportd`
holds the private key; find its keychain item and try to export the raw bytes.
The tell-tale of SEP protection is that the data cannot be read out:

```bash
# a) Look for candidate private-key / rapport items:
security dump-keychain 2>/dev/null | grep -iE 'rapport|remotepair|RPIdentity|alloy' | head
# b) Frida-hook rapportd and watch what it does with the signing key. If the key
#    is a SecKeyRef backed by the Secure Enclave, exporting it fails:
#      - SecKeyCopyExternalRepresentation(key, &err) returns NULL, and
#      - err is errSecUnimplemented / "not exportable" / kSecAttrTokenIDSecureEnclave
#    whereas a software key returns the raw 32/64 bytes.
```

A Frida one-liner (attach to `rapportd`, hook the export call):

```javascript
// probe-edsk.js —  frida -n rapportd -l probe-edsk.js  (SIP disabled)
const f = Module.findExportByName('Security', 'SecKeyCopyExternalRepresentation');
Interceptor.attach(f, {
  onLeave(ret) {
    console.log('SecKeyCopyExternalRepresentation ->',
      ret.isNull() ? 'NULL  (SEP-protected / non-exportable)' : 'data (SOFTWARE key, exportable!)');
  }
});
// Also check whether the signing key advertises the Secure Enclave token:
const attrs = Module.findExportByName('Security', 'SecKeyCopyAttributes');
Interceptor.attach(attrs, { onLeave() { /* inspect kSecAttrTokenID == kSecAttrTokenIDSecureEnclave */ } });
```

**Interpretation:**
- `NULL` / `errSecUnimplemented` / `kSecAttrTokenIDSecureEnclave` present →
  `edSK` is **SEP-protected and non-exportable** → approach (a) is impossible;
  use (b).
- Raw bytes returned → the key is a **software key** → approach (a) is *possible*
  on this machine (feed the 64-byte blob to `rpidentity-to-json.py
  --from-self-dump`, which keeps the first 32 as the seed). Still verify it is an
  Ed25519 signing key (32-byte pub) and not a Curve25519 agreement key.

## Honest unknowns / risks

- **`edSK` exportability (probe target).** Strongly expected to be
  SEP-protected on T2/Apple-Silicon Macs → (a) infeasible. Unverified until the
  probe is run on real hardware; **this whole file is untested against a real
  device.**
- **Injection may be refused.** `SecItemAdd` into the `com.apple.rapport` access
  group likely requires that entitlement → `errSecMissingEntitlement (-34018)`.
  The injector falls back to a no-access-group item (seemoo's "Eve's little
  brother"), but see the next point.
- **Whether peers honour our injected identity is the real open question.**
  seemoo demonstrated the *item can be written*; whether a *current* (2026)
  iPhone/macOS actually accepts a same-account device solely on the basis of a
  synced RPIdentity item — vs. also requiring an Apple-ID/IDS attestation,
  circle-of-trust membership, or an access-group-scoped item — is **unverified**.
  If peers require more than the public key being present, (b) as written may be
  rejected even with correct crypto. This mirrors the "same-account trust" risk
  already called out in `docs/architecture.md`.
- **OPACK value exactness.** We encode `{edPK, dIRK}` with the short-form OPACK
  that matches `src/opack.rs` and the seemoo encoder. If a newer macOS expects
  extra fields in the value (e.g. a model/flags sub-dict, as `gena` hints in
  `rp-identities.plist`), the item may parse but be ignored. Unverified.
- **`dirk` semantics.** We mint a random 16-byte `dIRK`. Pair-Verify in
  `companion.rs` does not currently use `device_irk`, so a random value is
  harmless there; it matters only for BLE identity-resolving, which is out of
  scope for the content fetch.
- **Reading peers via the plain `security` CLI returns at most one item** and
  usually none (synchronizable). The Frida path is the reliable one, and needs
  SIP disabled — the same prerequisite as the existing BLE export Path B.
