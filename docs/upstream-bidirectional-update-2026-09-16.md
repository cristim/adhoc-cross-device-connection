# Upstream refresh: bidirectional transfers

Checked GitHub API live on 2026-09-16, including default-branch commits,
Omdrop branches, open OpenDrop/OWL pull requests, recent LocalSend changes,
and comments on Sapporo #87/#48. This is a source review, not a device test.

## Result

No newly available native-Airdrop-compatible sending fix was found in the reviewed
repositories. LocalSend already provides a separate two-way LAN protocol; its
recent fixes do not turn it into Apple's Airdrop-compatible or Universal Clipboard.

| Repository | Current revision | Change since our pinned review | Bidirectional relevance |
| --- | --- | --- | --- |
| Omdrop plugin | `daee7a3ed9620b5d1bad277f731fdf9eabc38d53` | None | README still says sending is not working |
| Omdrop AWDL | `e81d4fd3413e52d11076c176e40f88745f5e076c` | Three commits after `0a7303d`: package-repository migration, BCM4387 model documentation, generated recipe patch/checksum handling | No new send mode; no underlying runtime/driver-patch changes in the comparison |
| OWL | `da255a70f221784c836d943dd3f243bc798f223b` | None | AWDL link implementation, not an Airdrop-compatible application or clipboard implementation |
| OpenDrop | `11fe7ba7861093b302bc0637e8cb10adf2d29337` | None | Existing send and receive remain useful references; no new upload fix found |
| LocalSend | `230fb692962668ca22ce0e61a8f53ce1cfd32102` | Earlier review used moving main links, so no precise prior SHA comparison is possible | Already two-way using its own protocol; recent relevant changes below |
| Sapporo | `77c32fc95822b3b431ef559c2bd50c513e1d8eae` | None; #87/#48 comments still last updated September 4 | Touch ID/SEP work, no new Airdrop-compatible or clipboard transport |

Sources: [Omdrop plugin](https://github.com/brentkearney/omdrop-plugin),
[driver comparison](https://github.com/brentkearney/omdrop-awdl/compare/0a7303da01757f7bdfa91be13b2b69b2c4d3b7fd...e81d4fd3413e52d11076c176e40f88745f5e076c),
[current driver README](https://github.com/brentkearney/omdrop-awdl/blob/e81d4fd3413e52d11076c176e40f88745f5e076c/README.md),
[OWL history](https://github.com/seemoo-lab/owl/commits/master/),
[OpenDrop history](https://github.com/seemoo-lab/opendrop/commits/master/),
[LocalSend revision](https://github.com/localsend/localsend/tree/230fb692962668ca22ce0e61a8f53ce1cfd32102),
[Sapporo #87](https://github.com/stevederico/Sapporo/issues/87),
[Sapporo #48](https://github.com/stevederico/Sapporo/issues/48).

## OpenDrop's unmerged work

- [#132](https://github.com/seemoo-lab/opendrop/pull/132), updated September 8:
  removes the Python startup dependency on pkg_resources. No upload behavior fix.
- [#116](https://github.com/seemoo-lab/opendrop/pull/116/files): old, unmerged
  chunked Discover receiver support. Our Rust parser already handles chunked
  requests. Inspection also finds decimal chunk-size parsing and an unbounded
  read after the terminating chunk in this proposal; copying it would be a
  regression. Chunk sizes require hexadecimal parsing, and persistent HTTP
  connections cannot use socket EOF as the message delimiter.
- [#90](https://github.com/seemoo-lab/opendrop/pull/90/files), despite the title
  “Update client.py”, only adds an empty discovery update callback. It is not a
  sending fix. Other open PRs concern logging, types and CI.
- OWL's four open PRs concern build compatibility, documentation and CI, not a
  new bidirectional Apple transfer implementation.

## LocalSend changes worth knowing about

[PR #3424](https://github.com/localsend/localsend/pull/3424), merged September 14,
reduces the Rust file-reader queue from sixteen to four 512 KiB chunks: 8 MiB to
2 MiB of queued data per active stream. This is a useful bounded-streaming lesson,
not a fix for our acknowledged small upload. Our sender uses a pull-based 64 KiB
ReaderStream with disk-backed archive preparation, rather than that queue.

[PR #3415](https://github.com/localsend/localsend/pull/3415), merged September 14,
fixes macOS DMG Share Extension entitlements. It helps sending files from macOS
through LocalSend's share integration, not native Airdrop-compatible discovery or Upload.

[PR #3434](https://github.com/localsend/localsend/pull/3434) is still unmerged. It
proposes shipping macOS .app bundles as ditto ZIP archives, preserving permissions,
symlinks and metadata, with manual extraction. This is a reminder that general
folder transfer is not equivalent to preserving an executable app bundle. Our
current regular-file/directory archives deliberately reject symlinks.

Latest published release returned by the API:
[v1.18.2](https://github.com/localsend/localsend/releases/tag/v1.18.2), August 21.
Its notes include ignoring system proxies, restarting failed/resumed HTTP and
multicast servers, older-version compatibility and mixed file/folder drag-drop
fixes. September main-branch fixes above are not part of that earlier release.

[Automatic clipboard synchronization #2971](https://github.com/localsend/localsend/issues/2971)
remains a feature request. LocalSend text/clipboard sending is not Apple
Universal Clipboard authentication or automatic cross-device pasteboard support.

## Consequence for ac-dc

Keep native Airdrop-compatible troubleshooting focused on correlating Mac sharingd request
identifiers/timestamps with Linux TCP and HTTP responses. The photographed 04:40
successful decompression is not yet matched to a local outgoing request. The
04:46 QUIC route failure occurred after our 04:44:36 radio expiration, and is
not automatically the same connection as our TCP Upload timeout.

A LocalSend-compatible optional Rust LAN transport is a credible separate route
to two-way file/text transfer when LocalSend is installed on the Mac/iPhone. It
would require explicit integration and interoperability testing. This review did
not add that backend or replace the current Airdrop-compatible transport. No upstream
update reviewed here establishes that our native Apple upload now works.
