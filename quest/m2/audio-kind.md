# [M] Rust and JS share an optional audio kind, and a Rust voice encode sends nothing in silence

## Goal

`rs/moq-audio`'s encode `Config` and `@moq/publish`'s audio source describe
what they carry the same way: an optional kind, voice or music, absent when
unknown. In Rust it sets Opus's application and signal as JS's does (voice:
VoIP and voice; music: audio and music; absent: libopus defaults), and a voice
encode suppresses silence with push-to-talk's wire shape: libopus DTX decides,
the first no-transmit packet declares an end (`Producer::discontinuity`),
keep-alives are dropped, and resumed speech opens a new group. Players change
nothing. Bindings are unchanged until a binding user asks.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan):

- **The Rust publisher gets the gate too ✅** (rejected: JS only). Unlike
  WebCodecs, libopus returns its no-transmit packet as each frame is encoded,
  so Rust can declare the end the moment DTX starts. Today `dtx: true` sends
  an empty packet every 20 ms and no code sets it.
- **Mirror JS's kind ✅** (rejected: a Rust-only `signal` field; keeping
  `dtx: bool`).
- **No kind is absence, not a variant ✅** (rejected: a `None` variant; Rust
  saying none while JS keeps `"auto"`): Rust `kind: Option<Kind>` with
  `Kind { Voice, Music }`, JS `kind?: "voice" | "music"`, a bare track meaning
  none. JS's `"auto"` is removed, with a migration note.
- The `dtx` field is removed, and the backend-choosing `kind: Kind { Auto,
  Software }` becomes `backend: Backend { Auto, Software }`, freeing the name.
- Breaking for both APIs, so this is m2. Wire: none.

## Related

- [A browser voice source stops sending soon after background noise rises](/quest/m2/voice-rising-noise.md) - the browser gate's SILK VAD lags libopus's analysis, which this quest's DTX runs
