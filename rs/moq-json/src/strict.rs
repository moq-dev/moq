//! Strict JSON validation shared by every decoder: one value, no object repeating a key.
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

	/// A malformed frame stays a syntax error, and a refusal does not leak into the next check.
	#[test]
	fn malformed_json_is_not_a_duplicate() {
		let scratch = RefCell::new(Scratch::default());
		assert!(matches!(check(br#"{"a":1,"a":2}"#, &scratch), Err(Error::DuplicateKey)));
		assert!(matches!(check(br#"{"a":1,"b""#, &scratch), Err(Error::Json(_))));
		assert!(matches!(check(br#"{"a":1} {}"#, &scratch), Err(Error::Json(_))));
	}
}
