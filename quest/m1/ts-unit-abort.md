# [M] A TS import ends on a unit it cannot decode

## Goal

moq-mux's TS import ends with the cause on any media unit it fails to decode,
instead of publishing around it. `Fault`, its `Damaged { cause, published }`
partial success, and `unit_error`, which decides damage by matching about 15
error variants, are gone. Packet-level continuity handling (duplicate and
missing TS packets, `ts/health.rs`) is not a unit decode error and stays.

## Plan

Decided 2026-11-18 (maintainer, /quest-plan): ✅ abort on any error, as on
[moq-dev/moq#9041](https://github.com/moq-dev/moq/pull/9041) for HLS and
moq-stats (rejected: keeping tolerance with a `Written { units, damage }`
return). "We should not try to support random bit flips that we don't
understand and limp along with broken functionality." A case the import
tolerates must come from a source behavior that is understood and
reproduced, handled where it is detected, never by classifying an error
afterwards.

`Fault` came with [moq-dev/moq#8939](https://github.com/moq-dev/moq/pull/8939)
("TS import reports typed outcomes"), which the PR lists under `## Caused by`;
it lives in
`rs/moq-mux/src/container/ts/import.rs` and `ts/video.rs`. Before deleting a
tolerated case, check whether a test names a real source for it (such as
joining mid-PES at startup); keep such a case as an explicit check where it
happens, and list each kept one in the PR.

Public API: none (the TS import's error surface is unchanged). Behavior: a
TS ingest with a damaged unit ends instead of continuing. Wire: none.

## Related

- [TS timestamp step](/quest/m1/ts-step-damage.md) - investigates whether one stream's timestamp step has a real source
