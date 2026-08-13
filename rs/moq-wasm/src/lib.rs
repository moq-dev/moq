//! Browser/WASM bindings for `moq-net`, exposed to JavaScript via wasm-bindgen.
//!
//! Rather than reimplementing the wire protocols in TypeScript (as `@moq/net` does),
//! compile the real `moq-net` implementation to WebAssembly and drive the browser's
//! WebTransport from inside it. See `transport.rs` for the WebTransport adapter.
//!
//! # Layout
//!
//! The modules mirror `moq-net`'s role modules one-for-one (`broadcast`, `track`,
//! `group`, `announce`, `session`), and each type binds the methods of its counterpart.
//! That is deliberate: the binding is the surface most likely to drift, so keeping it
//! shaped like the crate it wraps makes a gap visible when reading the two side by side.
//! When `moq-net` grows a method a browser caller needs, add it to the matching module
//! here rather than starting a new flat entry point.
//!
//! Unlike `moq-net`, the types carry the role as a prefix (`TrackProducer`, not
//! `track::Producer`), against the usual convention of letting the module supply it.
//! wasm-bindgen resolves a type in a signature by its Rust ident alone and ignores both
//! the module path and `js_name`, so two modules each exporting a `Consumer` silently
//! generate typings where one stands in for the other. Unique idents are what keep the
//! generated `.d.ts` honest. The modules stay private and re-export flat, so nothing
//! reads `broadcast::BroadcastProducer`; a TS wrapper is free to re-namespace them back
//! into `Broadcast.Producer`.
//!
//! # Conventions at the boundary
//!
//! - Durations are milliseconds and timestamps are microseconds, matching `@moq/net`.
//! - Sequence numbers stay `u64`, which wasm-bindgen maps to a JS `bigint`.
//! - Closing takes an application close code (`moq_net::Error::App`), since a JS error
//!   has nothing to map onto the wire.
//! - `closed()` rejects rather than resolving: every close carries a reason.
//!
//! # Threading
//!
//! Everything runs on whichever thread instantiated the module, usually the main one. A
//! caller that drains a high-bitrate track should do it in a Worker so decode and render
//! are not sharing that thread.

// Browser-only crate. Empty on native so `cargo check --workspace` stays green.
#![cfg(target_arch = "wasm32")]

use wasm_bindgen::prelude::*;

mod announce;
mod broadcast;
mod group;
mod options;
mod session;
mod track;
mod transport;
mod util;

pub use announce::{Announce, AnnounceConsumer};
pub use broadcast::{BroadcastConsumer, BroadcastProducer};
pub use group::{GroupConsumer, GroupProducer};
pub use options::{Fetch, Frame, Subscription, TrackInfo};
pub use session::Session;
pub use track::{TrackConsumer, TrackProducer, TrackRequest, TrackSubscriber};

/// Install panic + tracing hooks for readable errors. Call once after the wasm
/// module's default `init()` loader resolves. (Named `setup` to avoid colliding
/// with wasm-bindgen's default `init` export, which loads the module itself.)
#[wasm_bindgen]
pub fn setup() {
	console_error_panic_hook::set_once();
	let _ = tracing_wasm::try_set_as_global_default();
}
