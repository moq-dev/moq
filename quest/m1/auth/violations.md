# [M] AUTH protocol violations close the session everywhere

## Goal

Every AUTH stream message that the lite or IETF drafts call a
PROTOCOL_VIOLATION closes the session in Rust and `@moq/net` alike, instead
of ending only its token. Examples: an AUTH_ERROR code past `u32`, an
out-of-range IETF AUTH_OK expiry, or an AUTH or AUTH_ERROR that fails to
decode. The lite AUTH_OK pattern and `Expires` decode gap belongs to
[Malformed grant](/quest/m1/auth/malformed-grant.md).

## Plan

AUTH endings (#4550) closes the session only
on an explicit `ProtocolViolation` in Rust lite. These gaps remain:

- JS reports the exact oversized code but doesn't close the session. Let the
  JS connection close on an `AuthSession` protocol violation.
- An out-of-range IETF AUTH_OK expiry still ends only its token.
- An AUTH_ERROR that fails to decode ends only the token, like every other
  lite stream.
- On the acceptor side, a presenter's AUTH that fails to decode only aborts
  its stream (Rust lite `AuthServe`, JS `#runBidis`), so a peer can repeat
  malformed AUTH streams without closing the session.
- Both IETF AUTH acceptors (Rust `Serve::run`, JS `IetfAuthWire.accept`)
  discard the decoded Request ID, so a reused ID is accepted. Validate it like
  every other request, where a duplicate is session-fatal.
- An AUTH reply of an unknown type (neither AUTH_OK nor AUTH_ERROR) fails
  the type dispatch before either decoder runs, and ends only the token.
- A second AUTH on an open IETF AUTH stream is never read: after the first,
  Rust `Serve::run` and JS `AuthSession.serve` wait only for the stream to
  close. Treat it like a repeated PUBLISH_NAMESPACE, a duplicate request.

This list comes from review, not a full audit. Walk every AUTH read path on
both sides, both protocols, and both languages, and fold in anything missing.

Close the session on each, in both languages and on both protocols, with one
regression test per case, and check the lite and IETF draft text agrees. Run
`just test interop --all`.

Public API: none. Wire: none beyond draft wording.
