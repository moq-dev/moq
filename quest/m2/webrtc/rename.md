# [M] Rename moq-rtc to moq-webrtc

## Goal

The published `moq-webrtc` crate holds what `moq-rtc`, `moq-ice`, and moq-tokio's `webrtc` feature hold today: the WHIP/WHEP gateway, the ICE driver, and the native data-channel connect/accept. `moq-rtc` and `moq-ice` are gone from the workspace, and moq-tokio has no `webrtc` feature.

## Plan

Decided 2026-10-30 with the maintainer (planning interview); see the [epic](/quest/m2/webrtc/README.md).

- Move `rs/moq-rtc` to `rs/moq-webrtc` and merge `rs/moq-ice` in as a module. Keep the gateway's `server` feature; add a feature for the data-channel transport so WHIP/WHEP-only builds skip SCTP.
- Move moq-tokio's `webrtc` module (`rs/moq-tokio/src/webrtc.rs`, host gathering, `.local` resolution) into moq-webrtc, built on moq-tokio's public session types. If the move needs something moq-tokio keeps private, bring the shape to the maintainer.
- Update moq-cli's `rtc` subcommand, the bindings or examples that use the transport, `test/p2p`, and every doc and quest naming the old crates.
- Publishing: the last `moq-rtc`/`moq-ice` releases stay on crates.io; `doc/setup/upgrade.md` says where things moved. Version numbers are the maintainer's.

Public API: breaking (crate rename; `moq_tokio::webrtc` moves). Wire: none.

## Related

- [moq-rtc REMB and TIAS](/quest/m2/rtc-remb-tias.md) - its open PR, #7373, edits moq-rtc; whichever lands second resolves the move
