# [S] Validate IETF unidirectional stream types

## Goal

Accept valid padding streams and close the session for genuinely unknown stream types according to the negotiated moq-transport draft.

## Plan

`rs/moq-net/src/ietf/session.rs` routes every non-SETUP uni stream to `run_uni_group`, which rejects padding and unknown types alike while leaving the session alive. The stream-only rejection uses INTERNAL_ERROR because SESSION_CLOSED would falsely claim a session shutdown.

Classify stream types before spawning a group handler. Handle PADDING according to each supported draft, including draining it where required, and propagate genuinely unknown types to the session driver. Keep ordinary group failures scoped to their streams. Add regressions for padding, unknown types causing session shutdown, and group failures preserving the session.

Consult [draft-19 section 3.4 and section 11.5.1](https://www.ietf.org/archive/id/draft-ietf-moq-transport-19.html) and [draft-20 section 11.5.1](https://www.ietf.org/archive/id/draft-ietf-moq-transport-20.html), which explicitly permits cancelling padding streams. Check the earlier supported drafts too.

## Related

- [IETF error codes](/quest/m0/ietf-error-codes.md) - request and session error registry follow-ups
