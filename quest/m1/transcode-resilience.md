# [M] Transcoder survives cold-start GOP bursts, corrupt frames, unprobeable rungs, catalog end, and history cold-starts

## Goal

`moq-transcode` delivers group 0 on realistic cached GOPs, isolates corrupt source packets to the affected group, lets active rungs drain when the source catalog finishes cleanly, skips individual unprobeable rungs without failing the ladder, and serves cold-start historical fetches without deadlocking under a `Sessions` budget.

## Plan

Five runtime resilience bugs in `rs/moq-transcode` found in the 2026-11-26 transcode audit:

1. **Cold-start broadcast channel lag (`feed.rs` / `rung.rs`):** `feed.rs` uses `CAPACITY = 16`, while `rung.rs` opens the encoder lazily on the first decoded frame (tens of milliseconds on hardware encoders). A cached 30/60 fps GOP decoded at hardware speed overflows 16 frames, triggering `RecvError::Lagged` -> `Item::Lost` and aborting group 0. Open the encoder before attaching to the feed or size cold-start buffering for a realistic GOP while keeping bounded lag-skip for genuinely slow rungs.
2. **Corrupt source packet kills shared `Feed` (`feed.rs`):** `decoder.decode(...).await?` propagates errors out of the shared decode task, terminating the `Feed` (`None` to every rung) and aborting every active rung track with `Cancel`. Isolate decode errors to the affected group: flush/reset the decoder, emit `Item::Lost`, and resume cleanly at the next group's keyframe.
3. **Catalog finish aborts active rungs (`lib.rs`):** `Transcoder::run` breaks on `catalogs.next() => Ok(None)` and immediately calls `tasks.shutdown().await`, aborting in-flight rung tracks instead of finishing `derived` and waiting for active rungs to drain until `source.closed()`. Keep abort-on-shutdown only for catalog errors or `source.closed()`.
4. **One unprobeable rung fails the entire ladder (`pipeline.rs` / `catalog.rs`):** `catalog::resolve` only filters `width < 2 || height < 2`, and `Pipeline::new` fails if any rung fails `config.probe().await` (for example, narrow or small geometry below a hardware encoder's minimum). Warn and skip an individual unprobeable rung instead of failing the whole ladder.
5. **History cold-start deadlock under `Sessions` budget (`rung.rs`):** `reaches_live` returns `true` whenever `edge` is `None`, even for bounded past ranges (`end: Some(...)`), holding the live serving permit on a paused or historical source and starving its own fetches. Do not treat bounded ranges (`end: Some(...)`) as reaching live when `edge` is `None`.

Add regression tests covering a 60-frame cached GOP with slow encoder open, a corrupt source packet followed by a clean group, finite catalog-end while media drains, one unprobeable rung beside a valid rung, and a bounded historical fetch under a 1-session budget.

Public API / wire: none.

## Related

- [Transcode live on subscribers](/quest/m4/transcode-live-on-subscribers.md) - sibling session-demand behavior when no budget is configured
