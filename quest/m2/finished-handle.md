# [M] Finishing a media producer returns a Finished handle

## Goal

The media producers and importers take `finish(self)` and return a
`Finished` handle, as moq-net's `track::Producer` and `group::Producer`
already do, so appending through the handle that finished is a compile error
instead of a runtime one. The handle keeps what finishing still allows:
aborting, which reports a later failure and releases the cache, and, where
the type supports it today, inserting below the end. It has no append. The
types are #8210's: moq-mux's container producer and the MKV, fMP4, FLV, and
TS importers, moq-audio's producer, and moq-json's `snapshot`, `stream`, and
`window` producers, with their `js/json` and `js/hang` mirrors. moq-rtmp and
moq-srt already consume on finish.

The compile-time guarantee covers only the consumed handle, and only in
Rust. A `Clone` producer that shares its state (such as moq-json's
producers and moq-mux's `catalog::Producer`) leaves other clones that can
still append, so those clones keep a shared runtime refusal after any clone
finishes. The JS mirrors, moq-ffi, and moq-c can't consume a handle, so they
keep a runtime guard that refuses an append after finish.

## Plan

Decided 2026-11-07 (maintainer, /quest-complete), a follow-up of
[moq-dev/moq#8210](https://github.com/moq-dev/moq/pull/8210), which made a
failed finish final with explicit state and recommended against
`finish(self)`. The maintainer chose the typestate:

- ✅ **Scope: moq-net and media.** Not chosen: moq-net only; media only.
- ✅ **Name: `Finished`**, as `track::Finished`, `group::Finished`, and so
  on.
- ✅ **Settle it in m2 before the cut** (maintainer, /quest-plan from the
  API sweep, 2026-11-07; rejected: shipping `finish(&mut self)` plus a
  refusal, then breaking after the cut).

[moq-dev/moq#8276](https://github.com/moq-dev/moq/pull/8276) landed the
moq-net layer and its `js/net` mirror. Follow its shape:

- A `Finished` wraps the producer (or its shared handle) privately and
  exposes only what finishing still allows. `track::Finished` keeps
  `create_group` below the end, `abort`, `poll_closed`, `consume`, and
  `subscribe`; `group::Finished` keeps `abort`, `frame_count`, and
  `consume`.
- Where dropping the last handle releases something readers still need,
  as dropping the last `track::Finished` unlists the track, mark the
  `Finished` `#[must_use]` and say so in its docs.
- A shared holder that must keep its producer (a session, a cloned output)
  finishes a clone, or a crate-private `finish_shared(&self)`, and keeps
  its own clone, whose appends the end refuses.
- A `compile_fail,E0599` doctest pins each missing append.
- JS: `finish()` returns `this` typed as a narrowing `Finished` interface;
  the runtime guard stays on the producer.

Decide per type what `Finished` exposes beyond abort, such as read access to
the cache or insert below the end, without adding anything the producer
can't do today. A `finish` that can fail returns the producer in its error
when the caller can retry, and the PR says which do. None in moq-net could:
a failed finish there is final.

This breaks every caller that finishes and then keeps using the producer.
List them in the PR, with every changed signature and the migration for each
package in its Impact section, including the runtime guards kept in
moq-ffi, moq-c, and the JS mirrors.

Public API: breaking in moq-mux, moq-audio, moq-json, `@moq/json`, and
`@moq/hang`; moq-ffi and moq-c keep their signatures and runtime checks.
Wire: none.

## Related

- [moq-audio codecs flush](https://github.com/moq-dev/moq/pull/8270) - rewrites `moq-audio`'s `encode/producer.rs`, whose `finish` this changes
- [Mux lifecycles hold one phase each](https://github.com/moq-dev/moq/pull/8211) - rewrites moq-mux's `catalog::Producer` lifecycle
- [A failed finish is final](https://github.com/moq-dev/moq/pull/8210) - the explicit state this builds on
