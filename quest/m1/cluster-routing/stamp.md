# [S] Validate stamped routing identities

## Goal

Finish cross-language validation of receiver-assigned session identities on
lite-07. Each session's own claims use its receiver's fresh identity, never
a declared value. A direct path and a CDN path remain separate identities;
only a matching explicit epoch permits resuming between them.

## Plan

Rust and JS now prepend the session's stamp to an empty incoming hop list,
retain the declared trailing hop as a loop hint, and forward the stamp.
Rust attributes routes and filters the returning session independently of
the declared hint. Caller-pinned `with_peer_hop` APIs are removed. Published
lite-06 and older, and moq-transport, keep their wire semantics. The lite
draft documents this choice. Key proof remains in
[Peer grants](/quest/m2/p2p/peer-grant.md); subject-qualified forwarded
identities and seqnos remain in
[Routes and announces](/quest/m1/cluster-routing/routes.md).

Local `just ci` passes, including 3,246 moq-net tests and 9,023 affected
Rust package tests. Forged declarations, independent sessions, loop hints,
split horizon, reconnect lifetimes, and Rust/JS stamp forwarding are covered.

Remaining: resolve the browser interop failures before marking this work
ready. `just test interop --all` passed the positive media matrix but failed
the browser's swapped-tone control after its Opus stream went offline. A
focused `--publishers rust --subscribers js --timeout 30` run passed that
control but failed positive playback after pause/resume, again offline.
The same focused run on the starting main commit `f6d0ace93e` passed those
media checks but failed the refused-session close-code check. These runs do
not establish whether the playback failure comes from stamping or an
independent browser/session-lifecycle problem.

Recommended next step: isolate the pause/resume withdrawal against main,
trace the announcement that makes the browser go offline, and fix the source
if stamping causes it. Otherwise identify the existing owner or plan the
separate lifecycle fix, record that evidence, and finish this quest. Keep
the assertion and timeout intact; a successful retry alone proves nothing.
