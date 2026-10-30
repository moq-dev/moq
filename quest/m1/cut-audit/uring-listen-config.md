# [S] Every relay runtime honors or refuses each listen setting

## Goal

A relay listen setting a runtime cannot deliver refuses to start, as
`doc/bin/relay/config.md` promises, on the io_uring workers as on tokio.
Each runtime maps `listen::Config` by exhaustive destructuring, so a new
field fails to compile until every runtime decides what to do with it.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. Source-inferred,
inherited.

The draft [moq-dev/moq#7022](https://github.com/moq-dev/moq/pull/7022)
refuses `preferred_v4` and `preferred_v6` in `Workers::bind` before TLS
loads or sockets bind, alongside the existing `lb.id` refusal. A valid
listener previously bound successfully with a preferred address it never
advertised; the new test reproduces that acceptance before the fix.

The remaining work is the compile-time guard for new listen fields.
`moq_tokio::listen::Config` is `#[non_exhaustive]` and contains private
legacy fields, so `rs/moq-relay/src/uring.rs` cannot destructure it
exhaustively. Removing the attribute alone does not make the private
fields accessible. `rs/moq-tokio/src/server.rs` and
`rs/moq-tokio/src/quic/backend.rs` own the current in-crate mappings;
WebSocket admission takes an already-built `moq_net::Server`, not a
`listen::Config`.

Choose whether to introduce a resolved listen configuration boundary that
each runtime can exhaustively consume, or narrow the compile-time guard
to the crate that owns `listen::Config`. Recommend the resolved boundary
instead of exposing legacy parser fields solely for destructuring. Its
public API shape is a maintainer decision; the draft adds no public API.

Public API: none. Wire: none. Config: an ignored combination now refuses to
start; note it in `doc/bin/relay/`.

## Related

- [One relay serve path](/quest/m2/relay-serve-path.md) - removes the duplicated mapping this quest guards
