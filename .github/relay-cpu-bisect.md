# Relay CPU bisect checkpoint

Stopped at the user request on 2026-11-01. The investigation is unfinished. No production optimization, complete culprit attribution, or feature-cost acceptance is claimed.

The reusable driver is `just bench-profile instructions`. Exact per-cell counts, boundary reports, source sample markers, artifact hashes, and excluded attempts are in [the evidence file](relay-cpu-bisect.json). Raw captures remain in this checkout’s ignored `.tmp/bench/`.

## Method

Each historical relay is paired with the same production control, [21d6da9fc1](https://github.com/moq-dev/moq/commit/21d6da9fc108b48373a961f34e43c3fcd38b0146), driven by the current generator. Builds use the profiling profile, frame pointers, and `moq-relay/io-uring,moq-bench/uring`, with a target per revision. Four io\_uring workers use cores 4-7; the generator uses 0-3. The chat workload has one publisher, 2,000 viewers, 10 fps, and one 300-byte frame per group.

Three alternating historical/current pairs count user and kernel instructions over acknowledged five-second PMU windows after seven seconds of warmup. Delivery is interpolated from the generator’s surrounding 500 ms reports at those same boundaries. This is sampled delivery accounting. The normal host summaries cover a different final window and are not substituted for these counts.

Fixed-period instruction profiles use 10,000,019 instructions/sample. The host’s original 1,000 Hz sample limit throttled every interrupt. Each successful gated run temporarily requested a 100,000 Hz limit and restored the original on exit. The kernel sometimes lowered the active limit itself; captures record it and refuse lost or throttled samples. The committed driver changes no sysctls.

## Measurements

Cells are medians of three, with min-max in brackets, in thousands of instructions per delivered frame. Each row has its own interleaved current control; this is not a directly interleaved pair of adjacent historical commits. Host load and scheduling still affect instruction counts. No confidence interval or arbitrary performance threshold is asserted.

| Historical revision | Historical kinstr/frame | Paired current kinstr/frame | Evidence |
| --- | ---: | ---: | --- |
| [cdff3c8f95](https://github.com/moq-dev/moq/commit/cdff3c8f952b1d6bd2fd40c3f64eb0fd53b97e95), [moq-dev/moq#5926](https://github.com/moq-dev/moq/pull/5926) merged | 108.7 \[108.0-118.3] | 156.8 \[156.0-157.7] | `run.IkXbDf` |
| [55f6ed2292](https://github.com/moq-dev/moq/commit/55f6ed229250d17fc99389237659906d011d8a94), Immediately before dynamic tracks ([moq-dev/moq#6171](https://github.com/moq-dev/moq/pull/6171)) | 142.7 \[138.5-143.9] | 154.3 \[153.0-155.4] | `run.vgmg4w` |
| [dcd7d2cbb8](https://github.com/moq-dev/moq/commit/dcd7d2cbb891dc1cf72c68295ba70fbd17a4ec15), Immediately before per-reader ranges ([moq-dev/moq#5929](https://github.com/moq-dev/moq/pull/5929)) | 105.8 \[105.1-109.2] | 157.4 \[156.8-160.9] | `run.FlzdYt` |
| [fe722cec9f](https://github.com/moq-dev/moq/commit/fe722cec9f8432e0e46cb633cc5e79e59e7724cb), Per-reader ranges ([moq-dev/moq#5929](https://github.com/moq-dev/moq/pull/5929)) | 112.1 \[111.9-113.2] | 157.1 \[155.7-164.7] | `run.2bOgus` |
| [e84ffb3de3](https://github.com/moq-dev/moq/commit/e84ffb3de3139f7245bed5697754fd6add9a022e), Immediately before resume end tracking ([moq-dev/moq#6224](https://github.com/moq-dev/moq/pull/6224)) | 127.7 \[126.5-134.2] | 156.4 \[155.3-156.6] | `run.iFmfVR` |

The five comparisons above delivered approximately 20,000 frames/s and their boundary reports recorded zero confirmed lost groups. The evidence retains exact delivery estimates, including variation; no outlier cells were trimmed. Failed attempts are excluded explicitly rather than counted as completed comparisons. The final completed command, `run.9iDBWq` at [fd59eb18a5](https://github.com/moq-dev/moq/commit/fd59eb18a50f6e0ac819e73d941a3832cbdac91f), is retained separately in the JSON as **inconclusive**: delivery estimates ranged from approximately 18,364 to 22,215 frames/s and boundary reports included confirmed loss. Its counters and stacks were captured, but those differing workloads cannot establish the instruction effect of the resume-end change. No repeat was started after the stop request.

## Source attribution and ownership

- The overall chat increase is reproduced, but the complete regression has not been bisected.
- The range-serving boundary [moq-dev/moq#5929](https://github.com/moq-dev/moq/pull/5929) shows a smaller historical increase. It does not explain the whole current gap. Its cost has not been accepted as a feature cost in the release-perf epic.
- [moq-dev/moq#6224](https://github.com/moq-dev/moq/pull/6224) introduced `Reader::learn_end`, called by synchronization, retention, and finish polling. Its attempted after-change comparison was inconclusive because delivery changed and losses were reported; the source introduction is verified, but its instruction effect has not been confirmed.
- The current control’s `poll_forward_drop` path accounts for roughly 17-21% of sampled instructions across these runs, versus about 6-7% at the old endpoint. This is inclusive stack attribution, not an isolated cost or a claim that all of the path is avoidable.
- The historical pre-dynamic capture puts about 12.5% of samples under `learn_end`, while current captures put roughly 0.6-1.1% there. The later [moq-dev/moq#7532](https://github.com/moq-dev/moq/pull/7532) watch changes already alter this path. Do not attribute the whole current increase to the historical end-polling step or remove its correctness behavior.
- [moq-dev/moq#7493](https://github.com/moq-dev/moq/pull/7493) owns the overlapping lite publisher and track/resume/cache polling fixes, including the end-declared flag and fewer cursor locks. Those production paths were not edited here. The build-cost audit [moq-dev/moq#7602](https://github.com/moq-dev/moq/pull/7602) and archive epic [moq-dev/moq#7604](https://github.com/moq-dev/moq/pull/7604) are also untouched.

The source markers in the evidence are inclusive, can overlap, and are not additive. They identify where to continue reading, not a finished attribution of deliberate versus avoidable costs.

## Remaining scope

The existing [quest](../quest/m1/release-perf/relay-cpu-bisect.md) retains the work. Obtain a comparable-delivery measurement of the resume-end boundary. Resolve the unbisected growth between per-reader ranges and the parent of that change, then the later interval from the pre-dynamic point to the pinned control. Account for intervening improvements as well as regressions. Coordinate overlapping fixes with the fanout-lock owner before changing those files. Confirm attributed steps on video and Tokio workers; implement any remaining avoidable costs with publisher/viewer benchmark sweeps. Record justified feature-cost acceptances in the release-perf epic, then complete checks and local other-vendor review. None of this work was started after the stop request.

## Validation

- Focused profile, relay lifecycle, and summary tests pass, along with Ruff and shellcheck. The profile tests cover matching windows, unsupported counters, CPU sets, loss/throttling, and identical cumulative snapshots at one timestamp.
- Two full `quest gate just ci` attempts failed on the known mbx `E2BIG` defect ([moq-dev/moq#7594](https://github.com/moq-dev/moq/pull/7594)) and runner-image `/proc/self/uid_map` namespace denial, including downstream build and tooling-test failures. The first also failed an unchanged JS announcement-sync test; the second passed JS tests. These are not full-gate passes.
- No other-vendor review or ready-for-review transition has been completed. The PR remains a draft. Public API and wire: no changes.
