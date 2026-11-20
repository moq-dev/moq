# [S] A WHEP re-offer plays under load

## Goal

`moq-webrtc`'s `tests::a_reoffer_from_the_same_address_plays` passes however loaded the host is.

## Plan

It failed once in a loaded `just check` on 2026-11-19 (while building [moq-dev/moq#9222](https://github.com/moq-dev/moq/pull/9222), which touches no WebRTC code): `received` hit its 10s `TIMEOUT` ("the viewer neither received nor closed", `rs/moq-webrtc/src/lib.rs:300`) after 10.18s, and it passed alone in 0.9s.
The panic does not say which `received` call timed out: the first viewer's packet, or the re-offered session's loop that pushes a packet every 20ms.
Reproduce first under CPU pressure, such as `cargo nextest run -p moq-webrtc a_reoffer --stress-count 50` inside `systemd-run --user --scope -p CPUQuota=100%`, then find which wait stalls and why: ICE over real sockets racing the 20ms push loop, or the new session joining past the delay budget so every pushed packet ages out before it reads.
Keep the epic's rules: no longer timeout, no retry.
