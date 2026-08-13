//! Group producer and consumer, mirroring `moq_net::group`.

use js_sys::Uint8Array;
use wasm_bindgen::prelude::*;

use crate::options::{self, Frame};
use crate::util::{Exclusive, js_err};

/// Writes frames into a single group.
#[wasm_bindgen]
pub struct GroupProducer {
	sequence: u64,
	inner: Exclusive<moq_net::group::Producer>,
}

impl GroupProducer {
	pub fn new(inner: moq_net::group::Producer) -> Self {
		Self {
			sequence: inner.sequence,
			inner: Exclusive::new(inner),
		}
	}
}

#[wasm_bindgen]
impl GroupProducer {
	/// This group's sequence number within its track.
	#[wasm_bindgen(getter)]
	pub fn sequence(&self) -> u64 {
		self.sequence
	}

	/// Append a frame, timestamped in microseconds.
	#[wasm_bindgen(js_name = writeFrame)]
	pub fn write_frame(&self, timestamp: f64, payload: Uint8Array) -> Result<(), JsValue> {
		let timestamp = options::timestamp(timestamp)?;
		let payload = payload.to_vec();
		self.inner
			.peek("writeFrame", |group| group.write_frame(timestamp, payload))?
			.map_err(js_err)
	}

	/// How many frames have been written so far.
	#[wasm_bindgen(js_name = frameCount)]
	pub fn frame_count(&self) -> Result<usize, JsValue> {
		self.inner.peek("frameCount", |group| group.frame_count())
	}

	/// Read the frames written to this group.
	pub fn consume(&self) -> Result<GroupConsumer, JsValue> {
		let inner = self.inner.peek("consume", |group| group.consume())?;
		Ok(GroupConsumer::new(inner))
	}

	/// Finish the group cleanly, marking it complete.
	pub fn close(&self) -> Result<(), JsValue> {
		self.inner.peek("close", |group| group.finish())?.map_err(js_err)
	}

	/// Abort the group with an application close code, discarding it.
	pub fn abort(&self, code: u16) -> Result<(), JsValue> {
		let group = self.inner.take("abort")?;
		group.abort(moq_net::Error::App(code)).map_err(js_err)
	}
}

// No `closed()` on a group producer, unlike `TrackProducer`. Awaiting it would have to
// hold the producer for the wait, and every `writeFrame` in the meantime would fail; the
// two things a caller most wants to do at once are exactly write and watch for a drop.
// `moq_net::group::Producer` is not `Clone`, so there is no second handle to wait on, and
// keeping a `group::Consumer` around instead would leave the group permanently "used" and
// distort the demand signal the track evicts against. Watch `TrackProducer.closed()`
// instead: a group dies with its track, and the caller owns the group's own lifetime.

/// Reads frames from a single group.
#[wasm_bindgen]
pub struct GroupConsumer {
	sequence: u64,
	inner: Exclusive<moq_net::group::Consumer>,
}

impl GroupConsumer {
	pub fn new(inner: moq_net::group::Consumer) -> Self {
		Self {
			sequence: inner.sequence,
			inner: Exclusive::new(inner),
		}
	}
}

#[wasm_bindgen]
impl GroupConsumer {
	/// This group's sequence number within its track.
	#[wasm_bindgen(getter)]
	pub fn sequence(&self) -> u64 {
		self.sequence
	}

	/// Units per second for this group's frame timestamps.
	#[wasm_bindgen(getter)]
	pub fn timescale(&self) -> Result<u64, JsValue> {
		self.inner.peek("timescale", |group| group.timescale().as_u64())
	}

	/// Read the next frame's payload, or `null` at the end of the group.
	///
	/// Use `readFrameTimed` when the presentation timestamp matters.
	#[wasm_bindgen(js_name = readFrame)]
	pub async fn read_frame(&self) -> Result<Option<Uint8Array>, JsValue> {
		let frame = self.read_frame_timed().await?;
		Ok(frame.map(|frame| frame.payload()))
	}

	/// Read the next frame with its timestamp, or `null` at the end of the group.
	#[wasm_bindgen(js_name = readFrameTimed)]
	pub async fn read_frame_timed(&self) -> Result<Option<Frame>, JsValue> {
		let result = self
			.inner
			.with("readFrame", async |mut group| {
				let result = group.read_frame().await;
				(group, result)
			})
			.await?;

		Ok(result.map_err(js_err)?.map(Frame::from))
	}

	/// Resolve with the number of frames in the group once it is complete.
	pub async fn finished(&self) -> Result<u64, JsValue> {
		let result = self
			.inner
			.with("finished", async |mut group| {
				let result = group.finished().await;
				(group, result)
			})
			.await?;

		result.map_err(js_err)
	}

	/// Another reader over the same group, starting from this one's position.
	#[wasm_bindgen(js_name = clone)]
	pub fn cloned(&self) -> Result<GroupConsumer, JsValue> {
		let inner = self.inner.peek("clone", |group| group.clone())?;
		Ok(GroupConsumer::new(inner))
	}
}
