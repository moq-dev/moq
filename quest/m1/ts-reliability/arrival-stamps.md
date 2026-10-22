# [M] Frames carry their local arrival time

## Goal

Each frame a moq-net reader receives carries the local instant it landed in
this node's cache, so a consumer judges lateness from that stamp instead of
the moment it happened to poll. TS export uses it, so a stall of the whole
path no longer lets held frames out past their deadline: the ignored arm
`a_path_stall_drops_what_it_made_late` in `rs/moq-mux` passes and loses its
`#[ignore]`.

Non-goals: wall-clock or cross-hop provenance
([received timestamps are media time](/quest/m1/lite-time-provenance.md)),
and any wire change.

## Plan

Found by the TS recovery arms
([moq-dev/moq#6139](https://github.com/moq-dev/moq/pull/6139)): the export
assumes a frame arrived when it last found the source empty, and during a
total stall nothing wakes it, so that guess can lag the real arrival by a
delay or more. Treating a poll after `Pending` as "arrived now" was tried and
reverted: it breaks the `two_legs_*` tests, because a caller polling on its own
cadence judges arrivals differently from one polling after every frame.

Decided by the maintainer on 2026-10-20 (`/quest-plan`):

- **Per frame**, not per group: frames in a group arrive over time, so
  lateness is per frame. The cost is one `moq_clock::Instant` per cached
  frame.
- **Stamped on completion**: when the frame's last byte lands in this node's
  cache, the earliest an exporter can emit it. A locally published frame and
  a backfilled (fetched) frame are stamped the same way, when they enter the
  cache.
- **Accessor only**: `frame::Frame` keeps the stamp in a private field read
  through `Frame::arrived() -> moq_clock::Instant`, with a constructor, rather
  than a public settable field or a separate reader method. Adding a private
  field breaks struct literals of `Frame`, so `rs/moq-net/MIGRATE.md` gets an
  entry.
- **Rust only**: the only consumer is the Rust TS export. `@moq/net` mirrors
  it when a JS consumer needs it.
- The stamp reads the ambient `moq_clock`, so simulated tests control it.

Decided 2026-10-22 (maintainer): once
[#6113](https://github.com/moq-dev/moq/pull/6113) lands, the container
consumer's hole budget is anchored on the stamped arrivals too, so the export
has one notion of arrival.
