# [M] js/net: a locally raised group error reaches the wire as INTERNAL_ERROR

## Goal

A JS publisher that resets a stream because of a group error sends the code for
that error, the same as the Rust publisher does. Today every locally minted
error arrives at the peer as `0`, which the moq-lite space reads as an internal
failure, so a routine event looks like a crash on our side.

## Plan

Dev only, which is why this sits in m1 rather than m0. `main` has no
`StreamCode`, no `StreamError`, and no `toTransport`: its `reset(reason)` just
calls `#writer.abort(reason)`, so there is no code-preserving path to fix
there. The structured code machinery this quest builds on exists only on dev,
so branch from dev and reconcile against that tree.

`Writer.reset` calls `withCode(reason)` in `js/net/src/stream.ts`, which
preserves a code only when `reason instanceof StreamError`. A `Lagged` raised
locally is a plain `Error`, so `js/net/src/ietf/publisher.ts` `stream.reset(error(err))`
sends `StreamCode.Internal`. Rust does the opposite and deliberately so:
`lite/publisher.rs` aborts the stream with the real reason precisely to stop
`Writer`'s `Drop` fallback reporting every failure as `Cancel`, and maps
`Error::Lagged` to `StreamError::TooFarBehind` (`0x5`).

The asymmetry is already load-bearing downstream. `js/json/src/window/consumer.ts`
accepts *both* the local class and the remote codes to work around it:

```ts
const lagged =
    err instanceof Moq.Group.Lagged ||
    (err instanceof Moq.StreamError &&
        (err.code === Moq.StreamCode.TooFarBehind || ...));
```

So the fix is a mapping from js/net's local error classes to stream codes,
mirroring Rust's `Error::to_code` / `Error::from_transport` pair, applied where
a stream is reset rather than at each call site. It is not a matter of passing
a literal at the one `ietf/publisher.ts` site: `Lagged` is not the only local
class that reaches `reset`, and the reverse direction has to agree or two peers
disagree about what a code means.

Scope is the moq-lite code space, matching what Rust sends today. The
moq-transport registry is a separate confusion tracked by
[IETF error codes](/quest/m0/ietf-error-codes.md),
which is fixing both directions there; do not fold the two together.

Land it with a test that a JS publisher's reset arrives at a peer as the right
code, and drop the `js/json` workaround once the class check is redundant.

## Closes

- [#2999](https://github.com/moq-dev/moq/issues/2999) - close this issue when the quest finishes

## Related

- [Group overflow](/quest/m1/group-overflow-abort.md) - adds a new stream code that a JS publisher cannot currently send
- [IETF error codes](/quest/m0/ietf-error-codes.md) - the moq-transport half
- [#3187](/quest/m1/3187-preserve-structured-protocol-error-codes-across-ffi-and-c.md) - the same structured-code loss across FFI and C
