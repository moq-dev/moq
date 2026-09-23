# Allocation-free binary stats

## Goal

A stats tick allocates nothing per entry, and any consumer can request a
FlatBuffers flavor of every stats track (`publisher.fb.z`, `subscriber.fb.z`,
`sessions.fb.z`, per tier) that is smaller and cheaper to produce and read than
`.json.z`, backed by a checked-in schema that other languages can generate
readers from and that later fields extend without breaking old readers. The
JSON tracks stay, unchanged on the wire, for dashboards and debugging.

Not here: a JS reader (the demo dashboard stays on JSON), replacing or
deprecating the JSON flavors, and any relay config switch. Both flavors are
served on demand, so an unrequested one costs nothing.

## Plan

Settled while planning:

- **Why:** a typed contract for non-Rust consumers, fewer bytes, less CPU, and
  less memory churn at scale. JSON's cost is mostly our pipeline: per-tick
  `BTreeMap<String, _>` frames, path clones, and merge-patch diffs through
  `serde_json::Value`. The format change alone would not fix that.
- **FlatBuffers via planus:** the builder resets without freeing, strings live
  in its buffer, reads are zero-copy views, and tables extend by appending
  fields. We chose it over protobuf because generated protobuf types own their
  strings and maps; this was decided over a hand-written protobuf codec.
- **One flavor, `.fb.z`:** a full snapshot per frame, with each group sharing
  one DEFLATE window, so repeated bytes compress away without deltas. There is
  no uncompressed `.fb`.
- **Extension-ready:** the schema leaves room for the client-stats extension
  (a nested table on each entry), which
  [schema](/quest/next/qos/stats/schema.md) fills in when it lands.
- **The line lands on dev:** quest 1 breaks the published `Registry::report()`,
  and quest 2 builds on it.

The line owns the end-to-end check: a relay test that subscribes to
`.json.z` and `.fb.z`, pairs frames from the same tick (deterministically,
for example by driving one tick under paused time), and asserts that the
counters agree.

## Quests

- [Allocation-free tick](/quest/next/stats-binary/tick.md) - the registry report and the stats producer reuse their buffers every tick
- [FlatBuffers flavor](/quest/next/stats-binary/flatbuffers.md) - moq-stats serves and reads `<name>.fb.z` from a checked-in schema
- [Stats format page](/quest/next/stats-binary/docs.md) - a doc/concept page for every stats track and both encodings

## Related

- [Client stats](/quest/next/qos/stats/README.md) - the extension the schema must leave room for
- [Flate track wrapper](/quest/future/flate/track.md) - the group-window discipline the `.fb.z` producer repeats
