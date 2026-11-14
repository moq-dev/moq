# [S] moq-webrtc's vendored stack has every upstream fix it needs

## Goal

moq-webrtc's vendored str0m, `str0m_proto`, `is`, dimpl, and sctp-proto
carry every fix their upstreams
([algesten/str0m](https://github.com/algesten/str0m),
[algesten/dimpl](https://github.com/algesten/dimpl), and
[algesten/sctp-proto](https://github.com/algesten/sctp-proto)) have landed
since the last check, each ported with its test or recorded as not applying,
and the `Upstream` section of `rs/moq-webrtc/src/vendor/mod.rs` names the
newest upstream commit checked for each.

## Plan

Recurring child of the [vendor sync](/quest/m3/vendor-sync/README.md), which
says how each run works and recurs. New in the 2026-11-12 audit: the stack was
vendored on 2026-11-07 in
[moq-dev/moq#8163](https://github.com/moq-dev/moq/pull/8163), after the
2026-11-05 scan.

Start from the revisions the module doc records: str0m `main` at
[`dc317173`](https://github.com/algesten/str0m/commit/dc317173b16fc6ed86633e6b08b229c9195deb87)
(2026-10-26; `str0m_proto`, `is`, and the test-only `netem` come from the same
repository), dimpl 0.9.0 at
[`c17b3b8`](https://github.com/algesten/dimpl/commit/c17b3b8b971b6eb2ecf7284f5dc0ecb86680ac4d),
and sctp-proto 0.10.7 as published on crates.io. Check each repository's
security advisories too: the stack parses untrusted SRTP, RTCP, DTLS, STUN,
and SCTP input from browsers, and cargo-deny cannot see an advisory against
vendored code.

The vendored code was trimmed to what MoQ uses (the
[WebRTC epic](/quest/m3/webrtc/README.md)'s "What goes" list: GCC bandwidth
estimation, the leaky pacer, H.266, RED, DTMF, G.7xx and comfort noise, VLA
and simulcast layers, RTCP app feedback, stats, and DTLS 1.3), so a fix
confined to removed code does not apply. Deleting an API with no consumer
beats porting a fix to it.

Out of scope: `str0m_rind` and dimpl's `crypto::rind`, ported from
moq-dev/rind, which moq maintains itself.

## Required

- [Cut and publish the prepared breaking release](/quest/m3/release/cut.md) - the next cycle starts after the cut
