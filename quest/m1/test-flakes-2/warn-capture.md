# [XS] Scoped WARN capture

## Goal

moq-net `model::group::test::drop_unfinished_warns` and the `model::track`
test of the same name count only the WARNs their own code emits.

## Plan

Both count WARNs through a global tracing capture, so another test's WARN,
or a missed one, changes the count
([#4104](https://github.com/moq-dev/moq/pull/4104)). Capture per test, with a
scoped subscriber or a filter on the test's own span, instead of a
process-global count.
