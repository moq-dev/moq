# [S] C consumers on the generated API

## Goal

The C interop client (`test/interop/clients/c`) and the `doc/lib/c` samples use
the generated `moq-c` API and pass `just test interop --all` and the doc-sample
build. Nothing in the repository calls the hand-written ABI any more except
the smoke client (`test/smoke/clients/c`), which
[Published generated C](/quest/m2/release/c-publish.md) moves.

## Plan

- Port the interop client first; it is the smallest real consumer and proves
  the callback and dispatcher shape end to end. It skips a disabled audio
  rendition through the generated record's `enabled`, as js-native does; the
  hand-written ABI has only `moq_consume_video_enabled` (decided 2026-10-23).
  It stays subscribe-only (decided 2026-10-31): the C++ interop client already
  covers moq-ffi's publish path, so a C publish lane would grow the matrix for
  little new coverage.
- Rewrite `doc/lib/c` around the generated API, including a sample that runs
  callbacks on the host's own loop through `moq_set_dispatcher`.

Waits for the cut and the generator switch (found when first started,
2026-10-28; decided 2026-10-31). Until the cut, [moq-c parity](/quest/m1/c/README.md) ships the
hand-written 0.7 and proves it with this interop client and the current
`doc/lib/c`, and `doc/` describes only what ships, so either half landing first
drops the shipping crate's proof and documents an unpublished API. The switch
re-cases every generated C type (`moq_session` becomes `MoqSession`), so a port
before it is written twice; `c/moq/test/probe.c` takes that rename with the
switch.

The generated API covers everything the client does, publishing included: the
C++ interop client already drives the same moq-ffi surface both ways.

`test/smoke/clients/c` checks the published archive with the hand-written ABI,
so it can move only once the generated archive publishes, after this quest;
[Published generated C](/quest/m2/release/c-publish.md) owns that move
(decided 2026-10-31).

## Required

- [Breaking release cut](/quest/m2/release/cut.md) - the hand-written 0.7 ships with this client and these docs as its proof
- [Switch to rust2's UniFFI](/quest/m2/uniffi-fork.md) - the generated C type names settle before consumers are written against them
