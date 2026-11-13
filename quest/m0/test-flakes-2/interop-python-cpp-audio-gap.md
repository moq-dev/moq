# [S] Interop's python -> cpp cell holds up under load

## Goal

`just test interop --all` passes the `python -> cpp` media cell on a loaded
host without it ending in `error: audio gap`.

## Plan

Planned 2026-11-13 under the repository's flaky-test rule, while iterating
[moq-dev/moq#8599](https://github.com/moq-dev/moq/pull/8599). One of three
`just test interop --all --publishers go,rust,python --subscribers cpp,js,js-native-node,rust`
runs on a shared, loaded host failed `python -> cpp` about 3 seconds in, the
cpp subscriber reporting `error: audio gap`; the other two runs passed, and
the logs did not show the grant close #8587 fixed. The failing run's logs are
gone.

Reproduce with `just test interop --publishers python --subscribers cpp` in a
loop under load, keeping the logs, and find whether the Python publisher
skips audio timestamps (a stalled encoder or a dropped group) or the cpp
subscriber's gap check misreads a legitimate group boundary; fix whichever
end is wrong, rather than widening the gap tolerance.
