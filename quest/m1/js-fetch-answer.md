# [S] JS lite fetch waits for the publisher's answer

## Goal

`js/net`'s lite `fetchGroup` resolves only once the publisher has answered:
the first response byte, or a FIN for an empty group. A missing group rejects
the fetch itself, and every coalesced caller sees the same rejection, instead
of receiving a group whose first `readFrame()` fails. A JS publisher that
cannot serve a group resets the stream with `NotFound`, not a generic error,
so a Rust or JS subscriber can tell a miss from a failure.

## Plan

- Rust already behaves this way since #4164, which waits in the lite
  subscriber before accepting and rejects on reset. Mirror it: resolve after
  the first response byte or an empty-group FIN, and reject on reset. Today
  the fetch path returns its mirror before the response arrives; the stream
  reader can already block until data or FIN and throw on reset.
- The publisher side throws a plain error for a local miss, which reaches the
  wire as a generic reset code. Give it the `NotFound` code the Rust side uses.
- The IETF JS path refuses `fetchGroup` outright and is out of scope.
- Tests in the lite integration suite: a missing group rejects the fetch, a
  coalesced second caller rejects too, an existing group is unchanged, and a
  JS publisher's miss reaches a subscriber as `NotFound`.

Public API: none; a behavior change in when `fetchGroup` settles. Wire: no
format change; a miss resets with the existing `NotFound` code instead of a
generic one.

## Related

- [#4164](https://github.com/moq-dev/moq/pull/4164) - the same fix in Rust
