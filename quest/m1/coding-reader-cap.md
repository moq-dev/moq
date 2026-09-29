# [XS] Rust decoders cap a bare byte or string read

## Goal

Every Rust decode path that allocates from a peer-supplied length has an
upper bound. #4373 fixed a JS size-cap bypass; the lite and IETF message
decoders in Rust already check every length prefix, but `coding/reader.rs`
has no overall cap for a bare `Vec<u8>` or `String` decode.

## Plan

- Audit `rs/moq-net/src/coding/` for decodes that allocate from a length
  prefix without a bound, and list which call sites reach them from the
  wire.
- Where one is reachable, bound it by the message's size limit and refuse
  anything larger with a decode error. If none is reachable, add the bound
  anyway only if it's one line; otherwise delete this quest with the audit
  in the PR.

Public API: none. Wire: oversized fields are refused, which they already
should be.
