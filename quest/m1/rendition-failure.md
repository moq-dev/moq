# [M] A failed rendition fails alone

## Goal

When one rendition's encoder fails mid-stream in `@moq/publish`, only that
rendition goes down, and loudly: its track ends with the encoder's real
error, the catalog marks it disabled, and the application can see which
rendition failed and why. The other renditions, the catalog, and the network
broadcast keep running under the same epoch. A failure never starts a retry
loop.

## Plan

Raised as a declined Codex P1 on moq-dev/moq#5068, which made each track live
as long as its network broadcast.

Current behaviour:

- A video encoder error aborts its track (`producer.close(err)` in
  `js/publish/src/video/encoder.ts`) and nothing reacts right away. The catalog
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

Decided with the maintainer (2026-10-13):

- **Scope: JS `@moq/publish` only**, where the library owns the rendition
  tracks. Native publishers' apps own each track directly.
- **Fail locally and at once.** End the track with the encoder's error,
  advertise the rendition as `enabled: false` with its last config (which
  `@moq/watch` already skips; WHEP and HLS egress still serve a disabled
  rendition, a consumer gap outside this quest), and expose the failure to
  the application, for example as an error signal on the encoder or the
  `Rendition`. Failing never republishes the network broadcast, so remove or
  narrow the `#restart` path that does today.
- **Only a changed input or an explicit application call re-arms a failed
  rendition.** No automatic or timed retry, so a deterministic failure stays
  down instead of looping.
- **Do not resume numbering unless it's a continuation (same encoder).** A
  recovered rendition publishes under a new track name or a new broadcast
  epoch; when unsure, default to one of those, because that's safer. This
  keeps #5068's decision 7 for any replacement encoder. A new track name
  leaves the siblings running but needs
  [catalog rendition IDs](/quest/m1/catalog-track-id.md) to keep the catalog
  ID stable; a new epoch restarts the siblings, so reserve it for recovery,
  never for the failure itself.
- **Milestone: m1**, as reliability of a shipped feature.

Also:

- Watch for the interaction with [simulcast rung disable](/quest/m1/simulcast-rung-disable.md),
  which also drives `enabled`: a recovering grant must not re-enable a
  failed rendition.
- Tests on mock time with a fake encoder: a failing video rendition leaves a
  sibling rendition and the catalog writing under the same epoch; the
  failed rendition is advertised `enabled: false`; a dead audio encoder
  does not republish the broadcast when another rendition registers; a
  re-armed rendition never reuses a closed track's sequences.

## Related

- [Simulcast rung disable](/quest/m1/simulcast-rung-disable.md) - the other driver of a rendition's `enabled` state
- [Catalog rendition IDs](/quest/m1/catalog-track-id.md) - lets a rendition move to a new track name without changing its catalog ID
