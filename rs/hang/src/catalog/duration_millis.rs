//! Serde adapter for `Option<Duration>` ↔ JSON integer milliseconds.
//!
//! The hang catalog historically serialized its jitter fields as a bare integer
//! number of milliseconds (e.g. `"jitter": 100`). `std::time::Duration`'s default
//! serde impl would write `{"secs": 0, "nanos": 100_000_000}` instead, so apply
//! this module via `#[serde(default, with = "duration_millis")]` to preserve the
//! original on-wire shape:
//!
//! - `None` → omitted (with `#[serde(default)]`)
//! - `Some(duration)` → integer milliseconds (truncated; sub-ms precision is lost)
//!
//! Sub-millisecond precision isn't meaningful for the jitter use case, so the
//! truncation is fine.

use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub fn serialize<S: Serializer>(value: &Option<Duration>, ser: S) -> Result<S::Ok, S::Error> {
	match value {
		Some(d) => (d.as_millis() as u64).serialize(ser),
		None => ser.serialize_none(),
	}
}

pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Option<Duration>, D::Error> {
	Ok(Option::<u64>::deserialize(de)?.map(Duration::from_millis))
}
