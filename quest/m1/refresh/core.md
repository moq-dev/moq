# [L] Refresh in the track model

## Goal

A Rust or JS publisher sets `track::Info::refresh` and awaits
`track::Demand::wanted`, which reports `None` while unused and `Some(n)`
when consumers want group `n` or later. A local join against a group whose
bytes after the first frame exceed `refresh` times the first frame moves
`n` past the largest group. `used()` and `unused()` are gone everywhere,
including moq-ffi and the binding wrappers. IETF tracks advertising
DYNAMIC_GROUPS and downstream NEW_GROUP_REQUESTs use the same path.

## Plan

Decided 2026-10-16 (see the [epic](/quest/m1/refresh/README.md)).

- **API (Rust).** `track::Info::refresh: Option<u32>`, documented as the
  delta-to-first-frame byte ratio past which a join asks for a new group.
  `track::Demand::wanted() -> Option<u64>`, with `wanted_changed().await`
  and `poll_wanted_changed`, following the `subscription_changed` pattern.
  Delete `Demand::used`, `unused`, `is_used`, `poll_used`, and
  `poll_unused`, and migrate every caller (about 30 Rust files, including
  moq-audio, moq-mux, moq-gst, moq-hls, moq-boy, and moq-cli).
- **API (JS).** `track.Info.refresh` and `demand.wanted: Getter<bigint |
  undefined>` replace `demand.used` and `unused()`. Migrate js/publish,
  js/json, js/flate, and the other callers.
- **moq-ffi and bindings.** moq-ffi's `Demand` (`rs/moq-ffi/src/demand.rs`)
  follows, as do moq-c, the py, swift, kt, go, and dart wrappers, `cpp/moq`,
  `cpp/obs/src`, `doc/lib/*`, and `doc/bin/obs.md`. `MoqTrackInfo` gains `refresh`, beside `max_age_us`.
  Because json producers read the ratio from the track, a binding could not
  otherwise publish a refreshing json track (revised in the #5734 review).
- **Semantics.**
  - The first consumer of a track with no group yields `Some(0)`. A paused
    producer resumes on `Some` and opens a group when `n` passes its
    largest.
  - On a join, if the newest group's bytes after its first frame exceed
    `refresh` times the first frame, `wanted` becomes `Some(largest + 1)`.
    Otherwise the join reads the current group and nothing changes. A
    request on a group under the ratio is dropped, not deferred.
  - A pending `n` clears when `append_group` or `create_group` reaches it.
    Further joins coalesce into it until then, which is the IETF
    "outstanding until Largest Group grows" rule.
  - The model measures bytes from the group's existing cache accounting,
    with no app knowledge. It assumes a group's first frame is its
    independent start, which is the moq-lite meaning of a group.
  - `Subscription` stays subscriber-settable only and gains no request
    field, so a subscriber cannot express an explicit request.
- **IETF mapping.** Replace #5708's crate-private `dynamic_groups` and
  `Subscription::new_group`:
  - DYNAMIC_GROUPS=1 on ingress becomes `refresh: Some(0)`.
  - `refresh.is_some()` advertises DYNAMIC_GROUPS=1 on egress.
  - A downstream NEW_GROUP_REQUEST counts as a join under the ratio, even
    at a relay. This is a deliberate deviation from draft-22 §9.20.19,
    which says a relay MUST forward it. Decided 2026-10-16 in the #5734
    review: the ratio is the point of refresh, and every hop's cache holds
    the same group. Document it under deviations in
    `doc/concept/standard.md`.
  - The upstream IETF driver sends NEW_GROUP_REQUEST when `wanted` passes
    the largest group.

  Keep #5708's suppression and teardown tests passing.
- **Tests.** Model tests under simulated time cover the first consumer,
  unused, a join under the ratio, a join over it, coalesced joins, clearing
  on append, `refresh: None`, and a replaced producer. A benchmark sweeps
  tracks against subscribers per track, so the join check stays on the
  touched track.
- Update the Unreleased section of `doc/setup/upgrade.md` for the `Demand`
  break, and `doc/concept/standard.md` for the IETF mapping.

Public API: breaking (`Demand` in Rust, JS, moq-ffi, and every binding;
`track::Info::refresh` and `MoqTrackInfo::refresh` added). Wire: none in this quest. IETF fields are
interpreted as described above.
