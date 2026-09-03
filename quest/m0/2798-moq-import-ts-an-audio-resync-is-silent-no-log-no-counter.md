# [L] moq import ts: an audio resync is silent - no log, no counter, no downstream signal

## Goal

Implement and verify the behavior tracked in [#2798](https://github.com/moq-dev/moq/issues/2798)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

##### First, the fix works

Replicated #2751 against real content, building the merge commit (`36c3bbd73`, `moq-mux` 0.9.5) and its parent (`2b62488e5`, 0.9.4) so each arm has a before and an after. Source is a 20 s cut of a 9.95 Mbps DVB capture  -  H.264 1080i + MP2 + AC-3 + teletext + 3× SCTE-35  -  through a real relay to a real subscriber.

| Arm | one-byte change | 0.9.4 | 0.9.5 |
|---|---|---|---|
| MP2 header | sync `0xFF` → `0xFE` (`cmp -l` = 1) | **died at 12 s**, `missing MP2 frame sync` | ran to end, rc=0 |
| H.264 start code (control) | `0x01` → `0x00` | survived | survived |
| Full A/V, `--infinite` | none | **died at the first wrap** | survived 2+ wraps |

The looped arm is the production shape from the original report  -  the one that accumulated 216 publisher restarts  -  and it now runs through its wraps.

The cost is as small as it could reasonably be. Comparing the damaged run against a clean control by PTS set on every elementary stream: **exactly one 24 ms MP2 frame dropped**, at the damage point; **nothing published that the clean run did not publish**, on any PID, so the damaged frame is discarded rather than emitted as mixed bytes; video, AC-3, teletext and all three SCTE-35 PIDs untouched; all eight tracks delivered. Thank you  -  that is exactly the right shape, and confirm-before-trust is a better answer than the naive scan suggested in #2729.

##### The ask

A resync currently leaves no trace anywhere. On the recovered stream:

- **no log line**  -  the resync path has no `tracing` call at any level (the only new ones are `debug!` in `finish()` for a partial frame at end of stream);
- **no counter or metric** exposed by the importer;
- **no `container::Producer::discontinuity`**  -  that counter exists and is bumped on a timeline rewind, but a resync does not touch it;
- **nothing in the emitted TS**  -  0 continuity errors, 0 `discontinuity_indicator`s, identical to the clean control. The audio timeline simply steps 24 ms → 48 ms.

So a feed that is losing audio has no signal distinguishing it from a healthy one, at any layer.

This is a trade worth naming: before the fix, sync loss was *maximally* visible  -  the process died. I only discovered my own source was wrapping mid-frame because of the 216 restarts it caused. After the fix the identical condition produces a stream that looks perfectly well-formed. The robustness is plainly the right call, but the diagnostic signal went with it, and for a 24/7 contribution feed "quietly dropping frames" is the failure mode you most want to be able to see.

To be clear about what this is *not*. I checked whether two independent importers on the same damaged source resync differently, and they do not  -  both dropped precisely the same frame  -  so this is not a correctness or redundancy problem. And a PTS gap is arguably the right way for MPEG-TS to represent a lost access unit; `discontinuity_indicator` is about the timebase, not a missing frame. This is an observability request, not a claim that the output is wrong.

The minimum that would help is a `tracing::warn!` on a completed resync carrying the PID and the bytes discarded  -  enough to correlate against a source. A counter surfaced with the other importer statistics would be better, because the thing worth alarming on is a *rate* of resyncs rather than any single one. Whether it should also reach consumers through `container::Producer::discontinuity` is a larger question and I have no strong view; the counter alone would cover the operational case.

I appreciate the doc comment on `Resync` states the policy deliberately ("a few lost milliseconds of audio stay a gap in one track rather than an error that takes the whole session down")  -  this is not arguing with that policy, only asking that the gap be countable.

##### Environment

`moq-mux` 0.9.5 (`36c3bbd73`) against 0.9.4 (`2b62488e5`), macOS, `moq-relay` and `moq` built from the same tree, all on loopback. Source: real DVB capture, H.264 1080i25 + MP2 + AC-3 + teletext + 3× SCTE-35, 9.95 Mbps.

Follow-up to #2729 / #2751.

## Closes

- [#2798](https://github.com/moq-dev/moq/issues/2798) - close this issue when the quest finishes
