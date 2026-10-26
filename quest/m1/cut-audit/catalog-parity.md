# [M] Rust and JS accept the same catalogs

## Goal

A catalog that one language accepts, the other accepts, and a malformed
known-codec rendition is refused per rendition in both, so a browser never
plays what moq-cli, moqsrc, and HLS refuse, or the reverse.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. Reproduced with probes
in both languages, inherited.

| Input | JS | Rust |
|---|---|---|
| `codec:"avc1.6400"` | accepts | refuses |
| `codedWidth:5000000000` | accepts | refuses (u32) |
| legacy `video:[{track,config}]` | accepts | refuses |
| `container:{}` | refuses | accepts as Unknown |

`sampleRate:0` and `numberOfChannels:0` pass both and fail later in
`Timescale::new`. Rust: `rs/hang/src/catalog/video/codec.rs:62`,
`video/mod.rs:192`, `audio/mod.rs:136`, `container.rs:88`. JS:
`js/hang/src/catalog/video.ts:129,146,229`, `audio.ts:110`,
`container.ts:15`.

Decided 2026-10-25 (maintainer): strict and per rendition. Both languages validate codec strings and
dimension ranges the same way, refuse zero rates and channel counts, drop
the legacy array form in Rust's direction (or keep it in both), and skip a
malformed rendition while keeping the rest of the catalog. Add a shared
fixture corpus both test suites load, so a future divergence fails CI.

Also check the container consumer's skip measurement: Rust measures newest
content minus the next group's start (`rs/moq-mux/src/container/consumer.rs:576`),
JS from the oldest unread frame (`js/hang/src/container/consumer.ts:401`).
They may diverge on a backlog join (unverified); align them if a test shows
it.

Public API: stricter catalog acceptance in one or both languages. Wire: a
stricter catalog contract, stated in `drafts/draft-lcurley-moq-hang.md`.
