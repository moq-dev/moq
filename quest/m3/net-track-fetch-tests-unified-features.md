# [S] moq-net's fetch tests pass under the workspace's unified features

## Goal

`moq-net`'s `model::track::test::same_tick_fetch_protects` and
`fetch_refresh_survives_eviction` pass under `just test --workspace`, as they
do in `moq-net`'s own feature set, and whichever behavior is wrong (the test
or the code a unified feature turns on) is fixed at its root.

## Plan

At `f24ac5065e` both fail deterministically when moq-net builds with the
workspace's unified features (`just test --workspace -E 'package(moq-net) &
(test(=model::track::test::same_tick_fetch_protects) |
test(=model::track::test::fetch_refresh_survives_eviction))'`), and pass with
`just test -p moq-net`. `fetch_refresh_survives_eviction` panics on an
`unwrap()` of `Err(NotFound)` from a fetch. Several PRs reported the failure
as pre-existing (#9822, #9830, #9832, #9838, #9827). Start by finding which
feature changes the outcome: bisect the features unification adds to
moq-net, such as `async`, which #9830 reports failing alone.
