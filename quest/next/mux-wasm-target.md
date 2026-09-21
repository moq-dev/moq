# [S] moq-mux compiles for wasm32

## Goal

`cargo check -p moq-mux --target wasm32-unknown-unknown` passes. Two things
in the crate compile natively only through workspace feature unification and
break on the wasm target, which is what stops anything above `moq-net` from
reaching the browser through Rust.

## Plan

Found by the #2907 spike; both are latent bugs independent of any browser
work.

- `tokio::time::Instant` in `rs/moq-mux/src/codec/{av1,h264,h265}/split.rs`:
  `moq-mux` declares tokio with only the `macros` feature, so this compiles
  natively only because another crate turns the runtime on. Use
  `web_async::time`, the migration `moq-net` already made.
- `pub trait Stream: Send + 'static` in `rs/moq-mux/src/catalog/stream.rs`:
  moq-net's wasm stats types are `Rc<RefCell<..>>`, so the supertrait cannot
  be satisfied. Apply the `MaybeSend` treatment `moq-net` has in
  `src/util.rs`.
- Add the crate to the wasm clippy lane in `rs/justfile` beside `moq-wasm`
  and `moq-net`, so it stays green.

Public API: none. Wire: none.

## Related

- [Browser through moq-ffi](/quest/future/2907-bind-the-browser-through-moq-ffi-uniffi-instead-of-a.md) - the study these blockers were found by
