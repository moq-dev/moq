# [M] moq-net owns its transport seam

## Goal

moq-net's public API names only its own transport traits, and it has no
`web-transport-trait` dependency. A `web-transport-trait` or qmux major bump is
then a patch for moq-net: only the adapters in moq-tokio and moq-wasm change.

## Plan

Today `rs/moq-net/src/lib.rs` re-exports `web_transport_trait`, and
`Client::connect` / `Server::accept` are bounded by `transport::poll::Session`,
which on `dev` is a supertrait of `web_transport_trait::poll::Session` with a
blanket impl. So the upstream trait is public either way, and moving `main` from
qmux 0.5 to 0.6 (which needs `web-transport-trait` 0.5) would break moq-net.
That kept qmux 0.5.2 a hand-published backport on 2026-10-01.

Decisions (2026-10-01):

- ✅ Goal: trait bumps stop breaking moq-net. Rejected: only dropping the
  `pub use`, since the trait still bounds `connect`/`accept`.
- ✅ The seam is the poll traits moq-net already has: `transport::poll::{Session,
  SendStream, RecvStream}` become standalone traits with their own `poll_*`
  methods and error type, not supertraits of `web_transport_trait::poll`.
  Rejected: a new async trait.
- ✅ Adapters live outside moq-net: moq-tokio (native) and moq-wasm each wrap
  web-transport sessions in a newtype implementing moq-net's traits. Callers of
  moq-tokio's `Client`/`Server` see no change; direct moq-net users wrap
  explicitly. Rejected: a blanket impl behind a feature in moq-net (versioned or
  not) and a new adapter crate.
- ✅ Standalone m1 quest on `dev`, related to sans-io rather than a child of it.

Work:

- Remove `pub use web_transport_trait` and the dependency from moq-net;
  `Error::from_transport` takes moq-net's own transport error.
- Port moq-net's tests and test transports to the owned traits.
- Add the adapters in moq-tokio and moq-wasm (and wherever else a crate hands a
  web-transport session to moq-net, such as moq-ffi), with an adapter test each.
- Update `doc/` and the rustdoc on `transport`.

Public API: breaking (moq-net's transport bounds and re-export), so `dev`.
Wire: none.

## Related

- [Sans-IO moq-net](/quest/m1/rs2ts/sans-io/README.md) - builds on the same seam
- [Raw stream codes](/quest/m1/raw-stream-codes.md) - the trait bump that exposed this
