# [M] Rename moq-rtc to moq-webrtc

## Goal

The published `moq-webrtc` crate holds what `moq-rtc`, `moq-ice`, and moq-tokio's `webrtc` feature hold today: the WHIP/WHEP gateway, the ICE driver, and the native data-channel connect/accept. `moq-rtc` and `moq-ice` are gone from the workspace, and moq-tokio has no `webrtc` feature.

## Plan

Decided 2026-10-30 with the maintainer (planning interview); see the [epic](/quest/m2/webrtc/README.md).

Milestone: ✅ m1 (moved from m2, maintainer, 2026-11-03). Not chosen: keep it in m2, or send [moq-rtc on rind](/quest/m1/rind/rtc.md) back to m2. [Vendor and trim the WebRTC stack](/quest/m1/webrtc-vendor.md) requires this, and m1's rind epic requires the vendoring, so this sits at the m1 top level. It stays a child of the m2 [WebRTC epic](/quest/m2/webrtc/README.md).

- Move `rs/moq-rtc` to `rs/moq-webrtc` and merge `rs/moq-ice` in as a module. Keep the gateway's `server` feature; add a feature for the data-channel transport so WHIP/WHEP-only builds skip SCTP.
- Move moq-tokio's `webrtc` module (`rs/moq-tokio/src/webrtc.rs`, host gathering, `.local` resolution) into moq-webrtc, built on moq-tokio's public session types. Its connect and accept go through moq-tokio's `connect_with`/`accept_with` handoffs, which run moq-net's session policy over the established data channel.
- Boundary decided 2026-11-04 (maintainer, from [moq-dev/moq#7804](https://github.com/moq-dev/moq/pull/7804)): ✅ move session policy into moq-net first ([Session policy lives in moq-net](/quest/m1/session-policy/README.md)), then this rename uses the thin handoffs. Not chosen: a moq-tokio handoff over today's policy, or moq-webrtc calling moq-net with its own policy. Removing SCTP stays with [Vendor and trim the WebRTC stack](/quest/m1/webrtc-vendor.md).
- Update moq-cli's `rtc` subcommand, the bindings or examples that use the transport, `test/p2p`, and every doc and quest naming the old crates.
- Publishing: the last `moq-rtc`/`moq-ice` releases stay on crates.io; `doc/setup/upgrade.md` says where things moved. Version numbers are the maintainer's.

Public API: breaking (crate rename; `moq_tokio::webrtc` moves). Wire: none.

## Required

- [moq-tokio becomes a thin shell](/quest/m1/session-policy/tokio-handoff.md) - the `accept_with`/`connect_with` handoffs moq-webrtc uses

## Related

- [moq-rtc REMB and TIAS](/rs/moq-rtc/src/server/whip.rs) - #7373 adds the capped answer in moq-rtc; carry it through the move
