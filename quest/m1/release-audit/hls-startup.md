# [S] Announce a finished HLS import before draining it

## Goal

A valid finite HLS import announces its ready catalog and reaches normal
finish and linger handling. An actually empty source still fails clearly.

## Plan

P2 regression introduced by [moq-dev/moq#6253](https://github.com/moq-dev/moq/pull/6253).
At main 1d43c096e0, `rs/moq-cli/src/hls.rs` races `importer.run()` against
`listed.next()` after initialization. For an initial ENDLIST with a populated
catalog, both futures are ready. Selecting successful completion reports
"the playlist ended before it declared a track" instead of announcing.

A public-API harness using the real Import and Catalog, a one-segment ENDLIST
fixture from the repository's bbb.mp4, and the unchanged CLI select reproduced
29 false early-end failures in 50 runs. It asserted that the catalog already
had tracks. This was a CLI-equivalent harness, not a spawned CLI test.

Reconcile readiness and completion in the CLI lifecycle so a ready catalog
is consumed before deciding that completion was empty. Cover initial ENDLIST,
completion after publication, and genuinely empty input with deterministic
regression tests. Do not add a retry or delay. No public API or wire change
is expected.

## Related

- [Finite HLS start position](/quest/m1/release-audit/hls-finite.md) - a separate contract for which segments to import
