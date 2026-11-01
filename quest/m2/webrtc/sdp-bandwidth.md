# [S] Several bandwidth lines per media section

## Goal

The vendored SDP parser and writer accept and emit several `b=` lines per media section, so a capped WHIP answer carries both `b=TIAS` and `b=AS` natively and moq-webrtc stops string-splicing `b=TIAS` into the answer.

## Plan

Decided 2026-10-30 with the maintainer (planning interview). str0m 0.24 reads `optional(bandwidth_line())` (`sdp/parser.rs:357`) into `bw: Option<Bandwidth>` (`sdp/data.rs:137,251`), so a second line made the whole answer unparseable and [moq-dev/moq#7373](https://github.com/moq-dev/moq/pull/7373) shipped TIAS only through `sdp::cap_answer`. Make it a `Vec<Bandwidth>`, emit both lines on a capped answer as the original Goal asked, delete the splice, and test that our own WHIP client parses the answer.

Public API: none. Wire: capped answers gain `b=AS`.

## Required

- [Vendor and trim the stack](/quest/m2/webrtc/vendor.md) - the parser this changes
- [moq-rtc REMB and TIAS](/quest/m2/rtc-remb-tias.md) - its PR, #7373, adds the capped answer
