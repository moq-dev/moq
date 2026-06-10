//! Browser/WASM bindings for `moq-net`, exposed to JavaScript via wasm-bindgen.
//!
//! This is an experiment: rather than reimplementing the moq-lite wire protocol
//! in TypeScript (as `@moq/net` does today), compile the real `moq-net` Rust
//! implementation to WebAssembly and drive the browser's WebTransport from
//! inside it. See `transport.rs` for the WebTransport adapter.
//!
//! Scope: the consume path (connect -> broadcast -> track -> group -> frame),
//! which is the highest-value target (the `@moq/watch` use case). The publish
//! path follows the same shape and is left as the obvious next step.
//!
//! moq-net's timers and `Instant` go through `web_async::time` (tokio on native,
//! wasmtimer on wasm), so the consume path runs in the browser. (`model/time.rs`
//! has an unused wall-clock helper that isn't wasm-portable, but nothing calls
//! it, so it never runs. See README.md.)

// Browser-only crate. Empty on native so `cargo check --workspace` stays green.
#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::Uint8Array;
use wasm_bindgen::prelude::*;

mod transport;

/// Map any displayable error into a JS exception.
fn js_err(e: impl std::fmt::Display) -> JsValue {
	JsError::new(&e.to_string()).into()
}

/// Install panic + tracing hooks for readable errors. Call once after the wasm
/// module's default `init()` loader resolves. (Named `setup` to avoid colliding
/// with wasm-bindgen's default `init` export, which loads the module itself.)
#[wasm_bindgen]
pub fn setup() {
	console_error_panic_hook::set_once();
	let _ = tracing_wasm::try_set_as_global_default();
}

/// A connected MoQ session.
#[wasm_bindgen]
pub struct Session {
	inner: moq_net::Session,
}

#[wasm_bindgen]
impl Session {
	/// Connect to a relay over the browser's WebTransport, using the system roots.
	pub async fn connect(url: String) -> Result<Session, JsValue> {
		let url = url::Url::parse(&url).map_err(js_err)?;
		let transport = transport::connect(url).await.map_err(js_err)?;
		Self::handshake(transport).await
	}

	/// Connect trusting only the given sha-256 certificate hashes (serverless dev).
	#[wasm_bindgen(js_name = connectWithHashes)]
	pub async fn connect_with_hashes(url: String, hashes: Vec<Uint8Array>) -> Result<Session, JsValue> {
		let url = url::Url::parse(&url).map_err(js_err)?;
		let hashes = hashes.iter().map(|h| h.to_vec()).collect();
		let transport = transport::connect_with_hashes(url, hashes).await.map_err(js_err)?;
		Self::handshake(transport).await
	}

	async fn handshake(transport: transport::Session) -> Result<Session, JsValue> {
		let client = moq_net::Client::new();
		let inner = client.connect(transport).await.map_err(js_err)?;
		Ok(Session { inner })
	}

	/// The negotiated protocol version (e.g. "lite-05" or an IETF draft).
	pub fn version(&self) -> String {
		self.inner.version().to_string()
	}

	/// Resolve when the session closes (cleanly or with an error).
	pub async fn closed(&self) -> Result<(), JsValue> {
		self.inner.closed().await.map_err(js_err)
	}

	/// Subscribe to a broadcast by path, waiting until it is announced.
	pub async fn consume(&self, path: String) -> Result<Option<Broadcast>, JsValue> {
		let broadcast = self.inner.consumer().announced_broadcast(path.as_str()).await;
		Ok(broadcast.map(|inner| Broadcast { inner }))
	}
}

/// A consumer handle for a single broadcast.
#[wasm_bindgen]
pub struct Broadcast {
	inner: moq_net::BroadcastConsumer,
}

#[wasm_bindgen]
impl Broadcast {
	/// Subscribe to a track by name, resolving once the publisher accepts.
	pub async fn subscribe(&self, name: String) -> Result<Track, JsValue> {
		let track = self.inner.track(&name).map_err(js_err)?;
		let subscriber = track.subscribe(None).map_err(js_err)?.await.map_err(js_err)?;
		Ok(Track {
			inner: Rc::new(RefCell::new(Some(subscriber))),
		})
	}
}

/// A subscriber to a single track, yielding groups.
#[wasm_bindgen]
pub struct Track {
	// Rc<RefCell<Option<..>>> for interior mutability: wasm-bindgen async methods
	// take `&self` and must produce 'static futures, so we move the value out of
	// the cell for the duration of the await rather than holding a borrow across
	// it (which would make the future self-referential). One in-flight call at a
	// time; a re-entrant call while one is pending errors instead of aliasing.
	inner: Rc<RefCell<Option<moq_net::TrackSubscriber>>>,
}

#[wasm_bindgen]
impl Track {
	/// Receive the next group in arrival order, or `null` when the track ends.
	#[wasm_bindgen(js_name = recvGroup)]
	pub async fn recv_group(&self) -> Result<Option<Group>, JsValue> {
		let cell = self.inner.clone();
		let mut sub = cell
			.borrow_mut()
			.take()
			.ok_or_else(|| js_err("recvGroup already in progress"))?;
		let result = sub.recv_group().await;
		*cell.borrow_mut() = Some(sub);

		let group = result.map_err(js_err)?;
		Ok(group.map(|g| Group {
			sequence: g.sequence,
			inner: Rc::new(RefCell::new(Some(g))),
		}))
	}
}

/// A consumer for a single group, yielding frames.
#[wasm_bindgen]
pub struct Group {
	sequence: u64,
	inner: Rc<RefCell<Option<moq_net::GroupConsumer>>>,
}

#[wasm_bindgen]
impl Group {
	#[wasm_bindgen(getter)]
	pub fn sequence(&self) -> u64 {
		self.sequence
	}

	/// Read the next frame in the group, or `null` at the end of the group.
	#[wasm_bindgen(js_name = readFrame)]
	pub async fn read_frame(&self) -> Result<Option<Uint8Array>, JsValue> {
		let cell = self.inner.clone();
		let mut group = cell
			.borrow_mut()
			.take()
			.ok_or_else(|| js_err("readFrame already in progress"))?;
		let result = group.read_frame().await;
		*cell.borrow_mut() = Some(group);

		let frame = result.map_err(js_err)?;
		Ok(frame.map(|bytes| Uint8Array::from(bytes.as_ref())))
	}
}
