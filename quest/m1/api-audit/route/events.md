# [M] Listing and follow

## Goal

Applications read announcements as a listing: each path is present or
absent. Instance changes, whether a restart or a takeover, are handled
inside `follow` and `request`, not seen as announce events. The `Start`,
`Update`, `End`, `Restart` and `Takeover` variants leave the app API in
Rust, JS, moq-ffi, moq-c and the wrappers. Route metadata changes (cost, hops,
preference) move to a relay-level `routes()` stream that moq-relay and
stats use.

Non-goals: the wire's ANNOUNCE and ANNOUNCE_RESTART messages, which stay as
they are, and the route handle ([One Route handle](/quest/m1/api-audit/route/handle.md)).

## Plan

Decided 2026-10-30 (maintainer); see the [epic](/quest/m1/api-audit/route/README.md).
The audit found that almost every production caller merges the variants,
for example `rs/moq-relay/src/internal.rs:933`, `rs/moq-rtc/src/lib.rs:81`
and `rs/moq-cli/src/archive.rs:85`. Every JS consumer branches only on
`end`. Only `rs/moq-boy/src/input.rs:79` handles `Restart` alone, and it
moves to `follow`.

- The listing yields present and absent per path, with the announcement's
  read-only facts. A path that changes identity stays present. To see an
  identity change, hold a `follow`.
- `follow` yields a new broadcast when the path's identity changes, as the
  read side of [Identity and epoch](/quest/m1/api-audit/route/identity.md)
  defines (an epoch change within one identity is invisible). It also needs an `End`
  signal for when the path goes absent, so that `moq_mux::follow::Instances`
  can fold into it: delete the wrapper's duplicate epoch filter and keep
  its one-shot fixed mode only if a caller needs it.
- `routes()` lives on the origin consumer for relays and stats, and is
  hidden from the app-level docs.
- Format the go and python interop clients while touching them (two lines
  were already unformatted on 2026-10-30).
- `Takeover` (landed in [moq-dev/moq#6994](https://github.com/moq-dev/moq/pull/6994))
  goes with the rest, along with its predecessor fields in
  `MoqAnnounceEvent` and `moq_announce_event`, unless
  [announce takeover](/quest/m1/announce-takeover.md) has already removed it.
- Bindings: replace `MoqAnnounceEvent`'s variants, and the go and python
  interop clients' and carla `publish.py:303-340`'s Restart handling, with
  the listing plus `follow`.
- Update `doc/concept/identity.md`'s follower section and the binding docs.

Run `just check` and `just test interop --all`.

Public API: breaking across the announce stream in every language. Wire:
none.

## Related

- [Announce takeover](/quest/m1/announce-takeover.md) - a job's exact route continues its claim's readers inside `follow`, with no public event
