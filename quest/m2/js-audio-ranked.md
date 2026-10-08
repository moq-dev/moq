# [S] Rank audio renditions in JS and HLS

## Goal

`@moq/hang` orders audio renditions the way Rust `hang::catalog::Audio::ranked`
(#4993) does: enabled first, then highest bitrate, sample rate, and channels,
unknown bitrate last, ties in name order. JS video `ranked` gains the same
enabled-first rule Rust's has (today it ranks by area, bitrate, and name). The Rust HLS exporter
(`rs/moq-hls/src/export/renditions.rs`) lists audio renditions by
`Audio::ranked` instead of by name.

## Plan

Decided 2026-10-08: the JS ranking includes `enabled` first, like Rust, and
this quest also fixes JS video `ranked` (`js/hang/src/catalog/video.ts`),
which ranks by picture area without it, so both kinds agree with Rust. The
HLS exporter is Rust, so it calls `Audio::ranked` directly; no JS is
involved there.

Mirror `ranked` for video (#4988). Name it to match Rust per AGENTS.md.
Test each ordering rule in both languages, including a disabled rendition
that would otherwise rank first.

Public API: a new `@moq/hang` audio `ranked`, and a behavior change to video
`ranked`. Wire: none.
