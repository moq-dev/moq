//! CPU cost: JSON Merge Patch + DEFLATE vs DEFLATE-only streaming.
//!
//! Both approaches keep one shared DEFLATE window per group, so the wire bytes are comparable. The
//! question is how much CPU the merge-patch machinery (Value tree, diff, merge apply) adds on top of
//! just feeding the full snapshot through the same window every tick.
//!
//! It also compares two ways to *generate* the merge patch: the current `serde_json::to_value` +
//! tree `diff`, against a serde Serializer ([`diff_serialize`]) that walks `T` and diffs each field
//! against the previous value directly, never building a full Value tree for the new value.
//!
//! Run with: `cargo run --release -p moq-json --example bench`

use std::hint::black_box;
use std::task::Poll;
use std::time::Instant;

use moq_flate::{Decoder, Encoder};
use moq_json::{ConsumerConfig, Producer, ProducerConfig};
use serde::Serialize;
use serde::ser::{SerializeMap, SerializeSeq, SerializeStruct, Serializer};
use serde_json::{Map, Value, json};

/// One second of telemetry: a big static core plus a few moving numbers (mirrors examples/telemetry.rs).
fn telemetry(tick: u64) -> Value {
	let t = tick as f64;
	let lat = 37.7749 + (t * 0.0001).sin() * 0.01;
	let lon = -122.4194 + (t * 0.0001).cos() * 0.01;

	json!({
		"device": {
			"id": "veh-4417-a2",
			"model": "Sentinel X2",
			"firmware": "4.18.2-rc1",
			"serial": "SNX2-0000-4417-A2C9",
			"region": "us-west-2",
			"fleet": "logistics-prod",
			"tags": ["cold-chain", "long-haul", "priority"],
		},
		"config": {
			"sample_hz": 1,
			"upload_hz": 1,
			"geofence": "bay-area",
			"thresholds": { "temp_c": 8.0, "humidity": 85, "shock_g": 3.5, "battery_pct": 15 },
			"contacts": ["ops@example.com", "fleet@example.com"],
		},
		"ts": 1_700_000_000 + tick,
		"uptime_s": tick,
		"location": {
			"lat": (lat * 1e6).round() / 1e6,
			"lon": (lon * 1e6).round() / 1e6,
			"alt_m": 12 + (tick % 5),
			"heading": (tick * 7) % 360,
			"speed_kph": 40 + (tick % 25),
			"fix": "3d",
			"sats": 9 + (tick % 3),
		},
		"sensors": {
			"temp_c": ((4.0 + (t * 0.05).sin() * 1.5) * 100.0).round() / 100.0,
			"humidity": 60 + (tick % 10),
			"shock_g": (((t * 0.3).sin().abs()) * 100.0).round() / 100.0,
			"door_open": tick % 30 == 0,
		},
		"power": {
			"battery_pct": 100 - (tick / 6) % 100,
			"charging": false,
			"voltage_mv": 12_400 - (tick % 50) as i64,
			"current_ma": 850 + (tick % 120) as i64,
		},
		"network": {
			"rssi_dbm": -70 - (tick % 15) as i64,
			"type": "lte",
			"bytes_up": 1_024 * tick,
			"bytes_down": 256 * tick,
			"latency_ms": 35 + (tick % 40),
		},
		"counters": {
			"events": tick,
			"errors": tick / 50,
			"reconnects": tick / 120,
		},
	})
}

// ---- A copy of the crate-private diff::diff so we can micro-time the merge-patch step. ----

fn diff(old: &Value, new: &Value) -> Value {
	if let (Value::Object(old), Value::Object(new)) = (old, new) {
		let mut patch = Map::new();
		diff_objects(old, new, &mut patch);
		Value::Object(patch)
	} else {
		new.clone()
	}
}

fn diff_objects(old: &Map<String, Value>, new: &Map<String, Value>, patch: &mut Map<String, Value>) {
	for key in old.keys() {
		if !new.contains_key(key) {
			patch.insert(key.clone(), Value::Null);
		}
	}
	for (key, new_val) in new {
		let old_val = old.get(key);
		if old_val == Some(new_val) {
			continue;
		}
		if let (Some(Value::Object(old_obj)), Value::Object(new_obj)) = (old_val, new_val) {
			let mut sub = Map::new();
			diff_objects(old_obj, new_obj, &mut sub);
			if !sub.is_empty() {
				patch.insert(key.clone(), Value::Object(sub));
			}
			continue;
		}
		patch.insert(key.clone(), new_val.clone());
	}
}

// ---------------------------------------------------------------------------------------------
// Candidate: a serde Serializer that emits an RFC 7396 merge patch directly from `T`, diffing
// against the previous value as it visits each field. Unlike `to_value` + `diff`, it never
// materializes a full Value tree for the new value: unchanged subtrees cost a comparison and no
// allocation, and only the changed nodes are built into the patch. This is the "visit each field
// and compare" approach, skipping the intermediate map-backed Value for the new value.
// ---------------------------------------------------------------------------------------------

/// One node's verdict from the diffing serializer.
enum Node {
	/// Equal to the baseline; nothing to emit.
	Same,
	/// Differs; the new value to splice into the patch.
	Diff(Value),
}

const NULL: Value = Value::Null;

/// Serializer that diffs `T` against `baseline` and yields a merge patch. `forced` is set if a
/// genuine null is emitted (merge patch can't represent it, so the caller must snapshot).
#[derive(Copy, Clone)]
struct Differ<'a> {
	baseline: &'a Value,
	forced: &'a std::cell::Cell<bool>,
}

impl<'a> Differ<'a> {
	/// The baseline child for `key` and whether the baseline actually had that key (a missing key
	/// means the field is an addition, which `MapDiff` uses to keep deletion detection cheap).
	fn child(&self, key: &str) -> (Differ<'a>, bool) {
		let (baseline, existed) = match self.baseline {
			Value::Object(m) => match m.get(key) {
				Some(value) => (value, true),
				None => (&NULL, false),
			},
			_ => (&NULL, false),
		};
		(
			Differ {
				baseline,
				forced: self.forced,
			},
			existed,
		)
	}

	/// Compare a freshly built scalar/array against the baseline, flagging emitted nulls as forced.
	fn scalar(self, value: Value) -> Result<Node, SerError> {
		if self.baseline == &value {
			Ok(Node::Same)
		} else {
			if value.is_null() {
				self.forced.set(true);
			}
			Ok(Node::Diff(value))
		}
	}
}

/// Generate a merge patch for `new` against `old`, returning the patch and whether a null forced a
/// snapshot. Matches `diff` (object roots recurse; any other root forces a snapshot).
fn diff_serialize<T: Serialize>(old: &Value, new: &T) -> (Value, bool) {
	let forced = std::cell::Cell::new(false);
	let node = new
		.serialize(Differ {
			baseline: old,
			forced: &forced,
		})
		.expect("serializing into a merge patch is infallible for JSON-shaped data");
	match node {
		Node::Same => (Value::Object(Map::new()), forced.get()),
		// A non-object patch (or non-object baseline) can't be a recursive merge patch, so force a
		// snapshot just like `diff` does for non-object roots.
		Node::Diff(value) => {
			let non_object_root = !value.is_object() || !old.is_object();
			(value, forced.get() || non_object_root)
		}
	}
}

/// Minimal serde error for the diffing serializer.
#[derive(Debug)]
struct SerError(String);

impl std::fmt::Display for SerError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(&self.0)
	}
}

impl std::error::Error for SerError {}

impl serde::ser::Error for SerError {
	fn custom<M: std::fmt::Display>(msg: M) -> Self {
		SerError(msg.to_string())
	}
}

/// Build a Value with no diffing (used for array elements, which merge patch replaces wholesale).
fn to_plain<T: Serialize + ?Sized>(value: &T) -> Result<Value, SerError> {
	serde_json::to_value(value).map_err(|e| SerError(e.to_string()))
}

impl<'a> Serializer for Differ<'a> {
	type Ok = Node;
	type Error = SerError;
	type SerializeSeq = SeqDiff<'a>;
	type SerializeTuple = SeqDiff<'a>;
	type SerializeTupleStruct = SeqDiff<'a>;
	type SerializeTupleVariant = serde::ser::Impossible<Node, SerError>;
	type SerializeMap = MapDiff<'a>;
	type SerializeStruct = MapDiff<'a>;
	type SerializeStructVariant = serde::ser::Impossible<Node, SerError>;

	fn serialize_bool(self, v: bool) -> Result<Node, SerError> {
		self.scalar(Value::Bool(v))
	}
	fn serialize_i8(self, v: i8) -> Result<Node, SerError> {
		self.scalar(Value::from(v))
	}
	fn serialize_i16(self, v: i16) -> Result<Node, SerError> {
		self.scalar(Value::from(v))
	}
	fn serialize_i32(self, v: i32) -> Result<Node, SerError> {
		self.scalar(Value::from(v))
	}
	fn serialize_i64(self, v: i64) -> Result<Node, SerError> {
		self.scalar(Value::from(v))
	}
	fn serialize_u8(self, v: u8) -> Result<Node, SerError> {
		self.scalar(Value::from(v))
	}
	fn serialize_u16(self, v: u16) -> Result<Node, SerError> {
		self.scalar(Value::from(v))
	}
	fn serialize_u32(self, v: u32) -> Result<Node, SerError> {
		self.scalar(Value::from(v))
	}
	fn serialize_u64(self, v: u64) -> Result<Node, SerError> {
		self.scalar(Value::from(v))
	}
	fn serialize_f32(self, v: f32) -> Result<Node, SerError> {
		self.scalar(Value::from(v))
	}
	fn serialize_f64(self, v: f64) -> Result<Node, SerError> {
		self.scalar(Value::from(v))
	}
	fn serialize_char(self, v: char) -> Result<Node, SerError> {
		self.scalar(Value::from(v.to_string()))
	}
	fn serialize_str(self, v: &str) -> Result<Node, SerError> {
		// Strings are the common churn-free field, so compare against the baseline without allocating a
		// `Value::String` on the unchanged path. This is the whole point of visiting fields directly.
		if matches!(self.baseline, Value::String(b) if b == v) {
			Ok(Node::Same)
		} else {
			Ok(Node::Diff(Value::from(v)))
		}
	}
	fn serialize_bytes(self, v: &[u8]) -> Result<Node, SerError> {
		self.scalar(to_plain(v)?)
	}
	fn serialize_none(self) -> Result<Node, SerError> {
		self.scalar(Value::Null)
	}
	fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<Node, SerError> {
		value.serialize(self)
	}
	fn serialize_unit(self) -> Result<Node, SerError> {
		self.scalar(Value::Null)
	}
	fn serialize_unit_struct(self, _name: &'static str) -> Result<Node, SerError> {
		self.scalar(Value::Null)
	}
	fn serialize_unit_variant(self, _name: &'static str, _idx: u32, variant: &'static str) -> Result<Node, SerError> {
		self.scalar(Value::from(variant))
	}
	fn serialize_newtype_struct<T: Serialize + ?Sized>(self, _name: &'static str, value: &T) -> Result<Node, SerError> {
		value.serialize(self)
	}
	fn serialize_newtype_variant<T: Serialize + ?Sized>(
		self,
		_name: &'static str,
		_idx: u32,
		_variant: &'static str,
		value: &T,
	) -> Result<Node, SerError> {
		// Externally-tagged enum: replace wholesale, matching how `to_value` shapes it.
		self.scalar(to_plain(value)?)
	}
	fn serialize_seq(self, len: Option<usize>) -> Result<SeqDiff<'a>, SerError> {
		Ok(SeqDiff {
			differ: self,
			items: Vec::with_capacity(len.unwrap_or(0)),
		})
	}
	fn serialize_tuple(self, len: usize) -> Result<SeqDiff<'a>, SerError> {
		self.serialize_seq(Some(len))
	}
	fn serialize_tuple_struct(self, _name: &'static str, len: usize) -> Result<SeqDiff<'a>, SerError> {
		self.serialize_seq(Some(len))
	}
	fn serialize_tuple_variant(
		self,
		_name: &'static str,
		_idx: u32,
		_variant: &'static str,
		_len: usize,
	) -> Result<Self::SerializeTupleVariant, SerError> {
		Err(SerError("tuple variants are unsupported".into()))
	}
	fn serialize_map(self, _len: Option<usize>) -> Result<MapDiff<'a>, SerError> {
		Ok(MapDiff {
			differ: self,
			patch: Map::new(),
			seen: Vec::new(),
			added_key: false,
			pending_key: None,
		})
	}
	fn serialize_struct(self, _name: &'static str, len: usize) -> Result<MapDiff<'a>, SerError> {
		self.serialize_map(Some(len))
	}
	fn serialize_struct_variant(
		self,
		_name: &'static str,
		_idx: u32,
		_variant: &'static str,
		_len: usize,
	) -> Result<Self::SerializeStructVariant, SerError> {
		Err(SerError("struct variants are unsupported".into()))
	}
}

/// Arrays are replaced wholesale by merge patch, so this builds the full new array and compares it
/// to the baseline in one shot.
struct SeqDiff<'a> {
	differ: Differ<'a>,
	items: Vec<Value>,
}

impl SerializeSeq for SeqDiff<'_> {
	type Ok = Node;
	type Error = SerError;
	fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), SerError> {
		self.items.push(to_plain(value)?);
		Ok(())
	}
	fn end(self) -> Result<Node, SerError> {
		self.differ.scalar(Value::Array(self.items))
	}
}

impl serde::ser::SerializeTuple for SeqDiff<'_> {
	type Ok = Node;
	type Error = SerError;
	fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), SerError> {
		SerializeSeq::serialize_element(self, value)
	}
	fn end(self) -> Result<Node, SerError> {
		SerializeSeq::end(self)
	}
}

impl serde::ser::SerializeTupleStruct for SeqDiff<'_> {
	type Ok = Node;
	type Error = SerError;
	fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), SerError> {
		SerializeSeq::serialize_element(self, value)
	}
	fn end(self) -> Result<Node, SerError> {
		SerializeSeq::end(self)
	}
}

/// Objects recurse: each entry diffs against the baseline's child, and only changed entries land in
/// the patch. Keys present in the baseline but absent now become explicit null deletions.
struct MapDiff<'a> {
	differ: Differ<'a>,
	patch: Map<String, Value>,
	seen: Vec<String>,
	// Set when a field's key was absent from the baseline. Lets `finish` skip the deletion scan when
	// the new keys are exactly the baseline keys (the common, churn-free case).
	added_key: bool,
	pending_key: Option<String>,
}

impl MapDiff<'_> {
	fn entry(&mut self, key: String, existed: bool, node: Node) {
		self.added_key |= !existed;
		if let Node::Diff(value) = node {
			self.patch.insert(key.clone(), value);
		}
		self.seen.push(key);
	}

	fn finish(self) -> Result<Node, SerError> {
		let mut patch = self.patch;
		if let Value::Object(base) = self.differ.baseline {
			// A deletion is only possible if some key was added or the counts differ. Otherwise the new
			// keys are exactly the baseline keys, so there's nothing to delete and we skip the scan. This
			// keeps the common path O(1) rather than O(n^2), which matters since this runs inside the
			// benchmarked candidate. A removed key is a clean delete (explicit null), and unlike a value
			// set to null it does not force a snapshot.
			if self.added_key || self.seen.len() != base.len() {
				let seen: std::collections::HashSet<&str> = self.seen.iter().map(String::as_str).collect();
				for key in base.keys() {
					if !seen.contains(key.as_str()) {
						patch.insert(key.clone(), Value::Null);
					}
				}
			}
		}
		if patch.is_empty() {
			Ok(Node::Same)
		} else {
			Ok(Node::Diff(Value::Object(patch)))
		}
	}
}

impl SerializeMap for MapDiff<'_> {
	type Ok = Node;
	type Error = SerError;
	fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), SerError> {
		// Extract the key string in a single allocation (no intermediate Value), since the reference
		// diff walks keys for free and we don't want key handling to swamp the win.
		self.pending_key = Some(key.serialize(KeySer)?);
		Ok(())
	}
	fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), SerError> {
		let key = self.pending_key.take().expect("serialize_key precedes serialize_value");
		let (child, existed) = self.differ.child(&key);
		let node = value.serialize(child)?;
		self.entry(key, existed, node);
		Ok(())
	}
	fn end(self) -> Result<Node, SerError> {
		self.finish()
	}
}

/// Serializes a map key to its `String`, the only form JSON object keys take. Anything else is an
/// error, mirroring `serde_json`'s own key handling.
struct KeySer;

impl Serializer for KeySer {
	type Ok = String;
	type Error = SerError;
	type SerializeSeq = serde::ser::Impossible<String, SerError>;
	type SerializeTuple = serde::ser::Impossible<String, SerError>;
	type SerializeTupleStruct = serde::ser::Impossible<String, SerError>;
	type SerializeTupleVariant = serde::ser::Impossible<String, SerError>;
	type SerializeMap = serde::ser::Impossible<String, SerError>;
	type SerializeStruct = serde::ser::Impossible<String, SerError>;
	type SerializeStructVariant = serde::ser::Impossible<String, SerError>;

	fn serialize_str(self, v: &str) -> Result<String, SerError> {
		Ok(v.to_owned())
	}
	fn serialize_char(self, v: char) -> Result<String, SerError> {
		Ok(v.to_string())
	}
	fn serialize_unit_variant(self, _name: &'static str, _idx: u32, variant: &'static str) -> Result<String, SerError> {
		Ok(variant.to_owned())
	}
	fn serialize_newtype_struct<T: Serialize + ?Sized>(
		self,
		_name: &'static str,
		value: &T,
	) -> Result<String, SerError> {
		value.serialize(self)
	}
	fn serialize_bool(self, v: bool) -> Result<String, SerError> {
		Ok(v.to_string())
	}
	fn serialize_i64(self, v: i64) -> Result<String, SerError> {
		Ok(v.to_string())
	}
	fn serialize_u64(self, v: u64) -> Result<String, SerError> {
		Ok(v.to_string())
	}
	fn serialize_i8(self, v: i8) -> Result<String, SerError> {
		Ok(v.to_string())
	}
	fn serialize_i16(self, v: i16) -> Result<String, SerError> {
		Ok(v.to_string())
	}
	fn serialize_i32(self, v: i32) -> Result<String, SerError> {
		Ok(v.to_string())
	}
	fn serialize_u8(self, v: u8) -> Result<String, SerError> {
		Ok(v.to_string())
	}
	fn serialize_u16(self, v: u16) -> Result<String, SerError> {
		Ok(v.to_string())
	}
	fn serialize_u32(self, v: u32) -> Result<String, SerError> {
		Ok(v.to_string())
	}
	fn serialize_f32(self, _v: f32) -> Result<String, SerError> {
		Err(SerError("float map key".into()))
	}
	fn serialize_f64(self, _v: f64) -> Result<String, SerError> {
		Err(SerError("float map key".into()))
	}
	fn serialize_bytes(self, _v: &[u8]) -> Result<String, SerError> {
		Err(SerError("bytes map key".into()))
	}
	fn serialize_none(self) -> Result<String, SerError> {
		Err(SerError("null map key".into()))
	}
	fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<String, SerError> {
		value.serialize(self)
	}
	fn serialize_unit(self) -> Result<String, SerError> {
		Err(SerError("unit map key".into()))
	}
	fn serialize_unit_struct(self, _name: &'static str) -> Result<String, SerError> {
		Err(SerError("unit struct map key".into()))
	}
	fn serialize_newtype_variant<T: Serialize + ?Sized>(
		self,
		_name: &'static str,
		_idx: u32,
		_variant: &'static str,
		_value: &T,
	) -> Result<String, SerError> {
		Err(SerError("newtype variant map key".into()))
	}
	fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq, SerError> {
		Err(SerError("seq map key".into()))
	}
	fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple, SerError> {
		Err(SerError("tuple map key".into()))
	}
	fn serialize_tuple_struct(self, _name: &'static str, _len: usize) -> Result<Self::SerializeTupleStruct, SerError> {
		Err(SerError("tuple struct map key".into()))
	}
	fn serialize_tuple_variant(
		self,
		_name: &'static str,
		_idx: u32,
		_variant: &'static str,
		_len: usize,
	) -> Result<Self::SerializeTupleVariant, SerError> {
		Err(SerError("tuple variant map key".into()))
	}
	fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, SerError> {
		Err(SerError("map map key".into()))
	}
	fn serialize_struct(self, _name: &'static str, _len: usize) -> Result<Self::SerializeStruct, SerError> {
		Err(SerError("struct map key".into()))
	}
	fn serialize_struct_variant(
		self,
		_name: &'static str,
		_idx: u32,
		_variant: &'static str,
		_len: usize,
	) -> Result<Self::SerializeStructVariant, SerError> {
		Err(SerError("struct variant map key".into()))
	}
}

impl SerializeStruct for MapDiff<'_> {
	type Ok = Node;
	type Error = SerError;
	fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, value: &T) -> Result<(), SerError> {
		let (child, existed) = self.differ.child(key);
		let node = value.serialize(child)?;
		self.entry(key.to_owned(), existed, node);
		Ok(())
	}
	fn end(self) -> Result<Node, SerError> {
		self.finish()
	}
}

/// A large mostly-static document: a big config blob that never changes plus a few counters that
/// tick. This is the shape where a tiny merge patch should beat re-feeding the whole snapshot.
fn big_static(tick: u64) -> Value {
	// ~6 KB of static config (routes table) that is identical every tick.
	let routes: Vec<Value> = (0..80)
		.map(|i| {
			json!({
				"id": format!("route-{i:04}"),
				"cidr": format!("10.{}.{}.0/24", i / 16, i % 16),
				"gateway": format!("10.0.{i}.1"),
				"metric": 100 + i,
				"enabled": true,
				"tags": ["prod", "egress", "monitored"],
			})
		})
		.collect();

	json!({
		"meta": { "version": "9.2.1", "node": "edge-router-77", "region": "us-east-1" },
		"routes": routes,
		"counters": {
			"packets_in": 1_000_000 + tick * 137,
			"packets_out": 990_000 + tick * 131,
			"errors": tick / 7,
			"uptime_s": tick,
		},
	})
}

/// A large doc of nested scalar objects (no big arrays) where only a couple of fields move. This is
/// the diffing serializer's sweet spot: it prunes the ~100 unchanged string/number fields without
/// allocating a Value for them, while `to_value` rebuilds the whole tree every tick.
fn big_nested(tick: u64) -> Value {
	let mut sensors = Map::new();
	for i in 0..100 {
		// Only the first two sensors' readings move; the other 98 objects are identical every tick.
		let value = if i < 2 { 20 + (tick % 13) as i64 } else { 20 + i };
		sensors.insert(
			format!("sensor-{i:03}"),
			json!({
				"id": format!("sensor-{i:03}"),
				"location": format!("rack-{}-slot-{}", i / 10, i % 10),
				"unit": "celsius",
				"status": "nominal",
				"calibrated": true,
				"value": value,
			}),
		);
	}
	json!({ "site": "dc-7", "sensors": Value::Object(sensors) })
}

const TICKS: u64 = 60;
const ITERS: u32 = 4000;

/// Print the headline merge-vs-deflate comparison (wire + producer + consumer CPU) for a workload.
fn compare(label: &str, frames: &[Value]) {
	let snapshot_len = serde_json::to_vec(&frames[0]).unwrap().len();
	let merge_wire: usize = merge_codec_frames(frames, 6).iter().map(|f| f.len()).sum();
	let deflate_wire = deflate_only_wire(frames, 6);
	let merge_stream = merge_codec_frames(frames, 6);
	let deflate_stream = deflate_only_frames(frames, 6);

	println!("=== {label} (~{snapshot_len} B snapshot, {TICKS} ticks, level 6) ===");
	println!("                                  wire      producer        consumer");
	let mp = bench_quiet(|| black_box_drop(merge_codec_frames(frames, 6)));
	let mc = bench_quiet(|| black_box_drop(merge_codec_consume(&merge_stream)));
	let dp = bench_quiet(|| black_box_drop(deflate_only_frames(frames, 6)));
	let dc = bench_quiet(|| black_box_drop(deflate_only_consume(&deflate_stream)));
	println!("  merge-patch + deflate    {merge_wire:>7} B   {mp:>8.0} ns/t    {mc:>8.0} ns/t");
	println!("  deflate-only             {deflate_wire:>7} B   {dp:>8.0} ns/t    {dc:>8.0} ns/t");
	println!(
		"  merge vs deflate-only:   {:.2}x bytes   {:+.0}% producer   {:+.0}% consumer\n",
		merge_wire as f64 / deflate_wire as f64,
		100.0 * (mp / dp - 1.0),
		100.0 * (mc / dc - 1.0),
	);
}

fn black_box_drop<T>(t: T) {
	black_box(t);
}

/// Compare merge-patch generation two ways, per tick (no DEFLATE):
///   reference: `to_value(new)` then `diff(last, new)`, keeping the rebuilt tree as `last`.
///   candidate: `diff_serialize(last, new)` then `merge` the patch into `last`.
/// Both maintain `last` exactly as the real producer would. First asserts the patches are identical.
fn compare_diff_gen(label: &str, frames: &[Value]) {
	// Correctness: the candidate must produce byte-identical patches and never spuriously force.
	let mut last = frames[0].clone();
	for f in &frames[1..] {
		let reference = diff(&last, f);
		let (candidate, forced) = diff_serialize(&last, f);
		assert_eq!(reference, candidate, "{label}: candidate patch differs from reference");
		assert!(!forced, "{label}: candidate forced a snapshot unexpectedly");
		json_patch::merge(&mut last, &reference);
	}

	let reference = bench_quiet(|| {
		let mut last = frames[0].clone();
		for f in &frames[1..] {
			let new = serde_json::to_value(f).unwrap();
			black_box(diff(&last, &new));
			last = new;
		}
	});
	let candidate = bench_quiet(|| {
		let mut last = frames[0].clone();
		for f in &frames[1..] {
			let (patch, _forced) = diff_serialize(&last, f);
			json_patch::merge(&mut last, &patch);
			black_box(&patch);
		}
	});
	println!("  {label}");
	println!("    reference (to_value + diff)   {reference:>8.0} ns/tick");
	println!(
		"    candidate (diff_serialize)    {candidate:>8.0} ns/tick   ({:+.0}%)",
		100.0 * (candidate / reference - 1.0)
	);
}

/// Generous ratio + compression: every tick after the first lands as a compressed delta in one group.
fn merge_cfg() -> ProducerConfig {
	let mut config = ProducerConfig::default();
	config.delta_ratio = 1_000_000;
	config.compression = true;
	config
}

/// Time a closure that runs the whole TICKS stream once, averaged over ITERS runs. Returns ns/tick.
fn bench_quiet(mut run: impl FnMut()) -> f64 {
	for _ in 0..50 {
		run();
	}
	let start = Instant::now();
	for _ in 0..ITERS {
		run();
	}
	start.elapsed().as_nanos() as f64 / (ITERS as f64 * TICKS as f64)
}

fn bench(name: &str, run: impl FnMut()) -> f64 {
	let per_tick = bench_quiet(run);
	println!("  {name:<42} {per_tick:>8.1} ns/tick");
	per_tick
}

fn main() {
	let frames: Vec<Value> = (0..TICKS).map(telemetry).collect();
	let big: Vec<Value> = (0..TICKS).map(big_static).collect();

	compare("telemetry: small doc, many fields move", &frames);
	compare("big-static: large doc, few fields move", &big);

	// ---------------- Merge-patch generation: to_value+diff vs a diffing serializer ----------------
	let nested: Vec<Value> = (0..TICKS).map(big_nested).collect();
	println!("Merge-patch generation (no DEFLATE), reference vs serde diffing serializer:");
	compare_diff_gen("telemetry:   small doc, many fields move", &frames);
	compare_diff_gen("big-static:  large doc + big static array", &big);
	compare_diff_gen("big-nested:  large doc of scalars, few move", &nested);
	println!();

	// ---------------- DEFLATE level sweep: the dominant CPU lever ----------------
	println!("\nDEFLATE level sweep (merge-patch producer): size vs CPU");
	for level in [1u32, 3, 6, 9] {
		let wire: usize = merge_codec_frames(&frames, level).iter().map(|f| f.len()).sum();
		let ns = bench_quiet(|| {
			black_box(merge_codec_frames(&frames, level));
		});
		println!("  level {level}: {wire:>6} B   {ns:>8.1} ns/tick");
	}

	// ---------------- Real Producer/Consumer (includes moq-net plumbing + double serialize) ----
	println!("\nFull stack (real Producer/Consumer, level 6, includes moq-net plumbing):");
	bench("Producer::update loop", || {
		black_box(merge_producer_wire(&frames));
	});
	let real_stream = merge_producer_frames(&frames);
	bench("Consumer::poll_next loop", || {
		black_box(merge_consume(&real_stream));
	});

	// ---------------- Where the merge-patch CPU goes (producer side) ----------------
	println!("\nProducer breakdown (merge-patch path, per tick):");
	bench("serde_json::to_value (build Value tree)", || {
		for f in &frames {
			black_box(serde_json::to_value(f).unwrap());
		}
	});
	bench("serde_json::to_vec (serialize snapshot)", || {
		for f in &frames {
			black_box(serde_json::to_vec(f).unwrap());
		}
	});
	let values: Vec<Value> = frames.iter().map(|f| serde_json::to_value(f).unwrap()).collect();
	bench("diff (generate merge patch)", || {
		for w in values.windows(2) {
			black_box(diff(&w[0], &w[1]));
		}
	});
	let patches: Vec<Value> = values.windows(2).map(|w| diff(&w[0], &w[1])).collect();
	bench("serde_json::to_vec (serialize patch)", || {
		for p in &patches {
			black_box(serde_json::to_vec(p).unwrap());
		}
	});
	let patch_bytes: Vec<Vec<u8>> = patches.iter().map(|p| serde_json::to_vec(p).unwrap()).collect();
	bench("deflate frame (compress patch)", || {
		let mut enc = Encoder::new();
		enc.frame(&serde_json::to_vec(&frames[0]).unwrap());
		for p in &patch_bytes {
			black_box(enc.frame(p));
		}
	});
	bench("deflate frame (compress full snapshot)", || {
		let mut enc = Encoder::new();
		for f in &frames {
			black_box(enc.frame(&serde_json::to_vec(f).unwrap()));
		}
	});

	println!("\nConsumer breakdown (merge-patch path, per tick):");
	bench("json_patch::merge (apply patch)", || {
		let mut cur = values[0].clone();
		for p in &patches {
			json_patch::merge(&mut cur, p);
			black_box(&cur);
		}
	});
	bench("from_value clone (reconstruct T)", || {
		for v in &values {
			let _: Value = black_box(serde_json::from_value(v.clone()).unwrap());
		}
	});
}

/// Run the real Producer over the stream and return total wire bytes.
fn merge_producer_wire(frames: &[Value]) -> usize {
	let track = moq_net::Track::new("bench").produce();
	let consumer = track.consume();
	let mut producer = Producer::<Value>::new(track, merge_cfg());
	for f in frames {
		producer.update(f).unwrap();
	}
	producer.finish().unwrap();
	drain_wire(consumer)
}

/// Run the real Producer and capture the raw (compressed) frames, grouped, for the consumer bench.
fn merge_producer_frames(frames: &[Value]) -> Vec<Vec<Vec<u8>>> {
	let track = moq_net::Track::new("bench").produce();
	let consumer = track.consume();
	let mut producer = Producer::<Value>::new(track, merge_cfg());
	for f in frames {
		producer.update(f).unwrap();
	}
	producer.finish().unwrap();
	collect_groups(consumer)
}

/// Decode a captured merge-patch stream the way the real Consumer does.
fn merge_consume(stream: &[Vec<Vec<u8>>]) -> Value {
	let track = build_track(stream);
	let mut cc = ConsumerConfig::default();
	cc.compression = true;
	let mut consumer = moq_json::Consumer::<Value>::new(track, cc);
	let waiter = kio::Waiter::noop();
	let mut last = Value::Null;
	while let Poll::Ready(Ok(Some(v))) = consumer.poll_next(&waiter) {
		last = v;
	}
	last
}

/// Merge-patch producer, pure codec: frame 0 is a snapshot, the rest are compressed merge patches.
/// Mirrors the real `Inner` delta path without moq-net Track/Group plumbing.
fn merge_codec_frames(frames: &[Value], level: u32) -> Vec<Vec<u8>> {
	let mut enc = Encoder::with_level(level);
	let mut out = Vec::with_capacity(frames.len());
	out.push(enc.frame(&serde_json::to_vec(&frames[0]).unwrap()).to_vec());
	let mut last = serde_json::to_value(&frames[0]).unwrap();
	for f in &frames[1..] {
		let next = serde_json::to_value(f).unwrap();
		let patch = diff(&last, &next);
		let bytes = serde_json::to_vec(&patch).unwrap();
		out.push(enc.frame(&bytes).to_vec());
		last = next;
	}
	out
}

/// Merge-patch consumer, pure codec: decode each frame, applying snapshot then merge patches, and
/// reconstruct only the final value (backlog collapse).
fn merge_codec_consume(stream: &[Vec<u8>]) -> Value {
	let mut dec = Decoder::new();
	let mut cur = serde_json::from_slice(&dec.frame(&stream[0]).unwrap()).unwrap();
	for f in &stream[1..] {
		let patch: Value = serde_json::from_slice(&dec.frame(f).unwrap()).unwrap();
		json_patch::merge(&mut cur, &patch);
	}
	let v: Value = serde_json::from_value(cur).unwrap();
	v
}

/// Deflate-only producer: feed the full snapshot through one shared window each tick.
fn deflate_only_frames(frames: &[Value], level: u32) -> Vec<Vec<u8>> {
	let mut enc = Encoder::with_level(level);
	frames
		.iter()
		.map(|f| enc.frame(&serde_json::to_vec(f).unwrap()).to_vec())
		.collect()
}

fn deflate_only_wire(frames: &[Value], level: u32) -> usize {
	deflate_only_frames(frames, level).iter().map(|f| f.len()).sum()
}

/// Deflate-only consumer: decode each frame and parse the full snapshot.
fn deflate_only_consume(stream: &[Vec<u8>]) -> Value {
	let mut dec = Decoder::new();
	let mut last = Value::Null;
	for f in stream {
		let plain = dec.frame(f).unwrap();
		last = serde_json::from_slice(&plain).unwrap();
	}
	last
}

// ---- moq-net plumbing helpers ----

fn drain_wire(consumer: moq_net::TrackConsumer) -> usize {
	collect_groups(consumer).iter().flatten().map(|f| f.len()).sum()
}

/// Capture the stored frames preserving group boundaries: one inner `Vec` per group. Each group is
/// its own DEFLATE stream, so the boundaries must survive the round-trip or `build_track` would feed
/// a later group's frames into an earlier group's window and decode against the wrong dictionary.
fn collect_groups(consumer: moq_net::TrackConsumer) -> Vec<Vec<Vec<u8>>> {
	let waiter = kio::Waiter::noop();
	let mut out = Vec::new();
	let mut track = consumer;
	while let Poll::Ready(Ok(Some(mut group))) = track.poll_next_group(&waiter) {
		let mut frames = Vec::new();
		while let Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) {
			frames.push(frame.to_vec());
		}
		out.push(frames);
	}
	out
}

/// Replay captured groups onto a fresh track, one moq-net group per captured group.
fn build_track(groups: &[Vec<Vec<u8>>]) -> moq_net::TrackConsumer {
	let mut track = moq_net::Track::new("bench").produce();
	let consumer = track.consume();
	for frames in groups {
		let mut group = track.append_group().unwrap();
		for f in frames {
			group.write_frame(bytes::Bytes::from(f.clone())).unwrap();
		}
		group.finish().unwrap();
	}
	track.finish().unwrap();
	consumer
}
