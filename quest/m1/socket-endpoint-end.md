# [S] A socket endpoint's every ending runs its cleanup

## Goal

`moq_net::socket::Endpoint` models its lifecycle as one state whose ended
variant carries its error, and every ending, including the route broadcast
closing, goes through `close()`, so track streams already handed out end with
it as they do in JS. `is_endpoint` reaches moq-ffi, every binding, and the
socket guide.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
In `rs/moq-net/src/model/socket.rs`, `Handler::None` means both "not made
yet" and "ended", with `end: Option<Error>` stored beside it, and `poll_end`
returns `Closed` when a route broadcast closes without recording it or running
`close()`: when the origin shuts down, streams already handed out stay open
until the application drops the `Endpoint`. Shape it as REVIEW.md's State
section asks (`Open { .. } | Closed(Error)`).
[moq-dev/moq#8468](https://github.com/moq-dev/moq/pull/8468)'s
`socket::is_endpoint` exists only in Rust and JS.

Public API: moq-ffi and every binding gain `is_endpoint`. Wire: none.
