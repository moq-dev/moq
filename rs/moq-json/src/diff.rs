use serde_json::{Map, Value};

/// The result of diffing two JSON values into an RFC 7396 merge patch.
pub struct Diff {
	/// A merge patch that transforms the old value into the new one.
	pub patch: Value,

	/// Set when the change can't be faithfully expressed as a merge patch, so the caller
	/// should publish a full snapshot instead. This happens when a value is set to JSON null,
	/// which merge patch reads as a key deletion. Arrays are fine: merge patch replaces them
	/// wholesale, which is still typically smaller than a full snapshot.
	pub forced_snapshot: bool,
}

/// Generate an RFC 7396 merge patch transforming `old` into `new`.
///
/// Only object roots produce a recursive patch; any other root forces a snapshot.
pub fn diff(old: &Value, new: &Value) -> Diff {
	if let (Value::Object(old), Value::Object(new)) = (old, new) {
		let mut patch = Map::new();
		let mut forced = false;
		diff_objects(old, new, &mut patch, &mut forced);
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

fn diff_objects(old: &Map<String, Value>, new: &Map<String, Value>, patch: &mut Map<String, Value>, forced: &mut bool) {
	// Keys present in old but missing from new become explicit null deletions.
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

		// Recurse into nested objects so unchanged sibling keys stay out of the patch.
		if let (Some(Value::Object(old_obj)), Value::Object(new_obj)) = (old_val, new_val) {
			let mut sub = Map::new();
			diff_objects(old_obj, new_obj, &mut sub, forced);
			if !sub.is_empty() {
				patch.insert(key.clone(), Value::Object(sub));
			}
			continue;
		}

		// Added or replaced with a non-object value. A literal null can't be stored: merge patch
		// would delete the key. Arrays are kept in the patch and replace the target wholesale.
		if new_val.is_null() {
			*forced = true;
		}
		patch.insert(key.clone(), new_val.clone());
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use serde_json::json;

	/// Applying the patch to old should reproduce new (RFC 7396 semantics).
	fn assert_roundtrip(old: Value, new: Value) {
		let result = diff(&old, &new);
		assert!(!result.forced_snapshot, "expected a delta, got a forced snapshot");
		let mut applied = old;
		json_patch::merge(&mut applied, &result.patch);
		assert_eq!(applied, new);
	}

	#[test]
	fn changed_scalar() {
		assert_roundtrip(json!({ "a": 1, "b": 2 }), json!({ "a": 1, "b": 3 }));
	}

	#[test]
	fn added_key() {
		let result = diff(&json!({ "a": 1 }), &json!({ "a": 1, "b": 2 }));
		assert!(!result.forced_snapshot);
		assert_eq!(result.patch, json!({ "b": 2 }));
	}

	#[test]
	fn removed_key_is_null() {
		let result = diff(&json!({ "a": 1, "b": 2 }), &json!({ "a": 1 }));
		assert!(!result.forced_snapshot, "removing a key is a clean delete");
		assert_eq!(result.patch, json!({ "b": null }));
		assert_roundtrip(json!({ "a": 1, "b": 2 }), json!({ "a": 1 }));
	}

	#[test]
	fn nested_object_only_includes_changed_keys() {
		let result = diff(&json!({ "o": { "x": 1, "y": 2 } }), &json!({ "o": { "x": 1, "y": 9 } }));
		assert!(!result.forced_snapshot);
		assert_eq!(result.patch, json!({ "o": { "y": 9 } }));
	}

	#[test]
	fn changed_array_is_wholesale_delta() {
		let result = diff(&json!({ "a": [1, 2] }), &json!({ "a": [1, 2, 3] }));
		assert!(!result.forced_snapshot);
		assert_eq!(result.patch, json!({ "a": [1, 2, 3] }));
		assert_roundtrip(json!({ "a": [1, 2] }), json!({ "a": [1, 2, 3] }));
	}

	#[test]
	fn added_array_is_delta() {
		let result = diff(&json!({ "a": 1 }), &json!({ "a": 1, "b": [1] }));
		assert!(!result.forced_snapshot);
		assert_eq!(result.patch, json!({ "b": [1] }));
	}

	#[test]
	fn nested_array_is_delta() {
		let result = diff(&json!({ "o": { "x": 1 } }), &json!({ "o": { "x": 1, "list": [1] } }));
		assert!(!result.forced_snapshot);
		assert_eq!(result.patch, json!({ "o": { "list": [1] } }));
		assert_roundtrip(json!({ "o": { "x": 1 } }), json!({ "o": { "x": 1, "list": [1] } }));
	}

	#[test]
	fn set_to_null_forces_snapshot() {
		// A genuine null value can't be represented: merge patch would delete the key.
		let result = diff(&json!({ "a": 1 }), &json!({ "a": null }));
		assert!(result.forced_snapshot);
	}

	#[test]
	fn replacing_object_with_scalar() {
		assert_roundtrip(json!({ "a": { "x": 1 } }), json!({ "a": 5 }));
	}

	#[test]
	fn non_object_root_forces_snapshot() {
		let result = diff(&json!(1), &json!(2));
		assert!(result.forced_snapshot);
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
}
