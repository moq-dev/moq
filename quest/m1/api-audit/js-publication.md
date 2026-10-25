# [S] Validate JS origin publications

## Goal

The JS publication split in [moq-dev/moq#6795](https://github.com/moq-dev/moq/pull/6795)
passes the full local gate and is ready for review. An origin returns a
`Publication` with announcement control and a readonly fill `broadcast`;
standalone and prepared fill producers cannot announce.

## Plan

Implementation and consumer migration are complete in that PR. The approved
shape and lifetime mirror Rust's `origin::Publication`: releasing a publication
reference does not retract its broadcast, and `unannounce` retracts sooner.

Remaining work is validation. Re-run `just ci` after the host load gate admits
its jobs, then remove this quest and its links and mark the PR ready. The first
attempt remained at admission for the benchmark Rust check and was stopped;
no gate result is claimed. The shared test typecheck also needs the generated
`@moq/wasm` declarations, absent from a fresh checkout until its WASM build.

Completed checks: 2,832 net unit tests, 123 focused publisher/watcher/E2EE
tests, package and independent JS harness typechecks, net declaration build
and API report, Markdown, and workflow checks. Stale publication tests cover a
held fill handle and a replacement under a new epoch.

Public API: breaking in `@moq/net`; retain the returned publication for
`announce`, `unannounce`, and `route`, and use its `broadcast` to fill tracks.
Wire: none.

## Related

- [Binding publications](/quest/m1/api-audit/binding-publication.md) - mirrors the native split across the bindings
