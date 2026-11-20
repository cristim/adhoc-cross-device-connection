# Known issues

Pre-existing issues noticed in passing, not yet fixed.

## A poisoned `sessions.approved` mutex silently drops a connection with no HTTP response

`src/airdrop/mod.rs`, `connection`: `.lock().unwrap()` on the approved-sessions
mutex panics the connection task if a prior panic elsewhere already poisoned
it, closing the socket with no response. Found while verifying PR #10's fix
for #18 (every other rejection path there now answers with 400/413/415/507).
Orthogonal to that fix; not blocking.
