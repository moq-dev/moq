# [M] Max age is optional and travels as MAX_CACHE_DURATION

## Goal

A track's max age is set only by its publisher. `None` means the publisher set
no limit, and the value crosses moq-transport as MAX_CACHE_DURATION, so it
survives an IETF hop the way it already survives moq-lite 05+.
`origin::Config::default_max_age` goes away.

## Plan

- `track::Info::max_age` becomes `Option<Duration>`, defaulting to `None`.
  `None` retains groups until the cache pool or `origin::Config::cache_duration`
  evicts them. `Some(0)` keeps only the live edge. Mirror this in JS
  (`maxAge` optional, drop `DEFAULT_MAX_AGE_MS`).
- Remove `track::DEFAULT_MAX_AGE` and `origin::Config::default_max_age`. A
  track whose wire carries no max age gets `None`, never a local fallback.
  Audit the callers that rely on the 5s default (moq-rtmp, moq-gst, moq-mux,
  hang, JS lite tests) and give each an explicit value wherever it matters.
  HLS/DASH egress asks for its window from the publisher; the old 5s was too
  short for it anyway (`moq import` already sends 30s).
- IETF: MAX_CACHE_DURATION (0x04) is milliseconds, a message parameter through
  draft 15 and a track property from draft 16. The subscriber reads it on
  every draft, and absent means `None`, which is what the drafts mean by
  omission. The publisher sends `Some(n)` as n and omits it for `None`, but
  only on draft 17+: older moq-net peers reject 0x04 on draft 15 and trailing
  properties on draft 16, so those drafts stay receive-only. Today Rust drops the property and JS
  parses but never uses it.
- MAX_CACHE_DURATION is wall-clock and max age is media time with the newest
  group always kept, so the mapping is approximate. Accept that rather than
  modeling a second clock.
- moq-lite-07 (still WIP, off by default) makes TRACK_INFO's Max Age
  optional: the value plus one, with 0 meaning none. Lite05/06 map `None` to
  the largest varint in both directions. Update
  `drafts/draft-lcurley-moq-lite.md` in the same PR.
- EXPIRES stays 0 on send and ignored on receive. It is subscription lifetime,
  not retention.
- Breaks the published `track::Info` and `origin::Config`, so the PR retargets
  to `dev`. Update `doc/concept/moq-lite.md` and the affected rustdoc and JS
  docs in the same PR.
- Test: Rust-to-Rust and Rust-to-JS sessions over IETF and lite-07 carry
  `None`, `Some(0)`, and a non-zero max age end to end, including through a
  relay hop, and drafts 15 and 16 decode but never send it. Run `just test interop --all`.
