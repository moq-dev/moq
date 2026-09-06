# [S] moq-hls reads cache misses against a table the wire stopped using

## Goal

`moq-hls` answers 404 when the decoded error identifies a cache miss, and 500
for genuine failures or an IETF stream reset that cannot distinguish a miss.
Returning 404 over IETF depends on the request-error path preserving that
classification. Decode using the negotiated registry so classification cannot
drift from `moq-net` again.

## Plan

`is_cache_miss` in `rs/moq-hls/src/export/rendition.rs` compares a wire code
against `moq_net::Error::to_code()`:

```rust
let code = err.to_code();
code == moq_net::Error::NotFound.to_code()   // 13
    || code == moq_net::Error::Old.to_code() // 2
    || code == moq_net::Error::Evicted.to_code() // 31
```

That table is the crate's own legacy numbering, and no stream reset has carried
it since #2620 replaced it with the `StreamError` registry. A remote miss now
arrives as `Error::Remote(0x20 | 0x22 | 0x23)` on a moq-lite wire, and as
`Error::Remote(0)` on a moq-transport one, since that registry has no value for
any of the three. None of those match, so every miss that crossed a session,
which in a relay is all of them, is served as a 500 instead of a 404.

Worse, one value collides: `Error::Old.to_code()` is 2, which is DELIVERY_TIMEOUT
on both wires, so a peer's delivery timeout classifies as a cache miss.

The tests do not catch it because they build the remote shape out of the same
stale table (`Error::Remote(local.to_code())`), so they agree with the code
rather than with the wire.

The work:

- Classify on the decoded error, not a hand-compared code. `StreamError` already
  names `NotFound`, `Old`, and `Evicted`, so the fix is for `moq-net` to keep
  them named through `Error` rather than flattening them into `Remote(code)`,
  and for `moq-hls` to match variants.
- Decide what a moq-transport upstream can say at all: that registry has no
  value for a cache miss on a stream reset, so a relay fetching over it cannot
  distinguish one from a failure. Either the miss travels as a request error
  rather than a stream reset, or the 500 is correct there and only the moq-lite
  path is fixable.
- Rewrite the tests to build the remote shape from the wire registry
  (`StreamError::to_code`, `ietf::error::to_stream_code`) so they fail when the
  two drift again.

## Related

- [IETF error codes](/quest/m0/ietf-error-codes.md) - the registry work that this classification has to follow
