# [M] Max age defaults to zero and travels as EXPIRES

## Goal

A track's max age is set by its publisher alone, defaults to 0 (live edge
only), and crosses moq-transport as EXPIRES, so max age survives a Rust-to-Rust
IETF hop the way it already survives moq-lite 05+.
`origin::Config::default_max_age` goes away.

## Plan

- Remove `origin::Config::default_max_age`. A track whose protocol carries no
  max age (moq-lite 01-04) gets 0.
- `track::DEFAULT_MAX_AGE` and JS `DEFAULT_MAX_AGE_MS` become 0. Audit every
  caller that relies on the 5s default (moq-rtmp, moq-gst, moq-mux, hang, JS
  lite tests) and give each an explicit max age wherever history matters.
  HLS/DASH egress needs a window the publisher asks for, since the old 5s
  default was too short for it anyway (`moq import` already sends 30s).
- The IETF publisher writes `Info::max_age` in milliseconds as EXPIRES in
  SUBSCRIBE_OK (the fixed field on draft 14, the parameter afterwards) and in
  PUBLISH wherever the draft allows it. The default of 0 keeps sending "never
  expires", so third-party subscribers only see an expiry when a publisher
  opts into history.
- The subscriber maps EXPIRES straight to max age: 0 is 0. This replaces the
  default fallback from [IETF EXPIRES](/quest/m0/ietf-expires.md).
- Mirror the encode and decode in `js/net`'s IETF path.
- Breaks the published `origin::Config` and changes default retention, so the
  PR retargets to `dev`. Update `doc/concept/moq-lite.md` and the
  `origin::Config` and `track::Info` docs in the same PR.
- Test: a Rust-to-Rust IETF session carries a non-zero max age end to end, and
  a default track sends EXPIRES 0. Run `just test interop --all`.

## Required

- [IETF EXPIRES](/quest/m0/ietf-expires.md) - the decoders accept EXPIRES first
