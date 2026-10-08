# [S] JavaScript papercuts from the 2026-10-07 audit

## Goal

Three small JavaScript fixes from the 2026-10-07 code audit, each with a
regression test that fails without it:

- `@moq/net` delivers an IETF object with status 0 and the end bit clear as an
  empty frame and keeps reading the subgroup, as Rust does.
- A buffered audio rendition change settles while muted.
- An invalid attribute on `<moq-watch>` or `<moq-publish>` warns and falls
  back to the default instead of throwing.

## Plan

- `js/net/src/ietf/object.ts` (around line 350) returns `new Frame()` with an
  undefined payload for status 0, and `subscriber.ts` breaks out of the
  subgroup on that. Rust delivers an empty object and continues
  (`rs/moq-net/src/ietf/subscriber.rs`), and the JS file's own TODO agrees.
  Only third-party publishers hit this, since Rust peers set the end bit.
- `js/watch/src/audio/decoder.ts` awaits `ring.wait` (around lines 462 and
  562) without racing the effect's abort. Mute freezes the playhead, so the
  previous spawn never settles until something flushes the ring. Race it
  against the effect.
- `js/watch/src/element.ts` and `js/publish/src/element.ts`: `new URL(bad)`
  throws out of `attributeChangedCallback`, and `volume="foo"` parses to
  `NaN`, which later throws inside the gain node. Per `js/AGENTS.md`, an
  invalid value warns and falls back to the default, as `parseDelay` and
  `parseVisible` already do.

Public API: none. Wire: none.

## Related

- [Rust papercuts](/quest/m1/papercuts-rs.md) - the Rust half of the same audit
- [Audio jitter target](/quest/m0/audio-jitter-target/README.md) - owns the audio ring and its backpressure
