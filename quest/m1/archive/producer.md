# [XL] Archive producer

## Goal

`archive::Producer` takes a path prefix and stores every track of every
broadcast under it into an `object_store::ObjectStore`,
each broadcast under its own key prefix. It holds what the publisher's cache
stops serving: a lost group becomes a gap and never stalls the archive.
Datagrams are lossy and never cached, so a track's datagrams are recorded
from when its subscription takes effect; earlier ones are not. An
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
- **Datagrams keep their form.** A track's groups and datagrams share one
  sequence space, so each stored sequence records its forwarding form, group
  or datagram, and a datagram is stored as its one payload and timestamp in
  sequence order beside the groups. The consumer replays a datagram as a
  datagram, never as a cached one-frame group, so the replay keeps the
  source's lossy contract. A datagram is never split and counts as one
  sequence for keys, cuts, and resume. Datagrams sent before the track's
  SUBSCRIBE took effect are not recorded, as above.
- **Keys.** `<prefix>/<track>/<first group>-<first frame>-<first pts>` for a
  timed track and `<prefix>/<track>/<first group>` for an untimed one,
  zero-padded so keys sort in content order. The frame index keeps the
  fragments of an oversized timed group unique and ordered, since they share
  the group and pts can repeat or go backward in decode order; pts stays for
  time lookups. `<track>` is the name in the existing canonical percent
  encoding (`rs/moq-archive/src/path.rs`, everything but `A-Z a-z 0-9 _ -`
  escaped), so a name holding `/` stays one segment and no name collides with
  `.info` or `.end`; a consumer refuses a non-canonical segment. A sorted LIST answers
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
- **Completion markers**, decided by the maintainer on 2026-10-19 ("record
  the error"). A track has ended once its ordered groups and its datagrams
  (`poll_recv_datagram`) have both reached their end, so no received
  datagram is lost. How it ended is decided then: a clean end at its
  declared exclusive `Position` (`track::Subscriber::poll_finished`; group
  and frame, the shape `track::Producer::finish_at` takes, so a mid-group
  end survives), or the stream error code it ended with
  (`StreamError::to_code`). The markers wait for the broadcast's listing to
  end with the instance, since until then a track removed and re-added
  under its name continues its sequences and a written marker would be
  wrong. Then, with every frame and datagram stored, the producer writes
  each track's small `<prefix>/<track>/end`, recording how its last run
  ended, followed by `<prefix>/.end`, which says no track will be added.
  Until then an ended track just stops growing. The names are chosen for the consumer's
  refresh: a track's `end` sorts after its zero-padded numeric keys, so
  the track's `StartAfter` LIST reaches it with no extra request, and it can
  never be an object key; the broadcast's `.end` sits beside the track
  segments, where the delimiter LIST that finds added tracks sees it, and
  no canonical track name starts with `.`. The consumer ends each track as
  its marker says and otherwise treats the archive as live. Only a crash, or a producer
  stopped before the end, leaves no marker, so its archive reads as still
  growing. A producer resuming mid-broadcast follows every stored track as
  live, including one that ended before the restart; it writes markers only
  for tracks whose end it saw, so a stored track that ended unseen leaves
  the broadcast without `.end`, as a stop before the end does. A resume
  under a broadcast that already has its `.end` records nothing. DVR
  pruning never deletes a marker.
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
DVR pruning with grace, a track added mid-recording, two broadcasts under
the prefix landing under their own key prefixes, a track interleaving groups
and datagrams stored with each sequence's form, a clean end writing no
marker until the listing ends and then each track's `end` (including a
mid-group end) followed by the broadcast's `.end`, a track removed and
re-added under its name before the broadcast ends whose replay holds both
runs in sequence and whose marker records the second run's end, a track
whose groups and datagrams interleave and end at once storing every
received datagram before its `end`, a track ending with an error writing an
error marker while its broadcast still gets `.end`, and a producer stopped
mid-recording writing no marker.

Public API: `archive::Producer` and its config replace `Writer`, `Control`,
and the timeline-driven types. Wire: none.

## Required

- [TRACKS](/quest/m1/archive/tracks.md) - how it learns each broadcast's tracks

## Related

- [Archive consumer](/quest/m1/archive/consumer.md) - reads this layout back
