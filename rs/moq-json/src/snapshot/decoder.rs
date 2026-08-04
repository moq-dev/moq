//! The track-free half of snapshot consuming: frame payloads in, values out.

use std::marker::PhantomData;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::{Error, Result};

/// Configuration for a [`Decoder`], and so for the [`Consumer`](super::Consumer) wrapping one.
///
/// Build from [`Default`] and override fields (the struct is `#[non_exhaustive]`, so new options
/// stay additive), or chain the `with_*` setters.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ConsumerConfig {
	/// Whether the frames are DEFLATE-compressed. Must match the encoder's
	/// [`ProducerConfig::compression`](super::ProducerConfig::compression). Defaults to `false`.
	pub compression: bool,
}

impl ConsumerConfig {
	/// Set [`compression`](Self::compression) (a builder, since the struct is `#[non_exhaustive]`).
	pub fn with_compression(mut self, compression: bool) -> Self {
		self.compression = compression;
		self
	}
}

/// Reconstructs a JSON value from the snapshot and delta frames of a group.
///
/// The track-free core of [`Consumer`](super::Consumer), and the mirror of
/// [`Encoder`](super::Encoder). The caller reads frames from wherever it likes and routes each one
/// by its position in the group: the first frame of every group is a
/// [`snapshot`](Self::snapshot), the rest are [`delta`](Self::delta)s.
///
/// ```ignore
/// match frame.keyframe {
///     true => decoder.snapshot(&frame.payload)?,
///     false => decoder.delta(&frame.payload)?,
/// }
/// let value = decoder.decode()?;
/// ```
///
/// Applying and materializing are separate on purpose. Frames must be applied in order (the merge
/// patches and the DEFLATE window are both sequential), but a consumer catching up on a backlog only
/// wants the value at the head, so it applies every frame and calls [`decode`](Self::decode) once.
/// A caller that wants a value per frame just calls it every time.
pub struct Decoder<T> {
	/// Whether frames are DEFLATE-compressed, matching the encoder's config.
	compression: bool,

	/// The current group's DEFLATE decoder (one window per group), rebuilt at each snapshot.
	flate: Option<moq_flate::Decoder>,

	/// The reconstructed value, `None` until the first snapshot.
	current: Option<Value>,

	_marker: PhantomData<fn() -> T>,
}

impl<T> Decoder<T> {
	/// Create a decoder with no value, awaiting its first [`snapshot`](Self::snapshot).
	pub fn new(config: ConsumerConfig) -> Self {
		Self {
			compression: config.compression,
			flate: None,
			current: None,
			_marker: PhantomData,
		}
	}

	/// Apply a group's first frame: a full snapshot that replaces the current value.
	///
	/// Also starts the group's DEFLATE window, so this must be called at every group boundary, not
	/// only the first.
	pub fn snapshot(&mut self, payload: &[u8]) -> Result<()> {
		// Each group is its own compressed stream, so the window starts cold here.
		self.flate = self.compression.then(moq_flate::Decoder::new);
		self.current = Some(match self.flate.as_mut() {
			Some(flate) => serde_json::from_slice(&flate.frame(payload)?)?,
			None => serde_json::from_slice(payload)?,
		});
		Ok(())
	}

	/// Apply one of a group's later frames: an [RFC 7396](https://www.rfc-editor.org/rfc/rfc7396.html)
	/// merge patch against the current value.
	///
	/// Errors with [`Error::MissingSnapshot`] when no snapshot has been applied yet, since a patch
	/// has nothing to apply to.
	pub fn delta(&mut self, payload: &[u8]) -> Result<()> {
		if self.current.is_none() {
			return Err(Error::MissingSnapshot);
		}

		let patch: Value = match self.flate.as_mut() {
			Some(flate) => serde_json::from_slice(&flate.frame(payload)?)?,
			None => serde_json::from_slice(payload)?,
		};

		json_patch::merge(self.current.as_mut().expect("a snapshot precedes any delta"), &patch);
		Ok(())
	}

	/// The reconstructed value as raw JSON, or `None` before the first snapshot.
	pub fn value(&self) -> Option<&Value> {
		self.current.as_ref()
	}
}

impl<T: DeserializeOwned> Decoder<T> {
	/// Materialize the reconstructed value as `T`, or `None` before the first snapshot.
	///
	/// Deserializing from the reconstructed [`Value`] rather than the frame bytes costs the line and
	/// column a parse error would carry, so the error is prefixed with the JSON path of the offending
	/// field instead. Without it a rejected field deep in a document reports only its own complaint,
	/// with nothing to say where it came from.
	pub fn decode(&self) -> Result<Option<T>> {
		let Some(current) = self.current.as_ref() else {
			return Ok(None);
		};

		let value = serde_path_to_error::deserialize(current).map_err(|err| {
			let path = err.path().to_string();
			match path.as_str() {
				// The whole document, not a field within it: nothing useful to prefix.
				"." => Error::Json(err.into_inner().to_string()),
				_ => Error::Json(format!("{}: {}", path, err.into_inner())),
			}
		})?;

		Ok(Some(value))
	}
}

#[cfg(test)]
mod test {
	use super::super::{Encoder, ProducerConfig};
	use super::*;
	use serde_json::json;

	/// Round-trip a sequence of values through an encoder and decoder, yielding the value the
	/// decoder reconstructs after each frame.
	fn roundtrip(config: ProducerConfig, values: &[Value]) -> Vec<Value> {
		let compression = config.compression;
		let mut encoder = Encoder::<Value>::new(config);
		let mut decoder = Decoder::<Value>::new(ConsumerConfig::default().with_compression(compression));

		let mut out = Vec::new();
		for value in values {
			let Some(frame) = encoder.update(value).unwrap() else {
				continue;
			};
			match frame.keyframe {
				true => decoder.snapshot(&frame.payload).unwrap(),
				false => decoder.delta(&frame.payload).unwrap(),
			}
			frame.commit();
			out.push(decoder.decode().unwrap().unwrap());
		}
		out
	}

	#[test]
	fn plaintext_roundtrip() {
		let values = vec![
			json!({ "a": 1, "b": 1 }),
			json!({ "a": 1, "b": 2 }),
			json!({ "a": 5, "b": 2 }),
		];
		assert_eq!(roundtrip(ProducerConfig::default(), &values), values);
	}

	#[test]
	fn compressed_roundtrip() {
		let values = vec![
			json!({ "a": 1, "b": 1 }),
			json!({ "a": 1, "b": 2 }),
			json!({ "a": 5, "b": 2 }),
		];
		let config = ProducerConfig::default().with_compression(true);
		assert_eq!(roundtrip(config, &values), values);
	}

	/// The window is per group, so a keyframe mid-stream has to restart it on both sides. A decoder
	/// that kept the old window here would fail to inflate the new group's snapshot.
	#[test]
	fn compressed_roundtrip_across_a_group_boundary() {
		// A tight ratio guarantees at least one roll partway through.
		let values: Vec<Value> = (0..=40).map(|n| json!({ "n": n })).collect();
		let config = ProducerConfig::default().with_delta_ratio(2).with_compression(true);
		assert_eq!(roundtrip(config, &values).last().unwrap(), &json!({ "n": 40 }));
	}

	#[test]
	fn no_value_before_the_first_snapshot() {
		let decoder = Decoder::<Value>::new(ConsumerConfig::default());
		assert_eq!(decoder.value(), None);
		assert_eq!(decoder.decode().unwrap(), None);
	}

	#[test]
	fn a_delta_before_a_snapshot_is_an_error() {
		let mut decoder = Decoder::<Value>::new(ConsumerConfig::default());
		assert!(matches!(decoder.delta(br#"{"a":1}"#), Err(Error::MissingSnapshot)));
	}

	/// A backlog is applied in full but materialized once: the intermediate reconstructions are
	/// stale, and deserializing each one is exactly the cost the split exists to avoid.
	#[test]
	fn frames_apply_without_materializing() {
		let mut encoder = Encoder::<Value>::new(ProducerConfig::default().with_delta_ratio(100));
		let mut decoder = Decoder::<Value>::new(ConsumerConfig::default());

		for n in 0..=20 {
			let frame = encoder.update(&json!({ "n": n })).unwrap().unwrap();
			match frame.keyframe {
				true => decoder.snapshot(&frame.payload).unwrap(),
				false => decoder.delta(&frame.payload).unwrap(),
			}
			frame.commit();
		}

		assert_eq!(decoder.decode().unwrap(), Some(json!({ "n": 20 })));
	}

	#[test]
	fn a_rejected_field_names_its_path() {
		#[derive(serde::Deserialize, Debug)]
		#[allow(dead_code)]
		struct Inner {
			count: u8,
		}
		#[derive(serde::Deserialize, Debug)]
		#[allow(dead_code)]
		struct Outer {
			inner: Inner,
		}

		let mut decoder = Decoder::<Outer>::new(ConsumerConfig::default());
		decoder.snapshot(br#"{"inner":{"count":300}}"#).unwrap();

		let err = decoder.decode().unwrap_err();
		assert!(err.to_string().starts_with("json: inner.count: "), "{err}");
	}
}
