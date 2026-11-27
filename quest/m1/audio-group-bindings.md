# [M] Binding importers group audio at 100 ms themselves

## Goal

The codec importers that `moq-ffi`, `libmoq` and the language bindings
(Python, Swift, Go, Kotlin, Dart, C) use close an audio group once it holds
about 100 ms, as the Rust and JS publishers do after
[the 100 ms default](https://github.com/moq-dev/moq/pull/9561), so a binding user gets the
relay CPU and memory savings without calling `cut()`. An explicit `cut()`
still works. Non-goal: picking the default again, beyond the measurement
below.

## Plan

Decided 2026-11-24 (maintainer, /quest-plan after
[moq-dev/moq#9561](https://github.com/moq-dev/moq/pull/9561)): ✅ plan it in
m1 (rejected: a later milestone; drop it and keep docs only). #9561 changed
only the binding docs to say "about 100 ms".

Decided 2026-11-24 (maintainer): also benchmark 200 ms against 100 ms. Run
`bench-audio` and `just test audio-quality` at both spans, with at least two
runs per arm since single audio-quality runs sit within noise. Record the
numbers in the PR; if 200 ms wins clearly on cost without hurting quality or
join latency, bring the default back to the maintainer as a `Waiting`
decision rather than changing it here. For reference, #9561 measured a
32 x 8 room at 2.05 cores / 105 MiB with one group per packet and 0.85 cores /
61 MiB at 100 ms.

Mirror the Rust importer's grouping rather than adding a second policy;
update every binding and wrapper together, with each binding's tests.
