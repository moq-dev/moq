# [XL] Archive producer

## Goal

`archive::Producer` takes one SUBSCRIBE_ALL prefix subscription and stores
every track of every matching broadcast into an `object_store::ObjectStore`,
each broadcast under its own key prefix. It holds what the publisher's cache
stops serving: a lost group becomes a gap and never stalls the archive. An
optional DVR mode prunes old objects. It replaces `moq_archive::Writer` and
its timelines and recovery machinery.

## Plan

Decided by the maintainer on 2026-10-17 (see the
[epic](/quest/m1/archive/README.md)):

- **Input.** One prefix subscription records every broadcast under it as it
  appears. The application maps each broadcast (path and epoch) to its key
  prefix, so a different epoch is a different prefix and the producer stores
  no epoch. Rejected: one producer per broadcast fed by per-broadcast
  requests, since SUBSCRIBE_ALL is prefix-scoped.
- **Ordering.** Each track records through an ascending subscription with max
  delay = min(the publisher's max age from TRACK_INFO, a configured cap). A
  group still missing at the cap is skipped and stays absent from the store,
  which is how a gap is recorded. Anything younger is still in the live cache.
- **Objects.** Groups coalesce in order into per-track objects, cut at a group
  boundary once a configured duration passes. An oversized timed group splits
  between frames. An untimed track (no timescale) cuts by wall clock at group
  boundaries. Each object preserves group sequences, frame boundaries,
  timestamps, and payloads. Rejected: whole-broadcast objects per window and
  size-based cuts.
- **Keys.** `<prefix>/<track>/<first group>-<first pts>` for a timed track and
  `<prefix>/<track>/<first group>` for an untimed one, zero-padded so keys sort
  in content order. A sorted LIST answers group and time lookups; there is no
  index object or manifest. Rejected for untimed: `<first group>-<first
  frame>`, since an untimed group is never split.
- **Untimed groups never split.** While an untimed group is open, the bounded
  flush rewrites its object in place under the same key, so keys stay unique
  without a frame suffix; the consumer re-reads the newest object of a growing
  track. Folded from the untimed quest (#5720 landed untimed in the old writer).
- **Track properties.** A small `<prefix>/<track>/.info` holds the
  properties TRACK_INFO needs (timescale, priority, max age), written before
  the track's first object, so a consumer knows them before reading media.
  Properties never change for a track name, so a resume that finds different
  ones fails loud. Rejected: properties only in each object's header.
- **Bounded flush** (folded from the idle-flush quest): an object holding a
  frame is stored within a bounded wall-clock delay of that frame's arrival,
  even when its track goes idle. The bound covers closing the object, not
  upload latency.
- **No manual `flush()` or `cut()`** until a caller needs one.
- **Resume.** Under a key prefix that already holds objects, each track
  continues after its last stored group.
- **DVR mode** stays in the crate, for hosts without bucket lifecycle rules:
  it deletes a track's objects older than a configured window after a grace
  period, so readers in flight finish, and always keeps each track's newest
  object so a static catalog outlives the media around it. Rejected: no
  retention in the crate.
- **Layout docs.** Delete the hang draft's Recording section
  (`drafts/draft-lcurley-moq-hang.md`): the layout is a `moq-archive` format,
  not a hang one. Document it in moq-archive rustdoc and the epic's Archives
  doc page.

Test on mock time with the in-memory store: a lost group recorded as a gap
without stalling later groups, duration and wall-clock cuts, a split oversized
timed group, an open untimed group rewritten in place, an idle track flushed
within the bound, resume after a restart (including mismatched properties),
DVR pruning with grace, a track added mid-recording, and two broadcasts under
the prefix landing under their own key prefixes.

Public API: `archive::Producer` and its config replace `Writer`, `Control`,
and the timeline-driven types. Wire: none.

## Required

- [SUBSCRIBE_ALL](/quest/m1/archive/subscribe-all.md) - the prefix subscription it records

## Related

- [Archive consumer](/quest/m1/archive/consumer.md) - reads this layout back
