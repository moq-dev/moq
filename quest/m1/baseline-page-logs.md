# [XS] The browser baseline keeps its page logs

## Goal

`just test wasm-baseline` keeps the publisher and consumer page logs and the
binding's error reason in its artifact, so a failing cell can be diagnosed
from the artifact alone.

## Plan

Found 2026-10-28 by [moq-dev/moq#7135](https://github.com/moq-dev/moq/pull/7135):
the harness (`test/justfile`'s `wasm-baseline`) kept neither page's log, and
the binding's error reason was lost, so diagnosing an `evaluate:
MoqError.Internal` failure meant rebuilding that context locally.

Decided 2026-10-28 (maintainer): m1, since the baseline is a Nightly lane.

Write each page's console and the error reason beside the existing
diagnostics, and check the Nightly job uploads them.

Public API: none. Wire: none.
