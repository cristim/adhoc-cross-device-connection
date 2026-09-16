# Sapporo #87 and #48: lessons for ac-dc and brcmfmac-awdl

Research date: 2026-09-16. Scope: read both complete issues and their comments, follow relevant source references, and assess their usefulness to Rust Airdrop-compatible and Universal Clipboard. No hardware probes, credential reads, installations, or implementation changes were performed.

## Conclusion

These issues concern **Touch ID and the Secure Enclave (SEP)**. They contain no Airdrop-compatible or Universal Clipboard implementation, no AWDL discovery protocol, and no demonstrated way to obtain or provision a trusted Rapport identity. Their strongest contributions to our project are disciplined capability reporting, protocol error interpretation, evidence-driven diagnostics, and narrowly authorized privileged operations.

Do not make SEP support a prerequisite for our Airdrop-compatible UI or clipboard investigation. Our clipboard trust question is whether the actual Apple peer accepts our signing identity; biometric endpoint availability does not answer it. The existing [clipboard research](clipboard-research.md) identifies concrete wire-format problems that deserve attention first.

## Sources and reproducibility

Full GitHub API issue bodies and comments were retrieved, not just the rendered issue summaries. Both requested issues had one comment at retrieval. Research copies are under `/tmp/sapporo-research`; that directory is temporary and is not an application dependency.

| Source | Revision or stable comment |
| --- | --- |
| [Sapporo #87](https://github.com/stevederico/Sapporo/issues/87) | [Brent Kearney's technical correction](https://github.com/stevederico/Sapporo/issues/87#issuecomment-5536546453) |
| [Sapporo #48](https://github.com/stevederico/Sapporo/issues/48) | [Brent Kearney's later experimental findings](https://github.com/stevederico/Sapporo/issues/48#issuecomment-5543860841) |
| [T2 Touch ID source](https://github.com/jmurth1234/t2-touchid-linux/tree/ea46d8a0aef3e73b0e2f747aa18721dbcd265bce) | `ea46d8a0aef3e73b0e2f747aa18721dbcd265bce` |
| [Sapporo kernel](https://github.com/stevederico/Sapporo/tree/77c32fc95822b3b431ef559c2bd50c513e1d8eae) | `77c32fc95822b3b431ef559c2bd50c513e1d8eae` |
| [m1n1 Mesa tracer](https://github.com/AsahiLinux/m1n1/blob/b4654b32941d51afdb77579d63e7cb1aa6c03ecc/proxyclient/hv/trace_mesa.py) | `b4654b32941d51afdb77579d63e7cb1aa6c03ecc` |

Evidence labels below distinguish source inspection, reported hardware measurements, and proposed adaptations. None of the cited hardware results were reproduced on our machine.

## 1. What the issues establish—and what they do not

### #87: T2 transport is not a portable Touch ID service

The issue describes an Intel T2 stack using bridgeOS BiometricKit through BridgeXPC. The comment reports successful reuse of a low-level buffer-registration request on an M1 Pro, while the response status occupies a different field. It also demonstrates that acceptance of a buffer for an endpoint does not establish that the endpoint's application exists. The inspected transport source confirms separate control, AppleKeyStore and ACM operations; it is not an Airdrop-compatible implementation. These findings support comparing layers separately, not porting the entire T2 stack to Apple Silicon. [Issue and detailed comment](https://github.com/stevederico/Sapporo/issues/87#issuecomment-5536546453)

Crucial qualification: the comment says a T2 match does not reach SEP, but its code-level observation is narrower: this Linux transport exposes AKS/ACM while matching requests go to bridgeOS BiometricKit. That does not, by itself, establish everything bridgeOS does internally. We should not promote a host-side trace into a universal claim about biometric internals.

### #48: update the hypothesis when measurements disagree

The later comment explicitly corrects the issue body's boot-policy explanation: missing authentication applications were measured, but the input controlling their absence was not identified. The report covers one M1 Pro, modified Asahi kernel and device tree; it reports no reachable biometric application and no successful enable operation across its experiments. This is useful negative evidence for that setup, not proof of permanent impossibility on all Apple hardware or firmware. Neither missing biometric endpoints nor their caller-identity requirements prove that Rapport Ed25519 keys are unavailable or SEP-backed. [Experimental update](https://github.com/stevederico/Sapporo/issues/48#issuecomment-5543860841)

The source of the [Mesa tracer](https://github.com/AsahiLinux/m1n1/blob/b4654b32941d51afdb77579d63e7cb1aa6c03ecc/proxyclient/hv/trace_mesa.py) does explicitly note encryption after power-on. That corroborates a limitation of that particular sensor-observation approach; it supplies no clipboard key-extraction method.

### Some issue-body information is already stale

The pinned T2 [README](https://github.com/jmurth1234/t2-touchid-linux/blob/ea46d8a0aef3e73b0e2f747aa18721dbcd265bce/README.md) now describes authentication results on both `MacBookPro16,2` / bridgeOS `23P1072` and `MacBookPro15,2` / `23P350`, whereas the issue body describes one proven machine. Its feature table separately identifies exposed operations and hardware-tested operations. The additional hardware result does not validate every operation on the second machine. This is a good model for our own release notes.

## 2. Specific ideas worth implementing in Rust

### A. Capability evidence instead of one “ready” flag

The T2 [readiness classifier](https://github.com/jmurth1234/t2-touchid-linux/blob/ea46d8a0aef3e73b0e2f747aa18721dbcd265bce/src/t2_user_readiness.py) is a pure function over collected evidence, returns a structured state and next step, and distinguishes malformed evidence from merely unavailable capability. Unknown state bits do not silently become success.

**Proposed adaptation:** keep `awdlctl status` as a radio report, then let ac-dc separately report:

- Interface configured and usable IPv6 address present.
- Peer discovered, with discovery source and observation age.
- TCP/TLS connection established.
- Transfer request accepted or declined.
- Payload completely received and saved.
- For clipboard: peer signature verified, our identity accepted, metadata retrieved, content decoded, clipboard owned.

A running service or configured interface should never display “Airdrop-compatible works.” Keep unknown, failed, unsupported, and not-yet-tested distinct in the UI. Track successful Mac and iPhone tests independently, and keep clipboard trust separate from Airdrop-compatible Everyone mode.

### B. Preserve error domains and transaction correlation

The inspected [T2 transport](https://github.com/jmurth1234/t2-touchid-linux/blob/ea46d8a0aef3e73b0e2f747aa18721dbcd265bce/src/t2_sep_transport.c) validates sizes, waits for the matching endpoint/tag, bounds unrelated replies, and treats remote failure separately from transport failure. Its reply interpretation is platform-specific, as #87's comment demonstrates.

**Proposed adaptation:** retain distinct Rust errors for OS/socket failure, netlink failure, Broadcom firmware status, protocol rejection, authentication rejection, and timeout. Include operation and stage without exposing secret data. Our previous `-30` output interpreted a numeric failure as a filesystem error; overlapping namespaces should be made explicit rather than letting an unrelated errno phrase dominate the UI. Do not invent a firmware-name mapping when the transport does not establish the namespace. Validate Companion transaction IDs and expected message types before accepting responses.

### C. Keep the UI unprivileged and authorize narrowly

The T2 [PolicyKit collector](https://github.com/jmurth1234/t2-touchid-linux/blob/ea46d8a0aef3e73b0e2f747aa18721dbcd265bce/src/t2_polkit_grant.py) binds authorization to a process identity including start time, checks identity again after authorization, bounds grant lifetime and rejects unsupported actions. This is concrete source evidence, not proof that the whole broker has been independently audited.

**Proposed adaptation:** the GTK application runs as the desktop user. Only the fixed radio-start/stop operation crosses the privileged boundary. Never authorize an arbitrary shell command, writable executable path, or argument string. Starting a receive window should not implicitly authorize driver installation, key export, or firmware experiments. Return a structured unavailable/denied/dismissed result and keep ordinary discovery and transfer UI responsive.

### D. Durable intent and reconciliation for recovery

The T2 [mutation journal](https://github.com/jmurth1234/t2-touchid-linux/blob/ea46d8a0aef3e73b0e2f747aa18721dbcd265bce/src/t2_mutation_journal.py) validates records, checks secret-bearing fields, and syncs file and directory data. Its domain-specific biometric journal is much heavier than our needs.

**Proposed adaptation:** retain the existing saved Wi-Fi-band recovery intent and reconcile it after interruption. For file transfers, use private incomplete files, publish the final name only after successful completion, and clean up abandoned partials. Keep logs to transfer IDs, counts and stages; never log clipboard contents or key material. A crash after bytes are saved but before UI confirmation should not cause duplicate or overwritten files on retry.

### E. Measure live behavior, not just successful startup

The [#86 follow-up](https://github.com/stevederico/Sapporo/issues/86#issuecomment-5536095015) distinguishes a completed boot handshake from later transport liveness and describes a false negative caused by unreliable log-delta measurement. The [#85 follow-up](https://github.com/stevederico/Sapporo/issues/85#issuecomment-5536193778) explains why a repeated teardown test can skip the relevant cleanup branch and falsely appear to pass.

**Proposed adaptation:** tests should prove the relevant branch ran. Verify an actual Apple peer response, a saved payload matching the sent bytes, and radio cleanup after cancellation. Add timeout/restart/resume scenarios with fresh peer observations rather than trusting a stale “ready” status. Use bounded journald timestamps or cursors for capture and explicit operation results for decisions; do not infer a protocol reply solely from log-line counts.

## 3. Things not to borrow into this project

- T2 BridgeXPC, biometric enrollment, PAM modifications and SEP endpoint probes do not provide AWDL, Airdrop-compatible or Companion messages.
- The T1 bring-up commands are not demonstrated M-series commands. The linked issues do not supply a general enable-SEP-service API.
- AKS caller identifiers are not an Apple Account identity or Rapport signing identity. Do not import them into the clipboard credential format.
- Do not perform raw SEP opcode sweeps, sensor re-provisioning, device-tree changes or boot-policy mutations as part of Airdrop-compatible installation. They are unrelated experiments and reported to have disruptive failure modes.
- Do not copy the entire biometric policy/journaling framework into our small transfer application. Adapt the specific invariants needed for radio ownership, transfer lifecycle and authorization.

## 4. Code availability and license boundaries

| Material | Inspected license | Reuse assessment |
| --- | --- | --- |
| T2 userspace and kernel transport | [GPL-2.0-only](https://github.com/jmurth1234/t2-touchid-linux/blob/ea46d8a0aef3e73b0e2f747aa18721dbcd265bce/README.md#license), confirmed in inspected file headers | Study behavior and independently implement applicable patterns; do not paste or translate protected implementation into MIT ac-dc while discarding obligations. No direct runtime dependency needed. |
| Sapporo `drivers/soc/apple/sep.rs` | [GPL-2.0-only OR MIT](https://github.com/stevederico/Sapporo/blob/77c32fc95822b3b431ef559c2bd50c513e1d8eae/drivers/soc/apple/sep.rs) | File-level dual license differs from broad kernel metadata; Rust code is available but solves a different problem. Preserve applicable notices if ever reused. |
| m1n1 Mesa tracer | MIT file header | Useful research reference; not an Airdrop-compatible/clipboard component. |
| T1Bridge | Issues report unpublished source at their writing | No reusable implementation was established by this review; do not assume an issue's description grants code access or licensing. |
| Issue comments and experimental results | No separate software license established | Cite findings, distinguish hypotheses and observations, and avoid importing prose or code wholesale. |

Translating code to Rust does not remove its license obligations. Our separate GPL driver repository also does not automatically make GPL code suitable for the MIT application.

## 5. Next experiments with the user's Mac and iPhone

1. Finish the Airdrop-compatible UI with explicit receive-window lifetime, recipient choice, accept/decline, progress, cancellation and understandable stage errors.
2. Establish native Mac ↔ iPhone Airdrop-compatible success, then independently test Linux send and receive with each Apple device. Record OS versions, file sizes and hashes, discovery behavior and exact failures. Prefer synthetic files.
3. Test rejection, timeout, stale peer, cancelled transfer, window expiry and restart. Keep “implemented” separate from “verified with Apple.”
4. Return to the clipboard report's frame/AAD, OPACK and message-envelope corrections. Then attempt one authenticated short-text pull from an explicitly selected Mac, followed separately by the iPhone.
5. If authentication fails, locate the exact Pair-Verify stage and peer error. These SEP issues do not justify weakening verification or declaring the key permanently inaccessible.

**Research outcome:** useful architectural and experimental discipline; no new Airdrop-compatible wire implementation and no shortcut around Universal Clipboard account trust.
