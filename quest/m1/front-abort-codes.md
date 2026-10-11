# [S] Fronts abort instead of dropping

## Goal

A subscription whose broadcast's source closes, whose route is withdrawn,
or whose origin shuts down ends with a deliberate stream code, never
`internal error`. The local-to-wire error conversions in
`rs/moq-net/src/error.rs` match every `Error` variant explicitly, so a new
variant cannot hide behind a catch-all.

From [#4349](https://github.com/moq-dev/moq/issues/4349): a clean publisher
close ended the subscriber's in-flight subscription with `internal error`,
and subscribes during cross-node churn answered the same.

## Plan

Decided 2026-10-11:

- `Error::Dropped` means a producer went away without aborting, which is a
  bug, not a routine end. Fronts end with it today
  (`rs/moq-net/src/model/front.rs`: `Event::Retired`, `Event::Closed`, a
  resolved front whose route leaves the table, `source_closed`), and
  `Action::End` in `origin.rs` passes it to every unfinished track; it has
  no arm in `From<&Error> for StreamError`, so it reaches the wire as
  `internal error` (0x0).
- Codes: a closed source or withdrawn route ends with `Unroutable`, the same
  answer a fresh subscribe already gets once the broadcast is gone. A local
  origin `Retired` or `Closed` ends with `Cancel`. A track the publisher
  finished still ends with FIN.
- Remove the `_ => Internal` catch-alls in both `From<&Error>` conversions
  (`StreamError` and `SessionError`). `Dropped` stays `Internal`
  deliberately, since it now marks a real bug.
- No wire change: every code already exists. Check `js/net` for the same
  catch-all only if it has an equivalent conversion.

Tests: front unit tests assert the new codes in place of
`Action::End { err: Error::Dropped }`, and a relay test closes a broadcast
under an active subscription and expects `Unroutable`, not `internal error`.

## Related

- [Reproduce cross-node announce flapping](/quest/m1/cross-node-flapping.md) - the churn where subscribes answered `internal error`
