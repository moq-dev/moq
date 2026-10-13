# [M] A failed rendition fails alone

## Goal

When one rendition's encoder fails mid-stream in `@moq/publish`, only that
rendition goes down, and loudly: its track ends with the encoder's real
error, viewers stop selecting it, and the application can see which
rendition failed and why. The other renditions, the catalog, and the network
broadcast keep running under the same epoch. A failure never starts a retry
loop.

## Plan

Raised as a declined Codex P1 on moq-dev/moq#5068, which made each track live
as long as its network broadcast.

Current behaviour:

- A video encoder error aborts its track (`producer.close(err)` in
  `js/publish/src/video/encoder.ts`) and nothing else reacts. The catalog
  keeps advertising the rendition as enabled, so viewers keep selecting a
  track that has ended.
- A fatal audio encoder error closes the current track and every later one
  handed to that rendition, for the encoder's whole life. Its catalog entry
  stays too.
- `Broadcast` (`js/publish/src/broadcast.ts`) notices the closed track only
  when the rendition set next changes, for any rendition. It then bumps
  `#restart`, which publishes a new network broadcast under a new epoch, so
  every healthy rendition and the catalog restart with it. With a dead audio
  encoder this repeats on every later rendition change, because the new
  broadcast's audio track closes at once.

Direction:

- Separate failing from recovering. Failing is local and immediate: end the
  track with the error, take the rendition out of selection, and expose the
  failure to the application (for example an error signal on the encoder or
  the `Rendition`).
- Only a change of inputs (codec, config, capture, or an explicit
  application action) may re-arm a failed rendition, never the close itself,
  so a deterministic failure stays down instead of looping. No timed retry.
- One rendition failing never republishes the network broadcast. Remove the
  `#restart` path for closed rendition tracks, or narrow it to whatever the
  questions below settle.
- Watch for the interaction with [simulcast rung disable](/quest/m1/simulcast-rung-disable.md),
  which also drives `enabled`: a recovering grant must not re-enable a
  failed rendition.
- Tests on mock time with a fake encoder: a failing video rendition leaves a
  sibling rendition and the catalog writing under the same epoch; viewers
  stop being offered the failed rendition; a dead audio encoder does not
  republish the broadcast when another rendition registers; any recovery
  never reuses a sequence.

Open questions for the maintainer:

1. How the catalog shows a failed rendition. (a) `enabled: false` with its
   last config, which `@moq/watch` and native consumers already skip
   (recommended, since it leaves room to recover); (b) remove its entry,
   which fits a permanent failure.
2. How a failed rendition recovers inside the same broadcast. This revisits
   #5068's decision 7, which moved every track to a new network broadcast.
   (a) It doesn't: it stays down until the broadcast is replaced for another
   reason; (b) the replacement track continues the name's sequences, writing
   its first group explicitly after the closed track's last one
   (recommended: the same rendition continuing its numbering is the same
   content, and only that rendition's viewers resubscribe); (c) the
   replacement publishes under a new track name, which needs
   [catalog rendition IDs](/quest/m1/catalog-track-id.md) to keep the
   catalog ID stable.
3. What re-arms a failed rendition. (a) Only a changed input or an explicit
   application call (recommended); (b) also one automatic retry with a fresh
   `VideoEncoder`, since a GPU reset can kill one instance; audio never
   retries.
4. Scope. (a) JS `@moq/publish` only, where the library owns the rendition
   tracks (recommended); (b) also the native multi-encoder publishers (OBS
   multitrack, moq-ffi), after checking how they handle one encoder failing.

## Related

- [Simulcast rung disable](/quest/m1/simulcast-rung-disable.md) - the other driver of a rendition's `enabled` state
- [Catalog rendition IDs](/quest/m1/catalog-track-id.md) - lets a rendition move to a new track name without changing its catalog ID
