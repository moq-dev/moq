# [M] Cut moq-json allocation churn

## Goal

A `moq-json` update allocates a small, bounded amount that does not grow
with the document's size: an unchanged field costs a comparison, not a heap
node. The same holds for the decode side. Every user benefits: catalogs,
timelines, room chat, and the stats JSON tracks. A benchmark with a counting
allocator shows allocations per update before and after, swept over document
size and the fraction of fields that changed.

Not here: resident memory (the retained baseline trees), which we deferred
unless the churn work cuts it for free.

## Plan

Where it allocates today, as a starting point rather than a fixed list:

- `snapshot::Encoder`: `diff` builds a patch `Value` tree, `merge` folds it
  into the baseline, and a snapshot is serialized to bytes and then parsed
  back into a new baseline tree.
- `snapshot::Decoder`: it inflates into a fresh `Bytes`, parses a `Value`,
  and deserializes `T` from that `Value`.
- `stream` and `window`: check whether they repeat the same pattern.
- `moq_flate::Encoder::frame` returns a fresh `Bytes` per frame. One owned
  payload per published frame is the floor (the track caches it), but the
  compressor's working buffer can be reused.

Directions worth weighing: serialize a patch straight to reused bytes
instead of building a patch tree first, reuse serialization and inflate
buffers across updates, and apply a patch to the baseline without building
it twice. The snapshot round trip is deliberate (the baseline is exactly the
emitted bytes; see its comment), so keep that property if you change it.

Additive changes land on `main`. If a public accessor such as
`Encoder::value()` has to change shape, that is a published break: raise it
before moving the quest to dev.
