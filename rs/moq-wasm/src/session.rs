//! Session setup and the publish/subscribe entry points, mirroring `moq_net::Session`.

use js_sys::Uint8Array;
use wasm_bindgen::prelude::*;

use crate::announce::AnnounceConsumer;
use crate::broadcast::{BroadcastConsumer, BroadcastProducer};
use crate::transport;
use crate::util::js_err;

/// A connected MoQ session.
#[wasm_bindgen]
pub struct Session {
	inner: moq_net::Session,
	url: String,

	// Broadcasts the remote announces land here; read via `consume` / `announced`.
	subscribe: moq_net::origin::Consumer,

	// Broadcasts we publish to the remote are created here.
	publish: moq_net::origin::Producer,
}

#[wasm_bindgen]
impl Session {
	/// Connect to a relay over the browser's WebTransport, using the system roots.
	pub async fn connect(url: String) -> Result<Session, JsValue> {
		let parsed = url::Url::parse(&url).map_err(js_err)?;
		let transport = transport::connect(parsed).await.map_err(js_err)?;
		Self::handshake(transport, url).await
	}

	/// Connect trusting only the given sha-256 certificate hashes (serverless dev).
	#[wasm_bindgen(js_name = connectWithHashes)]
	pub async fn connect_with_hashes(url: String, hashes: Vec<Uint8Array>) -> Result<Session, JsValue> {
		let parsed = url::Url::parse(&url).map_err(js_err)?;
		let hashes = hashes.iter().map(|h| h.to_vec()).collect();
		let transport = transport::connect_with_hashes(parsed, hashes).await.map_err(js_err)?;
		Self::handshake(transport, url).await
	}

	async fn handshake(transport: transport::Session, url: String) -> Result<Session, JsValue> {
		// Separate origins for the two directions: a shared one would echo our own
		// published broadcasts back into the tree we subscribe from.
		let subscribe = moq_net::Origin::random().produce();
		let publish = moq_net::Origin::random().produce();

		let client = moq_net::Client::new()
			.with_subscriber(subscribe.clone())
			.with_publisher(&publish);

		let (inner, driver) = client.connect(transport).await.map_err(js_err)?;

		// The session only makes progress while its driver runs. The driver holds no
		// session clone, so dropping this `Session` still closes the transport, which
		// in turn ends the spawned task.
		web_async::spawn(async move {
			let _ = driver.await;
		});

		Ok(Session {
			inner,
			url,
			subscribe: subscribe.consume(),
			publish,
		})
	}

	/// The URL this session connected to.
	#[wasm_bindgen(getter)]
	pub fn url(&self) -> String {
		self.url.clone()
	}

	/// The negotiated protocol version, e.g. "moq-lite-05" or "moq-transport-19".
	#[wasm_bindgen(getter)]
	pub fn version(&self) -> String {
		self.inner.version().to_string()
	}

	/// Consume the broadcast at the given path, resolving once it is routable.
	///
	/// Rejects if the path is neither announced nor served by a dynamic router. Use
	/// `announcedBroadcast` to wait indefinitely for one that may not be online yet.
	pub async fn consume(&self, path: String) -> Result<BroadcastConsumer, JsValue> {
		let consumer = self.subscribe.request_broadcast(path.as_str()).await.map_err(js_err)?;
		Ok(BroadcastConsumer::new(consumer))
	}

	/// Wait until the broadcast at the given path is announced, then consume it.
	///
	/// Resolves `null` if the session closes first.
	#[wasm_bindgen(js_name = announcedBroadcast)]
	pub async fn announced_broadcast(&self, path: String) -> Option<BroadcastConsumer> {
		let broadcast = self.subscribe.announced_broadcast(path.as_str()).await;
		broadcast.map(BroadcastConsumer::new)
	}

	/// Subscribe to announcements under a path prefix.
	///
	/// Yielded paths are relative to the prefix. Pass nothing for every broadcast.
	pub fn announced(&self, prefix: Option<String>) -> Result<AnnounceConsumer, JsValue> {
		let origin = match prefix {
			Some(prefix) => self
				.subscribe
				.with_root(prefix.as_str())
				.ok_or_else(|| js_err("prefix outside the session's allowed paths"))?,
			None => self.subscribe.consume(),
		};
		Ok(AnnounceConsumer::new(origin.announced()))
	}

	/// Publish a broadcast at the given path, announcing it to the remote.
	///
	/// Returns the producer to write tracks into; keep it alive for as long as the
	/// broadcast should stay available.
	pub fn publish(&self, path: String) -> Result<BroadcastProducer, JsValue> {
		let route = moq_net::broadcast::Route::new().with_announce(true);
		let inner = self.publish.create_broadcast(path.as_str(), route).map_err(js_err)?;
		Ok(BroadcastProducer::new(inner))
	}

	/// Close the session with an application close code.
	pub fn close(&self, code: Option<u16>) {
		self.inner.abort(moq_net::Error::App(code.unwrap_or(0)));
	}

	/// Reject when the session closes, with the reason it closed.
	///
	/// Every close carries a reason, including a clean one, so this never resolves.
	pub async fn closed(&self) -> Result<(), JsValue> {
		Err(js_err(self.inner.closed().await))
	}
}
