# [S] js/publish: encoders advertise catalog delay

## Goal

The browser publisher advertises `delay` the way `moq-mux` does: each rendition
reports how far its minimum flush lateness trails the broadcast's earliest
rendition, as a lifetime maximum that is never lowered. A browser video encoder
running behind its audio encoder advertises that offset on video, so
`js/watch` holds it instead of playing video late.

## Plan

- `js/publish/src/jitter.ts` already measures each rendition's lateness on
  `performance.now()`, and every js/publish timestamp shares that clock, so a
  broadcast-wide sliding minimum beside the per-rendition one is enough. Mirror
  `moq_mux::catalog::Estimator`: one window per rendition, one shared by the
  broadcast, and `delay` is the gap between their minima.
- The shared minimum belongs to the `Broadcast` the encoders register on, not to
  the encoders, so a swapped broadcast starts fresh. Keep it off the public API.
- The draft's rule stands: a consumer never subtracts `delay` across
  renditions and holds the largest `delay + jitter` it subscribes to, so the
  publisher reports its sliding-baseline maximum as is.
- `CatalogProducer` already refuses a lowered or zero `delay`.

## Related

- [Data jitter](/quest/m1/data-jitter.md) - the same measurement for JSON and binary tracks in Rust
