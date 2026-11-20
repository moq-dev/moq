# [M] A scale-less track ages by arrival, and lite-05/06 stamp its group's start

## Goal

On a scale-less track (no timescale), Max Age expiry uses arrival age alone,
in Rust, `@moq/net`, and the lite-07 draft: group starts are bare numbers and
never count as milliseconds. Over lite-05/06, which can only declare
milliseconds, Rust and JS both stamp a scale-less frame with its group's bare
start, so a frame carries the same timestamp on every path and every fetch.
One shared byte fixture pins that encoding for both languages.

## Plan

Decided 2026-11-20 (maintainer, /quest-plan of a pre-cut audit of the lite-07
and hang-04 wire changes):

- ✅ Arrival age only (rejected: counting key gaps as milliseconds). A
  sequence-keyed track would otherwise expire by how many groups passed: keys
  0, 10000, and 20000 arriving together already expire group 0 under a 5 ms
  budget (Rust `a_scale_less_track_ages_wide_keys_by_their_distance`).
- ✅ The bare start on lite-05/06 in both languages (rejected: the send time
  in both). A send time stamps the same frame differently at each relay and on
  each later fetch, which breaks the identity rule in AGENTS.md.

This matches what the rest of the code base already says. The draft measures
Min Age by arrival on these tracks, Rust's `track::Info::max_age` doc says an
untimed track is judged by the wall clock alone, and
`drafts/draft-lcurley-moq-timestamp.md` does the same for untimed moq-transport
tracks. Only Max Age disagrees.

Where it lives:

- Draft `drafts/draft-lcurley-moq-lite.md`, Expiration: "On a scale-less
  Track, timestamp age counts bare units as the milliseconds of a Max Age."
- Rust `rs/moq-net/src/model/track.rs`: the scale-less edge and drift paths
  (the docs that say "its max age counts bare units") and the test above.
- JS `js/net/src/track.ts`: the live edge and reach that count a scale-less
  track's bare starts as milliseconds.
- The bridge: Rust `lite/publisher.rs` `Stamp::wire` already sends the bare
  value; JS `lite/publisher.ts` `wireTime` stamps `Timestamp.now()` for a
  scale-less track on lite-05/06, which changes.
- `track::Info`'s `unchecked` (a lite-05/06 copy skips the rewind check
  because untimed origins stamped send times) likely stays, since released
  publishers still do.

Tests: arrival-age expiry on a scale-less track whose starts jump; the
lite-06 bytes of a scale-less frame match the shared fixture in Rust and JS.

Public API: none; a behavior change. Wire: no byte change; lite-07-wip expiry
semantics, and a scale-less frame's lite-05/06 timestamp in JS.

## Related

- [lite-07 Group Streams carry each time once](/quest/m2/time-addressing/group-end-whole.md) - a scale-less FRAME stops carrying a timestamp on lite-07; lite-05/06 still need this one
