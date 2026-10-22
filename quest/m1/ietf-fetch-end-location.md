# [S] FETCH_OK End Object 0 is the whole group

## Goal

On moq-transport drafts 14 to 19, moq-net reads and writes a FETCH_OK End
Location `{g, 0}` as all of group g, the same way it already writes the FETCH
request. A peer answering our whole-group FETCH with `{g, 0}` is served, not
refused, and a peer reading our answer no longer concludes that group g+1 does
not exist. A FETCH ending at `{2^64-1, 0}` gets the NOT_SUPPORTED that other
multi-group ranges get, on draft 17 and later, the drafts whose encoding can
carry it (14 to 16 cap a varint at `2^62-1`).

## Plan

Decided 2026-10-21 (maintainer): fix all three sides in one PR, on behavior
rather than on draft text. The request side already spells the whole group
`{g, 0}`, while FETCH_OK is written and read as exclusive `{g+1, 0}`, so one
message has two readings depending on direction. IETF pairings between our own
released and current builds are out of scope, so no staged rollout.

- Subscriber (`rs/moq-net/src/ietf/subscriber.rs`, FETCH_OK handling): before
  draft 20, End Object 0 means the whole End Group, so `{g, 0}` covers g and
  End Of Track sets the final sequence to g+1. Keep accepting `{g+1, 0}`,
  which moxygen's relay cache and moqtest send, as covering g.
- Publisher (`rs/moq-net/src/ietf/publisher.rs`, standalone FETCH range):
  keep the request's wire form instead of widening `{g, 0}` to `{g+1, 0}`.
  That removes the `checked_add(1)` overflow (which also reaches draft 20+
  through the Absolute filter rewrite) and the `end.group - 1` step.
- Publisher FETCH_OK: echo `{g, 0}` for a covered whole group.

Tests: replace the `{g+1, 0}` pins (`a_standalone_fetch_serves_one_whole_group`,
the `end_location()` helper, and the subscriber's group-fetch and abandon
fixtures) with the whole-group reading, and add a subscriber case for a
`{g+1, 0}` answer and a publisher case for `{u64::MAX, 0}`. Run
`just test interop --all`; moxygen must still interoperate.

Document the pre-draft-20 FETCH_OK meaning next to the draft-20 line in
`doc/concept/standard.md`.

The candidate implements all three sides and reproduces all three failures
before the fix. Whole-group and legacy FETCH_OK answers are tested with and
without End Of Track on drafts 14-19; drafts 20-22 retain inclusive answers.
The numeric-limit range test covers both a missing single group and an
unsupported multi-group request on every draft. JavaScript still refuses
FETCH, with decoder coverage for the maximum whole-group range. The migration
note, standard documentation and IETF extension draft are synchronized.

Compatibility still needs confirmation. The versioned plan above excludes
released/current moq-net IETF pairings, while the repository asks for
published-wire compatibility. Also, `just test interop --all` runs the
in-tree language matrix, not moxygen. At upstream moxygen commit
`fcc97fc53d561f6ff4c654026824e2c47d55733e`,
[`MoQTestClient::validateFetchOk`](https://github.com/facebookexperimental/moxygen/blob/fcc97fc53d561f6ff4c654026824e2c47d55733e/moxygen/moqtest/MoQTestClient.cpp#L1201)
strictly compares the answer to
[`fetchEndLocation`](https://github.com/facebookexperimental/moxygen/blob/fcc97fc53d561f6ff4c654026824e2c47d55733e/moxygen/moqtest/Utils.cpp#L401),
which still widens whole-group Object 0 to the next group. For a draft-18
track with groups 0-4 and objects 0-1, a request ending `{2,0}` sends bytes
`02 00`; the candidate replies `02 00`. Moxygen's parser and fetch handle
keep `{2,0}`, while its separate expectation helper produces `{3,0}`, so
moqtest marks a semantic failure. This is a validator conflict, not evidence
of a live FETCH delivery failure. Confirm the all-at-once compatibility exception for this
candidate, or choose a staged publisher rollout; do not claim the in-tree
matrix proves moxygen FETCH compatibility. The PR stays draft for that choice.

Public API: none. Wire: drafts 14 to 19 FETCH_OK End Location changes from
`{g+1, 0}` to `{g, 0}` for a whole group.

## Closes

- [#6199](https://github.com/moq-dev/moq/issues/6199) - FETCH_OK End Location Object 0 means the whole group
