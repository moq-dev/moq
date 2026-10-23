# [S] One clean close

## Goal

A clean session close has one spelling in moq-net, so a caller tells success
from failure with one match arm. Session failures are recorded inside
moq-net, so `stats::Session::fail` is not public.

## Plan

Decided 2026-10-22 (API audit). A local close now maps to `Session(Cancel)`
(`rs/moq-net/src/session.rs`), contradicting the doc in `error.rs`, and
`rs/moq-relay/src/connection.rs` needs five arms to mean "not a failure".

- Collapse the clean spellings into `Error::Cancel`, fixing the docs.
- Record failures where moq-net observes them and make
  `stats::Session::fail` `pub(crate)`.
- Mirror in `@moq/net` if it has the same split.

Public API: moq-net breaking (error mapping). Wire: none.
