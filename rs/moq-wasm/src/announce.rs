//! Broadcast announcements, mirroring `moq_net::announce`.

use wasm_bindgen::prelude::*;

use crate::broadcast::BroadcastConsumer;
use crate::util::Exclusive;

/// A path, and the broadcast now available there.
#[wasm_bindgen]
pub struct Announce {
	path: String,
	broadcast: Option<BroadcastConsumer>,
}

#[wasm_bindgen]
impl Announce {
	/// The broadcast's path, relative to the prefix that was announced.
	#[wasm_bindgen(getter)]
	pub fn path(&self) -> String {
		self.path.clone()
	}

	/// The broadcast now available at that path, or `null` if it went away.
	///
	/// A replacement arrives as a `null` followed by a new handle, never as a swap in
	/// place, so a consumer can tear down the old one before wiring up the new.
	#[wasm_bindgen(getter)]
	pub fn broadcast(&self) -> Option<BroadcastConsumer> {
		self.broadcast.as_ref().map(BroadcastConsumer::cloned)
	}
}

impl From<moq_net::announce::Update> for Announce {
	fn from(value: moq_net::announce::Update) -> Self {
		Self {
			path: value.path.to_string(),
			broadcast: value.broadcast.map(BroadcastConsumer::new),
		}
	}
}

/// A stream of broadcast announcements under a path prefix.
#[wasm_bindgen]
pub struct AnnounceConsumer {
	inner: Exclusive<moq_net::announce::Consumer>,
}

impl AnnounceConsumer {
	pub fn new(inner: moq_net::announce::Consumer) -> Self {
		Self {
			inner: Exclusive::new(inner),
		}
	}
}

#[wasm_bindgen]
impl AnnounceConsumer {
	/// Wait for the next announcement, or `null` once the stream ends.
	pub async fn next(&self) -> Result<Option<Announce>, JsValue> {
		let result = self
			.inner
			.with("next", async |mut announced| {
				let result = announced.next().await;
				(announced, result)
			})
			.await?;

		Ok(result.map(Announce::from))
	}
}
