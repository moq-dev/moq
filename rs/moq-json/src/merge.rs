//! Apply a JSON merge patch directly into the retained value.

use serde_json::{Map, Value};

/// Validate the complete frame before changing the retained value, then apply its patch in place.
/// Existing object keys and numeric values are reused instead of building a patch tree.
pub(crate) fn apply_bytes(target: &mut Value, bytes: &[u8], scratch: &RefCell<CheckScratch>) -> serde_json::Result<()> {
	use serde::de::DeserializeSeed;
	let mut check = serde_json::Deserializer::from_slice(bytes);
	Check { scratch, depth: 0 }.deserialize(&mut check)?;
	check.end()?;
	apply_generated_bytes(target, bytes)
}

/// Apply a patch emitted by our serializer, whose keys and syntax were already checked.
pub(crate) fn apply_generated_bytes(target: &mut Value, bytes: &[u8]) -> serde_json::Result<()> {
	use serde::de::DeserializeSeed;
	let mut decoder = serde_json::Deserializer::from_slice(bytes);
	Patch {
		target,
		deletion: false,
	}
	.deserialize(&mut decoder)?;
	Ok(())
}

use std::cell::RefCell;

#[derive(Default)]
pub(crate) struct CheckScratch {
	keys: Vec<Vec<String>>,
}

struct Check<'a> {
	scratch: &'a RefCell<CheckScratch>,
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
						std::cmp::Ordering::Equal => return Err(serde::de::Error::custom("duplicate JSON object key")),
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
			return Err(serde::de::Error::custom("duplicate JSON object key"));
		}
		Ok(())
	}
}

struct Key<'de>(std::borrow::Cow<'de, str>);

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

struct Patch<'a> {
	target: &'a mut Value,
	deletion: bool,
}

impl<'de> serde::de::DeserializeSeed<'de> for Patch<'_> {
	type Value = bool;

	fn deserialize<D: serde::Deserializer<'de>>(self, decoder: D) -> Result<bool, D::Error> {
		decoder.deserialize_any(self)
	}
}

impl<'de> serde::de::Visitor<'de> for Patch<'_> {
	type Value = bool;

	fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
		formatter.write_str("a JSON merge patch")
	}

	fn visit_unit<E: serde::de::Error>(self) -> Result<bool, E> {
		if self.deletion {
			Ok(true)
		} else {
			*self.target = Value::Null;
			Ok(false)
		}
	}

	fn visit_none<E: serde::de::Error>(self) -> Result<bool, E> {
		self.visit_unit()
	}

	fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<bool, E> {
		*self.target = Value::Bool(value);
		Ok(false)
	}

	fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<bool, E> {
		*self.target = Value::from(value);
		Ok(false)
	}

	fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<bool, E> {
		*self.target = Value::from(value);
		Ok(false)
	}

	fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<bool, E> {
		*self.target = Value::from(value);
		Ok(false)
	}

	fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<bool, E> {
		*self.target = Value::from(value);
		Ok(false)
	}

	fn visit_string<E: serde::de::Error>(self, value: String) -> Result<bool, E> {
		*self.target = Value::String(value);
		Ok(false)
	}

	fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<bool, A::Error> {
		let mut values = Vec::with_capacity(seq.size_hint().unwrap_or(0));
		while let Some(value) = seq.next_element()? {
			values.push(value);
		}
		*self.target = Value::Array(values);
		Ok(false)
	}

	fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<bool, A::Error> {
		if !self.target.is_object() {
			*self.target = Value::Object(Map::new());
		}
		let object = self.target.as_object_mut().expect("target is an object");
		while let Some(Key(key)) = map.next_key::<Key<'de>>()? {
			if let Some(value) = object.get_mut(key.as_ref()) {
				let remove = map.next_value_seed(Patch {
					target: value,
					deletion: true,
				})?;
				if remove {
					object.remove(key.as_ref());
				}
			} else {
				let mut value = Value::Null;
				if !map.next_value_seed(Patch {
					target: &mut value,
					deletion: true,
				})? {
					object.insert(key.into_owned(), value);
				}
			}
		}
		Ok(false)
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use serde_json::json;

	#[test]
	fn agrees_with_merge_patch_for_nested_additions_and_deletions() {
		for (mut target, patch) in [
			(
				json!({"same": 1, "old": {"gone": 1, "keep": 2}}),
				json!({"old": {"gone": null, "new": 3}}),
			),
			(json!({"old": 1}), json!({"new": {"absent": null, "kept": 1}})),
			(json!(1), json!({"nested": {"absent": null, "kept": 1}})),
			(json!({"old": 1}), json!([1, 2, 3])),
		] {
			let mut expected = target.clone();
			json_patch::merge(&mut expected, &patch);
			apply_bytes(
				&mut target,
				serde_json::to_string(&patch).unwrap().as_bytes(),
				&RefCell::new(CheckScratch::default()),
			)
			.unwrap();
			assert_eq!(target, expected);
		}
	}

	#[test]
	fn malformed_patch_does_not_change_the_baseline() {
		let mut target = json!({"a": 1, "b": 2});
		assert!(apply_bytes(&mut target, br#"{"a":3,"b":[]"#, &RefCell::new(CheckScratch::default())).is_err());
		assert_eq!(target, json!({"a": 1, "b": 2}));
	}

	#[test]
	fn empty_object_patch_does_not_panic() {
		let mut target = json!({ "value": 1 });
		apply_bytes(&mut target, br#"{"value":{}}"#, &RefCell::new(CheckScratch::default())).unwrap();
		assert_eq!(target, json!({ "value": {} }));
	}

	#[test]
	fn duplicate_keys_are_rejected_before_mutating_the_baseline() {
		let mut target = json!({ "outer": { "x": 1 }, "tail": 2 });
		let patch = br#"{"tail":3,"outer":{"x":2,"x":4}}"#;
		let err = apply_bytes(&mut target, patch, &RefCell::new(CheckScratch::default())).unwrap_err();
		assert!(err.to_string().contains("duplicate JSON object key"));
		assert_eq!(target, json!({ "outer": { "x": 1 }, "tail": 2 }));
	}

	#[test]
	fn escaped_keys_and_array_nulls_match_merge_patch() {
		let mut target = json!({r#"escaped"key"#: 1, "array": [1]});
		let patch = br#"{"escaped\"key":2,"array":[null,3]}"#;
		let mut expected = target.clone();
		json_patch::merge(&mut expected, &serde_json::from_slice(patch).unwrap());
		apply_bytes(&mut target, patch, &RefCell::new(CheckScratch::default())).unwrap();
		assert_eq!(target, expected);
	}
}
