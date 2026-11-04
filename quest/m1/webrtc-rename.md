# [M] Rename moq-rtc to moq-webrtc

## Goal

The published `moq-webrtc` crate holds what `moq-rtc`, `moq-ice`, and moq-tokio's `webrtc` feature hold today: the WHIP/WHEP gateway, the ICE driver, and the native data-channel connect/accept. `moq-rtc` and `moq-ice` are gone from the workspace, and moq-tokio has no `webrtc` feature.

## Plan

Decided 2026-10-30 with the maintainer (planning interview); see the [epic](/quest/m2/webrtc/README.md).

Milestone: ✅ m1 (moved from m2, maintainer, 2026-11-03). Not chosen: keep it in m2, or send [moq-rtc on rind](/quest/m1/rind/rtc.md) back to m2. [Vendor and trim the WebRTC stack](/quest/m1/webrtc-vendor.md) requires this, and m1's rind epic requires the vendoring, so this sits at the m1 top level. It stays a child of the m2 [WebRTC epic](/quest/m2/webrtc/README.md).

- Move `rs/moq-rtc` to `rs/moq-webrtc` and merge `rs/moq-ice` in as a module. Keep the gateway's `server` feature; add a feature for the data-channel transport so WHIP/WHEP-only builds skip SCTP.
- Move moq-tokio's `webrtc` module (`rs/moq-tokio/src/webrtc.rs`, host gathering, `.local` resolution) into moq-webrtc, built on moq-tokio's public session types. If the move needs something moq-tokio keeps private, bring the shape to the maintainer.
- Update moq-cli's `rtc` subcommand, the bindings or examples that use the transport, `test/p2p`, and every doc and quest naming the old crates.
- Publishing: existing `moq-rtc` releases stay on crates.io; `doc/setup/upgrade.md` says where things moved. Registry lookup on 2026-11-03 found no `moq-ice` or `moq-webrtc` crate. There is no published `moq-ice` to retire. First publication of `moq-webrtc` belongs to the release cut, with version numbers chosen by the maintainer; this implementation does not publish or bump versions.

Implementation discovery, 2026-11-03: the move cannot use only today's public moq-tokio API. `Client::connect_webrtc` reads private client settings and its moq-net client; `Listener::accept_webrtc` owns the listener's private admission state, deadline, and `server::Request` construction, including qmux receive limits. The driver also uses the private fallback-runtime helpers. Exposing these internals or replacing their behavior needs the reserved maintainer choice below.

The locked str0m 0.24.1 also depends on `sctp-proto` unconditionally and has no SCTP feature. A feature on moq-webrtc alone can hide the data-channel API and its direct dependencies, but cannot make WHIP/WHEP builds skip SCTP. The following vendoring quest currently requires this rename, so the compile-time split needs a scope decision rather than a reversed prerequisite.

Public API: breaking (crate rename; `moq_tokio::webrtc` moves). Wire: none.

## Waiting

- Maintainer: choose the external session boundary before moving the data-channel module. Recommended: add a public transport handoff on moq-tokio's client/listener that owns establishment, admission, deadlines, and request construction, keeping those fields private; moq-webrtc supplies the qmux transport and link metadata. Alternative: make data channels use moq-net's public client/server directly, with moq-webrtc owning their runtime, timeout, and admission policy and returning moq-net's handshake instead of moq-tokio's request. The latter changes the Plan's session boundary and caller API. Resume once the boundary is recorded; an agent cannot choose it because this Plan explicitly reserves private-to-public shapes for the maintainer.
- Maintainer: choose how to make gateway builds exclude SCTP. Recommended: move the minimum str0m vendoring and SCTP feature gate from [vendoring](/quest/m1/webrtc-vendor.md) into this quest, leaving the other trimming there. Alternative: explicitly defer the transitive SCTP split to vendoring and revise this quest's Goal/Plan accordingly, with the rename only gating its own data-channel code. Resume once that scope is recorded; a local feature cannot change an unconditional upstream dependency, and silently deferring it would leave this quest incomplete.

## Related

- [moq-rtc REMB and TIAS](/rs/moq-rtc/src/server/whip.rs) - #7373 adds the capped answer in moq-rtc; carry it through the move
