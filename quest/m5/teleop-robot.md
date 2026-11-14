# [S] Robot teleoperation conventions

## Goal

The track layout, discovery, and timing every teleoperated machine needs are
documented conventions in `doc/`, beside the delivery shapes the
[teleoperation guide](/doc/concept/use-case/teleoperation.md) already
documents, so an integrator writes an adapter rather than a stack. There is no
`moq-robot` crate, and this quest ships documentation only;
[the MAVLink gateway](/quest/m5/teleop-mavlink.md) implements the conventions
in its own quest. It is the gate for the rest of the questline.

## Plan

Decided 2026-11-07 (maintainer, /quest-plan from the API sweep of open PRs and remaining quests): ✅ no `moq-robot` crate (rejected: a generic robot
crate, too niche for a published library with no second consumer). The
shapes below become documented conventions, and `moq-mavlink` ships as a
moq-cli gateway like moq-srt. If a second consumer appears, generalize
an announce-prefix fan-in helper into moq-net instead of adding a robot
crate.

Decided 2026-10-08: parked in m5. No teleoperation consumer needs it yet;
telemetry beside video needs [data sync](/quest/m5/watch-data-sync.md),
not this primitive.

Narrowed 2026-11-12 (/quest-audit): the teleoperation guide
(`doc/concept/use-case/teleoperation.md`, with `rs/moq-json/examples/teleop.rs`)
already documents the two delivery shapes (moq-json snapshot for replaceable
state, an ordered moq-json stream for commands), the group cache bound and
`Evicted`, a new command track per control session, command IDs and
acknowledgments, and that a connected subscriber is not an authorized
operator. What remains extends that guide rather than restating it.

### Direction

The operator publishes and the robot subscribes. The robot serves its own
broadcast and subscribes an announce prefix for controllers; observers get the
control stream for free, and nothing needs relay support that does not already
exist.

| Broadcast | Tracks |
|---|---|
| `robot/<id>` | `video`, `telemetry` (lossy), `rpc` (reliable) |
| `control/<id>/<operator>` | `command` (lossy), `rpc` (reliable) |

### Contents

- The broadcast and track layout above, and how a robot learns its operators
  by announce prefix, documented against moq-net's origin announce stream
  (`origin::Consumer`'s announcements under a prefix). moq-boy no longer reads
  viewer broadcasts (its viewers dial a socket endpoint), so there is no
  fan-in code to move; a helper lands in moq-net only when a second consumer
  needs one.
- The catalog entries. The hang catalog already advertises data tracks in its
  `json` and `binary` sections (`rs/hang/src/catalog/{json,binary}.rs`), and
  each entry carries `extra` fields. Decide whether the robot's fields ride
  there or in a namespaced root section through `moq-mux`'s catalog
  extension. No hang schema change.
- Opaque bytes: plain tracks with one group per latest value for the lossy
  class, or one bounded ordered group for the append-log, setting the track's
  `compression` flag when useful. The lossy class is lossy on the wire because
  the publisher's `Info::max_age` evicts an aged-out group with `Error::Old`
  (`rs/moq-net/src/model/track.rs`), resetting its stream.
- Per-stage timestamp instrumentation, generalised from moq-boy's `status`
  track (`rs/moq-boy/src/status.rs`). Check it against the publisher-reported
  stats track ([media stats](/doc/concept/stats.md#media-reports)) before
  adding a second stats surface. Capability only: publishing a competitive
  benchmark is out of scope, since camera and USB dominate glass-to-glass
  latency.

### Cross-track correlation

Decided in the 2026-09-30 audit: correlation folds in here rather than being
its own quest. A command, the telemetry sample it produced, and the video frame
showing the result share one timebase, so a recording is usable as training
data and an operator sees what the machine actually saw.

The bridge is each broadcast's fixed catalog `media.clock`, documented in
`doc/concept/hang.md`. Media and command tracks keep their own timescales;
convert PTS explicitly into the broadcast clock before joining samples. The
robot's video and telemetry share a clock; the operator's command broadcast
supplies its own mapping. Command and telemetry tracks declare
`Timescale::MILLI` explicitly: an undeclared Rust timescale means untimed, so a
track that leaves it out carries no timestamps to join.

The clock assumption: the two hosts' wall clocks are synchronized by the
deployment, not by the library. State this in the docs, since a join across
unsynchronized hosts looks valid while being wrong. Report whether a mapping is
present, but never infer synchronization from its presence. No per-record
anchors or clock-sync mechanism; see
[#2278](https://github.com/moq-dev/moq/issues/2278).

### Check against moq-boy

Check the conventions against `moq-boy`, the only existing consumer, as the
no-arbitration case: it merges viewers' JSON button states, where the gateway
routes MAVLink frames, so if the documented shapes cannot describe its crowd
control they are the wrong shapes. moq-boy changes only where it contradicts a
convention.

## Related

- [Unbounded groups](/quest/m3/unbounded-groups/README.md) - changes the group cache bound the reliable class's one log relies on
- [arbitration](/quest/m5/teleop-arbitration.md) - which controller is obeyed
