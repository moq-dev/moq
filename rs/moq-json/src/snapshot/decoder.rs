//! The track-free half of snapshot consuming: frame payloads in, values out.

use std::cell::RefCell;
use std::marker::PhantomData;

use serde::de::DeserializeOwned;
use serde_json::Value;

use super::consumer::Config;
use crate::{Error, Result};

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
	max_size: Option<usize>,

	/// The current group's DEFLATE decoder (one window per group), rebuilt at each snapshot.
	flate: Option<moq_flate::Decoder>,

	/// Reused output for inflated delta frames.
	plain: Vec<u8>,

	/// Reused key buffers for validating remote patches before changing the baseline.
	check: RefCell<crate::merge::CheckScratch>,

	/// The reconstructed value, `None` until the first snapshot.
	current: Option<Value>,

	_marker: PhantomData<fn() -> T>,
}

impl<T> Decoder<T> {
	/// Create a decoder with no value, awaiting its first [`snapshot`](Self::snapshot).
	pub fn new(config: Config) -> Self {
		Self {
			compression: config.compression.is_deflate(),
			max_size: config.max_size,
			flate: None,
			plain: Vec::new(),
			check: RefCell::new(crate::merge::CheckScratch::default()),
			current: None,
			_marker: PhantomData,
		}
	}

	/// Apply a group's first frame: a full snapshot that replaces the current value.
	///
	/// Also starts the group's DEFLATE window, so this must be called at every group boundary, not
	/// only the first.
	pub fn snapshot(&mut self, payload: &[u8]) -> Result<()> {
		self.current = None;
		// Every group starts a new DEFLATE window; enforce the budget while inflating.
		self.flate = self.compression.then(|| {
			moq_flate::Decoder::with_max_frame_size(self.max_size.map_or(moq_flate::DEFAULT_MAX_FRAME_SIZE, |size| {
				(size as u64).min(moq_flate::DEFAULT_MAX_FRAME_SIZE)
			}))
		});
		let inflated = self.flate.as_mut().map(|flate| flate.frame(payload)).transpose()?;
		let plain = inflated.as_deref().unwrap_or(payload);
		if let Some(limit) = self.max_size
			&& plain.len() > limit
		{
			return Err(Error::TooLarge(limit));
		}
		self.current = Some(serde_json::from_slice(plain)?);
		self.check_size()
	}

	/// Apply a merge patch, refusing frames or reconstructed state beyond the configured budget.
	/// A failed patch invalidates the baseline; start a new snapshot before applying more deltas.
	pub fn delta(&mut self, payload: &[u8]) -> Result<()> {
		if self.current.is_none() {
			return Err(Error::MissingSnapshot);
		}
		let result = (|| {
			let plain = match self.flate.as_mut() {
				Some(flate) => {
					flate.frame_into(payload, &mut self.plain)?;
					self.plain.as_slice()
				}
				None => payload,
			};
			if let Some(limit) = self.max_size
				&& plain.len() > limit
			{
				return Err(Error::TooLarge(limit));
			}
			crate::merge::apply_bytes(
				self.current.as_mut().expect("a snapshot precedes any delta"),
				plain,
				&self.check,
			)?;
			self.check_size()
		})();
		if result.is_err() {
			self.current = None;
		}
		result
	}

	fn check_size(&mut self) -> Result<()> {
		let Some(limit) = self.max_size else { return Ok(()) };
		// Count the compact representation without allocating another full snapshot.
		// A patch and the prior state are each bounded, and an oversized result is
		// released before it can be materialized or used by another patch.
		if serde_json::to_writer(Budget(limit), self.current.as_ref().unwrap()).is_err() {
			self.current = None;
			return Err(Error::TooLarge(limit));
		}
		Ok(())
	}

	/// The reconstructed value as raw JSON, or `None` before the first snapshot.
	pub fn value(&self) -> Option<&Value> {
		self.current.as_ref()
	}
}

struct Budget(usize);

impl std::io::Write for Budget {
	fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
		if bytes.len() > self.0 {
			return Err(std::io::Error::other("JSON size budget exceeded"));
		}
		self.0 -= bytes.len();
		Ok(bytes.len())
	}

	fn flush(&mut self) -> std::io::Result<()> {
		Ok(())
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

		// Tracking the path allocates for every key walked, which dwarfed the decode itself on large
		// documents, so it only runs again to explain a failure.
		if let Ok(value) = T::deserialize(current) {
			return Ok(Some(value));
		}

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
	use super::super::consumer::Config as ConsumerConfig;
	use super::super::{Config, Encoder};
	use super::*;
	use crate::Compression;
	use serde_json::{Value, json};

	fn consume(compression: Compression) -> ConsumerConfig {
		ConsumerConfig {
			compression,
			..Default::default()
		}
	}

	fn deflate() -> Config {
		Config {
			compression: Compression::Deflate,
			..Default::default()
		}
	}

	/// Round-trip a sequence of values through an encoder and decoder, yielding the value the
	/// decoder reconstructs after each frame.
	fn roundtrip(config: Config, values: &[Value]) -> Vec<Value> {
		let compression = config.compression;
		let mut encoder = Encoder::<Value>::new(config);
		let mut decoder = Decoder::<Value>::new(consume(compression));

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
	fn inflated_snapshots_and_patches_obey_the_budget() {
		for snapshot in [true, false] {
			let mut config = consume(Compression::Deflate);
			config.max_size = Some(1024);
			let mut decoder = Decoder::<Value>::new(config);
			let mut encoder = moq_flate::Encoder::new();
			if !snapshot {
				decoder.snapshot(&encoder.frame(b"{}")).unwrap();
			}
			let payload = encoder.frame(
				serde_json::to_string(&json!({"large": "x".repeat(4096)}))
					.unwrap()
					.as_bytes(),
			);
			assert!(payload.len() < 1024);
			let result = if snapshot {
				decoder.snapshot(&payload)
			} else {
				decoder.delta(&payload)
			};
			assert!(matches!(result, Err(Error::Flate(moq_flate::Error::TooLarge(1024)))));
			assert!(decoder.value().is_none());
		}
	}

	#[test]
	fn patches_cannot_accumulate_past_the_budget() {
		for compression in [Compression::None, Compression::Deflate] {
			let mut config = consume(compression);
			config.max_size = Some(13);
			let mut decoder = Decoder::<Value>::new(config);
			let mut encoder = moq_flate::Encoder::new();
			let mut payload = |bytes: &[u8]| {
				if compression == Compression::Deflate {
					encoder.frame(bytes).to_vec()
				} else {
					bytes.to_vec()
				}
			};
			decoder.snapshot(&payload(br#"{"a":1}"#)).unwrap();
			decoder.delta(&payload(br#"{"b":2}"#)).unwrap(); // exactly 13 bytes
			assert_eq!(decoder.value(), Some(&json!({"a":1,"b":2})));
			decoder.delta(&payload(br#"{"a":null}"#)).unwrap(); // deletion frees budget
			decoder.delta(&payload(br#"{"c":3}"#)).unwrap();
			assert!(matches!(
				decoder.delta(&payload(br#"{"d":4}"#)),
				Err(Error::TooLarge(13))
			));
			assert!(decoder.value().is_none());
			assert!(matches!(decoder.delta(b"{}"), Err(Error::MissingSnapshot)));
			// A fresh group can recover after refusal.
			let bytes = if compression == Compression::Deflate {
				moq_flate::Encoder::new().frame(b"{}").to_vec()
			} else {
				b"{}".to_vec()
			};
			decoder.snapshot(&bytes).unwrap();
		}
	}

	#[test]
	fn plain_frames_obey_the_budget_before_parsing() {
		let mut config = ConsumerConfig::default();
		config.max_size = Some(2);
		let mut decoder = Decoder::<Value>::new(config);
		assert!(matches!(decoder.snapshot(b"not json"), Err(Error::TooLarge(2))));
		decoder.snapshot(b"{}").unwrap();
		assert!(matches!(decoder.delta(b"not json"), Err(Error::TooLarge(2))));
		assert!(decoder.value().is_none());
	}

	#[test]
	fn plaintext_roundtrip() {
		let values = vec![
			json!({ "a": 1, "b": 1 }),
			json!({ "a": 1, "b": 2 }),
			json!({ "a": 5, "b": 2 }),
		];
		assert_eq!(roundtrip(Config::default(), &values), values);
	}

	#[test]
	fn compressed_roundtrip() {
		let values = vec![
			json!({ "a": 1, "b": 1 }),
			json!({ "a": 1, "b": 2 }),
			json!({ "a": 5, "b": 2 }),
		];
		assert_eq!(roundtrip(deflate(), &values), values);
	}

	/// The window is per group, so a keyframe mid-stream has to restart it on both sides. A decoder
	/// that kept the old window here would fail to inflate the new group's snapshot.
	#[test]
	fn compressed_roundtrip_across_a_group_boundary() {
		// A tight ratio guarantees at least one roll partway through.
		let values: Vec<Value> = (0..=40).map(|n| json!({ "n": n })).collect();
		let config = deflate().with_delta_ratio(2);
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
		let mut encoder = Encoder::<Value>::new(Config::default().with_delta_ratio(100));
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
