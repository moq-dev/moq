# [S] A route too large to re-encode is skipped, not fatal to the stream

## Goal

When a relay cannot re-encode one route for a downstream announce stream
(the downstream's shorter prefix or an extra hop pushes the message past the
encode cap), that downstream simply does not see that route. Its other
routes keep flowing, and asking again does not fail the same way. The same
holds for moq-transport's namespace advertisement.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. Reproduced: announcing
`"x/" + "a"*65530` on the lite test harness ended the downstream announce
task. New on main: [moq-dev/moq#4820](https://github.com/moq-dev/moq/pull/4820)
lowered the message cap from 64 MiB to 64 KiB.

`AnnounceRun::start` (`rs/moq-net/src/lite/publisher.rs:1194`) calls
`stream.writer.buffer(..)?`; `EncodeError::TooLarge`
(`lite/announce.rs:297`) propagates to `AnnounceServe::poll` (about line
906), which aborts the whole stream. Cluster peers and `moq ls` ask for the
root prefix, so one hostile publisher withdraws every route a relay sends
them. On lite-06+ the encoder has already spent an announce id.

- `outgoing()` returns `None` for a route this wire cannot carry, like a
  reflected or overlong hop chain. Encode before assigning the id.
- Never abort an announce stream for one route.
- Check the IETF publisher's namespace encoding against its 4096-byte limit
  (`ietf/namespace.rs:66`) for the same failure, and fix it the same way.
- A test announces a near-cap suffix under a short downstream prefix and
  asserts the sibling routes still arrive.

Public API: none. Wire: none.

## Related

- [A compressed announce path never outgrows its literal form](/quest/m1/cut-audit/announce-path-bytes.md) - the receive-side bound
