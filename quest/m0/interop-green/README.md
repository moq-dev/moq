# Interop and Video pass on main

## Goal

The scheduled Interop and Video workflows pass on `main`, so a red run
means a new break. Interop has failed every scheduled run since
2026-10-22, and Video fails on a stale expectation.

## Plan

Decided 2026-10-27 (maintainer), from an external release-to-main audit
and a triage of Interop runs
[40517499786](https://github.com/moq-dev/moq/actions/runs/40517499786),
[40344007093](https://github.com/moq-dev/moq/actions/runs/40344007093), and
[40195571889](https://github.com/moq-dev/moq/actions/runs/40195571889):
one child per distinct cause, each landing on its own, as the
[Nightly epic](/quest/m0/nightly/README.md) does.

Not failures: the `FAIL pinned round`, `timeout round`, `empty round`, and
port-reservation lines are negative self-tests in
`interop/relay-log.test.sh` and `lib/harness.test.sh` that pass.
Already owned: the intermittent media-matrix timestamp-gap cells
(`cpp -> js-native-node`, `go -> c`, browser `rust -> js` timeouts) belong to
[test flakes](/quest/m0/test-flakes-2/README.md), with
[moq-dev/moq#6990](https://github.com/moq-dev/moq/pull/6990) open.

This README's own work, after the children: confirm a scheduled Interop
and Video run each pass.

Public API: none. Wire: none.

## Required

- [Browser tests expect current behavior](/quest/m0/interop-green/stale-browser-tests.md) - the renderer and element version tests match the shipped validation and version list
- [Remeasure the planned wire break](/quest/m0/interop-green/wire-compat-remeasure.md) - the lite-07-wip planned break is measured against `@moq/net` 0.4.6
