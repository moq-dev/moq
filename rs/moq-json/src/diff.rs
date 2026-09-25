//! Generate an [RFC 7396](https://www.rfc-editor.org/rfc/rfc7396.html) JSON Merge Patch directly
//! from a value, diffing it against the previously published value as it is serialized.
//!
//! A serde [`Serializer`] walks the new value and compares each field against the corresponding
//! node of the old [`Value`], so unchanged scalars and subtrees cost only a comparison (no
//! allocation) and only changed fields are written into reusable patch buffers. This avoids
//! materializing a `Value` tree for either the new value or the patch on the encoding path.

use std::cell::{Cell, RefCell};

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
	let result = bytes(old, new, &RefCell::new(Scratch::default())).unwrap_or(PatchBytes {
		patch: Vec::new(),
		forced_snapshot: true,
	});
	Diff {
		patch: if result.patch.is_empty() {
			Value::Object(Map::new())
		} else {
			serde_json::from_slice(&result.patch).expect("the diff serializer emits JSON")
		},
		forced_snapshot: result.forced_snapshot,
	}
}

#[derive(Default)]
pub(crate) struct Scratch {
	seen: Vec<Vec<String>>,
	pending: Vec<String>,
	bytes: Vec<Vec<u8>>,
	last_capacity: usize,
}

impl Scratch {
	fn buffer(&mut self, depth: usize) -> &mut Vec<u8> {
		if self.bytes.len() <= depth {
			self.bytes.resize_with(depth + 1, Vec::new);
		}
		&mut self.bytes[depth]
	}
}

pub(crate) struct PatchBytes {
	pub patch: Vec<u8>,
	pub forced_snapshot: bool,
}

/// Diff directly into JSON bytes, reusing child buffers across updates.
pub(crate) fn bytes<T: Serialize>(old: &Value, new: &T, scratch: &RefCell<Scratch>) -> Result<PatchBytes, String> {
	let forced = Cell::new(false);
	let node = new.serialize(Differ {
		baseline: old,
		present: true,
		forced: &forced,
		scratch,
		depth: 0,
	});
	match node {
		Ok(Node::Same) => Ok(PatchBytes {
			patch: Vec::new(),
			forced_snapshot: forced.get(),
		}),
		Ok(Node::Diff) => {
			let mut scratch = scratch.borrow_mut();
			let patch = std::mem::take(scratch.buffer(0));
			scratch.last_capacity = patch.capacity();
			let forced_snapshot = forced.get() || !patch.starts_with(b"{") || !old.is_object();
			Ok(PatchBytes { patch, forced_snapshot })
		}
		Err(err) => Err(err.0),
	}
}

/// One node's verdict from the diffing serializer.
enum Node {
	/// Equal to the baseline; nothing to emit.
	Same,
	/// Differs; the patch bytes are in this node's scratch buffer.
	Diff,
}

const NULL: Value = Value::Null;

/// Serializer that diffs `T` against `baseline` and yields a merge patch. `forced` is set if a
/// genuine null is emitted (merge patch can't represent it, so the caller must snapshot).
#[derive(Copy, Clone)]
struct Differ<'a> {
	baseline: &'a Value,
	present: bool,
	forced: &'a Cell<bool>,
	scratch: &'a RefCell<Scratch>,
	depth: usize,
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
				present: existed,
				forced: self.forced,
				scratch: self.scratch,
				depth: self.depth + 1,
			},
			existed,
		)
	}

	/// Serialize a changed scalar into its reusable buffer.
	fn scalar<T: Serialize + ?Sized>(self, value: &T, equal: bool, null: bool) -> Result<Node, Error> {
		if equal {
			return Ok(Node::Same);
		}
		if null {
			self.forced.set(true);
		}
		let mut scratch = self.scratch.borrow_mut();
		let capacity = scratch.last_capacity;
		let bytes = scratch.buffer(self.depth);
		bytes.clear();
		if self.depth == 0 {
			bytes.reserve(capacity);
		}
		serde_json::to_writer(bytes, value).map_err(|err| Error(err.to_string()))?;
		Ok(Node::Diff)
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
		self.scalar(&v, self.baseline == &Value::Bool(v), false)
	}
	fn serialize_i8(self, v: i8) -> Result<Node, Error> {
		self.scalar(&v, self.baseline == &Value::from(v), false)
	}
	fn serialize_i16(self, v: i16) -> Result<Node, Error> {
		self.scalar(&v, self.baseline == &Value::from(v), false)
	}
	fn serialize_i32(self, v: i32) -> Result<Node, Error> {
		self.scalar(&v, self.baseline == &Value::from(v), false)
	}
	fn serialize_i64(self, v: i64) -> Result<Node, Error> {
		self.scalar(&v, self.baseline == &Value::from(v), false)
	}
	fn serialize_i128(self, v: i128) -> Result<Node, Error> {
		{
			let plain = to_plain(&v)?;
			self.scalar(&plain, self.baseline == &plain, false)
		}
	}
	fn serialize_u8(self, v: u8) -> Result<Node, Error> {
		self.scalar(&v, self.baseline == &Value::from(v), false)
	}
	fn serialize_u16(self, v: u16) -> Result<Node, Error> {
		self.scalar(&v, self.baseline == &Value::from(v), false)
	}
	fn serialize_u32(self, v: u32) -> Result<Node, Error> {
		self.scalar(&v, self.baseline == &Value::from(v), false)
	}
	fn serialize_u64(self, v: u64) -> Result<Node, Error> {
		self.scalar(&v, self.baseline == &Value::from(v), false)
	}
	fn serialize_u128(self, v: u128) -> Result<Node, Error> {
		{
			let plain = to_plain(&v)?;
			self.scalar(&plain, self.baseline == &plain, false)
		}
	}
	fn serialize_f32(self, v: f32) -> Result<Node, Error> {
		self.scalar(&v, self.baseline == &Value::from(v), false)
	}
	fn serialize_f64(self, v: f64) -> Result<Node, Error> {
		self.scalar(&v, self.baseline == &Value::from(v), false)
	}
	fn serialize_char(self, v: char) -> Result<Node, Error> {
		let mut utf8 = [0; 4];
		self.scalar(&v, self.baseline.as_str() == Some(v.encode_utf8(&mut utf8)), false)
	}
	fn serialize_str(self, v: &str) -> Result<Node, Error> {
		// Strings are the common churn-free field, so compare against the baseline without allocating a
		// `Value::String` on the unchanged path.
		if matches!(self.baseline, Value::String(b) if b == v) {
			Ok(Node::Same)
		} else {
			self.scalar(&v, false, false)
		}
	}
	fn serialize_bytes(self, v: &[u8]) -> Result<Node, Error> {
		let mut seq = self.serialize_seq(Some(v.len()))?;
		for byte in v {
			SerializeSeq::serialize_element(&mut seq, byte)?;
		}
		SerializeSeq::end(seq)
	}
	fn serialize_none(self) -> Result<Node, Error> {
		self.scalar(&Value::Null, self.present && self.baseline.is_null(), true)
	}
	fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<Node, Error> {
		value.serialize(self)
	}
	fn serialize_unit(self) -> Result<Node, Error> {
		self.scalar(&Value::Null, self.present && self.baseline.is_null(), true)
	}
	fn serialize_unit_struct(self, _name: &'static str) -> Result<Node, Error> {
		self.scalar(&Value::Null, self.present && self.baseline.is_null(), true)
	}
	fn serialize_unit_variant(self, _name: &'static str, _idx: u32, variant: &'static str) -> Result<Node, Error> {
		self.scalar(&variant, self.baseline.as_str() == Some(variant), false)
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
		Variant { name: variant, value }.serialize(self)
	}
	fn serialize_seq(self, len: Option<usize>) -> Result<SeqDiff<'a>, Error> {
		let _ = len;
		self.scratch.borrow_mut().buffer(self.depth).clear();
		Ok(SeqDiff {
			differ: self,
			changed: false,
			len: 0,
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
			items: self.child(variant).0.serialize_seq(Some(len))?,
		})
	}
	fn serialize_map(self, _len: Option<usize>) -> Result<MapDiff<'a>, Error> {
		self.scratch.borrow_mut().buffer(self.depth).clear();
		Ok(MapDiff {
			differ: self,
			entries: 0,
			seen_len: 0,
			ordered: true,
			added_key: false,
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
			fields: self.child(variant).0.serialize_map(Some(_len))?,
		})
	}
}

/// Serialize an externally tagged newtype without materializing its payload.
struct Variant<'a, T: ?Sized> {
	name: &'static str,
	value: &'a T,
}

impl<T: Serialize + ?Sized> Serialize for Variant<'_, T> {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		let mut map = serializer.serialize_map(Some(1))?;
		map.serialize_entry(self.name, self.value)?;
		map.end()
	}
}

/// Finish an externally tagged variant from the already serialized child patch.
fn finish_variant(differ: Differ<'_>, variant: &'static str, node: Node) -> Result<Node, Error> {
	let (_, existed) = differ.child(variant);
	let mut outer = differ.serialize_map(Some(1))?;
	outer.entry(variant, existed, node)?;
	SerializeMap::end(outer)
}

struct VariantSeq<'a> {
	differ: Differ<'a>,
	variant: &'static str,
	items: SeqDiff<'a>,
}

impl serde::ser::SerializeTupleVariant for VariantSeq<'_> {
	type Ok = Node;
	type Error = Error;
	fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
		SerializeSeq::serialize_element(&mut self.items, value)
	}
	fn end(self) -> Result<Node, Error> {
		finish_variant(self.differ, self.variant, SerializeSeq::end(self.items)?)
	}
}

struct VariantMap<'a> {
	differ: Differ<'a>,
	variant: &'static str,
	fields: MapDiff<'a>,
}

impl serde::ser::SerializeStructVariant for VariantMap<'_> {
	type Ok = Node;
	type Error = Error;
	fn serialize_field<T: Serialize + ?Sized>(&mut self, key: &'static str, value: &T) -> Result<(), Error> {
		SerializeStruct::serialize_field(&mut self.fields, key, value)
	}
	fn end(self) -> Result<Node, Error> {
		finish_variant(self.differ, self.variant, SerializeStruct::end(self.fields)?)
	}
}

/// Arrays are replaced wholesale by merge patch, but an unchanged array needs no copy.
struct SeqDiff<'a> {
	differ: Differ<'a>,
	changed: bool,
	len: usize,
}

impl SeqDiff<'_> {
	fn begin(&mut self, old: &[Value]) -> Result<(), Error> {
		let mut scratch = self.differ.scratch.borrow_mut();
		let capacity = scratch.last_capacity;
		let bytes = scratch.buffer(self.differ.depth);
		bytes.clear();
		if self.differ.depth == 0 {
			bytes.reserve(capacity);
		}
		bytes.push(b'[');
		for item in &old[..self.len] {
			if bytes.len() > 1 {
				bytes.push(b',');
			}
			serde_json::to_writer(&mut *bytes, item).map_err(|err| Error(err.to_string()))?;
		}
		self.changed = true;
		Ok(())
	}
}

impl SerializeSeq for SeqDiff<'_> {
	type Ok = Node;
	type Error = Error;
	fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
		let old = self.differ.baseline.as_array();
		if !self.changed {
			let baseline = old.and_then(|old| old.get(self.len)).unwrap_or(&NULL);
			// Null inside an array is data, not a merge-patch deletion.
			let forced = Cell::new(false);
			let equal = matches!(
				value.serialize(Differ {
					baseline,
					present: true,
					forced: &forced,
					scratch: self.differ.scratch,
					depth: self.differ.depth + 1,
				})?,
				Node::Same
			) && old.is_some_and(|old| self.len < old.len());
			if !equal {
				self.begin(old.map(Vec::as_slice).unwrap_or(&[]))?;
			}
		}
		if self.changed {
			let mut scratch = self.differ.scratch.borrow_mut();
			let bytes = scratch.buffer(self.differ.depth);
			if bytes.len() > 1 {
				bytes.push(b',');
			}
			serde_json::to_writer(bytes, value).map_err(|err| Error(err.to_string()))?;
		}
		self.len += 1;
		Ok(())
	}
	fn end(mut self) -> Result<Node, Error> {
		if !self.changed {
			if let Some(old) = self.differ.baseline.as_array() {
				if self.len == old.len() {
					return Ok(Node::Same);
				}
				self.begin(old)?;
			} else {
				self.begin(&[])?;
			}
		}
		self.differ.scratch.borrow_mut().buffer(self.differ.depth).push(b']');
		Ok(Node::Diff)
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

/// Objects recurse; only changed entries are written into the reusable patch buffer.
struct MapDiff<'a> {
	differ: Differ<'a>,
	entries: usize,
	seen_len: usize,
	ordered: bool,
	added_key: bool,
}

impl MapDiff<'_> {
	fn write_entry(&mut self, key: &str, child: Option<usize>) -> Result<(), Error> {
		let depth = self.differ.depth;
		let mut scratch = self.differ.scratch.borrow_mut();
		let capacity = scratch.last_capacity;
		let bytes = scratch.buffer(depth);
		if self.entries == 0 {
			if depth == 0 {
				bytes.reserve(capacity);
			}
			bytes.push(b'{');
		} else {
			bytes.push(b',');
		}
		serde_json::to_writer(&mut *bytes, key).map_err(|err| Error(err.to_string()))?;
		bytes.push(b':');
		if let Some(child) = child {
			let (parents, children) = scratch.bytes.split_at_mut(child);
			parents[depth].extend_from_slice(&children[0]);
		} else {
			scratch.bytes[depth].extend_from_slice(b"null");
		}
		self.entries += 1;
		Ok(())
	}

	fn entry(&mut self, key: &str, existed: bool, node: Node) -> Result<(), Error> {
		self.added_key |= !existed;
		if let Node::Diff = node {
			self.write_entry(key, Some(self.differ.depth + 1))?;
		}
		let mut scratch = self.differ.scratch.borrow_mut();
		if scratch.seen.len() <= self.differ.depth {
			scratch.seen.resize_with(self.differ.depth + 1, Vec::new);
		}
		let seen = &mut scratch.seen[self.differ.depth];
		if self.seen_len > 0 {
			match seen[self.seen_len - 1].as_str().cmp(key) {
				std::cmp::Ordering::Equal => return Err(Error("duplicate JSON object key".into())),
				std::cmp::Ordering::Greater => self.ordered = false,
				std::cmp::Ordering::Less => {}
			}
		}
		if self.seen_len == seen.len() {
			seen.push(key.to_owned());
		} else {
			seen[self.seen_len].clear();
			seen[self.seen_len].push_str(key);
		}
		self.seen_len += 1;
		Ok(())
	}

	fn finish(mut self) -> Result<Node, Error> {
		if !self.ordered {
			let mut scratch = self.differ.scratch.borrow_mut();
			let seen = &mut scratch.seen[self.differ.depth][..self.seen_len];
			seen.sort_unstable();
			if seen.windows(2).any(|pair| pair[0] == pair[1]) {
				return Err(Error("duplicate JSON object key".into()));
			}
		}
		if let Value::Object(base) = self.differ.baseline
			&& (self.added_key || self.seen_len != base.len())
		{
			let depth = self.differ.depth;
			let mut scratch = self.differ.scratch.borrow_mut();
			let Scratch {
				seen,
				bytes,
				last_capacity,
				..
			} = &mut *scratch;
			// A map emptied of every key offered none, so nothing sized `seen` for this depth.
			let seen: &mut [String] = match seen.get_mut(depth) {
				Some(seen) => &mut seen[..self.seen_len],
				None => &mut [],
			};
			let out = &mut bytes[depth];
			if self.ordered {
				seen.sort_unstable();
			}
			for key in base.keys() {
				if seen.binary_search_by(|seen| seen.as_str().cmp(key)).is_err() {
					if self.entries == 0 {
						if depth == 0 {
							out.reserve(*last_capacity);
						}
						out.push(b'{');
					} else {
						out.push(b',');
					}
					serde_json::to_writer(&mut *out, key).map_err(|err| Error(err.to_string()))?;
					out.extend_from_slice(b":null");
					self.entries += 1;
				}
			}
		}
		if self.entries == 0 {
			if self.differ.baseline.is_object() {
				return Ok(Node::Same);
			}
			self.differ
				.scratch
				.borrow_mut()
				.buffer(self.differ.depth)
				.extend_from_slice(b"{}");
			return Ok(Node::Diff);
		}
		self.differ.scratch.borrow_mut().buffer(self.differ.depth).push(b'}');
		Ok(Node::Diff)
	}
}

impl SerializeMap for MapDiff<'_> {
	type Ok = Node;
	type Error = Error;
	fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Error> {
		let mut scratch = self.differ.scratch.borrow_mut();
		if scratch.pending.len() <= self.differ.depth {
			scratch.pending.resize_with(self.differ.depth + 1, String::new);
		}
		let pending = &mut scratch.pending[self.differ.depth];
		pending.clear();
		key.serialize(KeySer(pending))
	}
	fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
		let depth = self.differ.depth;
		let key = std::mem::take(&mut self.differ.scratch.borrow_mut().pending[depth]);
		let (child, existed) = self.differ.child(&key);
		let node = value.serialize(child)?;
		self.entry(&key, existed, node)?;
		self.differ.scratch.borrow_mut().pending[depth] = key;
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
		self.entry(key, existed, node)?;
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
struct KeySer<'a>(&'a mut String);

impl Serializer for KeySer<'_> {
	type Ok = ();
	type Error = Error;
	type SerializeSeq = Impossible<(), Error>;
	type SerializeTuple = Impossible<(), Error>;
	type SerializeTupleStruct = Impossible<(), Error>;
	type SerializeTupleVariant = Impossible<(), Error>;
	type SerializeMap = Impossible<(), Error>;
	type SerializeStruct = Impossible<(), Error>;
	type SerializeStructVariant = Impossible<(), Error>;

	fn serialize_str(self, v: &str) -> Result<(), Error> {
		{
			self.0.push_str(v);
			Ok(())
		}
	}
	fn serialize_char(self, v: char) -> Result<(), Error> {
		{
			use std::fmt::Write;
			write!(self.0, "{v}").expect("writing to String cannot fail");
			Ok(())
		}
	}
	fn serialize_bool(self, v: bool) -> Result<(), Error> {
		{
			use std::fmt::Write;
			write!(self.0, "{v}").expect("writing to String cannot fail");
			Ok(())
		}
	}
	fn serialize_i8(self, v: i8) -> Result<(), Error> {
		{
			use std::fmt::Write;
			write!(self.0, "{v}").expect("writing to String cannot fail");
			Ok(())
		}
	}
	fn serialize_i16(self, v: i16) -> Result<(), Error> {
		{
			use std::fmt::Write;
			write!(self.0, "{v}").expect("writing to String cannot fail");
			Ok(())
		}
	}
	fn serialize_i32(self, v: i32) -> Result<(), Error> {
		{
			use std::fmt::Write;
			write!(self.0, "{v}").expect("writing to String cannot fail");
			Ok(())
		}
	}
	fn serialize_i64(self, v: i64) -> Result<(), Error> {
		{
			use std::fmt::Write;
			write!(self.0, "{v}").expect("writing to String cannot fail");
			Ok(())
		}
	}
	fn serialize_u8(self, v: u8) -> Result<(), Error> {
		{
			use std::fmt::Write;
			write!(self.0, "{v}").expect("writing to String cannot fail");
			Ok(())
		}
	}
	fn serialize_u16(self, v: u16) -> Result<(), Error> {
		{
			use std::fmt::Write;
			write!(self.0, "{v}").expect("writing to String cannot fail");
			Ok(())
		}
	}
	fn serialize_u32(self, v: u32) -> Result<(), Error> {
		{
			use std::fmt::Write;
			write!(self.0, "{v}").expect("writing to String cannot fail");
			Ok(())
		}
	}
	fn serialize_u64(self, v: u64) -> Result<(), Error> {
		{
			use std::fmt::Write;
			write!(self.0, "{v}").expect("writing to String cannot fail");
			Ok(())
		}
	}
	fn serialize_unit_variant(self, _name: &'static str, _idx: u32, variant: &'static str) -> Result<(), Error> {
		{
			self.0.push_str(variant);
			Ok(())
		}
	}
	fn serialize_newtype_struct<T: Serialize + ?Sized>(self, _name: &'static str, value: &T) -> Result<(), Error> {
		value.serialize(self)
	}
	fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<(), Error> {
		value.serialize(self)
	}
	fn serialize_f32(self, _v: f32) -> Result<(), Error> {
		Err(Error("float map key".into()))
	}
	fn serialize_f64(self, _v: f64) -> Result<(), Error> {
		Err(Error("float map key".into()))
	}
	fn serialize_bytes(self, _v: &[u8]) -> Result<(), Error> {
		Err(Error("bytes map key".into()))
	}
	fn serialize_none(self) -> Result<(), Error> {
		Err(Error("null map key".into()))
	}
	fn serialize_unit(self) -> Result<(), Error> {
		Err(Error("unit map key".into()))
	}
	fn serialize_unit_struct(self, _name: &'static str) -> Result<(), Error> {
		Err(Error("unit struct map key".into()))
	}
	fn serialize_newtype_variant<T: Serialize + ?Sized>(
		self,
		_name: &'static str,
		_idx: u32,
		_variant: &'static str,
		_value: &T,
	) -> Result<(), Error> {
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
	fn replacing_scalar_with_empty_object_is_a_change() {
		check(json!({ "value": 1 }), json!({ "value": {} }));
		check(json!({ "value": null }), json!({ "value": {} }));
		let result = diff(&json!(1), &json!({}));
		assert!(result.forced_snapshot);
		assert_eq!(result.patch, json!({}));
	}

	#[test]
	fn duplicate_serialized_map_keys_force_a_snapshot() {
		struct Duplicate;
		impl Serialize for Duplicate {
			fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
				let mut map = serializer.serialize_map(Some(3))?;
				map.serialize_entry("key", &1)?;
				map.serialize_entry("other", &2)?;
				map.serialize_entry("key", &3)?;
				map.end()
			}
		}
		assert!(diff(&json!({ "key": 1, "other": 2 }), &Duplicate).forced_snapshot);
	}

	#[test]
	fn emptying_a_map_removes_every_key() {
		// No key is serialized at the emptied map's depth, so nothing has sized the
		// scratch for it yet.
		check(json!({ "a": 1 }), json!({}));
		check(json!({ "a": 1, "b": 2 }), json!({}));
		check(json!({ "x": { "a": 1 } }), json!({ "x": {} }));
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
	fn added_null_key_forces_snapshot() {
		check(json!({ "a": 1 }), json!({ "a": 1, "x": null }));
		check(
			json!({ "items": [{ "a": 1 }] }),
			json!({ "items": [{ "a": 1, "x": null }] }),
		);
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
