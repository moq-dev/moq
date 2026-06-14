//! Browser/WASM bindings for `moq-net`, exposed to JavaScript via wasm-bindgen.
//!
//! This is an experiment: rather than reimplementing the moq-lite wire protocol
//! in TypeScript (as `@moq/net` does today), compile the real `moq-net` Rust
//! implementation to WebAssembly and drive the browser's WebTransport from
//! inside it. See `transport.rs` for the WebTransport adapter.
//!
//! The exported classes are deliberately primitive (frames are `Uint8Array`,
//! options are positional). The hand-written `@moq/wasm` TypeScript shim
//! (`js/wasm/src`) wraps them to present the exact `@moq/net` surface: the
//! string/json/bool conveniences, options-object signatures, the `Connection`
//! / `Path` / `Time` namespaces, and a reactive `state.closed` signal. Keeping
//! those in TS keeps this layer thin and the wasm boundary chatter-free.
//!
//! moq-net's timers and `Instant` go through `web_async::time` (tokio on native,
//! wasmtimer on wasm), so both the consume and publish paths run in the browser.

// Browser-only crate. Empty on native so `cargo check --workspace` stays green.
#![cfg(target_arch = "wasm32")]

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use js_sys::{Object, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;

mod transport;

/// Map any displayable error into a JS exception.
fn js_err(e: impl std::fmt::Display) -> JsValue {
	JsError::new(&e.to_string()).into()
}

/// Read an optional boolean property off a JS object.
fn get_bool(obj: &JsValue, key: &str) -> Option<bool> {
	Reflect::get(obj, &JsValue::from_str(key))
		.ok()
		.and_then(|v| v.as_bool())
}

/// Read an optional number property off a JS object.
fn get_f64(obj: &JsValue, key: &str) -> Option<f64> {
	Reflect::get(obj, &JsValue::from_str(key)).ok().and_then(|v| v.as_f64())
}

/// Build a `moq_net::TrackInfo` from a JS `{ compress?, cache?, priority?, ordered? }`.
/// Unset fields keep their defaults. `cache` is milliseconds, matching `@moq/net`.
fn parse_track_info(value: &JsValue) -> moq_net::TrackInfo {
	let mut info = moq_net::TrackInfo::default();
	if value.is_object() {
		if let Some(c) = get_bool(value, "compress") {
			info.compress = c;
		}
		if let Some(ms) = get_f64(value, "cache") {
			info.cache = Duration::from_millis(ms as u64);
		}
		if let Some(p) = get_f64(value, "priority") {
			info.priority = p as u8;
		}
		if let Some(o) = get_bool(value, "ordered") {
			info.ordered = o;
		}
	}
	info
}

/// Serialize a `moq_net::TrackInfo` to a JS `{ compress, cache, priority, ordered }`.
fn track_info_to_js(info: &moq_net::TrackInfo) -> JsValue {
	let obj = Object::new();
	let _ = Reflect::set(&obj, &"compress".into(), &info.compress.into());
	let _ = Reflect::set(&obj, &"cache".into(), &(info.cache.as_millis() as f64).into());
	let _ = Reflect::set(&obj, &"priority".into(), &(info.priority as f64).into());
	let _ = Reflect::set(&obj, &"ordered".into(), &info.ordered.into());
	obj.into()
}

/// Install panic + tracing hooks for readable errors. Call once after the wasm
/// module's default `init()` loader resolves. (Named `setup` to avoid colliding
/// with wasm-bindgen's default `init` export, which loads the module itself.)
#[wasm_bindgen]
pub fn setup() {
	console_error_panic_hook::set_once();

	// Cap tracing at WARN. The default is TRACE, which logs every wire message;
	// under heavy announce churn that floods the console and can freeze the page.
	let mut config = tracing_wasm::WASMLayerConfigBuilder::new();
	config.set_max_level(tracing::Level::WARN);
	tracing_wasm::set_as_global_default_with_config(config.build());
}

/// A connected MoQ session: the wasm counterpart of `@moq/net`'s `Connection.Established`.
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
		// The default client shares one duplex origin for publish + consume,
		// surfaced after connect as `Session::publisher` / `Session::consumer`.
		let inner = moq_net::Client::new().connect(transport).await.map_err(js_err)?;
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

	/// Close the session.
	pub fn close(&mut self) {
		self.inner.close(moq_net::Error::Cancel);
	}

	/// The read handle over remote broadcasts: announce discovery + consume.
	pub fn consumer(&self) -> OriginConsumer {
		OriginConsumer {
			inner: self.inner.consumer().clone(),
		}
	}

	/// Publish a local broadcast at the given path, announcing it to the relay.
	///
	/// The broadcast must have been created with `new Broadcast()`. The announce
	/// stays live until the broadcast is closed (dropped).
	pub fn publish(&self, path: String, broadcast: &Broadcast) -> Result<(), JsValue> {
		broadcast.publish_to(self.inner.publisher(), &path)
	}
}

/// The read handle over an origin's broadcasts: the wasm counterpart of
/// `moq-net`'s `OriginConsumer`. Carries both announce discovery and consume.
#[wasm_bindgen]
pub struct OriginConsumer {
	inner: moq_net::OriginConsumer,
}

#[wasm_bindgen]
impl OriginConsumer {
	/// Stream announce / unannounce events for the broadcasts under this origin.
	pub fn announced(&self) -> Announced {
		Announced {
			inner: Rc::new(RefCell::new(Some(self.inner.announced()))),
		}
	}

	/// Subscribe to a broadcast by path, waiting until it is announced.
	pub async fn consume(&self, path: String) -> Result<Option<Broadcast>, JsValue> {
		let broadcast = self.inner.announced_broadcast(path.as_str()).await;
		Ok(broadcast.map(Broadcast::from_consumer))
	}
}

/// A stream of announce / unannounce events, yielding `{ path, active }`.
/// Mirrors `moq-net`'s `AnnounceConsumer`.
#[wasm_bindgen]
pub struct Announced {
	// `next` is `&mut self`; held in a cell so the async method can move it out
	// across the await (one in-flight call at a time), like the other consumers.
	inner: Rc<RefCell<Option<moq_net::AnnounceConsumer>>>,
}

#[wasm_bindgen]
impl Announced {
	/// The next announce event as `{ path: string, active: boolean }`, or `null`
	/// once the stream ends. `active` is false only for an unannounce.
	pub async fn next(&self) -> Result<Option<Object>, JsValue> {
		let cell = self.inner.clone();
		let mut consumer = cell
			.borrow_mut()
			.take()
			.ok_or_else(|| js_err("announced.next already in progress"))?;
		let result = consumer.next().await;
		*cell.borrow_mut() = Some(consumer);

		Ok(result.map(|(path, status)| {
			let active = !matches!(status, moq_net::Announced::Ended);
			let obj = Object::new();
			let _ = Reflect::set(&obj, &"path".into(), &JsValue::from_str(path.as_str()));
			let _ = Reflect::set(&obj, &"active".into(), &active.into());
			obj
		}))
	}

	/// Stop receiving announce events.
	pub fn close(&self) {
		self.inner.borrow_mut().take();
	}
}

// Producer-side broadcast state: the broadcast itself, its dynamic-track request
// handler, and the announce guard once published.
struct ProducerInner {
	producer: moq_net::BroadcastProducer,
	// `requested_track` needs `&mut self`; held in a cell so the async `requested`
	// method can move it out across the await (one in-flight call at a time).
	dynamic: Rc<RefCell<Option<moq_net::BroadcastDynamic>>>,
	// Keeps the broadcast announced; dropping it unannounces.
	publish: RefCell<Option<moq_net::OriginPublish>>,
}

enum BroadcastInner {
	Producer(Rc<ProducerInner>),
	Consumer(moq_net::BroadcastConsumer),
}

/// A broadcast: either one you publish (`new Broadcast()`) or one you consume
/// (`session.consume(path)`). Mirrors `@moq/net`'s dual-use `Broadcast`.
#[wasm_bindgen]
pub struct Broadcast {
	inner: BroadcastInner,
}

#[wasm_bindgen]
impl Broadcast {
	/// Create a publishable broadcast. Hand it to `session.publish(path, broadcast)`,
	/// then answer `requested()` to serve tracks.
	#[wasm_bindgen(constructor)]
	pub fn new() -> Broadcast {
		let producer = moq_net::BroadcastInfo::new().produce();
		let dynamic = producer.dynamic();
		Broadcast {
			inner: BroadcastInner::Producer(Rc::new(ProducerInner {
				producer,
				dynamic: Rc::new(RefCell::new(Some(dynamic))),
				publish: RefCell::new(None),
			})),
		}
	}

	fn from_consumer(inner: moq_net::BroadcastConsumer) -> Broadcast {
		Broadcast {
			inner: BroadcastInner::Consumer(inner),
		}
	}

	fn publish_to(&self, origin: &moq_net::OriginProducer, path: &str) -> Result<(), JsValue> {
		match &self.inner {
			BroadcastInner::Producer(p) => {
				let guard = origin.publish_broadcast(path, p.producer.consume()).map_err(js_err)?;
				*p.publish.borrow_mut() = Some(guard);
				Ok(())
			}
			BroadcastInner::Consumer(_) => Err(js_err("cannot publish a consumed broadcast")),
		}
	}

	/// Wait for the next track the peer requests, or `null` once the broadcast ends.
	/// Producer side only.
	pub async fn requested(&self) -> Result<Option<TrackRequest>, JsValue> {
		let cell = match &self.inner {
			BroadcastInner::Producer(p) => p.dynamic.clone(),
			BroadcastInner::Consumer(_) => return Err(js_err("cannot accept requests on a consumed broadcast")),
		};

		let mut dynamic = cell
			.borrow_mut()
			.take()
			.ok_or_else(|| js_err("requested already in progress"))?;
		let result = dynamic.requested_track().await;
		*cell.borrow_mut() = Some(dynamic);

		// A closed/dropped broadcast yields no more requests (JS `undefined`).
		Ok(result.ok().map(|inner| TrackRequest { inner: Some(inner) }))
	}

	/// Get a lazy consumer handle for a track by name. Consumer side only.
	pub fn track(&self, name: String) -> Result<TrackConsumer, JsValue> {
		match &self.inner {
			BroadcastInner::Consumer(c) => {
				let track = c.track(&name).map_err(js_err)?;
				Ok(TrackConsumer { inner: track })
			}
			BroadcastInner::Producer(_) => Err(js_err("cannot consume a track on a published broadcast")),
		}
	}

	/// Close the broadcast. Drops the announce guard (unpublishing) on the
	/// producer side; drops the read handle on the consumer side.
	pub fn close(&self) {
		if let BroadcastInner::Producer(p) = &self.inner {
			p.publish.borrow_mut().take();
		}
	}
}

impl Default for Broadcast {
	fn default() -> Self {
		Self::new()
	}
}

/// A track the peer requested, yielded by `Broadcast.requested`. Answer it with
/// `accept(info)` (returning a producer) or `reject(reason)`.
#[wasm_bindgen]
pub struct TrackRequest {
	// `accept`/`reject` consume the request; `Option` lets the `&mut self` methods
	// take it (wasm-bindgen can't take `self` by value).
	inner: Option<moq_net::TrackRequest>,
}

#[wasm_bindgen]
impl TrackRequest {
	#[wasm_bindgen(getter)]
	pub fn name(&self) -> String {
		self.inner.as_ref().map(|r| r.name().to_string()).unwrap_or_default()
	}

	/// The subscriber's requested priority (0 if none yet).
	#[wasm_bindgen(getter)]
	pub fn priority(&self) -> u8 {
		self.inner
			.as_ref()
			.and_then(|r| r.subscription())
			.map(|s| s.priority)
			.unwrap_or(0)
	}

	/// Accept the request, committing the immutable track properties and
	/// returning a producer to write groups into.
	pub fn accept(&mut self, info: JsValue) -> Result<TrackProducer, JsValue> {
		let req = self.inner.take().ok_or_else(|| js_err("request already answered"))?;
		let producer = req.accept(parse_track_info(&info));
		Ok(TrackProducer::new(producer))
	}

	/// Reject the request, closing the track.
	pub fn reject(&mut self) -> Result<(), JsValue> {
		let req = self.inner.take().ok_or_else(|| js_err("request already answered"))?;
		req.reject(moq_net::Error::Cancel);
		Ok(())
	}
}

/// A lazy consumer handle for a track. Mirrors `@moq/net`'s `TrackConsumer`.
#[wasm_bindgen]
pub struct TrackConsumer {
	inner: moq_net::TrackConsumer,
}

#[wasm_bindgen]
impl TrackConsumer {
	#[wasm_bindgen(getter)]
	pub fn name(&self) -> String {
		self.inner.name().to_string()
	}

	/// Open a live subscription at the given priority (default 0).
	pub async fn subscribe(&self, priority: Option<u8>) -> Result<TrackSubscriber, JsValue> {
		let subscription = moq_net::Subscription::default().with_priority(priority.unwrap_or(0));
		let subscriber = self
			.inner
			.subscribe(subscription)
			.map_err(js_err)?
			.await
			.map_err(js_err)?;
		Ok(TrackSubscriber::new(subscriber))
	}

	/// Fetch the track's immutable publisher properties without subscribing.
	/// Lite-05+ only.
	pub async fn info(&self) -> Result<JsValue, JsValue> {
		let info = self.inner.info().await.map_err(js_err)?;
		Ok(track_info_to_js(&info))
	}
}

/// The write side of a track. Mirrors `@moq/net`'s `TrackProducer`.
#[wasm_bindgen]
pub struct TrackProducer {
	// TrackProducer is cheaply clonable; cloning for the async `closed` waiter
	// avoids holding a RefCell borrow across an await.
	inner: Rc<RefCell<moq_net::TrackProducer>>,
}

#[wasm_bindgen]
impl TrackProducer {
	fn new(inner: moq_net::TrackProducer) -> Self {
		Self {
			inner: Rc::new(RefCell::new(inner)),
		}
	}

	#[wasm_bindgen(getter)]
	pub fn name(&self) -> String {
		self.inner.borrow().name().to_string()
	}

	/// Append a new group with the next sequence number.
	#[wasm_bindgen(js_name = appendGroup)]
	pub fn append_group(&self) -> Result<Group, JsValue> {
		let group = self.inner.borrow_mut().append_group().map_err(js_err)?;
		Ok(Group::from_producer(group))
	}

	/// Append a frame as its own single-frame group.
	#[wasm_bindgen(js_name = writeFrame)]
	pub fn write_frame(&self, frame: Uint8Array) -> Result<(), JsValue> {
		self.inner.borrow_mut().write_frame(frame.to_vec()).map_err(js_err)
	}

	/// Close the track, finishing cleanly (no error) or aborting.
	pub fn close(&self) -> Result<(), JsValue> {
		self.inner.borrow_mut().finish().map_err(js_err)
	}

	/// Abort the track with an error message.
	pub fn abort(&self, reason: String) -> Result<(), JsValue> {
		self.inner
			.borrow_mut()
			.abort(moq_net::Error::Transport(reason))
			.map_err(js_err)
	}

	/// Resolve when the track closes. Resolves to an error string, or `null` on
	/// a clean close.
	pub async fn closed(&self) -> Option<String> {
		let producer = self.inner.borrow().clone();
		let err = producer.closed().await;
		match err {
			moq_net::Error::Cancel | moq_net::Error::Closed => None,
			other => Some(other.to_string()),
		}
	}
}

/// The read side of a live track subscription. Mirrors `@moq/net`'s `TrackSubscriber`.
#[wasm_bindgen]
pub struct TrackSubscriber {
	// `recv_group` is `&mut self` and the future must be `'static`; move the
	// subscriber out of the cell for the await rather than borrowing across it.
	// A re-entrant call while one is pending errors instead of aliasing.
	inner: Rc<RefCell<Option<moq_net::TrackSubscriber>>>,
	info: moq_net::TrackInfo,
}

#[wasm_bindgen]
impl TrackSubscriber {
	fn new(inner: moq_net::TrackSubscriber) -> Self {
		let info = inner.info().clone();
		Self {
			inner: Rc::new(RefCell::new(Some(inner))),
			info,
		}
	}

	#[wasm_bindgen(getter)]
	pub fn name(&self) -> String {
		self.inner
			.borrow()
			.as_ref()
			.map(|s| s.name().to_string())
			.unwrap_or_default()
	}

	/// The track's immutable publisher properties (resolved at subscribe time).
	pub fn info(&self) -> JsValue {
		track_info_to_js(&self.info)
	}

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
		Ok(group.map(Group::from_consumer))
	}

	/// Return the next group with a strictly-greater sequence than the last,
	/// skipping late arrivals. `null` when the track ends.
	#[wasm_bindgen(js_name = nextGroup)]
	pub async fn next_group(&self) -> Result<Option<Group>, JsValue> {
		let cell = self.inner.clone();
		let mut sub = cell
			.borrow_mut()
			.take()
			.ok_or_else(|| js_err("nextGroup already in progress"))?;
		let result = sub.next_group().await;
		*cell.borrow_mut() = Some(sub);

		let group = result.map_err(js_err)?;
		Ok(group.map(Group::from_consumer))
	}

	/// Change this subscription's priority.
	#[wasm_bindgen(js_name = updatePriority)]
	pub fn update_priority(&self, priority: u8) -> Result<(), JsValue> {
		let mut guard = self.inner.borrow_mut();
		let sub = guard.as_mut().ok_or_else(|| js_err("recvGroup in progress"))?;
		let subscription = sub.subscription().with_priority(priority);
		sub.update(subscription);
		Ok(())
	}

	/// Stop the subscription. Dropping the inner subscriber unsubscribes.
	pub fn close(&self) {
		self.inner.borrow_mut().take();
	}
}

enum GroupInner {
	Producer(Rc<RefCell<moq_net::GroupProducer>>),
	// `read_frame` is `&mut self`; take/restore across the await like the subscriber.
	Consumer(Rc<RefCell<Option<moq_net::GroupConsumer>>>),
}

/// A group of frames: writable when produced, readable when consumed. Mirrors
/// `@moq/net`'s dual-use `Group`.
#[wasm_bindgen]
pub struct Group {
	sequence: u64,
	inner: GroupInner,
}

#[wasm_bindgen]
impl Group {
	fn from_producer(inner: moq_net::GroupProducer) -> Group {
		Group {
			sequence: inner.sequence,
			inner: GroupInner::Producer(Rc::new(RefCell::new(inner))),
		}
	}

	fn from_consumer(inner: moq_net::GroupConsumer) -> Group {
		Group {
			sequence: inner.sequence,
			inner: GroupInner::Consumer(Rc::new(RefCell::new(Some(inner)))),
		}
	}

	#[wasm_bindgen(getter)]
	pub fn sequence(&self) -> u64 {
		self.sequence
	}

	/// Write a frame to the group. Producer side only.
	#[wasm_bindgen(js_name = writeFrame)]
	pub fn write_frame(&self, frame: Uint8Array) -> Result<(), JsValue> {
		match &self.inner {
			GroupInner::Producer(p) => p.borrow_mut().write_frame(frame.to_vec()).map_err(js_err),
			GroupInner::Consumer(_) => Err(js_err("cannot write to a consumed group")),
		}
	}

	/// Read the next frame in the group, or `null` at the end. Consumer side only.
	#[wasm_bindgen(js_name = readFrame)]
	pub async fn read_frame(&self) -> Result<Option<Uint8Array>, JsValue> {
		let cell = match &self.inner {
			GroupInner::Consumer(c) => c.clone(),
			GroupInner::Producer(_) => return Err(js_err("cannot read from a produced group")),
		};

		let mut group = cell
			.borrow_mut()
			.take()
			.ok_or_else(|| js_err("readFrame already in progress"))?;
		let result = group.read_frame().await;
		*cell.borrow_mut() = Some(group);

		let frame = result.map_err(js_err)?;
		Ok(frame.map(|bytes| Uint8Array::from(bytes.as_ref())))
	}

	/// Close the group: finish it cleanly on the producer side, drop the read
	/// handle on the consumer side.
	pub fn close(&self) -> Result<(), JsValue> {
		match &self.inner {
			GroupInner::Producer(p) => p.borrow_mut().finish().map_err(js_err),
			GroupInner::Consumer(c) => {
				c.borrow_mut().take();
				Ok(())
			}
		}
	}
}
