# [S] Several bandwidth lines per media section

## Goal

A capped WHIP answer carries both `b=TIAS` and `b=AS`, and moq-webrtc stops string-splicing `b=TIAS` into the answer, because str0m's SDP parser and writer accept and emit several `b=` lines per media section.

## Plan

Decided 2026-10-30 with the maintainer (planning interview) as a change to the vendored str0m. Changed 2026-11-16 (maintainer, /quest-plan of the pre-cut audit): moq-webrtc [returns to upstream str0m](https://github.com/moq-dev/moq/pull/8963), so the parser is upstream's again.

At the vendored revision str0m read `optional(bandwidth_line())` into `bw: Option<Bandwidth>`, so a second line made the whole answer unparseable, and [moq-dev/moq#7373](https://github.com/moq-dev/moq/pull/7373) shipped TIAS only through `sdp::cap_answer` (`rs/moq-webrtc/src/sdp.rs`). 0.25 still does (`bw: Option<Bandwidth>` at `src/sdp/data.rs:174` and `:290`, found by [the switch](https://github.com/moq-dev/moq/pull/8963)), so the change (a `Vec<Bandwidth>`) is upstream's to take: write the report for [upstream reports](/quest/m4/upstream-reports.md), which the maintainer approves before it is filed, and keep the splice until a release carries it. Then emit both lines on a capped answer, delete the splice, and test that our own WHIP client parses the answer.

Public API: none. Wire: capped answers gain `b=AS`.
