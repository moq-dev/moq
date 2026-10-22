# [XS] OBS drain holds up under load

## Goal

OBS "stop drains the session" (`cpp/obs/test/moq-output-test.cpp`, a
16 MiB frame under TSan) passes reliably under a loaded `just check`, fixed
at its cause. Seen in
[moq-dev/moq#6131](https://github.com/moq-dev/moq/pull/6131) and
[moq-dev/moq#6137](https://github.com/moq-dev/moq/pull/6137).

## Plan

Reproduce before fixing, with no retries, sleeps, or raised timeouts.
On 2026-10-22 UTC, the unchanged full OBS TSan suite on main
`e0cde150d75fe6c00e18efcbfea773e3770f1a5d` failed the `ended` and final
timestamp assertions (`moq-output-test.cpp:665-666`) at natural load 35-42
on 24 CPUs. A temporary clean merge of
[moq-dev/moq#5762](https://github.com/moq-dev/moq/pull/5762), head
`f2e52db684f7c2cca6162bd417819820297815b5`, made the unchanged full suite
pass. Baseline repeats also passed: this covers a reproduced failure,
not proof that every historical flake has the same cause. That PR owns the
close-tail drain fix; verify its merged result under load before closing
this quest. Keep the lifecycle assertions rather than weakening them around
throughput. `just obs test` is the repository entry point; exact original
shell invocations were not retained.

The JS obligation is complete: the loaded js/net suite passed all 2,493
tests, including the bounded Path assertions merged in
[moq-dev/moq#6188](https://github.com/moq-dev/moq/pull/6188). No RSS-based
assertion remains to replace here.

The original audio-quality RTT and fresh-worktree interop conditions are
implemented: the sampler observes PROBE while sampling, and the interop
entry point installs Bun dependencies before Rust invokes the JS codecs.

Public API: none. Wire: none.
