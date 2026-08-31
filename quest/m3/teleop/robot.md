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
losslessness. A group caches at most `MAX_GROUP_CACHE` bytes and evicts frames
off the front (`rs/moq-net/src/model/group.rs`); a reader that falls behind the
retained window, or joins late, gets `Error::Lagged` and cannot recover the
start of the log. So the contract is ordered, gap-free delivery for a live
reader that keeps up, and recovery after a reconnect belongs to the application
protocol. That is an acceptable division for MAVLink, whose mission, parameter
and file-transfer services already carry their own stop-and-wait
retransmission, but it must be stated, not assumed.

The framing is where the guarantee lives, not the subscription flags:

- `Subscription::ordered` is a scheduling tie-break whose own doc says groups
  may arrive out of order or not at all. It aggregates across subscribers by
  `&&`, and the IETF transport does not carry it. A class built on it would not
  be reliable, which is why the reliable class is a single group instead.
- What makes the lossy class lossy on the wire is the publisher's
  `Info::latency_max`: `commit_group` calls `evict_expired`, which calls
  `slot.group.abort(Error::Old)` (`rs/moq-net/src/model/track.rs`), and an abort
  resets the QUIC stream, so stale bytes stop being retransmitted.
- A subscriber cannot weaken either class. `clamp_combined`
  (`rs/moq-net/src/model/track.rs`) clamps the aggregate window down to
  `Info::latency_max`, and `Subscription::default()` is already
  `Duration::ZERO`, so a raw observer subscribing through `moq-net` neither
  widens the window nor has to be prevented from trying.

### Contents

- The catalog section, through `moq-mux`'s `CatalogExt` and
  `RenditionConfig<E>`. hang stays media-only; the `CatalogExt` doc example is
  already a `telemetry` section (`rs/moq-mux/src/catalog/tracks.rs`, with a
  `gps` rendition in its tests), so this is the designed seam and needs no hang
  schema change.
- Announce-prefix fan-in, generalised from `rs/moq-boy/src/input.rs`.
- The two delivery classes, as `moq-json`'s snapshot and stream modes with
  the group structure and `Info::latency_max` each one needs.
- Per-stage timestamp instrumentation, generalised from moq-boy's `status`
  track. Check it against the publisher-reported stats track
  ([publisher-stats](/quest/m2/qos/publisher-stats.md), moq#2734) before adding
  a second stats surface. Capability only: publishing a competitive benchmark
  is out of scope, because Transitive's breakdown puts camera plus USB at
  roughly 100 ms of a 130 ms glass-to-glass total, so we would mostly be
  measuring somebody's webcam.

Port `moq-boy` onto the crate in the same change, as the no-arbitration case.
It is the only existing consumer, and if the abstraction cannot express crowd
control then it is the wrong abstraction.

## Related

- [arbitration](/quest/m3/teleop/arbitration.md) - which controller is obeyed
