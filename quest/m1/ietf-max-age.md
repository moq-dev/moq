# [M] Max age defaults to zero and travels as MAX_CACHE_DURATION

## Goal

A track's max age is set by its publisher alone, defaults to 0 (live edge
only), and crosses moq-transport as MAX_CACHE_DURATION, so it survives a
Rust-to-Rust IETF hop the way it already survives moq-lite 05+.
`origin::Config::default_max_age` goes away.

## Plan

- Remove `origin::Config::default_max_age`. A track whose protocol carries no
  max age (moq-lite 01-04, or an IETF publisher that omits
  MAX_CACHE_DURATION) gets 0.
- `track::DEFAULT_MAX_AGE` and JS `DEFAULT_MAX_AGE_MS` become 0. Audit every
  caller that relies on the 5s default (moq-rtmp, moq-gst, moq-mux, hang, JS
  lite tests) and give each an explicit max age wherever history matters.
  HLS/DASH egress needs a window the publisher asks for, since the old 5s
  default was too short for it anyway (`moq import` already sends 30s).
- MAX_CACHE_DURATION (0x04) is the track's retention in milliseconds: a
  message parameter through draft 16 and a track property afterwards. The IETF
  publisher writes `Info::max_age` there in SUBSCRIBE_OK and PUBLISH, and the
  subscriber reads it back into `Info::max_age`. Today Rust drops the property
  and JS parses but never uses it. Before choosing the encoding, confirm what
  the drafts say absent and 0 mean. libquicr sends 0.
- EXPIRES stays 0 on send and ignored on receive. It is subscription lifetime,
  not retention.
- Mirror the encode and decode in `js/net`'s IETF path.
- Breaks the published `origin::Config` and changes default retention, so the
  PR retargets to `dev`. Update `doc/concept/moq-lite.md` and the
  `origin::Config` and `track::Info` docs in the same PR.
- Test: a Rust-to-Rust and a Rust-to-JS IETF session carry a non-zero max age
  end to end, and a default track sends 0. Run `just test interop --all`.

## Required

- [IETF EXPIRES](/quest/m0/ietf-expires.md) - the decoders accept EXPIRES first
