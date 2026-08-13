//! Option bags and value types crossing the JS boundary.
//!
//! These mirror the plain-data types in `moq-net` (`track::Subscription`, `track::Info`,
//! `frame::Frame`). They are classes rather than positional arguments so a new knob is an
//! added field instead of a changed signature.
//!
//! Units are the ones `@moq/net` uses, since that is what a JS caller expects: durations
//! in milliseconds, timestamps in microseconds. Sequences stay `u64`, which wasm-bindgen
//! maps to a JS `bigint`.

use std::time::Duration;

use js_sys::Uint8Array;
use moq_net::{Timescale, Timestamp};
use wasm_bindgen::prelude::*;

use crate::util::js_err;

/// Per-subscription options, requested when a subscription opens and adjustable later
/// via `TrackSubscriber.update`.
#[wasm_bindgen]
#[derive(Clone, Default)]
pub struct Subscription {
	/// Delivery priority relative to this session's other subscriptions. Higher wins.
	pub priority: u8,
	/// Whether groups are prioritized in sequence order, rather than newest-first.
	pub ordered: bool,
	/// Maximum age in milliseconds of a non-latest group before it is skipped.
	#[wasm_bindgen(js_name = latencyMax)]
	pub latency_max: f64,
	/// First group the publisher should deliver, or unset to start at the latest group.
	#[wasm_bindgen(js_name = startGroup)]
	pub start_group: Option<u64>,
	/// Last group the publisher should deliver (inclusive), or unset for no end.
	#[wasm_bindgen(js_name = endGroup)]
	pub end_group: Option<u64>,
}

#[wasm_bindgen]
impl Subscription {
	/// A subscription with every field at its default: live edge, unordered, priority 0.
	#[wasm_bindgen(constructor)]
	pub fn new() -> Self {
		Self::default()
	}
}

impl From<Subscription> for moq_net::track::Subscription {
	fn from(value: Subscription) -> Self {
		// Field-by-field rather than a struct literal: the wire types are
		// `#[non_exhaustive]`, so a new knob shows up here as a default rather than a
		// compile error. Mirror it above when a caller should be able to set it.
		let mut out = Self::default();
		out.priority = value.priority;
		out.ordered = value.ordered;
		out.latency_max = Duration::from_secs_f64(value.latency_max.max(0.0) / 1000.0);
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

/// Immutable per-track properties, set by the publisher and reported over the wire.
#[wasm_bindgen]
#[derive(Clone)]
pub struct TrackInfo {
	/// Units per second for this track's frame timestamps. Defaults to 1000 (milliseconds).
	pub timescale: u64,
	/// Maximum age in milliseconds of a non-latest group before the publisher evicts it.
	#[wasm_bindgen(js_name = latencyMax)]
	pub latency_max: f64,
	/// Tie-break priority between subscriptions of equal subscriber priority.
	pub priority: u8,
	/// Whether groups are prioritized in sequence order, rather than newest-first.
	pub ordered: bool,
}

#[wasm_bindgen]
impl TrackInfo {
	/// Track properties at their defaults: millisecond timescale, unordered, priority 0.
	#[wasm_bindgen(constructor)]
	pub fn new() -> Self {
		moq_net::track::Info::default().into()
	}
}

impl Default for TrackInfo {
	fn default() -> Self {
		Self::new()
	}
}

impl TryFrom<TrackInfo> for moq_net::track::Info {
	type Error = JsValue;

	fn try_from(value: TrackInfo) -> Result<Self, Self::Error> {
		// See the note in `Subscription`: `#[non_exhaustive]`, so fill in field by field.
		let mut out = Self::default();
		out.timescale = Timescale::new(value.timescale).map_err(js_err)?;
		out.latency_max = Duration::from_secs_f64(value.latency_max.max(0.0) / 1000.0);
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
