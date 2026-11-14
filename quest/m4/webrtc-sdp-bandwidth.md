# [S] Several bandwidth lines per media section

## Goal

The vendored SDP parser and writer accept and emit several `b=` lines per media section, so a capped WHIP answer carries both `b=TIAS` and `b=AS` natively and moq-webrtc stops string-splicing `b=TIAS` into the answer.

## Plan

Decided 2026-10-30 with the maintainer (planning interview). The vendored str0m reads `optional(bandwidth_line())` (`rs/moq-webrtc/src/vendor/str0m/sdp/parser.rs:51,320`) into `bw: Option<Bandwidth>` (`sdp/data.rs:174,288`), so a second line made the whole answer unparseable and [moq-dev/moq#7373](https://github.com/moq-dev/moq/pull/7373) shipped TIAS only through `sdp::cap_answer`. Make it a `Vec<Bandwidth>`, emit both lines on a capped answer as the original Goal asked, delete the splice, and test that our own WHIP client parses the answer.

Public API: none. Wire: capped answers gain `b=AS`.
