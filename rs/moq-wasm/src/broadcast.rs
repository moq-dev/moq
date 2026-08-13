//! Broadcast producer and consumer, mirroring `moq_net::broadcast`.

use std::cell::RefCell;

use wasm_bindgen::prelude::*;

use crate::options::{Subscription, TrackInfo};
use crate::track::{TrackConsumer, TrackProducer, TrackRequest, TrackSubscriber};
use crate::util::{Exclusive, js_err};

/// Publishes tracks under a single broadcast.
#[wasm_bindgen]
pub struct BroadcastProducer {
	inner: Exclusive<moq_net::broadcast::Producer>,

	// Created on the first `requestedTrack` call, not up front: minting a `Dynamic`
	// registers an on-demand handler, which is what makes a request for an unknown
	// track queue instead of failing. A caller that never serves on demand should not
	// silently get that behavior.
	dynamic: RefCell<Option<Exclusive<moq_net::broadcast::Dynamic>>>,
}

impl BroadcastProducer {
	pub fn new(inner: moq_net::broadcast::Producer) -> Self {
		Self {
			inner: Exclusive::new(inner),
			dynamic: RefCell::new(None),
		}
	}

	fn dynamic(&self) -> Result<Exclusive<moq_net::broadcast::Dynamic>, JsValue> {
		let mut slot = self.dynamic.borrow_mut();
		if let Some(dynamic) = slot.as_ref() {
			return Ok(dynamic.clone());
		}

		let dynamic = Exclusive::new(self.inner.peek("requestedTrack", |b| b.dynamic())?);
		*slot = Some(dynamic.clone());
		Ok(dynamic)
	}
}

#[wasm_bindgen]
impl BroadcastProducer {
	/// A broadcast nobody is publishing yet, ready to have tracks added.
	#[wasm_bindgen(constructor)]
	pub fn new_js() -> Self {
		Self::new(moq_net::broadcast::Info::new().produce())
	}

	/// Create a track, optionally declaring its properties.
	#[wasm_bindgen(js_name = createTrack)]
	pub fn create_track(&self, name: String, info: Option<TrackInfo>) -> Result<TrackProducer, JsValue> {
		let info = info.map(TryInto::try_into).transpose()?;
		let inner = self
			.inner
			.peek("createTrack", |b| b.create_track(name, info))?
			.map_err(js_err)?;
		Ok(TrackProducer::new(inner))
	}

	/// Reserve a track by name, deciding its properties later.
	///
	/// Consumers can discover the name immediately; call `accept` on the returned
	/// request once the properties are known (e.g. after inspecting the media).
	#[wasm_bindgen(js_name = reserveTrack)]
	pub fn reserve_track(&self, name: String) -> Result<TrackRequest, JsValue> {
		let inner = self
			.inner
			.peek("reserveTrack", |b| b.reserve_track(name))?
			.map_err(js_err)?;
		Ok(TrackRequest::new(inner))
	}

	/// Remove a track by name.
	#[wasm_bindgen(js_name = removeTrack)]
	pub fn remove_track(&self, name: String) -> Result<(), JsValue> {
		self.inner
			.peek("removeTrack", |b| b.remove_track(&name))?
			.map_err(js_err)
	}

	/// Wait for the next track a consumer asked for but nobody serves yet.
	///
	/// Answer it with `accept` or `reject`. Rejects once the broadcast closes.
	#[wasm_bindgen(js_name = requestedTrack)]
	pub async fn requested_track(&self) -> Result<TrackRequest, JsValue> {
		let dynamic = self.dynamic()?;
		let result = dynamic
			.with("requestedTrack", async |mut d| {
				let result = d.requested_track().await;
				(d, result)
			})
			.await?;

		Ok(TrackRequest::new(result.map_err(js_err)?))
	}

	/// Read this broadcast's tracks directly, without going over the wire.
	pub fn consume(&self) -> Result<BroadcastConsumer, JsValue> {
		let inner = self.inner.peek("consume", |b| b.consume())?;
		Ok(BroadcastConsumer::new(inner))
	}

	/// Finish the broadcast cleanly, marking it complete.
	pub fn close(&self) -> Result<(), JsValue> {
		self.inner.peek("close", |b| b.finish())
	}

	/// Abort the broadcast with an application close code.
	pub fn abort(&self, code: u16) -> Result<(), JsValue> {
		let broadcast = self.inner.take("abort")?;
		broadcast.abort(moq_net::Error::App(code)).map_err(js_err)
	}
}

/// A handle to a broadcast, used to reach its tracks.
#[wasm_bindgen]
pub struct BroadcastConsumer {
	inner: moq_net::broadcast::Consumer,
}

impl BroadcastConsumer {
	pub fn new(inner: moq_net::broadcast::Consumer) -> Self {
		Self { inner }
	}
}

#[wasm_bindgen]
impl BroadcastConsumer {
	/// A handle to a track by name, without subscribing yet.
	pub fn track(&self, name: String) -> Result<TrackConsumer, JsValue> {
		let inner = self.inner.track(&name).map_err(js_err)?;
		Ok(TrackConsumer::new(inner))
	}

	/// Subscribe to a track by name, resolving once the publisher accepts.
	pub async fn subscribe(
		&self,
		name: String,
		subscription: Option<Subscription>,
	) -> Result<TrackSubscriber, JsValue> {
		self.track(name)?.subscribe(subscription).await
	}

	/// Another handle to the same broadcast.
	#[wasm_bindgen(js_name = clone)]
	pub fn cloned(&self) -> BroadcastConsumer {
		BroadcastConsumer::new(self.inner.clone())
	}

	/// Reject once the broadcast closes, with the reason it closed.
	///
	/// Every close carries a reason, including a clean one, so this never resolves.
	pub async fn closed(&self) -> Result<(), JsValue> {
		Err(js_err(self.inner.closed().await))
	}
}
