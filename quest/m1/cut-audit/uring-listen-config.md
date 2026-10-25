# [S] Every relay runtime honors or refuses each listen setting

## Goal

A relay listen setting a runtime cannot deliver refuses to start, as
`doc/bin/relay/config.md` promises, on the io_uring workers as on tokio.
Each runtime maps `listen::Config` by exhaustive destructuring, so a new
field fails to compile until every runtime decides what to do with it.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. Source-inferred,
inherited.

`Workers::bind` (`rs/moq-relay/src/uring.rs:135-265`) refuses
`listen.lb.id` but never reads `preferred_v4`/`preferred_v6`, and
moq-uring's `quic::server::Config` has no such knob (compare tokio's
`rs/moq-tokio/src/quic/backend.rs:796`). So `--runtime-io-uring
--listen-preferred-v4 192.0.2.1:443` starts cleanly and no client ever
migrates. The shared stream server uses `init_streams()` (`relay.rs:268`),
which skips `validate_stream_only`, so nothing refuses it.

Refuse both fields in `Workers::bind` (or implement them), and destructure
`listen::Config` with no `..` in every runtime mapping (`uring.rs`,
`rs/moq-tokio/src/server.rs:424`, `websocket.rs:66`).

Public API: none. Wire: none. Config: an ignored combination now refuses to
start; note it in `doc/bin/relay/`.

## Related

- [One relay serve path](/quest/m2/relay-serve-path.md) - removes the duplicated mapping this quest guards
