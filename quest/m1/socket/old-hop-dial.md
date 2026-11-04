# [S] A socket dial over an older hop fails instead of waiting

## Goal

A socket dial or accept through a hop older than moq-lite-07 fails with
`Unsupported` instead of waiting forever. An endpoint's `.socket/<id>`
broadcast is hidden, and a moq-lite hop before 07 has no hidden opt-in on its
announce requests, so it never carries the broadcast: a dial across one never
finds its peer's endpoint, and an acceptor reached by a dialer behind one waits
for the dialer's endpoint for as long as the dial stays open.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- m0
- ✅ m1, in this epic
- m2

From [moq-dev/moq#7677](https://github.com/moq-dev/moq/pull/7677), which
landed Rust sockets on one hidden endpoint broadcast. Its first plan here, an
answer instance check, belonged to the earlier `.accept/<id>` design, which
used instances; the endpoint design uses none.

Facts from #7677's head, over the shared relay harness
(`rs/moq-net/tests/integration/socket.rs`): a moq-lite-06 client's request for
`room/.socket/bob` fails `Unroutable` where `room/plain/bob` resolves, and a
`tcp` session times out for lite-06 to lite-06, lite-07 to lite-06, and
lite-03 to lite-03. With a lite-06 dialer and a lite-07 acceptor, the acceptor
hears the dial, and its accept then waits until the dialer gives up.

- Find where an endpoint can learn that a hop on the path can't carry hidden
  broadcasts, such as the session's negotiated version, or a relay answering
  a hidden request it can't route with a distinct error, and fail the dial or
  accept `Unsupported` there. Weigh one that sees only the first hop against
  one that covers relay-to-relay hops, and record the choice.
- Python inherits it through moq-ffi. If [JS sockets](/quest/m1/socket/js.md)
  has landed, its dial and accept get the same check.
- Test through a pre-lite-07 hop: the dial and the accept fail promptly with
  `Unsupported`.

Public API: none (an existing error). Wire: none, unless a relay's answer
needs one.
