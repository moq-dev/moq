# [M] js/watch: the playout target follows the spec, proven in a real browser

## Goal

`js/watch` computes its auto target with the algorithm from
[Spec](/quest/m2/audio-jitter-target/spec.md) and passes the conformance
vector. The "Real-time" preset plays clean audio on a LAN and against the
public relay: zero underruns after convergence and no skip-aheads in steady
state, on both the isolated and the postMessage ring paths, confirmed by a
manual run on Chrome and Safari with a real microphone and 40 ms or more of
added RTT.

Boundaries: revalidate ring slack, re-stall, and underrun reporting on the
current implementation; the closed prototype does not establish correctness.

## Plan

PR #3517 is closed unmerged. Start from the then-current API, reuse only
validated prototype plumbing, and classify any published API break before
choosing the base branch.

- Reproduce the historical prototype's 14.56 s runaway with a committed trace:
  a wide initial timestamp gap followed by paced audio. Apply the shared spec's
  duration source, target clamps and bounded rise; the trace must converge
  without retaining a fabricated multi-second frame duration.
- Inspect the current container and ring before reuse. Implement arrival
  measurement at the spec's observation point and apply the shared conformance
  corpus. Validate ring slack, underrun counters, and recovery independently;
  none is considered correct merely because the prototype implemented it.
- Exercise isolated and postMessage output in real Chrome and Safari/iOS with
  recorded source, codec and network conditions. Distinguish source flush timing
  from receiver jitter and retain sanitized traces. A Chromium-only pass cannot
  close the iOS report.

## Required

- [Spec](/quest/m2/audio-jitter-target/spec.md) - the algorithm this implements
