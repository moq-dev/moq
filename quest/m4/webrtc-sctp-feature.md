# [S] WHIP/WHEP builds carry no SCTP

## Goal

The vendored `sctp_proto` and str0m's SCTP glue compile only with
moq-webrtc's `p2p` feature (its data-channel transport), so a WHIP/WHEP-only
build has no SCTP code in its tree.

## Plan

Planned 2026-11-07 (maintainer, /quest-complete), a follow-up of
[moq-dev/moq#8163](https://github.com/moq-dev/moq/pull/8163), which vendors
the stack but still compiles SCTP into every build. The [WebRTC epic](/quest/m3/webrtc/README.md)'s settled
decision ("SCTP stays ... but compiles only with the data-channel feature")
gets this owner. Gate the modules and the str0m paths that reach them,
and add a check, in the feature matrix `just rs features` runs, that a
build without `p2p` neither compiles nor links `sctp_proto`. The feature
keeps its name.

Public API: none. Wire: none.
