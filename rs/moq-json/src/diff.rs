//! Generate an [RFC 7396](https://www.rfc-editor.org/rfc/rfc7396.html) JSON Merge Patch directly
//! from a value, diffing it against the previously published value as it is serialized.
//!
//! A serde [`Serializer`] walks the new value and compares each field against the corresponding
//! node of the old [`Value`], so unchanged scalars and subtrees cost only a comparison (no
//! allocation) and only changed nodes are built into the patch. This avoids materializing a full
//! `Value` tree for the new value just to diff two trees.

use std::cell::Cell;
use std::collections::HashSet;

use serde::Serialize;
use serde::ser::{Impossible, SerializeMap, SerializeSeq, SerializeStruct, Serializer};
use serde_json::{Map, Value};

/// The result of diffing a value into an RFC 7396 merge patch.
pub struct Diff {
	/// A merge patch that transforms the old value into the new one.
	pub patch: Value,

	/// Set when the change can't be faithfully expressed as a merge patch, so the caller should
	/// publish a full snapshot instead. This happens when a value is set to JSON null, which merge
	/// patch reads as a key deletion, or when the root is not an object. Arrays are fine: merge patch
	/// replaces them wholesale, which is still typically smaller than a full snapshot.
	pub forced_snapshot: bool,
}

/// Generate an RFC 7396 merge patch transforming `old` into `new`.
///
/// Only object roots produce a recursive patch; any other root forces a snapshot. A merge patch that
/// would delete a key it shouldn't (a value genuinely set to null) also forces a snapshot.
pub fn diff<T: Serialize>(old: &Value, new: &T) -> Diff {
	let forced = Cell::new(false);
	let node = new.serialize(Differ {
		baseline: old,
		forced: &forced,
	});

	match node {
		// No field differed: an empty patch. A null somewhere may still have forced a snapshot.
		Ok(Node::Same) => Diff {
			patch: Value::Object(Map::new()),
			forced_snapshot: forced.get(),
		},
		// A non-object patch (or non-object baseline) can't be a recursive merge patch, so force a
		// snapshot for non-object roots.
		Ok(Node::Diff(patch)) => {
			let non_object_root = !patch.is_object() || !old.is_object();
			Diff {
				patch,
				forced_snapshot: forced.get() || non_object_root,
			}
		}
		// A value that isn't representable as JSON (e.g. a non-string map key) can't be diffed. Fall
		// back to a snapshot; the caller's own serialization surfaces the real error if there is one.
		Err(_) => Diff {
			patch: Value::Object(Map::new()),
			forced_snapshot: true,
		},
	}
}

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
	forced: &'a Cell<bool>,
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
	fn scalar(self, value: Value) -> Result<Node, Error> {
		if self.baseline == &value {
			Ok(Node::Same)
		} else {
			// A genuine null can't be stored: merge patch would read it as a key deletion.
			if value.is_null() {
				self.forced.set(true);
			}
			Ok(Node::Diff(value))
		}
	}
}

/// Minimal serde error for the diffing serializer. JSON-shaped data never produces one in practice.
#[derive(Debug)]
struct Error(String);

impl std::fmt::Display for Error {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(&self.0)
	}
}

impl std::error::Error for Error {}

impl serde::ser::Error for Error {
	fn custom<M: std::fmt::Display>(msg: M) -> Self {
		Error(msg.to_string())
	}
}

/// Build a Value with no diffing (used for array elements, which merge patch replaces wholesale).
fn to_plain<T: Serialize + ?Sized>(value: &T) -> Result<Value, Error> {
	serde_json::to_value(value).map_err(|e| Error(e.to_string()))
}

impl<'a> Serializer for Differ<'a> {
	type Ok = Node;
	type Error = Error;
	type SerializeSeq = SeqDiff<'a>;
	type SerializeTuple = SeqDiff<'a>;
	type SerializeTupleStruct = SeqDiff<'a>;
	type SerializeTupleVariant = VariantSeq<'a>;
	type SerializeMap = MapDiff<'a>;
	type SerializeStruct = MapDiff<'a>;
	type SerializeStructVariant = VariantMap<'a>;

	fn serialize_bool(self, v: bool) -> Result<Node, Error> {
		self.scalar(Value::Bool(v))
	}
	fn serialize_i8(self, v: i8) -> Result<Node, Error> {
		self.scalar(Value::from(v))
	}
	fn serialize_i16(self, v: i16) -> Result<Node, Error> {
		self.scalar(Value::from(v))
	}
	fn serialize_i32(self, v: i32) -> Result<Node, Error> {
		self.scalar(Value::from(v))
	}
	fn serialize_i64(self, v: i64) -> Result<Node, Error> {
		self.scalar(Value::from(v))
	}
	fn serialize_i128(self, v: i128) -> Result<Node, Error> {
		self.scalar(to_plain(&v)?)
	}
	fn serialize_u8(self, v: u8) -> Result<Node, Error> {
		self.scalar(Value::from(v))
	}
	fn serialize_u16(self, v: u16) -> Result<Node, Error> {
		self.scalar(Value::from(v))
	}
	fn serialize_u32(self, v: u32) -> Result<Node, Error> {
		self.scalar(Value::from(v))
	}
	fn serialize_u64(self, v: u64) -> Result<Node, Error> {
		self.scalar(Value::from(v))
	}
	fn serialize_u128(self, v: u128) -> Result<Node, Error> {
		self.scalar(to_plain(&v)?)
	}
	fn serialize_f32(self, v: f32) -> Result<Node, Error> {
		self.scalar(Value::from(v))
	}
	fn serialize_f64(self, v: f64) -> Result<Node, Error> {
		self.scalar(Value::from(v))
	}
	fn serialize_char(self, v: char) -> Result<Node, Error> {
		self.scalar(Value::from(v.to_string()))
	}
	fn serialize_str(self, v: &str) -> Result<Node, Error> {
		// Strings are the common churn-free field, so compare against the baseline without allocating a
		// `Value::String` on the unchanged path.
		if matches!(self.baseline, Value::String(b) if b == v) {
			Ok(Node::Same)
		} else {
			Ok(Node::Diff(Value::from(v)))
		}
	}
	fn serialize_bytes(self, v: &[u8]) -> Result<Node, Error> {
		self.scalar(to_plain(v)?)
	}
	fn serialize_none(self) -> Result<Node, Error> {
		self.scalar(Value::Null)
	}
	fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<Node, Error> {
		value.serialize(self)
	}
	fn serialize_unit(self) -> Result<Node, Error> {
		self.scalar(Value::Null)
	}
	fn serialize_unit_struct(self, _name: &'static str) -> Result<Node, Error> {
		self.scalar(Value::Null)
	}
	fn serialize_unit_variant(self, _name: &'static str, _idx: u32, variant: &'static str) -> Result<Node, Error> {
		self.scalar(Value::from(variant))
	}
	fn serialize_newtype_struct<T: Serialize + ?Sized>(self, _name: &'static str, value: &T) -> Result<Node, Error> {
		value.serialize(self)
	}
	fn serialize_newtype_variant<T: Serialize + ?Sized>(
		self,
		_name: &'static str,
		_idx: u32,
		variant: &'static str,
		value: &T,
	) -> Result<Node, Error> {
		// An externally-tagged newtype variant serializes as `{ "Variant": value }`. Diff that object
		// against the baseline like any other object, so the tag is preserved and the payload diffs
		// minimally (a variant switch deletes the old tag and adds the new one).
		variant_object(variant, to_plain(value)?).serialize(self)
	}
	fn serialize_seq(self, len: Option<usize>) -> Result<SeqDiff<'a>, Error> {
		Ok(SeqDiff {
			differ: self,
			items: Vec::with_capacity(len.unwrap_or(0)),
		})
	}
	fn serialize_tuple(self, len: usize) -> Result<SeqDiff<'a>, Error> {
		self.serialize_seq(Some(len))
	}
	fn serialize_tuple_struct(self, _name: &'static str, len: usize) -> Result<SeqDiff<'a>, Error> {
		self.serialize_seq(Some(len))
	}
	fn serialize_tuple_variant(
		self,
		_name: &'static str,
		_idx: u32,
		variant: &'static str,
		len: usize,
	) -> Result<VariantSeq<'a>, Error> {
		// A tuple variant serializes as `{ "Variant": [..] }`, replaced wholesale.
		Ok(VariantSeq {
			differ: self,
			variant,
			items: Vec::with_capacity(len),
		})
	}
	fn serialize_map(self, _len: Option<usize>) -> Result<MapDiff<'a>, Error> {
		Ok(MapDiff {
			differ: self,
			patch: Map::new(),
			seen: Vec::new(),
			added_key: false,
			pending_key: None,
		})
	}
	fn serialize_struct(self, _name: &'static str, len: usize) -> Result<MapDiff<'a>, Error> {
		self.serialize_map(Some(len))
	}
	fn serialize_struct_variant(
		self,
		_name: &'static str,
		_idx: u32,
		variant: &'static str,
		_len: usize,
	) -> Result<VariantMap<'a>, Error> {
		// A struct variant serializes as `{ "Variant": { .. } }`, replaced wholesale.
		Ok(VariantMap {
			differ: self,
			variant,
			fields: Map::new(),
		})
	}
}

/// Wrap a value as an externally-tagged variant object `{ variant: value }`.
fn variant_object(variant: &str, value: Value) -> Value {
	let mut object = Map::new();
	object.insert(variant.to_owned(), value);
	Value::Object(object)
}

/// Collects a tuple variant's fields into `{ variant: [..] }`, then diffs it against the baseline.
struct VariantSeq<'a> {
	differ: Differ<'a>,
	variant: &'static str,
	items: Vec<Value>,
}

impl serde::ser::SerializeTupleVariant for VariantSeq<'_> {
	type Ok = Node;
	type Error = Error;
	fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
		self.items.push(to_plain(value)?);
		Ok(())
	}
	fn end(self) -> Result<Node, Error> {
		variant_object(self.variant, Value::Array(self.items)).serialize(self.differ)
	}
}

/// Collects a struct variant's fields into `{ variant: { .. } }`, then diffs it against the baseline.
struct VariantMap<'a> {
	differ: Differ<'a>,
	variant: &'static str,
	fields: Map<String, Value>,
}

impl serde::ser::SerializeStructVariant for VariantMap<'_> {
	type Ok = Node;
	type Error = Error;
	fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, value: &T) -> Result<(), Error> {
		self.fields.insert(key.to_owned(), to_plain(value)?);
		Ok(())
	}
	fn end(self) -> Result<Node, Error> {
		variant_object(self.variant, Value::Object(self.fields)).serialize(self.differ)
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
	type Error = Error;
	fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
		self.items.push(to_plain(value)?);
		Ok(())
	}
	fn end(self) -> Result<Node, Error> {
		self.differ.scalar(Value::Array(self.items))
	}
}

impl serde::ser::SerializeTuple for SeqDiff<'_> {
	type Ok = Node;
	type Error = Error;
	fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
		SerializeSeq::serialize_element(self, value)
	}
	fn end(self) -> Result<Node, Error> {
		SerializeSeq::end(self)
	}
}

impl serde::ser::SerializeTupleStruct for SeqDiff<'_> {
	type Ok = Node;
	type Error = Error;
	fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
		SerializeSeq::serialize_element(self, value)
	}
	fn end(self) -> Result<Node, Error> {
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

	fn finish(self) -> Result<Node, Error> {
		let mut patch = self.patch;
		if let Value::Object(base) = self.differ.baseline {
			// A deletion is only possible if some key was added or the counts differ. Otherwise the new
			// keys are exactly the baseline keys, so there's nothing to delete and we skip the scan,
			// keeping the common path O(1) rather than O(n^2). A removed key is a clean delete (explicit
			// null), and unlike a value set to null it does not force a snapshot.
			if self.added_key || self.seen.len() != base.len() {
				let seen: HashSet<&str> = self.seen.iter().map(String::as_str).collect();
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
	type Error = Error;
	fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Error> {
		// Extract the key string in a single allocation (no intermediate Value).
		self.pending_key = Some(key.serialize(KeySer)?);
		Ok(())
	}
	fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
		let key = self.pending_key.take().expect("serialize_key precedes serialize_value");
		let (child, existed) = self.differ.child(&key);
		let node = value.serialize(child)?;
		self.entry(key, existed, node);
		Ok(())
	}
	fn end(self) -> Result<Node, Error> {
		self.finish()
	}
}

impl SerializeStruct for MapDiff<'_> {
	type Ok = Node;
	type Error = Error;
	fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, value: &T) -> Result<(), Error> {
		let (child, existed) = self.differ.child(key);
		let node = value.serialize(child)?;
		self.entry(key.to_owned(), existed, node);
		Ok(())
	}
	// A field skipped via `skip_serializing_if` is simply never offered here, so it stays out of `seen`
	// and `finish` emits it as a null deletion if the baseline had it (the default `skip_field` suffices).
	fn end(self) -> Result<Node, Error> {
		self.finish()
	}
}

/// Serializes a map key to its `String`, the only form JSON object keys take. Anything else is an
/// error, mirroring `serde_json`'s own key handling.
struct KeySer;

impl Serializer for KeySer {
	type Ok = String;
	type Error = Error;
	type SerializeSeq = Impossible<String, Error>;
	type SerializeTuple = Impossible<String, Error>;
	type SerializeTupleStruct = Impossible<String, Error>;
	type SerializeTupleVariant = Impossible<String, Error>;
	type SerializeMap = Impossible<String, Error>;
	type SerializeStruct = Impossible<String, Error>;
	type SerializeStructVariant = Impossible<String, Error>;

	fn serialize_str(self, v: &str) -> Result<String, Error> {
		Ok(v.to_owned())
	}
	fn serialize_char(self, v: char) -> Result<String, Error> {
		Ok(v.to_string())
	}
	fn serialize_bool(self, v: bool) -> Result<String, Error> {
		Ok(v.to_string())
	}
	fn serialize_i8(self, v: i8) -> Result<String, Error> {
		Ok(v.to_string())
	}
	fn serialize_i16(self, v: i16) -> Result<String, Error> {
		Ok(v.to_string())
	}
	fn serialize_i32(self, v: i32) -> Result<String, Error> {
		Ok(v.to_string())
	}
	fn serialize_i64(self, v: i64) -> Result<String, Error> {
		Ok(v.to_string())
	}
	fn serialize_u8(self, v: u8) -> Result<String, Error> {
		Ok(v.to_string())
	}
	fn serialize_u16(self, v: u16) -> Result<String, Error> {
		Ok(v.to_string())
	}
	fn serialize_u32(self, v: u32) -> Result<String, Error> {
		Ok(v.to_string())
	}
	fn serialize_u64(self, v: u64) -> Result<String, Error> {
		Ok(v.to_string())
	}
	fn serialize_unit_variant(self, _name: &'static str, _idx: u32, variant: &'static str) -> Result<String, Error> {
		Ok(variant.to_owned())
	}
	fn serialize_newtype_struct<T: Serialize + ?Sized>(self, _name: &'static str, value: &T) -> Result<String, Error> {
		value.serialize(self)
	}
	fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<String, Error> {
		value.serialize(self)
	}
	fn serialize_f32(self, _v: f32) -> Result<String, Error> {
		Err(Error("float map key".into()))
	}
	fn serialize_f64(self, _v: f64) -> Result<String, Error> {
		Err(Error("float map key".into()))
	}
	fn serialize_bytes(self, _v: &[u8]) -> Result<String, Error> {
		Err(Error("bytes map key".into()))
	}
	fn serialize_none(self) -> Result<String, Error> {
		Err(Error("null map key".into()))
	}
	fn serialize_unit(self) -> Result<String, Error> {
		Err(Error("unit map key".into()))
	}
	fn serialize_unit_struct(self, _name: &'static str) -> Result<String, Error> {
		Err(Error("unit struct map key".into()))
	}
	fn serialize_newtype_variant<T: Serialize + ?Sized>(
		self,
		_name: &'static str,
		_idx: u32,
		_variant: &'static str,
		_value: &T,
	) -> Result<String, Error> {
		Err(Error("newtype variant map key".into()))
	}
	fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq, Error> {
		Err(Error("seq map key".into()))
	}
	fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple, Error> {
		Err(Error("tuple map key".into()))
	}
	fn serialize_tuple_struct(self, _name: &'static str, _len: usize) -> Result<Self::SerializeTupleStruct, Error> {
		Err(Error("tuple struct map key".into()))
	}
	fn serialize_tuple_variant(
		self,
		_name: &'static str,
		_idx: u32,
		_variant: &'static str,
		_len: usize,
	) -> Result<Self::SerializeTupleVariant, Error> {
		Err(Error("tuple variant map key".into()))
	}
	fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, Error> {
		Err(Error("map map key".into()))
	}
	fn serialize_struct(self, _name: &'static str, _len: usize) -> Result<Self::SerializeStruct, Error> {
		Err(Error("struct map key".into()))
	}
	fn serialize_struct_variant(
		self,
		_name: &'static str,
		_idx: u32,
		_variant: &'static str,
		_len: usize,
	) -> Result<Self::SerializeStructVariant, Error> {
		Err(Error("struct variant map key".into()))
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use serde_json::json;

	/// A straightforward Value-vs-Value merge-patch diff, used only as a test oracle: the production
	/// `diff` (the serializer) must agree with it on every case.
	fn reference(old: &Value, new: &Value) -> Diff {
		fn objects(
			old: &Map<String, Value>,
			new: &Map<String, Value>,
			patch: &mut Map<String, Value>,
			forced: &mut bool,
		) {
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
					objects(old_obj, new_obj, &mut sub, forced);
					if !sub.is_empty() {
						patch.insert(key.clone(), Value::Object(sub));
					}
					continue;
				}
				if new_val.is_null() {
					*forced = true;
				}
				patch.insert(key.clone(), new_val.clone());
			}
		}

		if let (Value::Object(old_obj), Value::Object(new_obj)) = (old, new) {
			let mut patch = Map::new();
			let mut forced = false;
			objects(old_obj, new_obj, &mut patch, &mut forced);
			Diff {
				patch: Value::Object(patch),
				forced_snapshot: forced,
			}
		} else {
			Diff {
				patch: new.clone(),
				forced_snapshot: true,
			}
		}
	}

	/// The serializer must produce the same patch and forced flag as the reference oracle, and (when
	/// not forced) applying the patch to `old` must reproduce `new`.
	fn check(old: Value, new: Value) {
		let want = reference(&old, &new);
		let got = diff(&old, &new);
		assert_eq!(got.patch, want.patch, "patch mismatch for {old} -> {new}");
		assert_eq!(
			got.forced_snapshot, want.forced_snapshot,
			"forced mismatch for {old} -> {new}"
		);
		if !got.forced_snapshot {
			let mut applied = old.clone();
			json_patch::merge(&mut applied, &got.patch);
			assert_eq!(applied, new, "patch did not roundtrip for {old} -> {new}");
		}
	}

	#[test]
	fn changed_scalar() {
		check(json!({ "a": 1, "b": 2 }), json!({ "a": 1, "b": 3 }));
	}

	#[test]
	fn added_key() {
		let result = diff(&json!({ "a": 1 }), &json!({ "a": 1, "b": 2 }));
		assert!(!result.forced_snapshot);
		assert_eq!(result.patch, json!({ "b": 2 }));
		check(json!({ "a": 1 }), json!({ "a": 1, "b": 2 }));
	}

	#[test]
	fn removed_key_is_null() {
		let result = diff(&json!({ "a": 1, "b": 2 }), &json!({ "a": 1 }));
		assert!(!result.forced_snapshot, "removing a key is a clean delete");
		assert_eq!(result.patch, json!({ "b": null }));
		check(json!({ "a": 1, "b": 2 }), json!({ "a": 1 }));
	}

	#[test]
	fn nested_object_only_includes_changed_keys() {
		let result = diff(&json!({ "o": { "x": 1, "y": 2 } }), &json!({ "o": { "x": 1, "y": 9 } }));
		assert!(!result.forced_snapshot);
		assert_eq!(result.patch, json!({ "o": { "y": 9 } }));
		check(json!({ "o": { "x": 1, "y": 2 } }), json!({ "o": { "x": 1, "y": 9 } }));
	}

	#[test]
	fn unchanged_object_is_empty_patch() {
		let result = diff(&json!({ "a": 1, "o": { "x": 1 } }), &json!({ "a": 1, "o": { "x": 1 } }));
		assert!(!result.forced_snapshot);
		assert_eq!(result.patch, json!({}));
	}

	#[test]
	fn changed_array_is_wholesale_delta() {
		let result = diff(&json!({ "a": [1, 2] }), &json!({ "a": [1, 2, 3] }));
		assert!(!result.forced_snapshot);
		assert_eq!(result.patch, json!({ "a": [1, 2, 3] }));
		check(json!({ "a": [1, 2] }), json!({ "a": [1, 2, 3] }));
	}

	#[test]
	fn unchanged_array_is_pruned() {
		let result = diff(&json!({ "a": [1, 2, 3], "b": 1 }), &json!({ "a": [1, 2, 3], "b": 2 }));
		assert_eq!(
			result.patch,
			json!({ "b": 2 }),
			"an unchanged array stays out of the patch"
		);
	}

	#[test]
	fn added_array_is_delta() {
		check(json!({ "a": 1 }), json!({ "a": 1, "b": [1] }));
	}

	#[test]
	fn nested_array_is_delta() {
		check(json!({ "o": { "x": 1 } }), json!({ "o": { "x": 1, "list": [1] } }));
	}

	#[test]
	fn array_of_objects_replaces_wholesale() {
		check(
			json!({ "items": [{ "id": 1, "v": 1 }, { "id": 2, "v": 2 }] }),
			json!({ "items": [{ "id": 1, "v": 9 }, { "id": 2, "v": 2 }] }),
		);
	}

	#[test]
	fn set_to_null_forces_snapshot() {
		// A genuine null value can't be represented: merge patch would delete the key.
		let result = diff(&json!({ "a": 1 }), &json!({ "a": null }));
		assert!(result.forced_snapshot);
		assert!(reference(&json!({ "a": 1 }), &json!({ "a": null })).forced_snapshot);
	}

	#[test]
	fn nested_null_forces_snapshot() {
		let old = json!({ "o": { "x": 1 } });
		let new = json!({ "o": { "x": null } });
		assert!(diff(&old, &new).forced_snapshot);
		assert_eq!(diff(&old, &new).forced_snapshot, reference(&old, &new).forced_snapshot);
	}

	#[test]
	fn replacing_object_with_scalar() {
		check(json!({ "a": { "x": 1 } }), json!({ "a": 5 }));
	}

	#[test]
	fn replacing_scalar_with_object() {
		check(json!({ "a": 5 }), json!({ "a": { "x": 1 } }));
	}

	#[test]
	fn non_object_root_forces_snapshot() {
		let result = diff(&json!(1), &json!(2));
		assert!(result.forced_snapshot);
		assert_eq!(result.patch, json!(2));
	}

	#[test]
	fn array_root_forces_snapshot() {
		let result = diff(&json!([1, 2]), &json!([1, 2, 3]));
		assert!(result.forced_snapshot);
		assert_eq!(result.patch, json!([1, 2, 3]));
	}

	#[test]
	fn unchanged_scalar_root_is_not_forced() {
		// An equal non-object root is a no-op (empty patch), matching the producer's dedup.
		let result = diff(&json!(7), &json!(7));
		assert!(!result.forced_snapshot);
		assert_eq!(result.patch, json!({}));
	}

	#[test]
	fn floats_and_bools_and_strings() {
		check(
			json!({ "f": 1.5, "b": true, "s": "hi" }),
			json!({ "f": 2.5, "b": false, "s": "bye" }),
		);
	}

	// ---- Typed structs (the serializer's whole point: diff `T` without building its Value) ----

	#[derive(serde::Serialize, serde::Deserialize, Default, PartialEq, Debug)]
	struct Doc {
		#[serde(skip_serializing_if = "Option::is_none")]
		video: Option<String>,
		#[serde(skip_serializing_if = "Option::is_none")]
		scte35: Option<u32>,
		count: u64,
		tags: Vec<String>,
	}

	/// Diff a typed struct against the Value of a prior typed struct; the patch must roundtrip.
	fn check_struct(old: &Doc, new: &Doc) {
		let old_value = serde_json::to_value(old).unwrap();
		let result = diff(&old_value, new);

		// Cross-check against the oracle fed the equivalent Values.
		let new_value = serde_json::to_value(new).unwrap();
		let want = reference(&old_value, &new_value);
		assert_eq!(result.patch, want.patch, "struct patch differs from oracle");
		assert_eq!(result.forced_snapshot, want.forced_snapshot);

		if !result.forced_snapshot {
			let mut applied = old_value;
			json_patch::merge(&mut applied, &result.patch);
			assert_eq!(applied, new_value, "struct patch did not roundtrip");
		}
	}

	#[test]
	fn struct_field_change() {
		check_struct(
			&Doc {
				count: 1,
				tags: vec!["a".into()],
				..Default::default()
			},
			&Doc {
				count: 2,
				tags: vec!["a".into()],
				..Default::default()
			},
		);
	}

	#[test]
	fn struct_option_some_to_none_is_deletion() {
		// A skipped field (None) must become a null deletion of the previously-present key.
		let result = diff(
			&serde_json::to_value(Doc {
				video: Some("v1".into()),
				count: 1,
				..Default::default()
			})
			.unwrap(),
			&Doc {
				video: None,
				count: 1,
				..Default::default()
			},
		);
		assert!(!result.forced_snapshot, "deleting a skipped key is clean");
		assert_eq!(result.patch, json!({ "video": null }));
		check_struct(
			&Doc {
				video: Some("v1".into()),
				count: 1,
				..Default::default()
			},
			&Doc {
				video: None,
				count: 1,
				..Default::default()
			},
		);
	}

	#[test]
	fn struct_option_none_to_some_is_addition() {
		check_struct(
			&Doc {
				count: 1,
				..Default::default()
			},
			&Doc {
				scte35: Some(42),
				count: 1,
				..Default::default()
			},
		);
	}

	#[test]
	fn struct_unchanged_is_empty_patch() {
		let doc = Doc {
			video: Some("v".into()),
			scte35: Some(1),
			count: 7,
			tags: vec!["x".into(), "y".into()],
		};
		let result = diff(&serde_json::to_value(&doc).unwrap(), &doc);
		assert!(!result.forced_snapshot);
		assert_eq!(result.patch, json!({}));
	}

	#[test]
	fn struct_vec_changes_wholesale() {
		check_struct(
			&Doc {
				count: 1,
				tags: vec!["a".into(), "b".into()],
				..Default::default()
			},
			&Doc {
				count: 1,
				tags: vec!["a".into(), "c".into()],
				..Default::default()
			},
		);
	}

	#[derive(serde::Serialize)]
	struct Nested {
		inner: Inner,
		name: String,
	}
	#[derive(serde::Serialize)]
	struct Inner {
		a: u32,
		b: u32,
	}

	#[test]
	fn nested_struct_only_changed_field() {
		let old = serde_json::to_value(Nested {
			inner: Inner { a: 1, b: 2 },
			name: "n".into(),
		})
		.unwrap();
		let new = Nested {
			inner: Inner { a: 1, b: 9 },
			name: "n".into(),
		};
		let result = diff(&old, &new);
		assert_eq!(result.patch, json!({ "inner": { "b": 9 } }));
		assert!(!result.forced_snapshot);
	}

	#[derive(serde::Serialize)]
	enum Tag {
		Active,
		Idle,
	}

	#[derive(serde::Serialize)]
	struct Stated {
		state: Tag,
		seq: u32,
	}

	#[test]
	fn unit_enum_variant_is_string() {
		// Externally-tagged unit variants serialize as the variant name string.
		let old = serde_json::to_value(Stated {
			state: Tag::Active,
			seq: 1,
		})
		.unwrap();
		assert_eq!(old, json!({ "state": "Active", "seq": 1 }));
		let result = diff(
			&old,
			&Stated {
				state: Tag::Idle,
				seq: 1,
			},
		);
		assert!(!result.forced_snapshot);
		assert_eq!(result.patch, json!({ "state": "Idle" }));
	}

	/// Diff a typed value against the Value of a prior value: the patch must match the oracle fed the
	/// equivalent Values, and roundtrip.
	fn check_typed<T: Serialize>(old: &Value, new: &T) {
		let new_value = serde_json::to_value(new).unwrap();
		let want = reference(old, &new_value);
		let got = diff(old, new);
		assert_eq!(got.patch, want.patch, "patch differs from oracle");
		assert_eq!(got.forced_snapshot, want.forced_snapshot, "forced differs from oracle");
		if !got.forced_snapshot {
			let mut applied = old.clone();
			json_patch::merge(&mut applied, &got.patch);
			assert_eq!(applied, new_value, "patch did not roundtrip");
		}
	}

	#[derive(serde::Serialize)]
	enum Payload {
		Newtype(u32),
		Tuple(u32, String),
		Struct { x: u32, y: u32 },
	}

	#[derive(serde::Serialize)]
	struct Holder {
		payload: Payload,
		seq: u32,
	}

	#[test]
	fn newtype_variant_keeps_its_tag() {
		// Regression: a newtype variant must serialize as `{ "Newtype": v }`, not collapse to `v`.
		let old = serde_json::to_value(Holder {
			payload: Payload::Newtype(1),
			seq: 0,
		})
		.unwrap();
		assert_eq!(old, json!({ "payload": { "Newtype": 1 }, "seq": 0 }));
		let result = diff(
			&old,
			&Holder {
				payload: Payload::Newtype(2),
				seq: 0,
			},
		);
		assert_eq!(result.patch, json!({ "payload": { "Newtype": 2 } }));
		check_typed(
			&old,
			&Holder {
				payload: Payload::Newtype(2),
				seq: 0,
			},
		);
	}

	#[test]
	fn tuple_variant_keeps_its_tag() {
		let old = serde_json::to_value(Holder {
			payload: Payload::Tuple(1, "a".into()),
			seq: 0,
		})
		.unwrap();
		assert_eq!(old, json!({ "payload": { "Tuple": [1, "a"] }, "seq": 0 }));
		check_typed(
			&old,
			&Holder {
				payload: Payload::Tuple(2, "a".into()),
				seq: 0,
			},
		);
	}

	#[test]
	fn struct_variant_keeps_its_tag() {
		let old = serde_json::to_value(Holder {
			payload: Payload::Struct { x: 1, y: 2 },
			seq: 0,
		})
		.unwrap();
		assert_eq!(old, json!({ "payload": { "Struct": { "x": 1, "y": 2 } }, "seq": 0 }));
		check_typed(
			&old,
			&Holder {
				payload: Payload::Struct { x: 1, y: 9 },
				seq: 0,
			},
		);
	}

	#[derive(serde::Deserialize)]
	struct Vector {
		name: String,
		old: Value,
		new: Value,
		forced: bool,
		patch: Option<Value>,
	}

	/// Shared cross-impl fixture: the TS suite (js/json) asserts the same vectors so both
	/// implementations agree on every snapshot/delta decision and patch shape.
	#[test]
	fn golden_vectors() {
		let vectors: Vec<Vector> = serde_json::from_str(include_str!("../tests/vectors.json")).unwrap();
		for case in vectors {
			let result = diff(&case.old, &case.new);
			assert_eq!(result.forced_snapshot, case.forced, "{}: forced_snapshot", case.name);

			if let Some(expected) = case.patch {
				assert_eq!(result.patch, expected, "{}: patch", case.name);
				let mut applied = case.old.clone();
				json_patch::merge(&mut applied, &result.patch);
				assert_eq!(applied, case.new, "{}: roundtrip", case.name);
			}
		}
	}

	/// Exercise the diff over a sequence of evolving documents, asserting agreement with the oracle
	/// and full roundtrip at every step (the way the producer applies deltas).
	#[test]
	fn evolving_document_matches_oracle() {
		let mut docs = Vec::new();
		for tick in 0u64..40 {
			docs.push(json!({
				"id": "device-1",
				"static": { "model": "x", "tags": ["a", "b", "c"] },
				"counters": { "n": tick, "errors": tick / 10 },
				"reading": (tick as f64 * 0.5),
				"flags": { "online": tick % 2 == 0, "charging": tick % 3 == 0 },
				"list": [tick, tick + 1],
			}));
		}
		for pair in docs.windows(2) {
			check(pair[0].clone(), pair[1].clone());
		}
	}
}
