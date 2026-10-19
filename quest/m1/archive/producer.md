# [XL] Archive producer

## Goal

`archive::Producer` takes a path prefix and stores every track of every
broadcast under it into an `object_store::ObjectStore`,
each broadcast under its own key prefix. It holds what the publisher's cache
stops serving: a lost group becomes a gap and never stalls the archive. An
optional DVR mode prunes old objects. It replaces `moq_archive::Writer` and
its timelines and recovery machinery.

## Plan

Decided by the maintainer on 2026-10-17 (see the
[epic](/quest/m1/archive/README.md)):

- **Input.** One producer records every broadcast under a prefix as it
  appears: ANNOUNCE finds each broadcast announced at its own path,
  [TRACKS](/quest/m1/archive/tracks.md) lists its tracks, and each track
  records through its own SUBSCRIBE. A covering-prefix route is capability,
  not inventory, so broadcasts served under one on request without their own
  announcement are not recorded, and a TRACKS refused with NOT_FOUND on an
  announced path means it is not a broadcast; any other refusal, such as
  UNAUTHORIZED, is that broadcast's error and reaches the caller, never a
  silent skip
  (decided 2026-10-19, replacing one SUBSCRIBE_ALL). The application maps
  each broadcast (path and epoch) to its key prefix, so a different epoch is
  a different prefix and the producer stores no epoch. Open since discovery
  became per broadcast: keep the prefix input, or take one broadcast and leave
  the ANNOUNCE loop to the host. Recommended: keep the prefix, so every host
  does not repeat that loop.
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
- **Keys.** `<prefix>/<track>/<first group>-<first frame>-<first pts>` for a
  timed track and `<prefix>/<track>/<first group>` for an untimed one,
  zero-padded so keys sort in content order. The frame index keeps the
  fragments of an oversized timed group unique and ordered, since they share
  the group and pts can repeat or go backward in decode order; pts stays for
  time lookups. `<track>` is the name in the existing canonical percent
  encoding (`rs/moq-archive/src/path.rs`, everything but `A-Z a-z 0-9 _ -`
  escaped), so a name holding `/` stays one segment and no name collides with
  `.info`; a consumer refuses a non-canonical segment. A sorted LIST answers
  group and time lookups; there is no index object or manifest. Rejected for
  untimed: `<first group>-<first frame>`, since an untimed group is never
  split.
- **Untimed groups never split.** While an untimed group is open, the bounded
  flush rewrites its object in place under the same key, so keys stay unique
  without a frame suffix; the consumer re-reads the newest object of a growing
  track. Folded from the untimed quest (#5720 landed untimed in the old writer).
- **Track properties.** A small `<prefix>/<track>/.info` holds the source
  properties the stored media depends on (timescale, priority), written before
  the track's first object, so a consumer knows them before reading media.
  Properties never change for a track name, so a resume that finds different
  ones fails loud. The source's max age is not stored: it bounds the live
  cache, not the archive, and replaying it would expire archived history
  (timestamp age measures a group against the newest one), so the consumer
  advertises its own retention instead. Rejected: properties only in each
  object's header.
- **Bounded flush** (folded from the idle-flush quest): an object holding a
  frame is stored within a bounded wall-clock delay of that frame's arrival,
  even when its track goes idle. The bound covers closing the object, not
  upload latency.
- **No manual `flush()` or `cut()`** until a caller needs one.
- **Resume.** Under a key prefix that already holds objects, each track
  continues at the frame after its last stored one: its subscription starts
  at `Position(last group, stored frame count)`, so a group split across
  objects or still open at the restart loses nothing. A timed track stores
  the rest as a new object keyed by its first frame's index and pts. An untimed track
  reads its newest object back and keeps rewriting it in place with the
  appended frames. A completed group yields no more frames, so this is the
  same rule whether or not the restart landed mid-group; if the source has
  dropped the rest, the stored frames are all the archive holds of it. The
  start is per track: each track's SUBSCRIBE carries its own floor.
  Rejected: resuming after the
  last stored group (loses a split or open group's tail) and re-recording the
  last group from frame 0 (fails once the source has dropped its head).
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
timed group whose fragments get unique keys in frame order even when pts
repeats or goes backward, an open untimed group rewritten in place, an idle track flushed
within the bound, resume after a restart (including mismatched properties,
and a restart mid-group for both a split timed group and an open untimed
group, with no frame lost or duplicated), a track name holding `/` or
starting with `.` round-tripping as one segment,
DVR pruning with grace, a track added mid-recording, and two broadcasts under
the prefix landing under their own key prefixes.

Public API: `archive::Producer` and its config replace `Writer`, `Control`,
and the timeline-driven types. Wire: none.

## Required

- [TRACKS](/quest/m1/archive/tracks.md) - how it learns each broadcast's tracks

## Related

- [Archive consumer](/quest/m1/archive/consumer.md) - reads this layout back
