# [S] moq-c closes its small API gaps

## Goal

moq-c reports a malformed epoch the same way everywhere, lets a C consumer see
that audio is disabled as it can for video, and has a test of the hops on a
route received through a relay.

## Plan

Found 2026-10-28 by [moq-dev/moq#7151](https://github.com/moq-dev/moq/pull/7151)
and [moq-dev/moq#7156](https://github.com/moq-dev/moq/pull/7156).

Decided 2026-10-28 (maintainer): one PR for all three. moq-c already breaks
at this cut (0.6 to 0.7), so the error-code change rides that break.
Use `MOQ_ERROR_INVALID_CONFIG` for malformed epochs in both
`moq_broadcast_request_prepare` and `moq_publication_announce`, add the audio
enabled query, and test received relay hops.

Implementation is checkpointed in [moq-dev/moq#7246](https://github.com/moq-dev/moq/pull/7246):
`moq_consume_audio_enabled` mirrors the video query, epoch validation uses the
settled error code, and tests cover snapshot enablement, a consumer opened
from a disabled snapshot resuming, and ordered hops over two C sessions.
The README and C guide are updated; a C audio fixture calls the new query.
The PR Impact records the error-code migration and new query.

Remaining work is validation before marking ready and deleting this quest:
run the moq-c tests, compile and run the C fixtures against the generated
`moq.h` and `libmoq.a`, and pass the local CI gate. Resolve any relevant
failures and review findings.

Reproduced 2026-10-29: a malformed request epoch returned -15 instead of -40,
and compiling the C fixture against the original header failed because the
audio query was undeclared. Formatting, quest validation, and the gate's
other static checks passed. `just ci` failed before Rust validation because
mbx could not launch Cargo (`Argument list too long`); the CI tooling test
failed through the same wrapper. Merely prepending pinned Cargo still hit
mbx because `BASH_ENV` reinjected it in child shells. Omitting that hook
verified the pinned Cargo path, but its validation run remained waiting at
`quest gate` for host capacity and was stopped with exit 143 before tests
started. No successful run of the new tests or C fixture is claimed.

Public API: moq-c gains an audio enabled query and changes one error code.
Wire: none.
