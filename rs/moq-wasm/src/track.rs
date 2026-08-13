//! Track producer, consumer, subscriber, and request, mirroring `moq_net::track`.

use js_sys::Uint8Array;
use wasm_bindgen::prelude::*;

use crate::group::{GroupConsumer, GroupProducer};
use crate::options::{self, Fetch, Frame, Subscription, TrackInfo};
use crate::util::{Exclusive, js_err};

/// Publishes groups and frames for a single track.
#[wasm_bindgen]
pub struct TrackProducer {
	name: String,

	// The observers (`used`, `unused`, `closed`) run off this rather than `inner`. They
	// take `&self` and are meant to be awaited *while* publishing, so routing them through
	// `inner` would check the producer out for the whole wait and reject every write until
	// they resolved.
	//
	// It has to be `Demand` and not a `Producer` clone: `Demand` is a weak handle, so it
	// neither keeps the track alive nor pins its cached groups. A strong clone held across
	// a pending `closed()` would defeat close-on-last-drop, and the promise would keep the
	// very track it is waiting on alive.
	demand: moq_net::track::Demand,

	inner: Exclusive<moq_net::track::Producer>,
}

impl TrackProducer {
	pub fn new(inner: moq_net::track::Producer) -> Self {
		Self {
			name: inner.name().to_string(),
			demand: inner.demand(),
			inner: Exclusive::new(inner),
		}
	}
}

#[wasm_bindgen]
impl TrackProducer {
	/// This track's name within its broadcast.
	#[wasm_bindgen(getter)]
	pub fn name(&self) -> String {
		self.name.clone()
	}

	/// Start the next group, one sequence past the latest.
	#[wasm_bindgen(js_name = appendGroup)]
	pub fn append_group(&self) -> Result<GroupProducer, JsValue> {
		let inner = self
			.inner
			.peek("appendGroup", |track| track.append_group())?
			.map_err(js_err)?;
		Ok(GroupProducer::new(inner))
	}

	/// Append a single-frame group, timestamped in microseconds.
	#[wasm_bindgen(js_name = writeFrame)]
	pub fn write_frame(&self, timestamp: f64, payload: Uint8Array) -> Result<(), JsValue> {
		let timestamp = options::timestamp(timestamp)?;
		let payload = payload.to_vec();
		self.inner
			.peek("writeFrame", |track| track.write_frame(timestamp, payload))?
			.map_err(js_err)
	}

	/// The latest group sequence written, or `null` if nothing has been written.
	#[wasm_bindgen(getter)]
	pub fn latest(&self) -> Result<Option<u64>, JsValue> {
		self.inner.peek("latest", |track| track.latest())
	}

	/// Read this track's groups directly, without going over the wire.
	pub fn consume(&self) -> Result<TrackConsumer, JsValue> {
		let inner = self.inner.peek("consume", |track| track.consume())?;
		Ok(TrackConsumer::new(inner))
	}

	/// Subscribe to this track directly, without going over the wire.
	pub fn subscribe(&self, subscription: Option<Subscription>) -> Result<TrackSubscriber, JsValue> {
		let subscription = subscription.map(Into::into);
		let inner = self.inner.peek("subscribe", |track| track.subscribe(subscription))?;
		Ok(TrackSubscriber::new(inner))
	}

	/// Finish the track cleanly, marking it complete.
	pub fn close(&self) -> Result<(), JsValue> {
		self.inner.peek("close", |track| track.finish())?.map_err(js_err)
	}

	/// Finish the track once the given group sequence is reached (exclusive).
	#[wasm_bindgen(js_name = closeAt)]
	pub fn close_at(&self, sequence: u64) -> Result<(), JsValue> {
		self.inner
			.peek("closeAt", |track| track.finish_at(sequence))?
			.map_err(js_err)
	}

	/// Abort the track with an application close code.
	pub fn abort(&self, code: u16) -> Result<(), JsValue> {
		let track = self.inner.take("abort")?;
		track.abort(moq_net::Error::App(code)).map_err(js_err)
	}

	/// Reject once the track closes, with the reason it closed.
	///
	/// Every close carries a reason, including a clean one, so this never resolves.
	/// Safe to await while publishing.
	pub async fn closed(&self) -> Result<(), JsValue> {
		let demand = self.demand.clone();
		Err(js_err(demand.closed().await))
	}

	/// Resolve once somebody is subscribed, so the producer can start working.
	///
	/// The counterpart to `unused`, and what a publisher waits on before opening a camera
	/// or spinning up an encoder for a track nobody is watching yet. Safe to await while
	/// publishing.
	pub async fn used(&self) -> Result<(), JsValue> {
		let demand = self.demand.clone();
		demand.used().await.map_err(js_err)
	}

	/// Resolve once nobody is subscribed, so the producer can stop working.
	///
	/// Resolves immediately when nobody has ever subscribed, so use `used` to wait for the
	/// first viewer rather than expecting this to hold. Safe to await while publishing,
	/// which is the point: this is the demand signal a publisher races against its writes.
	pub async fn unused(&self) -> Result<(), JsValue> {
		let demand = self.demand.clone();
		demand.unused().await.map_err(js_err)
	}

	/// The subscription currently aggregated across every live subscriber, or `null`
	/// when nobody is subscribed.
	#[wasm_bindgen(getter)]
	pub fn subscription(&self) -> Result<Option<Subscription>, JsValue> {
		Ok(self
			.inner
			.peek("subscription", |track| track.subscription())?
			.map(Into::into))
	}
}

/// A handle to a track, used to subscribe or fetch.
#[wasm_bindgen]
pub struct TrackConsumer {
	name: String,
	inner: moq_net::track::Consumer,
}

impl TrackConsumer {
	pub fn new(inner: moq_net::track::Consumer) -> Self {
		Self {
			name: inner.name().to_string(),
			inner,
		}
	}
}

#[wasm_bindgen]
impl TrackConsumer {
	/// This track's name within its broadcast.
	#[wasm_bindgen(getter)]
	pub fn name(&self) -> String {
		self.name.clone()
	}

	/// Subscribe, resolving once the publisher accepts.
	pub async fn subscribe(&self, subscription: Option<Subscription>) -> Result<TrackSubscriber, JsValue> {
		let subscription = subscription.map(Into::into);
		let inner = self.inner.subscribe(subscription).await.map_err(js_err)?;
		Ok(TrackSubscriber::new(inner))
	}

	/// Fetch a single past group by sequence.
	#[wasm_bindgen(js_name = fetchGroup)]
	pub async fn fetch_group(&self, sequence: u64, options: Option<Fetch>) -> Result<GroupConsumer, JsValue> {
		let options = options.map(Into::into);
		let inner = self.inner.fetch_group(sequence, options).await.map_err(js_err)?;
		Ok(GroupConsumer::new(inner))
	}

	/// Look up the track's properties, resolving once the publisher reports them.
	pub async fn info(&self) -> Result<TrackInfo, JsValue> {
		let info = self.inner.info().await.map_err(js_err)?;
		Ok(info.into())
	}

	/// The latest group sequence seen, or `null` if none has arrived.
	#[wasm_bindgen(getter)]
	pub fn latest(&self) -> Option<u64> {
		self.inner.latest()
	}
}

/// An open subscription, yielding groups as they arrive.
#[wasm_bindgen]
pub struct TrackSubscriber {
	name: String,
	info: TrackInfo,

	// Preferences live on their own handle rather than behind `inner`. A read is pending
	// most of the time on a live track, and `inner` is checked out for its whole duration,
	// so routing `update` through it would mean priority and latency could only be changed
	// in the gaps between arrivals. This is what `moq_net::track::SubscriberControl` is for.
	control: moq_net::track::SubscriberControl,

	inner: Exclusive<moq_net::track::Subscriber>,
}

impl TrackSubscriber {
	pub fn new(inner: moq_net::track::Subscriber) -> Self {
		Self {
			name: inner.name().to_string(),
			info: inner.info().clone().into(),
			control: inner.control(),
			inner: Exclusive::new(inner),
		}
	}
}

#[wasm_bindgen]
impl TrackSubscriber {
	/// This track's name within its broadcast.
	#[wasm_bindgen(getter)]
	pub fn name(&self) -> String {
		self.name.clone()
	}

	/// The track's properties, as reported by the publisher when the subscription opened.
	#[wasm_bindgen(getter)]
	pub fn info(&self) -> TrackInfo {
		self.info.clone()
	}

	/// Receive the next group in arrival order, or `null` when the track ends.
	///
	/// Arrival order means a newer group preempts an older one still in flight, which is
	/// what a live player wants. Use `nextGroup` to read in sequence order instead.
	#[wasm_bindgen(js_name = recvGroup)]
	pub async fn recv_group(&self) -> Result<Option<GroupConsumer>, JsValue> {
		let result = self
			.inner
			.with("recvGroup", async |mut sub| {
				let result = sub.recv_group().await;
				(sub, result)
			})
			.await?;

		Ok(result.map_err(js_err)?.map(GroupConsumer::new))
	}

	/// Receive the next group in sequence order, or `null` when the track ends.
	#[wasm_bindgen(js_name = nextGroup)]
	pub async fn next_group(&self) -> Result<Option<GroupConsumer>, JsValue> {
		let result = self
			.inner
			.with("nextGroup", async |mut sub| {
				let result = sub.next_group().await;
				(sub, result)
			})
			.await?;

		Ok(result.map_err(js_err)?.map(GroupConsumer::new))
	}

	/// Read the next frame across group boundaries, or `null` when the track ends.
	///
	/// A convenience for tracks whose groups each hold one frame (a catalog, control
	/// state); it hides the group structure.
	#[wasm_bindgen(js_name = readFrame)]
	pub async fn read_frame(&self) -> Result<Option<Frame>, JsValue> {
		let result = self
			.inner
			.with("readFrame", async |mut sub| {
				let result = sub.read_frame().await;
				(sub, result)
			})
			.await?;

		Ok(result.map_err(js_err)?.map(Frame::from))
	}

	/// Resolve with the final group sequence once the track is complete.
	pub async fn finished(&self) -> Result<u64, JsValue> {
		let result = self
			.inner
			.with("finished", async |mut sub| {
				let result = sub.finished().await;
				(sub, result)
			})
			.await?;

		result.map_err(js_err)
	}

	/// Move this reader's cursor to the given group, skipping anything earlier.
	///
	/// A local cursor only: it does not change what the publisher sends. Use `update`
	/// with a `startGroup` for that.
	#[wasm_bindgen(js_name = startAt)]
	pub fn start_at(&self, sequence: u64) -> Result<(), JsValue> {
		self.inner.peek("startAt", |sub| sub.start_at(sequence))
	}

	/// Stop this reader after the given group, or pass nothing to remove the cap.
	///
	/// A local cursor only, like `startAt`.
	#[wasm_bindgen(js_name = endAt)]
	pub fn end_at(&self, sequence: Option<u64>) -> Result<(), JsValue> {
		self.inner.peek("endAt", |sub| sub.end_at(sequence))
	}

	/// The subscription this reader requested.
	///
	/// Readable while a read is in flight, unlike the cursor methods.
	#[wasm_bindgen(getter)]
	pub fn subscription(&self) -> Subscription {
		self.control.subscription().into()
	}

	/// Change the subscription, e.g. to raise priority or widen the latency window.
	///
	/// Safe to call while a read is in flight, which is the point: a player retunes
	/// priority or latency mid-stream, not in the gaps between groups.
	pub fn update(&self, subscription: Subscription) -> Result<(), JsValue> {
		self.control.update(subscription.into()).map_err(js_err)
	}

	/// The latest group sequence seen, or `null` if none has arrived.
	#[wasm_bindgen(getter)]
	pub fn latest(&self) -> Result<Option<u64>, JsValue> {
		self.inner.peek("latest", |sub| sub.latest())
	}
}

/// A track the peer subscribed to, waiting to be served.
#[wasm_bindgen]
pub struct TrackRequest {
	name: String,
	inner: Exclusive<moq_net::track::Request>,
}

impl TrackRequest {
	pub fn new(inner: moq_net::track::Request) -> Self {
		Self {
			name: inner.name().to_string(),
			inner: Exclusive::new(inner),
		}
	}
}

#[wasm_bindgen]
impl TrackRequest {
	/// The requested track's name.
	#[wasm_bindgen(getter)]
	pub fn name(&self) -> String {
		self.name.clone()
	}

	/// What the subscriber asked for, or `null` if it only wants the track's properties.
	#[wasm_bindgen(getter)]
	pub fn subscription(&self) -> Result<Option<Subscription>, JsValue> {
		Ok(self
			.inner
			.peek("subscription", |req| req.subscription())?
			.map(Into::into))
	}

	/// Serve the track, optionally declaring its properties.
	pub fn accept(&self, info: Option<TrackInfo>) -> Result<TrackProducer, JsValue> {
		let info = info.map(TryInto::try_into).transpose()?;
		let request = self.inner.take("accept")?;
		Ok(TrackProducer::new(request.accept(info)))
	}

	/// Refuse to serve the track, with an application close code.
	pub fn reject(&self, code: u16) -> Result<(), JsValue> {
		let request = self.inner.take("reject")?;
		request.reject(moq_net::Error::App(code));
		Ok(())
	}
}
