//! Apply a JSON merge patch directly into the retained value.

use std::cell::RefCell;

use serde_json::{Map, Value};

use crate::strict::{Key, Scratch};

/// Validate the complete frame before changing the retained value, then apply its patch in place.
/// Existing object keys and numeric values are reused instead of building a patch tree.
pub(crate) fn apply_bytes(target: &mut Value, bytes: &[u8], scratch: &RefCell<Scratch>) -> crate::Result<()> {
	crate::strict::check(bytes, scratch)?;
	Ok(apply_generated_bytes(target, bytes)?)
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
				&RefCell::new(Scratch::default()),
			)
			.unwrap();
			assert_eq!(target, expected);
		}
	}

	#[test]
	fn malformed_patch_does_not_change_the_baseline() {
		let mut target = json!({"a": 1, "b": 2});
		assert!(apply_bytes(&mut target, br#"{"a":3,"b":[]"#, &RefCell::new(Scratch::default())).is_err());
		assert_eq!(target, json!({"a": 1, "b": 2}));
	}

	#[test]
	fn empty_object_patch_does_not_panic() {
		let mut target = json!({ "value": 1 });
		apply_bytes(&mut target, br#"{"value":{}}"#, &RefCell::new(Scratch::default())).unwrap();
		assert_eq!(target, json!({ "value": {} }));
	}

	#[test]
	fn duplicate_keys_are_rejected_before_mutating_the_baseline() {
		let mut target = json!({ "outer": { "x": 1 }, "tail": 2 });
		let patch = br#"{"tail":3,"outer":{"x":2,"x":4}}"#;
		let err = apply_bytes(&mut target, patch, &RefCell::new(Scratch::default())).unwrap_err();
		assert!(matches!(err, crate::Error::DuplicateKey), "{err}");
		assert_eq!(target, json!({ "outer": { "x": 1 }, "tail": 2 }));
	}

	#[test]
	fn escaped_keys_and_array_nulls_match_merge_patch() {
		let mut target = json!({r#"escaped"key"#: 1, "array": [1]});
		let patch = br#"{"escaped\"key":2,"array":[null,3]}"#;
		let mut expected = target.clone();
		json_patch::merge(&mut expected, &serde_json::from_slice(patch).unwrap());
		apply_bytes(&mut target, patch, &RefCell::new(Scratch::default())).unwrap();
		assert_eq!(target, expected);
	}
}
