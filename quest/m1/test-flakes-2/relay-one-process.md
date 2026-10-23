# [M] Finish the relay one-process validation boundary

## Goal

Resolve the unclassified optional io_uring shared-process failure in
[moq-dev/moq#6547](https://github.com/moq-dev/moq/pull/6547), or settle that the
original default-feature command defines this quest's boundary. Preserve
`cargo test -p moq-relay --test integration` with default threads in one
process and its nightly coverage. No test-thread cap, retry, or longer deadline.

## Plan

The draft implements the original default-feature Goal. On base
[4c6127f6c7](https://github.com/moq-dev/moq/commit/4c6127f6c722198cfa6e6f482f6e73c9478e6f96),
the actual concurrent binary had 108 passes, 10 failures and one ignored test.

- Non-signal fixtures now use the existing `with_signals(false)`. Real SIGINT
  and SIGTERM from the shutdown tests had drained unrelated relays; their
  module mutex only protected the signal tests from each other. That change
  alone removed nine failures, including the apparent metrics, clock,
  endpoint and announcement failures. Those production owners are unchanged.
- Log captures keep scoped writers and an explicit no-op process fallback.
  A deterministic test reproduced tracing-core's single-dispatcher callsite
  registration across threads. Concurrent captures also prove writer isolation.
- Native io_uring workers and shared-runtime handoffs carry the embedder's
  scoped dispatcher and parent span. The connection log test exercises both.
- The existing nightly io_uring lane also runs the literal uncapped
  default-feature integration binary, alongside its existing nextest coverage.

Validation so far: required Nix CI passed 815 test executions, the final
uncapped default binary passed 120 tests with one existing ignored test, and
16 selected io_uring/log nextest cases passed. The initial final-source
uncapped io_uring binary passed 135 tests with one ignored test.

A later identical io_uring shared-process run failed eight worker cases and
stalled in the manual-clock handshake case. It was stopped after more than
three minutes; libtest had not flushed its captured panic details. A focused
refusal diagnostic passed, and one bounded identical `--nocapture` diagnostic
passed 135 tests. These passes do not explain or replace the failed run.
The local locked-memory limits are 8 MiB and ring accounting is shared per
user, but capacity was not proven as the cause. No host or cache settings
changed, and no further reproduction loop is planned in this checkpoint.

Decision before readiness:

1. Keep the original uncapped default-feature contract and the existing
   process-per-test io_uring contract (recommended). Retain the optional-mode
   failure as explicit evidence requiring separate classification.
2. Extend the one-process contract to the io_uring feature, classify the
   intermittent failure, and fix any genuine shared-state or lifecycle cause
   before finishing this quest.

Runtime measurements and complete checkpoint-specific logs remain with the
PR's isolated checkout. Public API: none. Wire: none.
