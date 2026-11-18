//! Strict JSON shared by every codec: decoders read one value with no object repeating a key, and
//! encoders write nothing nested deeper than a decoder reads.
//!
//! `serde_json` keeps the last of a repeated key while other parsers keep the first or refuse, so
//! a frame with one could mean two things to two readers. Each decoder runs this pass first.

use std::cell::RefCell;

use crate::Error;

/// Validate one JSON value's syntax, refusing any object that repeats a key.
pub(crate) fn check(bytes: &[u8], scratch: &RefCell<Scratch>) -> crate::Result<()> {
	use serde::de::DeserializeSeed;
	let mut decoder = serde_json::Deserializer::from_slice(bytes);
	let result = Check { scratch, depth: 0 }
		.deserialize(&mut decoder)
		.and_then(|()| decoder.end());
	// The visitor can only report a `serde_json::Error`, so it flags a duplicate for the typed error.
	match result {
		Ok(()) => Ok(()),
		Err(_) if std::mem::take(&mut scratch.borrow_mut().duplicate) => Err(Error::DuplicateKey),
		Err(err) => Err(err.into()),
	}
}

/// Containers deep at which serde_json refuses a document.
const MAX_DEPTH: usize = 128;

/// Serialize `value` as compact JSON, refusing nesting a decoder would refuse once a frame wraps it
/// in `envelope` more containers.
///
/// `serde_json` writes any nesting but refuses to read past its recursion limit, so an encoder
/// counts depth as it writes rather than publish a frame no reader accepts.
pub(crate) fn to_writer<W: std::io::Write, T: serde::Serialize + ?Sized>(
	writer: W,
	value: &T,
	envelope: usize,
) -> crate::Result<()> {
	let formatter = Nesting {
		depth: 0,
		limit: MAX_DEPTH - 1 - envelope,
	};
	value.serialize(&mut serde_json::Serializer::with_formatter(writer, formatter))?;
	Ok(())
}

/// [`to_writer`] into a new buffer.
pub(crate) fn to_vec<T: serde::Serialize + ?Sized>(value: &T, envelope: usize) -> crate::Result<Vec<u8>> {
	let mut bytes = Vec::with_capacity(128);
	to_writer(&mut bytes, value, envelope)?;
	Ok(bytes)
}

/// The compact formatter, failing once containers nest past `limit`.
struct Nesting {
	depth: usize,
	limit: usize,
}

impl Nesting {
	fn enter(&mut self) -> std::io::Result<()> {
		self.depth += 1;
		match self.depth > self.limit {
			true => Err(std::io::Error::other("JSON nested too deeply")),
			false => Ok(()),
		}
	}
}

impl serde_json::ser::Formatter for Nesting {
	fn begin_array<W: ?Sized + std::io::Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
		self.enter()?;
		writer.write_all(b"[")
	}

	fn end_array<W: ?Sized + std::io::Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
		self.depth -= 1;
		writer.write_all(b"]")
	}

	fn begin_object<W: ?Sized + std::io::Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
		self.enter()?;
		writer.write_all(b"{")
	}

	fn end_object<W: ?Sized + std::io::Write>(&mut self, writer: &mut W) -> std::io::Result<()> {
		self.depth -= 1;
		writer.write_all(b"}")
	}
}

/// Reused key buffers, one per nesting depth.
#[derive(Default)]
pub(crate) struct Scratch {
	keys: Vec<Vec<String>>,
	duplicate: bool,
}

struct Check<'a> {
	scratch: &'a RefCell<Scratch>,
	depth: usize,
}

impl<'de> serde::de::DeserializeSeed<'de> for Check<'_> {
	type Value = ();

	fn deserialize<D: serde::Deserializer<'de>>(self, decoder: D) -> Result<(), D::Error> {
		decoder.deserialize_any(self)
	}
}

impl<'de> serde::de::Visitor<'de> for Check<'_> {
	type Value = ();

	fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
		formatter.write_str("a JSON value")
	}

	fn visit_unit<E: serde::de::Error>(self) -> Result<(), E> {
		Ok(())
	}
	fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<(), E> {
		Ok(())
	}
	fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<(), E> {
		Ok(())
	}
	fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<(), E> {
		Ok(())
	}
	fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<(), E> {
		Ok(())
	}
	fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<(), E> {
		Ok(())
	}
	fn visit_string<E: serde::de::Error>(self, _: String) -> Result<(), E> {
		Ok(())
	}

	fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
		while seq
			.next_element_seed(Check {
				scratch: self.scratch,
				depth: self.depth + 1,
			})?
			.is_some()
		{}
		Ok(())
	}

	fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
		let mut len = 0;
		let mut ordered = true;
		while let Some(Key(key)) = map.next_key::<Key<'de>>()? {
			{
				let mut scratch = self.scratch.borrow_mut();
				if scratch.keys.len() <= self.depth {
					scratch.keys.resize_with(self.depth + 1, Vec::new);
				}
				let keys = &mut scratch.keys[self.depth];
				if len > 0 {
					match keys[len - 1].as_str().cmp(&key) {
						std::cmp::Ordering::Equal => {
							scratch.duplicate = true;
							return Err(serde::de::Error::custom("duplicate key"));
						}
						std::cmp::Ordering::Greater => ordered = false,
						std::cmp::Ordering::Less => {}
					}
				}
				if len == keys.len() {
					keys.push(key.into_owned());
				} else {
					keys[len].clear();
					keys[len].push_str(&key);
				}
				len += 1;
			}
			map.next_value_seed(Check {
				scratch: self.scratch,
				depth: self.depth + 1,
			})?;
		}
		if len == 0 || ordered {
			return Ok(());
		}
		let mut scratch = self.scratch.borrow_mut();
		let keys = &mut scratch.keys[self.depth][..len];
		keys.sort_unstable();
		if keys.windows(2).any(|pair| pair[0] == pair[1]) {
			scratch.duplicate = true;
			return Err(serde::de::Error::custom("duplicate key"));
		}
		Ok(())
	}
}

pub(crate) struct Key<'de>(pub(crate) std::borrow::Cow<'de, str>);

impl<'de> serde::Deserialize<'de> for Key<'de> {
	fn deserialize<D: serde::Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
		struct KeyVisitor;
		impl<'de> serde::de::Visitor<'de> for KeyVisitor {
			type Value = Key<'de>;

			fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
				formatter.write_str("a JSON object key")
			}

			fn visit_borrowed_str<E: serde::de::Error>(self, value: &'de str) -> Result<Self::Value, E> {
				Ok(Key(std::borrow::Cow::Borrowed(value)))
			}

			fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
				Ok(Key(std::borrow::Cow::Owned(value.to_owned())))
			}

			fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
				Ok(Key(std::borrow::Cow::Owned(value)))
			}
		}
		decoder.deserialize_str(KeyVisitor)
	}
}

#[cfg(test)]
pub(crate) mod test {
	use super::*;

	/// Frames every decoder must refuse or accept, shared with the TypeScript suite.
	#[derive(serde::Deserialize)]
	pub(crate) struct Vectors {
		pub refused: Vec<String>,
		pub accepted: Vec<String>,
	}

	pub(crate) fn vectors() -> Vectors {
		serde_json::from_str(include_str!("../tests/duplicate-keys.json")).unwrap()
	}

	#[test]
	fn duplicate_keys_are_refused() {
		let scratch = RefCell::new(Scratch::default());
		let vectors = vectors();
		for payload in &vectors.refused {
			let err = check(payload.as_bytes(), &scratch).unwrap_err();
			assert!(matches!(err, Error::DuplicateKey), "{payload}: {err}");
		}
		for payload in &vectors.accepted {
			check(payload.as_bytes(), &scratch).unwrap_or_else(|err| panic!("{payload}: {err}"));
		}
	}

	fn nest(levels: usize) -> serde_json::Value {
		(0..levels).fold(serde_json::json!(0), |value, _| serde_json::Value::Array(vec![value]))
	}

	/// Every writer refuses what its reader would find nested too deeply, then keeps writing.
	#[test]
	fn writers_refuse_what_readers_refuse() {
		use serde_json::{Value, json};

		// A stream record decodes up to the limit itself.
		let mut encoder = crate::stream::Encoder::<Value>::new(Default::default());
		let mut decoder = crate::stream::Decoder::<Value>::new(Default::default());
		let record = encoder.encode(&nest(127)).unwrap();
		assert_eq!(decoder.decode(record.payload()).unwrap(), nest(127));
		record.commit();
		assert!(matches!(encoder.encode(&nest(128)), Err(Error::Json(_))));
		assert_eq!(
			decoder.decode(encoder.encode(&json!(1)).unwrap().payload()).unwrap(),
			json!(1)
		);

		// A snapshot nests its deltas no deeper than the value they patch.
		let mut encoder = crate::snapshot::Encoder::<Value>::new(Default::default());
		let mut decoder = crate::snapshot::Decoder::<Value>::new(Default::default());
		let first = encoder.update(&json!({ "a": nest(126) })).unwrap().unwrap();
		decoder.snapshot(&first.payload).unwrap();
		first.commit();
		assert!(matches!(
			encoder.update(&json!({ "a": nest(127) })),
			Err(Error::Json(_))
		));
		let delta = encoder.update(&json!({ "a": nest(126), "b": 1 })).unwrap().unwrap();
		assert!(!delta.keyframe);
		decoder.delta(&delta.payload).unwrap();
		delta.commit();
		assert_eq!(decoder.decode().unwrap(), Some(json!({ "a": nest(126), "b": 1 })));

		// Without deltas a snapshot is never parsed back, so the check is all that refuses it.
		let mut encoder = crate::snapshot::Encoder::<Value>::new(crate::snapshot::Config::default().with_deltas(false));
		assert!(matches!(encoder.update(&nest(128)), Err(Error::Json(_))));
		let frame = encoder.update(&nest(127)).unwrap().unwrap();
		crate::snapshot::Decoder::<Value>::new(Default::default())
			.snapshot(&frame.payload)
			.unwrap();

		// A window record sits two containers deep in a header and one in an op.
		let mut encoder = crate::window::Encoder::<Value>::new(crate::window::Config::default().with_ops(true));
		let mut decoder = crate::window::Decoder::<Value>::new(Default::default());
		let mut group = decoder.group();
		let header = encoder.push(&nest(125)).unwrap();
		group.decode(&header.payload).unwrap();
		header.commit();
		assert!(matches!(encoder.push(&nest(126)), Err(Error::Json(_))));
		let op = encoder.push(&nest(125)).unwrap();
		assert!(!op.keyframe);
		group.decode(&op.payload).unwrap();
		op.commit();
		let restated = encoder.snapshot().unwrap();
		decoder.group().decode(&restated.payload).unwrap();
	}

	/// Nesting past serde_json's limit and numbers outside the double range, shared with the TypeScript suite.
	#[test]
	fn limits() {
		let scratch = RefCell::new(Scratch::default());
		let vectors: Vectors = serde_json::from_str(include_str!("../tests/limits.json")).unwrap();
		for payload in &vectors.refused {
			let err = check(payload.as_bytes(), &scratch).unwrap_err();
			assert!(matches!(err, Error::Json(_)), "{payload}: {err}");
		}
		for payload in &vectors.accepted {
			check(payload.as_bytes(), &scratch).unwrap_or_else(|err| panic!("{payload}: {err}"));
		}
	}

	/// A malformed frame stays a syntax error, and a refusal does not leak into the next check.
	#[test]
	fn malformed_json_is_not_a_duplicate() {
		let scratch = RefCell::new(Scratch::default());
		assert!(matches!(check(br#"{"a":1,"a":2}"#, &scratch), Err(Error::DuplicateKey)));
		assert!(matches!(check(br#"{"a":1,"b""#, &scratch), Err(Error::Json(_))));
		assert!(matches!(check(br#"{"a":1} {}"#, &scratch), Err(Error::Json(_))));
	}
}
