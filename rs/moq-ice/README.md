# moq-ice

Drives a [str0m](https://github.com/algesten/str0m) WebRTC connection over a
tokio UDP socket: the run loop that feeds it datagrams and timeouts, a STUN
Binding client for server-reflexive candidates, and per-peer wire counters.

str0m is sans-IO and requires every mutation to be followed by draining its
output. `Agent::rtc_mut` hands out a guard that drains when dropped, so callers
can't forget.

This is infrastructure, not an entry point: `moq-tokio`'s `webrtc` feature
builds MoQ data channels on it, and `moq-rtc` builds its WHIP/WHEP gateway.
