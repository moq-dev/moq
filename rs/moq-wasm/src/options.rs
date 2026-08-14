//! Option bags and value types crossing the JS boundary.
//!
//! These mirror the plain-data types in `moq-net` (`track::Subscription`, `track::Info`,
//! `frame::Frame`). They are bags rather than positional parameters so a new knob is an
//! added field instead of a changed signature.
//!
//! The three input bags cross as **plain JS objects** (serde), not exported classes.
//! wasm-bindgen passes an exported class by value, which zeroes the JS object's pointer,
//! and it encodes a null pointer as `None`. So a caller who built one bag and reused it
//! across two calls silently got defaults on the second: measured, a `TrackInfo` reused
//! across two `createTrack` calls gave the second track a millisecond timescale instead of
//! the microsecond one it asked for, with no error anywhere. A plain object has no
//! ownership to lose, and it matches how `@moq/net` already takes options.
//!
//! `Frame` stays a class: it is only ever returned, so it can't hit that, and its payload
//! rides as a `Uint8Array` rather than a serialized number array.
//!
//! Units are the ones `@moq/net` uses, since that is what a JS caller expects: durations
//! in milliseconds, timestamps in microseconds. Sequence numbers stay `u64`.

use std::time::Duration;

use js_sys::Uint8Array;
use moq_net::{Timescale, Timestamp};
use serde::{Deserialize, Serialize};
use tsify::Tsify;
use wasm_bindgen::prelude::*;

use crate::util::js_err;

/// The longest duration that still fits the wire, since these all get encoded as a
/// millisecond QUIC varint. About 146 million years, so it reads as "no limit" to any
/// caller while staying encodable.
///
/// `Duration::MAX` would not: its `as_millis()` is ~1.8e22, four orders of magnitude past
/// `VarInt::MAX`, so encoding a track's properties would fail well after the bad value was
/// accepted.
const MAX_ENCODABLE: Duration = Duration::from_millis(moq_net::VarInt::MAX.into_inner());

/// Convert a JS millisecond duration into a `Duration`, clamping rather than panicking.
///
/// `Duration::from_secs_f64` panics on a non-finite or out-of-range value, and `Infinity`
/// is how a JS caller naturally spells "no limit"; a panic would surface as an opaque wasm
/// trap instead of the error this API declares. So clamp: at or below zero (and NaN, via
/// the `max`) is `ZERO`, anything above is [`MAX_ENCODABLE`].
fn duration_from_millis(millis: f64) -> Duration {
	let millis = millis.max(0.0);
	match Duration::try_from_secs_f64(millis / 1000.0) {
		Ok(d) if d <= MAX_ENCODABLE => d,
		_ => MAX_ENCODABLE,
	}
}

/// Per-subscription options, requested when a subscription opens and adjustable later
/// via `TrackSubscriber.update`.
///
/// Field types and ranges are enforced on the way in (a string priority, or one past
/// `u8`, is an error), but a misspelled field is not: this deserializer never sees the
/// keys it wasn't asked for, so `deny_unknown_fields` would be a no-op. The generated
/// TypeScript interface is what catches a typo, the same as `@moq/net`.
#[derive(Tsify, Serialize, Deserialize, Clone, Default)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(rename_all = "camelCase", default)]
pub struct Subscription {
	/// Delivery priority relative to this session's other subscriptions. Higher wins.
	pub priority: u8,
	/// Whether groups are prioritized in sequence order, rather than newest-first.
	pub ordered: bool,
	/// Maximum age in milliseconds of a non-latest group before it is skipped.
	pub latency_max: f64,
	/// First group the publisher should deliver, or unset to start at the latest group.
	#[tsify(optional)]
	pub start_group: Option<u64>,
	/// Last group the publisher should deliver (inclusive), or unset for no end.
	#[tsify(optional)]
	pub end_group: Option<u64>,
}

impl From<Subscription> for moq_net::track::Subscription {
	fn from(value: Subscription) -> Self {
		// Field-by-field rather than a struct literal: the wire types are
		// `#[non_exhaustive]`, so a new knob shows up here as a default rather than a
		// compile error. Mirror it above when a caller should be able to set it.
		let mut out = Self::default();
		out.priority = value.priority;
		out.ordered = value.ordered;
		out.latency_max = duration_from_millis(value.latency_max);
		out.group_start = value.start_group;
		out.group_end = value.end_group;
		out
	}
}

impl From<moq_net::track::Subscription> for Subscription {
	fn from(value: moq_net::track::Subscription) -> Self {
		Self {
			priority: value.priority,
			ordered: value.ordered,
			latency_max: value.latency_max.as_secs_f64() * 1000.0,
			start_group: value.group_start,
			end_group: value.group_end,
		}
	}
}

/// Options for fetching a single past group.
#[derive(Tsify, Serialize, Deserialize, Clone, Default)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(rename_all = "camelCase", default)]
pub struct Fetch {
	/// Delivery priority for the fetched group's stream. Higher wins.
	pub priority: u8,
}

impl From<Fetch> for moq_net::group::Fetch {
	fn from(value: Fetch) -> Self {
		// See the note in `Subscription`: `#[non_exhaustive]`, so fill in field by field.
		let mut out = Self::default();
		out.priority = value.priority;
		out
	}
}

/// Immutable per-track properties, set by the publisher and reported over the wire.
#[derive(Tsify, Serialize, Deserialize, Clone)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(rename_all = "camelCase", default)]
pub struct TrackInfo {
	/// Units per second for this track's frame timestamps. Defaults to 1000 (milliseconds).
	pub timescale: u64,
	/// Maximum age in milliseconds of a non-latest group before the publisher evicts it.
	pub latency_max: f64,
	/// Tie-break priority between subscriptions of equal subscriber priority.
	pub priority: u8,
	/// Whether groups are prioritized in sequence order, rather than newest-first.
	pub ordered: bool,
}

impl Default for TrackInfo {
	/// Millisecond timescale, unordered, priority 0: `moq-net`'s own defaults, so an
	/// omitted field means the same thing here as it does on the wire.
	fn default() -> Self {
		moq_net::track::Info::default().into()
	}
}

impl TryFrom<TrackInfo> for moq_net::track::Info {
	type Error = JsValue;

	fn try_from(value: TrackInfo) -> Result<Self, Self::Error> {
		// See the note in `Subscription`: `#[non_exhaustive]`, so fill in field by field.
		let mut out = Self::default();
		out.timescale = Timescale::new(value.timescale).map_err(js_err)?;
		out.latency_max = duration_from_millis(value.latency_max);
		out.priority = value.priority;
		out.ordered = value.ordered;
		Ok(out)
	}
}

impl From<moq_net::track::Info> for TrackInfo {
	fn from(value: moq_net::track::Info) -> Self {
		Self {
			timescale: value.timescale.as_u64(),
			latency_max: value.latency_max.as_secs_f64() * 1000.0,
			priority: value.priority,
			ordered: value.ordered,
		}
	}
}

/// A single frame: its presentation timestamp and payload.
#[wasm_bindgen]
pub struct Frame {
	/// Presentation timestamp in microseconds.
	pub timestamp: f64,

	payload: Uint8Array,
}

#[wasm_bindgen]
impl Frame {
	/// Build a frame from a timestamp in microseconds and its payload.
	#[wasm_bindgen(constructor)]
	pub fn new(timestamp: f64, payload: Uint8Array) -> Self {
		Self { timestamp, payload }
	}

	/// The frame payload.
	#[wasm_bindgen(getter)]
	pub fn payload(&self) -> Uint8Array {
		self.payload.clone()
	}
}

impl From<moq_net::frame::Frame> for Frame {
	fn from(value: moq_net::frame::Frame) -> Self {
		Self {
			timestamp: value.timestamp.as_micros() as f64,
			payload: Uint8Array::from(value.payload.as_ref()),
		}
	}
}

/// Convert a JS microsecond timestamp into the wire type.
pub fn timestamp(micros: f64) -> Result<Timestamp, JsValue> {
	if !micros.is_finite() || micros < 0.0 {
		return Err(js_err("timestamp must be a non-negative finite number"));
	}
	Timestamp::from_micros(micros as u64).map_err(js_err)
}
