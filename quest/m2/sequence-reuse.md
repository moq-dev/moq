# [S] A recreated or restarted track cannot reuse group sequences

## Goal

moq-net refuses to give new content an identity it already used, in Rust and JS, instead of relying on the documented contract:

- Recreating a track name in a live broadcast (`create_track` after the previous producer dropped, `rs/moq-net/src/model/broadcast.rs`) no longer restarts `append_group` at 0 under a fresh `TrackState`. The broadcast remembers each name's highest sequence, and a group at or below it on the replacement is an error.
- A publisher restarting under the same instance at a higher epoch, which `js/net/src/origin.ts` splices onto the existing logical front (and Rust's origin likewise continues), is refused loudly when its sequences go backwards, instead of resume silently dropping its groups as already delivered or a recovering reader stitching frames from two streams into one group.
- An application that really wants to reuse a name seeds it explicitly with a sequence past the predecessor's highest; the library never does it silently.
- Datagrams are documented as outside the identity rule: on the datagram API in Rust and JS and in the moq-lite draft, a datagram sequence names no cached content and repeats are delivered.

## Plan

Found by the 2026-11-07 invariant audit (model-2, js-3, model-7, js-10).

Decided 2026-11-07 (maintainer, invariant-audit planning): reuse is misuse that produces unintended behaviour, so the library prevents it and an application that really wants it seeds from the high-water mark itself; the library does not seed automatically. Datagrams get no duplicate detection: spending resources to catch application bugs there is not worth it, so they are documented as exempt instead.

m2 because `append_group`/`create_group` and the origin splice start refusing calls that succeed today, which callers recreating tracks must adapt to (the questline's breaking-change test). The API shape for the explicit seed is the implementer's call; prefer reusing `create_group` with an explicit sequence over a new method. [moq-dev/moq#8063](https://github.com/moq-dev/moq/pull/8063) changes how a job's route continues its claim's instance; build on whichever lands first.

## Related

- [A job's exact route continues its claim's readers](/quest/m2/announce-takeover.md) - instance continuity across a claim's jobs
