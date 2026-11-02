# [S] Draft-22 interop with a native publisher holds

## Goal

Interop on moq-transport draft 22 with a Rust or C++ publisher stops failing
with "invalid fetch range" and an audio gap, or is shown not to reproduce on
current main.

## Plan

[moq-dev/moq#7085](https://github.com/moq-dev/moq/pull/7085) hit it and cited
`ffi-audio-gap.md`, which [moq-dev/moq#6990](https://github.com/moq-dev/moq/pull/6990)
deleted. [moq-dev/moq#7204](https://github.com/moq-dev/moq/pull/7204) has
since limited IETF publishers to one group of history, which may change it.
Reproduce on current main first. If it no longer reproduces, delete this quest
with that evidence; otherwise fix the range at its source.

Decided 2026-10-30 (maintainer, /quest-plan of the 2026-10-30 /quest-complete follow-ups): ✅ new flake child / drop.

Public API: none. Wire: none unless the fix shows a FETCH range bug.
