# [XS] An AUTH_OK that cannot be sent is refused, not half-written

## Goal

When the Rust IETF acceptor's grant cannot be encoded as one AUTH_OK for any
reason, it answers `AUTH_ERROR { NOT_SUPPORTED }` and nothing of the AUTH_OK
reaches the wire, the same as JS. Today the preflight in
`rs/moq-net/src/ietf/auth.rs` (`serve_issue`) catches only
`EncodeError::Unsupported`, so a prefix grant too large for the `u16` message
size falls through to `encode_message`, which fails mid-write and ends the
stream with a transport error instead of a refusal the presenter can read.
Found in review of [#4124](https://github.com/moq-dev/moq/pull/4124).

## Plan

- Refuse on any sizing error, not only `Unsupported`, including the message
  size ceiling the writer enforces. Never trim or widen the grant to make it
  fit: withholding it is the only safe answer.
- Check whether the lite AUTH_OK path has the same gap and fix both if so.
- Regression test mirroring JS's "a grant too large for one message is
  refused before anything is written": the presenter sees `Unsupported` and
  no AUTH_OK bytes were written.

Public API: none. Wire: none.
