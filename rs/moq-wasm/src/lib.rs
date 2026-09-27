//! Browser/WASM bindings for `moq-net`, exposed to JavaScript via wasm-bindgen.
//!
//! This is an experiment: rather than reimplementing the moq-lite wire protocol
//! in TypeScript (as `@moq/net` does today), compile the real `moq-net` Rust
//! implementation to WebAssembly and drive the browser's WebTransport from
//! inside it. See `transport.rs` for the dial.
//!
//! Scope: the consume path (connect -> broadcast -> track -> group -> frame),
//! which is the highest-value target (the `@moq/watch` use case). The publish
//! path follows the same shape and is left as the obvious next step.
//!
//! `moq_net::time::run` drives moq-net with the browser clock and timer; this
//! crate spawns it on the browser's microtask queue.
//!
//! Methods that wait return a `Promise` over cloned state rather than being an
//! `async fn(&self)`: wasm-bindgen keeps `&self` borrowed across such a method's
//! await, and a JS `free()` during it throws from inside Rust, which unwinds past
//! the shadow stack and corrupts the wasm heap. Freeing a handle only drops the
//! JS reference; freeing the `Session` also closes it, rejecting what is pending.

// Browser-only crate. Empty on native so `cargo check --workspace` stays green.
#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::pin::pin;
use std::rc::Rc;

use futures::future::{Either, select};
use js_sys::{Promise, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::future_to_promise;

pub mod transport;

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
	// The origin remote broadcasts are announced into; read via `consume`.
	consumer: moq_net::origin::Consumer,
}

#[wasm_bindgen]
impl Session {
	/// Connect to a relay over the browser's WebTransport, using the system roots.
	pub async fn connect(url: String) -> Result<Session, JsValue> {
		let url = url::Url::parse(&url).map_err(js_err)?;
		let transport = transport::connect(url, Default::default()).await.map_err(js_err)?;
		Self::handshake(transport).await
	}

	/// Connect trusting only the given sha-256 certificate hashes (serverless dev).
	#[wasm_bindgen(js_name = connectWithHashes)]
	pub async fn connect_with_hashes(url: String, hashes: Vec<Uint8Array>) -> Result<Session, JsValue> {
		let url = url::Url::parse(&url).map_err(js_err)?;
		let hashes = hashes.iter().map(|h| h.to_vec()).collect();
		let options = transport::Options {
			server_certificate_hashes: hashes,
			..Default::default()
		};
		let transport = transport::connect(url, options).await.map_err(js_err)?;
		Self::handshake(transport).await
	}

	async fn handshake(transport: transport::Session) -> Result<Session, JsValue> {
		// Wire a subscribe origin so the session has somewhere to insert the
		// broadcasts the remote announces; keep a consumer to read them.
		let (origin, origin_driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
		web_async::spawn(async move {
			moq_net::time::run(origin_driver).await;
		});
		let consumer = origin.consume();
		let client = moq_net::Client::new().with_subscriber(origin);
		// The driver holds no session clone, so dropping this `Session` still
		// closes the transport and ends the spawned task.
		let (inner, driver) = client
			.connect(web_async::time::Instant::now(), transport)
			.await
			.map_err(js_err)?;
		web_async::spawn(async move {
			moq_net::time::run(driver).await;
		});
		Ok(Session { inner, consumer })
	}

	/// The negotiated protocol version (e.g. "lite-05" or an IETF draft).
	pub fn version(&self) -> String {
		self.inner.version().to_string()
	}

	/// Reject when the session closes, with the reason it closed.
	///
	/// Every close carries a reason, including a clean one, so this never resolves.
	#[wasm_bindgen(unchecked_return_type = "Promise<void>")]
	pub fn closed(&self) -> Promise {
		let session = self.inner.clone();
		future_to_promise(async move { Err(js_err(session.closed().await)) })
	}

	/// Subscribe to a broadcast by path, waiting until a route covers it.
	///
	/// Rejects with the close reason if the session closes first.
	#[wasm_bindgen(unchecked_return_type = "Promise<Broadcast | undefined>")]
	pub fn consume(&self, path: String) -> Promise {
		let session = self.inner.clone();
		let consumer = self.consumer.clone();
		future_to_promise(async move {
			let request = pin!(async {
				consumer.routed(path.as_str()).await?;
				consumer.request_broadcast(path.as_str()).await.ok()
			});
			// The origin outlives the session, so its wait alone never ends on a close.
			let closed = pin!(session.closed());
			match select(request, closed).await {
				Either::Left((inner, _)) => Ok(inner.map_or(JsValue::UNDEFINED, |inner| Broadcast { inner }.into())),
				Either::Right((err, _)) => Err(js_err(err)),
			}
		})
	}
}

impl Drop for Session {
	// Pending calls hold their own clones, so the close-on-last-drop would wait for them.
	fn drop(&mut self) {
		self.inner.abort(moq_net::Error::Cancel);
	}
}

/// A consumer handle for a single broadcast.
#[wasm_bindgen]
pub struct Broadcast {
	inner: moq_net::broadcast::Consumer,
}

#[wasm_bindgen]
impl Broadcast {
	/// Subscribe to a track by name, resolving once the publisher accepts.
	#[wasm_bindgen(unchecked_return_type = "Promise<Track>")]
	pub fn subscribe(&self, name: String) -> Promise {
		let broadcast = self.inner.clone();
		future_to_promise(async move {
			let track = broadcast.track(&name).map_err(js_err)?;
			let subscriber = track.subscribe(None).await.map_err(js_err)?;
			Ok(Track {
				inner: Rc::new(RefCell::new(Some(subscriber))),
			}
			.into())
		})
	}
}

/// A subscriber to a single track, yielding groups.
#[wasm_bindgen]
pub struct Track {
	// Shared with the pending read, which moves the subscriber out of the cell for
	// the duration of the await instead of holding a borrow across it. One read in
	// flight at a time; a concurrent call errors instead of aliasing.
	inner: Rc<RefCell<Option<moq_net::track::Subscriber>>>,
}

#[wasm_bindgen]
impl Track {
	/// Receive the next group in arrival order, or `null` when the track ends.
	#[wasm_bindgen(js_name = recvGroup, unchecked_return_type = "Promise<Group | undefined>")]
	pub fn recv_group(&self) -> Promise {
		let cell = self.inner.clone();
		future_to_promise(async move {
			let mut sub = cell
				.borrow_mut()
				.take()
				.ok_or_else(|| js_err("recvGroup already in progress"))?;
			let result = sub.recv_group().await;
			*cell.borrow_mut() = Some(sub);

			let group = result.map_err(js_err)?;
			Ok(group.map_or(JsValue::UNDEFINED, |g| {
				Group {
					sequence: g.sequence,
					inner: Rc::new(RefCell::new(Some(g))),
				}
				.into()
			}))
		})
	}
}

/// A consumer for a single group, yielding frames.
#[wasm_bindgen]
pub struct Group {
	sequence: u64,
	// Shared with the pending read; see `Track`.
	inner: Rc<RefCell<Option<moq_net::group::Consumer>>>,
}

#[wasm_bindgen]
impl Group {
	#[wasm_bindgen(getter)]
	pub fn sequence(&self) -> u64 {
		self.sequence
	}

	/// Read the next frame in the group, or `null` at the end of the group.
	#[wasm_bindgen(js_name = readFrame, unchecked_return_type = "Promise<Uint8Array | undefined>")]
	pub fn read_frame(&self) -> Promise {
		let cell = self.inner.clone();
		future_to_promise(async move {
			let mut group = cell
				.borrow_mut()
				.take()
				.ok_or_else(|| js_err("readFrame already in progress"))?;
			let result = group.read_frame().await;
			*cell.borrow_mut() = Some(group);

			let frame = result.map_err(js_err)?;
			Ok(frame.map_or(JsValue::UNDEFINED, |frame| {
				Uint8Array::from(frame.payload.as_ref()).into()
			}))
		})
	}
}
