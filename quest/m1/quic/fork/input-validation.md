# [XS] Refuse malformed QUIC control input

## Goal

`moq-quic` closes the connection on two inputs its quinn import accepts:

- RETIRE_CONNECTION_ID for the first sequence never issued
  (`sequence == issued` in `rs/moq-quic/src/connection/cid_state.rs`
  `on_cid_retirement`, since `issued` is a count). RFC 9000 section 19.16
  requires PROTOCOL_VIOLATION.
- A repeated `grease_quic_bit` or `min_ack_delay` transport parameter
  (`rs/moq-quic/src/transport_parameters.rs`), which every other known
  parameter already refuses as malformed. RFC 9000 section 7.4 only says
  SHOULD, but this repo refuses malformed input.

Valid peers see no change.

## Plan

Found by the final-head audit of
[#4879](https://github.com/moq-dev/moq/pull/4879), whose body noted both;
upstream quinn has the same code. Decided 2026-10-07: one quest for both,
under the fork line but not blocking [the switch](/quest/m1/quic/fork/switch.md),
since the fixes are local to the parser and CID state.

Inline regressions: retiring a valid and an already-retired sequence, the
first never-issued sequence before and after issuing new CIDs, and single
versus duplicate parameters. Offering the fix upstream is optional.
