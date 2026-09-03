# [L] Bind the browser through moq-ffi/UniFFI instead of a second hand-written wasm API

## Goal

Implement and verify the behavior tracked in [#2907](https://github.com/moq-dev/moq/issues/2907)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Reach the browser through `moq-ffi` (UniFFI) instead of maintaining a second hand-written binding in `moq-wasm`.

#### Why

`rs/moq-ffi` already exports the model `moq-wasm` keeps trying to grow: `MoqBroadcast`/`MoqTrack`/`MoqGroup` producers and consumers, `MoqAnnounced`, `MoqTrackRequest`, `MoqSubscription`, `fetchGroup`, `requestedTrack`, `used`/`unused`. #2814 was a second hand-written copy of that surface, and it was closed for that reason.

`moq-ffi` also already solved the lifecycle problem that sank #2814. `moq-ffi/src/ffi.rs::Task<T>` is `Arc<Mutex<T>>` plus a `cancel` watch channel: concurrent calls queue instead of erroring, `cancel()` interrupts a pending call, `Drop` cancels, and `SubscriberControl` and `info` live outside the lock so `update()` works while a `recv_group()` is in flight. That design has been through five language bindings.

#### Spike results

Measured, not estimated.

**Baseline**: `cargo check --target wasm32-unknown-unknown -p moq-ffi` dies in `aws-lc-sys` C compilation (via `moq-native` to `rustls`) before type-checking a single line of `moq-ffi`. So the real cost was unknown.

**After gating, both targets compile clean.** Roughly 594 insertions and 308 deletions over 10 files, mostly `#[cfg]`. Native `cargo check -p moq-ffi --all-targets` stays clean. **82 of 160 exported methods survive on wasm32.** What drops: the `video`/`audio`/`server`/`json` modules (43), media and catalog methods in `consumer`/`producer` (23), and the native TLS and bind knobs (12). The entire raw `moq-net` model survives.

**What makes it viable**: uniffi 0.31 ships a `wasm-unstable-single-threaded` feature that drops `Send` from `UniffiCompatibleFuture` and `Send + Sync` from `FfiConverterArc`. Without it, moq-net's `LocalBoxFuture` wasm futures are a hard stop.

**Generated TypeScript** (`uniffi-bindgen-js` 0.2.1, reading metadata straight out of the `.wasm`): 108 KB `moq.ts` across 21 classes, `tsc --strict` clean. It is better than the hand-written surface on the two points that sank #2814:

- `u64` maps to `bigint`, not `number`. #2814's tsify mapping trapped on any value above 2^53.
- Async calls clone the object handle (`cloneObjectHandle`) rather than borrowing the wrapper, so a pending call owns its own `Arc`. `free()` during a pending call is fine. wasm-bindgen's `async fn(&self)` instead takes a `LongRefFromWasmAbi` anchor that pins the whole wrapper, which is why #2814 could not free a handle or end a subscription while a read was pending.

Also: idempotent `free()`, `Symbol.dispose`, a FinalizationRegistry backstop, `MoqError extends Error` with a discriminated tag union, Rust doc comments carried through, and loading by top-level await on `new URL('./moq.wasm', import.meta.url)`.

**Size is not an argument against UniFFI.** Raw unoptimized wasm: `moq-ffi` 3.12 MB (594 KB brotli) carrying 82 exported methods, versus `moq-wasm` on `main` 2.83 MB (549 KB brotli) carrying 8. About 8% more bytes for roughly ten times the surface.

#### Plan

Steps 1 to 3 are worth doing regardless of which JS generator wins, and each is independently landable.

##### 1. Gate `moq-ffi` for wasm32

Move `moq-native`, `moq-video`, `moq-audio`, `pollster` and tokio's runtime features to `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`; add `getrandom`'s `wasm_js` backend and uniffi's `wasm-unstable-single-threaded`. Gate the `video`, `audio`, `server`, `json` and `log` modules. Give `Task<T>` a wasm variant and `session.rs` a browser `Client` backed by `web-transport-wasm`.

Two things to decide rather than let happen:

- **Task semantics differ per target.** Native `Task::run` spawns, so the work survives the caller dropping the promise. The wasm variant awaits in place, so a drop cancels mid-operation.
- **The WebTransport adapter would be duplicated.** `moq-wasm/src/transport.rs` adapts `web-transport-wasm` to `web-transport-trait`. It should live in one crate rather than being copied a second time.

One gotcha worth writing down, since it rhymes with the wasm-bindgen ident trap #2814 documented: **`#[cfg]` on a method inside a `#[uniffi::export] impl` block is ignored by the proc macro.** It still emits scaffolding, producing duplicate symbols and calls to methods that no longer exist. Use separate `#[cfg]`'d `#[uniffi::export] impl` blocks.

##### 2. Fix the two `moq-mux` wasm blockers

Both are latent bugs today, independent of any browser work.

- `tokio::time::Instant` in `moq-mux/src/codec/{av1,h264,h265}/split.rs`. `moq-mux` declares tokio with only the `macros` feature, so this compiles natively purely through workspace feature unification. It should be `web_async::time`, the migration `moq-net` already did.
- `pub trait Stream: Send + 'static` in `moq-mux/src/catalog/stream.rs`. moq-net's wasm stats types are `Rc<RefCell<..>>`, so the supertrait cannot be satisfied. Needs the `MaybeSend` treatment `moq-net` already has in `src/util.rs`.

##### 3. Decouple `MoqBroadcastProducer` from the catalog

`MoqBroadcastProducer::from_inner` always constructs a hang catalog track, so the publish path is structurally coupled to `moq-mux`. There is no raw-model publish without changing that. This is the deepest item and the only one that is not a `#[cfg]`.

##### 4. Then pick a generator, with a browser harness in place

The remaining unknown is generator maturity, not design. `uniffi-bindgen-js` 0.2.1 was first published 2026-03-03 and has 323 total downloads. `uniffi-bindgen-react-native` (npm only) still describes itself as early development and not for production. `uniffi_bindgen` 0.31 itself ships only the kotlin, python, ruby and swift backends.

Nothing in this spike ran in a browser. Generated and type-checking is not working, which is why #2816 is the prerequisite for this step rather than a follow-up to it.

#### Related

- \#2814, closed in favor of this.
- \#2816, no browser test harness. Blocks step 4 and is the root cause of #2814 needing four hand-verified review rounds.
- \#2822 and #2835 are `moq-wasm` gaps that this approach would close by construction, since `moq-ffi` already binds datagrams and `track::Dynamic`.

## Closes

- [#2907](https://github.com/moq-dev/moq/issues/2907) - close this issue when the quest finishes
