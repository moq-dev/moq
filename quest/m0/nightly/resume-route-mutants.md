# [S] No resume or relay route mutant survives

## Goal

Nightly's mutants shards report no survivors in
`rs/moq-net/src/model/resume.rs` or `rs/moq-net/src/route/relay.rs`.

## Plan

Found by the 2026-11-20 triage of Nightly run
[44767045952](https://github.com/moq-dev/moq/actions/runs/44767045952), after
[moq-dev/moq#9217](https://github.com/moq-dev/moq/pull/9217) killed the
previous round. The survivors as of a759e4cf64, all in code changed since:

- `resume.rs`: `unraised` (700, 703: the `start < floor` and `end < floor`
  guards and their arithmetic), `Copy::ask_whole` (854), `Subscriber::poll_peek_group`
  (1172), `Subscriber::landed` (1328), `Resume::add` (1513, `at <= next`),
  `Mirrored::insert`/`remove` (1653, 1659), `Reader::resolve` (1936),
  `Reader::sync` (2043).
- `relay.rs`: `Router::publish` (2089 `Change::Live`, 2092),
  `Router::withdraw` (2149, 2163), `Router::adopt` (2391).

Line numbers drift; rerun `cargo mutants -f <file>` on current main first and
work from what still survives. For each one, add a test of the edge case the
guard decides, or delete the guard if no behavior depends on it (and say so
in the PR). The `verify-resume` and `verify-failover` models may show which
orderings a guard exists for.

Public API: none. Wire: none.
