# [M] Binary delta stats flavor

## Goal

A consumer can request a binary delta flavor of every stats track (name TBD,
for example `publisher.bin.z`, `subscriber.bin.z`, `sessions.bin.z`, per
tier) that a relay encodes and a reader decodes several times cheaper than
`.json.z`, in fewer bytes. The relay serves it only on request, so an
unrequested flavor costs nothing, and `moq_stats::Consumer`, the aggregate,
and a dependency-free JS decoder read it into the same frame types as JSON.
The JSON tracks stay unchanged on the wire.

Not here: replacing or deprecating `.json` / `.json.z`, a relay config switch,
or moving the demo dashboard off JSON.

## Plan

**Gate first.** Bandwidth is small in absolute terms (about 0.8 Mbps per
stats subscriber in the worst scenario below), so bytes alone do not justify
a new format. The motivation is relay encode CPU at 10k+ broadcasts and
aggregator fan-in. #4019 already cut `.json.z` decode allocations about 42x
(time 2.5-4x) by dropping per-key path tracking; `rs/moq-stats/benches/decode.rs`
measures it. Proceed only if CPU still matters after a profile of the
moq-json snapshot encoder (about 100 ms per tick at 10k broadcasts) has fixed
or ruled out its cost. Re-run the benchmark against that baseline; if JSON is
close enough, abandon this quest with the numbers.

**Evidence.** A prototype benchmark on top of #3955's commit `d86496be2`
simulated relay traffic from how `moq_net::stats` counts and how
`moq_stats::produce` emits (live-or-changed rows, pruned a drain after they
close), through the same group policy for every format and checked against
ground truth every tick. Only the traffic tracks were modelled, not
`sessions`. Bytes per tick, mean over 600 ticks:

| format | steady 1k | churn (conference) | large 10k x3 tiers | idle cams 2k |
|---|---:|---:|---:|---:|
| `.json.z` today | 14735 | 55211 | 101472 | 888 |
| `.fb.z` full snapshot (#3955) | 73653 | 87048 | 616416 | 20906 |
| stable-slot FlatBuffers XOR | 8105 | 18798 | 57464 | 1040 |
| protobuf merge patch, absolute values | 10670 | 53294 | 97011 | 834 |
| custom varint delta | 3754 | 9494 | 25616 | 364 |

At 10k broadcasts x 3 tiers, per tick: `.json.z` encodes in 98 ms and
decodes in 76 ms with 1M decode allocations (before #4019); a typed
`.json.z` merge patch (same wire, not landed) cut decode to 5.7 ms and 445
allocations; the varint delta encodes in 10 ms and decodes in 0.8 ms with 12
and 167 allocations. Against the best JSON decode, the remaining wins are
encode CPU (7-13x), bytes (2.4-5.8x), and a smaller decode margin (3-9x).

What the numbers taught, so the design does not relearn it:

- A full snapshot per frame fails once it outgrows DEFLATE's 32 KiB window;
  that is what sank `.fb.z`.
- Absolute cumulative counters do not compress: each is 5-6 near-random
  bytes. Varint deltas of monotonic counters are 1-3 bytes.
- Naive XOR against the previous frame keyframes on 95-99% of frames because
  any row join or leave shifts the layout.

**Format sketch** (the prototype's; adjust as the implementation learns):

- Each group is one DEFLATE window, rolled by the same policy as
  `moq_json::snapshot` (frame count, growth against the first frame, the
  cache bound). The first frame of a group is a keyframe: every row's path
  and absolute values.
- A delta frame carries new paths inline (ids assigned implicitly, continuing
  the group's dictionary), then each changed row as `(id gap varint,
  changed-field bitmask, one varint delta per set bit)`, then the removed ids
  as gaps. Ids ascend within each list, so gaps are small.
- A counter that goes backwards is a remove plus a re-add under a new id,
  which keeps every delta unsigned and matches the existing "a decrease starts
  a fresh segment" rule.
- An unchanged tick writes no frame.

Design points left open for the implementer:

- Extensibility: if every field is a varint, a reader can skip mask bits it
  does not know, so fields append without breaking old readers. Decide that
  or refuse unknown bits loudly, and say which.
- Gauges: the [client stats](/quest/m1/qos/stats/README.md) extension adds
  non-monotonic gauges (a latency, a target bitrate). Encode them absolute or
  zigzag, or have a producer with a non-`()` extension refuse this flavor, and
  document the contract.
- Measure `sessions` (`Presence`) too; the benchmark skipped it.
- Sweep the aggregate over node publishers x entries per node, for both
  flavors; the benchmark above only varied one relay's stream, so it cannot
  show the fan-in cost the gate cites.

**Serving.** Like `.fb.z` was planned: a flavor suffix accepted by
`requested_track_shape` and the track-name helpers for every name that takes
`.json.z`, created only when requested rather than with the plain/compressed
pair. Weigh replacing `compressed: bool` in the helpers with a flavor enum,
which is a published API break and goes to `dev` unless additive.

**Readers.** `Consumer` and the aggregate read either flavor into
`TrafficFrame` / `SessionsFrame`. The JS decoder is about 120 lines and
dependency-free apart from inflate (`@moq/flate`); read varints through
`BigInt` or split 32-bit halves, since counters exceed 2^53. It belongs with
the JS stats reader, `@moq/stats` if [browser reporters](/quest/m1/qos/stats/js.md)
has landed by then. Share fixtures between Rust and JS so the two stay wire
identical, and keep the benchmark in-tree, wired into CI at least nightly.

**Docs and spec.** No IETF draft covers stats today. Write the format into
the stats format page ([doc/concept/stats.md](/doc/concept/stats.md))
and the stats section of `doc/bin/relay/config.md`, plus the moq-stats crate
docs. Whether stats needs its own `draft-lcurley-moq-stats.md` is the
maintainer's call; ask before writing one.

Public API impact: additive on moq-stats unless the helpers change. Wire
impact: new on-demand tracks; existing tracks unchanged.

## Related

- [Stats format page](/doc/concept/stats.md) - where the new flavor is documented
- [Client stats](/quest/m1/qos/stats/README.md) - the extension and gauges the format must carry or refuse
- [Compressed tracks](/quest/m2/flate/README.md) - the group-window discipline this flavor repeats
