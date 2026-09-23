# [S] Validate IETF unidirectional stream types

## Goal

Accept valid padding streams and close the session for genuinely unknown
stream types according to the negotiated moq-transport draft. Behavior only,
so it ships on main.

## Plan

`rs/moq-net/src/ietf/session.rs` routes every non-SETUP uni stream to
`run_uni_group` (`:708`), which rejects padding and unknown types alike while
leaving the session alive. That stream-only rejection reaches the wire as
INTERNAL_ERROR on both branches, because nothing registers a code for it: on
dev the handler maps a session-scoped error to `StreamError::Internal` and
aborts the reader (`:697-704`); on main it is `reader.stop(to_stream_code(&err))`
(`:583` there), which falls through to INTERNAL_ERROR the same way.

draft-21 settles what each stream type means: a stream whose type the
endpoint does not recognize MUST close the session, and a padding stream
(type 0x132B3E28) MUST be discarded, which an endpoint may do by cancelling
it. The tree negotiates up to draft-20 (`rs/moq-net/src/ietf/version.rs:12`),
so apply that split to every supported draft and check the earlier ones for
the padding type value and whether draining is required.

- Classify stream types before spawning a group handler. Handle PADDING per
  draft, draining it where required and otherwise cancelling it with a
  stream-only code, and propagate a genuinely unknown type to the session
  driver as a protocol violation. Keep ordinary group failures scoped to their
  streams.
- The test `unknown_uni_type_does_not_claim_the_session_closed`
  (`session.rs:1267`) asserts the current behavior, an INTERNAL_ERROR stop and
  no session close, and flips: an unknown type now closes the session and
  stops nothing on its own.
- Add a padding test asserting a stream-only cancel with no session close, and
  keep `a_group_for_a_retired_alias_is_stopped_with_cancelled` (`:1255`), which
  pins that a dropped group never closes the session.

Consult [draft-21 section 11.5](https://www.ietf.org/archive/id/draft-ietf-moq-transport-21.html)
for the wording, and [draft-19 section 3.4 and section 11.5.1](https://www.ietf.org/archive/id/draft-ietf-moq-transport-19.html)
plus [draft-20 section 11.5.1](https://www.ietf.org/archive/id/draft-ietf-moq-transport-20.html)
for the drafts the tree negotiates.
