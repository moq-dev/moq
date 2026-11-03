# [XS] Register moq-dev draft 20 in the interop runner

## Goal

The `moq-dev-rs` and `moq-dev-js` entries in
`englishm/moq-interop-runner` declare moq-transport drafts 18, 20, and 22.
Their notes and the two README table rows agree with that registry.

## Plan

Protocol removal already landed in
[moq-dev/moq#6753](https://github.com/moq-dev/moq/pull/6753) and
[moq-dev/moq#6754](https://github.com/moq-dev/moq/pull/6754); draft 20 returned in
[moq-dev/moq#7190](https://github.com/moq-dev/moq/pull/7190).
[englishm/moq-interop-runner#178](https://github.com/englishm/moq-interop-runner/pull/178)
registered only drafts 18 and 22 before draft 20 returned.

The maintainer approved posting the registry correction there on 2026-10-24.
[englishm/moq-interop-runner#183](https://github.com/englishm/moq-interop-runner/pull/183)
adds draft 20 to both entries, distinguishes the registered interop set from
the wider capability of published releases in both notes, and replaces the
README's stale drafts 14-17 with 18, 20, and 22. Its registration and image
publication checks passed locally, and its final local review had no findings.
No live interop suite was run for the metadata-only correction.

Verified again on 2026-11-03: the published Rust client image contains draft-20
ALPN/config names; its lockfile resolves `moq-tokio` 0.19.28 and `moq-net`
0.3.15, whose published crate offers `moqt-20`. The published JS client image
contains `@moq/net` 0.4.7 and advertises `Ietf.ALPN.DRAFT_20`. The relay adapter
wraps `docker.io/moqdev/moq-relay:latest`, now the same digest as `0.17.11`,
whose binary and tagged source include draft 20. No release or pin bump is
needed, and no protocol code changes belong in this task.

After the external PR merges, delete this quest and its links. No further
protocol implementation or release work remains here.

## Waiting

- englishm/moq-interop-runner: review and merge [englishm/moq-interop-runner#183](https://github.com/englishm/moq-interop-runner/pull/183); verify both registry entries and README rows list drafts 18, 20, and 22 before deleting this quest

## Related

- [Interop coverage](/quest/m1/interop-coverage/README.md) - cross-implementation cases target drafts 18, 20, and 22
