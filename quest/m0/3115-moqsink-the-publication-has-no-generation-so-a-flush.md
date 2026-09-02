# [L] moqsink: a flushing restart after EOS opens a new publication generation

## Goal

`FLUSH_STOP` after `moqsink` has completed EOS resumes data flow, as GStreamer
specifies: the element publishes a fresh broadcast, catalog, and producers
without cycling through `READY`, and a buffer arriving after EOS is either
written into the new generation or refused, never into finalized producers.

## Plan

#2998 landed `Completion` in `rs/moq-gst/src/sink/session.rs`: a monotonic
per-session state (`Open`, `Eos`, `Failed`) whose first terminal transition
wins, with session identity carried by pointer equality on the handle. That
is the right shape for one generation and deliberately has no way back. In
`sink/imp.rs`, `FlushStart`, `FlushStop`, and `StreamStart` are all guarded on
the live state being open, so once `maybe_finish_locked` finished the
completion they are no-ops, `render` keeps answering `FlowError::Eos`, and the
only reset is `start_session` on the `READY` transition.

Design, settled with the #2998 author: `Completion::Eos` stays terminal for
its generation. The first `FLUSH_STOP` after EOS opens one new generation
globally (a fresh `CompletionHandle`, broadcast, catalog, and producers);
every other pad joins that generation on its next buffer rather than each pad
opening its own, so aggregate EOS membership is one set per generation.

- Separate "the producers were finalized" from "the EOS message was posted";
  the per-pad lifecycle from #2998 already distinguishes them for pads, and
  the element-level latch (`eos_delivered`) needs the same split so a
  generation can post EOS again.
- Pad lifecycles reset into the new generation on join, reusing
  `lifecycle.reset()` from `start_session`. That reset replaces the pad state
  with `Pad::new()`, which has no caps and no track, and a normal
  `FLUSH_STOP` resends the segment but not the sticky CAPS, so the joining
  pad must replay its sticky caps and rebuild its producer before accepting
  the first buffer of the new generation, or `push_buffer` drops it silently.
- The stale-error window in `post_session_error` (identity check, then a bus
  post outside the lock, which a sync handler may re-enter) is a separate
  delivery problem that generations do not close; leave it as documented.
- Tests: EOS on every pad, then `FLUSH_STOP` and buffers, asserts a second
  broadcast with a second catalog, media in it from the first post-flush
  buffer, and a second EOS; a pad that flushes late
  joins the current generation, not a third; the post-EOS buffer without a
  flush still answers `Eos`.

## Closes

- [#3115](https://github.com/moq-dev/moq/issues/3115) - close this issue when the quest finishes
