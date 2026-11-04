# [S] The JS receive-credit qmux test never completes an oversized frame

## Goal

`moq-tokio::integration receive_credit::javascript_partial_frame_gives_way_qmux`
(`rs/moq-tokio/tests/integration/receive_credit.rs`) passes reliably on
moq-transport-18, fixed at its cause, never by raising a timeout or adding a
retry. Done when a loaded local repeat of `just test receive-credit` passes
every run.

## Plan

Planned 2026-11-04 (maintainer, `/quest-plan`), from the follow-ups of
[moq-dev/moq#7729](https://github.com/moq-dev/moq/pull/7729). During that
PR's iterate, `just test receive-credit` failed 1 of 4 local runs, on
moq-transport-18 over qmux, with:

```text
moq-transport-18: a frame larger than the window completed
```

#7729 found the failure unrelated to its change: the Bun sender advertises
the same absent max age as it does on main.

**The evidence is local only.** The test is `#[ignore]`d, so merge-queue
runs never run it. Only the Interop workflow's "Receive credit recovery" step
does (`just test receive-credit`, in `.github/workflows/interop.yml`). None
of the 40 most recent Interop runs failed it: the 12 failed scheduled runs
from 2026-10-23 to 2026-11-03 failed other steps, and each log shows
`javascript_partial_frame_gives_way_qmux` passing (for example
[run 41668668824](https://github.com/moq-dev/moq/actions/runs/41668668824)).
No release-gate run failed either.

The assertion is in `javascript_exhausts`. A low-priority frame twice the
window must give way to a high-priority frame. So the partial low frame must
end in an error, never complete. Completing it means the receiver granted
the low stream credit past the window, or the sender finished it before the
high frame preempted it.

- Reproduce it by repeating the qmux case on moq-transport-18 under load,
  for example beside a full `cargo nextest`.
- Find which side let the frame complete: qmux's receive-credit lease on the
  WebSocket session, the IETF draft-18 stream priority, or the Bun sender's
  command timing (`test/receive-credit/sender.ts`). Fix that side.
- If the test starts the high frame before the low frame reaches
  three-quarters of the window on the wire, make it wait on the event it
  needs rather than adding a delay.

Follow the rules in [More tests under load](/quest/m0/test-flakes-2/README.md).
