# [XS] dispatch.sh's tests pass without KVM

## Goal

`sh/dispatch-query.test.py` passes on a Linux x86_64 host without
`/dev/kvm`, as it does on CI's KVM runners. dispatch.sh still refuses
`check flake` on a host without KVM, the refusal that
[moq-dev/moq#7281](https://github.com/moq-dev/moq/pull/7281) added.

## Plan

Decided by the maintainer on 2026-11-01: plan it in m0 ✅ / m1 / drop.

- Today 20 of its 30 tests fail on such a host. Every run whose selection
  includes `check flake` reaches the real `/dev/kvm` probe in
  `sh/dispatch.sh`, so the mocked CI run exits 1. `just check` on main
  fails locally there too.
- The test harness owns the fix: let it fake the probe's answer, for example
  by pointing dispatch.sh at a probe path the test sets. Don't loosen the
  refusal, and don't skip the tests when KVM is missing.
- Add a case for the refusal itself: with the probe reporting no KVM, a run
  selecting `check flake` fails and names the missing `/dev/kvm`.
