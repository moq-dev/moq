# [M] Encoded consumers read gaps and ends

## Goal

The encoded media consumers in moq-ffi (`MoqMediaTrackConsumer`), moq-c's
`moq_consume_*` reads, and their wrappers surface `Gap` and `End` with the same
tagged-record read shape the decoded consumers got in
[moq-dev/moq#6745](https://github.com/moq-dev/moq/pull/6745), instead of
discarding them.

## Plan

Decided 2026-10-25 (maintainer, on #6745): the decoded consumers keep the
tagged-record shape as landed, and the encoded consumers follow in their own
quest. Mirror #6745 exactly: the same record names, the same end semantics,
and the same wrapper idioms in py, swift, kt, go, dart, cpp, and cpp/obs.
Update `doc/lib/*` and run `just test interop --all`.

Public API: breaking in moq-ffi, moq-c, and every wrapper. Wire: none.
