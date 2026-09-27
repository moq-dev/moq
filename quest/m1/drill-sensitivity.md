# [XS] The subscriber-leaks-broadcasts mutation applies again

## Goal

The nightly `Tests (test drill-sensitivity)` job passes. It fails on
[run 36240326747](https://github.com/moq-dev/moq/actions/runs/36240326747)
because `test/drill/mutations/subscriber-leaks-broadcasts.patch` no longer
applies ("1 out of 2 hunks FAILED" on `rs/moq-net/src/lite/subscriber.rs`)
after a later change to the lite subscriber, so the
`cancel_under_backpressure_releases_the_reader` drill proves nothing.

## Plan

- Retarget the patch the way
  [#3953](https://github.com/moq-dev/moq/pull/3953) did: find where the lite
  subscriber now releases the broadcasts a session fed when it ends, and
  remove that behavior again. Keep the failure message the patch declares, or
  update it if the drill now fails with a different but still correct one.
- If the release moved somewhere a patch cannot remove cleanly, the drill may
  be pointing at the wrong layer; say so rather than force a patch.
- Prove it with `just test drill-sensitivity subscriber-leaks-broadcasts`, and
  run the other two mutations to confirm they still apply.
