# [XS] The merge gate compiles the C fixtures against the generated header

## Goal

A PR that changes a libmoq C function's signature fails the merge gate if any `rs/libmoq/c-tests/*.c` still calls the old shape, instead of the break first showing up in Nightly.

## Plan

Found by the 11-17 Nightly triage: [moq-dev/moq#8999](https://github.com/moq-dev/moq/pull/8999) merged 3 minutes after [moq-dev/moq#8996](https://github.com/moq-dev/moq/pull/8996) added `moq_consume_catalog`'s `delay_us` argument, and `abort-unwanted.c` kept the 3-argument call until [moq-dev/moq#8941](https://github.com/moq-dev/moq/pull/8941). Nothing in the gate compiles those files.

Recommended: `cc -fsyntax-only` each fixture against the generated header in the gate, about a second. `test/smoke/clients/c/subscribe.c` builds against the published package on purpose, so it stays out.

The Swift break the same night (`SmokeTests.swift`, [moq-dev/moq#8906](https://github.com/moq-dev/moq/pull/8906) merging after [moq-dev/moq#8948](https://github.com/moq-dev/moq/pull/8948)) stays nightly-only: Swift doesn't build on Linux runners.

Public API: none. Wire: none.
