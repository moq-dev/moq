# [L] Robot teleoperation primitive

## Goal

A `moq-robot` crate carries the track shapes and discovery every teleoperated
machine needs, so an integrator writes an adapter rather than a stack. It is
the gate for the rest of the questline.

## Plan

### Direction

The operator publishes and the robot subscribes. The robot serves its own
broadcast and subscribes an announce prefix for controllers; observers get the
control stream for free, and nothing needs relay support that does not already
exist.

| Broadcast | Tracks |
|---|---|
| `robot/<id>` | `video`, `telemetry` (lossy), `rpc` (reliable) |
| `control/<id>/<operator>` | `command` (lossy), `rpc` (reliable) |

### Two delivery classes, and both already exist

`moq-json` implements exactly this split, and the crate reuses it rather than
re-deriving it from track knobs: `snapshot` is lossy (one value updated over
time, intermediate updates collapsed, older groups dropped) and `stream` is
lossless (an ordered append-log where nothing is superseded).

The use-cases draft says the same thing in terms of framing
(`drafts/draft-lcurley-moq-use-cases.md`, "Interaction"): a GROUP per input for
latency, a single GROUP with a FRAME per input for reliability. A group is one
QUIC stream (`open_uni`, `rs/moq-net/src/lite/publisher.rs`), so frames inside
it are ordered and delivered exactly once.

That guarantee is scoped, and the crate must say so rather than promise
losslessness. A group holds at most `MAX_CACHE_BYTES` and `MAX_GROUP_FRAMES`
(`rs/moq-net/src/model/group.rs`), and a write past either aborts it with
`Error::GroupTooLarge`; `moq_json::stream` never rolls its one group, so that
bound ends the track, and a link drop or reconnect loses what was in flight.
So the contract is ordered, gap-free delivery for a live reader within one
bounded log, and recovery after a reconnect belongs to the application
protocol. That is an acceptable division for MAVLink, whose mission, parameter
and file-transfer services already carry their own stop-and-wait
retransmission, but it must be stated, not assumed.

The framing is where the guarantee lives, not the subscription flags:

- `track::Subscriber::ordered()` returns an `Ordered` handle that reads the
  groups it receives in sequence order. It is a local cursor, not a delivery
  guarantee: groups that aged out or were skipped never arrive. A class built
  on it would not be reliable, which is why the reliable class is a single
  group instead.
- What makes the lossy class lossy on the wire is the publisher's
  `Info::max_age`: `evict_expired` aborts an aged-out group with `Error::Old`
  (`rs/moq-net/src/model/track.rs`), and an abort resets the QUIC stream, so
  stale bytes stop being retransmitted.
- A subscriber cannot weaken either class. `clamp_combined`
  (`rs/moq-net/src/model/track.rs`) clamps the aggregate window down to
  `Info::max_age`, and `Subscription::max_age` already defaults to
  `Duration::ZERO`, so a raw observer subscribing through `moq-net` neither
  widens the window nor has to be prevented from trying.

### Contents

- The catalog entries. The hang catalog already advertises data tracks in its
  `json` and `binary` sections (`rs/hang/src/catalog/{json,binary}.rs`),
  written by the `moq-mux` data producers, and each entry carries `extra`
  fields. Decide whether the robot's fields ride there or in a namespaced
  root section through `moq-mux`'s `CatalogExt` and `RenditionConfig<E>`.
  No hang schema change.
- Announce-prefix fan-in, generalised from `rs/moq-boy/src/input.rs`.
- The two delivery classes, as the snapshot and stream modes with the group
  structure and `Info::max_age` each one needs: `moq-json`'s for JSON, and the
  opaque-bytes ones for binary frames (`moq-flate`).
- Per-stage timestamp instrumentation, generalised from moq-boy's `status`
  track. Check it against the publisher-reported stats broadcast
  ([client stats](/quest/m1/qos/stats/schema.md), moq#2734) before adding a
  second stats surface. Capability only: publishing a competitive benchmark
  is out of scope, because Transitive's breakdown puts camera plus USB at
  roughly 100 ms of a 130 ms glass-to-glass total, so we would mostly be
  measuring somebody's webcam.

Port `moq-boy` onto the crate in the same change, as the no-arbitration case.
It is the only existing consumer, and if the abstraction cannot express crowd
control then it is the wrong abstraction.

## Related

- [arbitration](/quest/m2/teleop/arbitration.md) - which controller is obeyed
