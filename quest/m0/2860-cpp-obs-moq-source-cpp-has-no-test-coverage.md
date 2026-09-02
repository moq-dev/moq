# [M] cpp/obs: moq-source.cpp has no test coverage

## Goal

`just obs test` exercises the consume path: the connection-epoch and
subscription-refcount bookkeeping in `cpp/obs/src/moq-source.cpp` is pinned by
tests, so a change like #2856 cannot blank OBS sources without a red build.

## Plan

`cpp/obs/test/` holds one file, `moq-output-test.cpp`, whose libmoq stub set is
publish-side only (`moq_origin_publish`, `moq_publish_*`, session and client
connect/close). `moq-source.cpp` is compiled by CMake but appears nowhere under
`test/`, and the `just obs test` recipe compiles exactly two translation units
under ThreadSanitizer. PR CI never runs this, so the source path has no
automated gate at all. It bit in #2856: consumption started from the
connected callback resolved against what was announced *now*, the announcement
had not arrived, and nothing retried; caught in review rather than by a test.

- Add `moq-source-test.cpp` with the consume half of the stub: announced
  listing, `moq_origin_request`, catalog and track consume, and their closes,
  matching the names in `rs/libmoq/src/api.rs`.
- Pin the orderings the build cannot check: connected fires and the
  announcement arrives later, and the source still subscribes; a broadcast
  never announced stays pending and `moq_source_disconnect` closes it, firing
  the terminal exactly once; a reconnect (epoch 2) while an epoch-1 delivery is
  in flight drops the stale delivery on the generation check and closes its
  handle; refcounts return to zero on the delivered, errored, and closed paths.
- Wire the new binary into `just obs test` under the same TSan build, and
  also run it without TSan from `just obs ci`, which is what `obs.yml`
  invokes; `just obs test` is manual, so on its own it cannot make the build
  red. Mention both in `doc/bin/obs.md` next to the output test.

## Closes

- [#2860](https://github.com/moq-dev/moq/issues/2860) - close this issue when the quest finishes
