//! The track-free half of stream consuming: frame payloads in, records out.

use std::marker::PhantomData;

use serde::de::DeserializeOwned;

use super::Config;
use crate::Result;

/// Decodes JSON records from frame payloads, sharing one DEFLATE window across the log.
///
/// The track-free core of [`Consumer`](super::Consumer), and the mirror of
/// [`Encoder`](super::Encoder). Payloads must be fed in the order they were encoded, since each one
/// builds on the window the earlier ones left behind. Call [`reset`](Self::reset) at a group
/// boundary, matching the encoder.
pub struct Decoder<T> {
	/// The DEFLATE decoder (one window for the whole log), `Some` while decompressing.
	flate: Option<moq_flate::Decoder>,
	compression: bool,
	_marker: PhantomData<fn() -> T>,
}

impl<T> Decoder<T> {
	/// Create a decoder with a cold window.
	pub fn new(config: Config) -> Self {
		Self {
			flate: config.compression.is_deflate().then(moq_flate::Decoder::new),
			compression: config.compression.is_deflate(),
			_marker: PhantomData,
		}
	}

	/// Start a cold DEFLATE window, for a caller that has just moved to a new group.
	pub fn reset(&mut self) {
		self.flate = self.compression.then(moq_flate::Decoder::new);
	}
}

impl<T: DeserializeOwned> Decoder<T> {
	/// Decode the next frame payload back into a record.
	pub fn decode(&mut self, payload: &[u8]) -> Result<T> {
		Ok(match self.flate.as_mut() {
			Some(flate) => serde_json::from_slice(&flate.frame(payload)?)?,
			None => serde_json::from_slice(payload)?,
		})
	}
}

#[cfg(test)]
mod test {
	use super::super::{Config, Encoder};
	use super::*;
	use crate::Compression;
	use serde_json::{Value, json};

	fn cfg(compression: bool) -> Config {
		Config {
			compression: if compression {
				Compression::Deflate
			} else {
				Compression::None
			},
		}
	}

	/// Round-trip a sequence of records through an encoder and decoder.
	fn roundtrip(compression: bool, values: &[Value]) -> Vec<Value> {
		let mut encoder = Encoder::<Value>::new(cfg(compression));
		let mut decoder = Decoder::<Value>::new(cfg(compression));

		values
			.iter()
			.map(|value| {
				let record = encoder.encode(value).unwrap();
				let decoded = decoder.decode(record.payload()).unwrap();
				record.commit();
				decoded
			})
			.collect()
	}

	#[test]
	fn plaintext_roundtrip_in_order() {
		let values: Vec<Value> = (0..5).map(|n| json!({ "n": n })).collect();
		assert_eq!(roundtrip(false, &values), values);
	}

	#[test]
	fn compressed_roundtrip_in_order() {
		let values: Vec<Value> = (0..20).map(|n| json!({ "group": n, "pts": n * 2_000 })).collect();
		assert_eq!(roundtrip(true, &values), values);
	}

	#[test]
	fn the_shared_window_shrinks_repetitive_records() {
		let mut encoder = Encoder::<Value>::new(cfg(true));
		let sizes: Vec<usize> = (0..8)
			.map(|n| {
				let record = encoder.encode(&json!({ "group": n, "pts": n * 2_000 })).unwrap();
				let len = record.payload().len();
				record.commit();
				len
			})
			.collect();

		let raw = serde_json::to_vec(&json!({ "group": 7, "pts": 14_000 })).unwrap().len();
		assert!(
			*sizes.last().unwrap() < raw / 2,
			"windowed record {} should be far below its raw size {raw}",
			sizes.last().unwrap()
		);
	}

	/// A caller that rolls a group has to restart both windows, or the new group's frames decode
	/// against context the decoder on the other side never received.
	#[test]
	fn reset_starts_a_cold_window_on_both_sides() {
		let mut encoder = Encoder::<Value>::new(cfg(true));
		let mut decoder = Decoder::<Value>::new(cfg(true));

		for n in 0..4 {
			let record = encoder.encode(&json!({ "n": n })).unwrap();
			assert_eq!(decoder.decode(record.payload()).unwrap(), json!({ "n": n }));
			record.commit();
		}

		encoder.reset();
		decoder.reset();

		let record = encoder.encode(&json!({ "n": 99 })).unwrap();
		assert_eq!(decoder.decode(record.payload()).unwrap(), json!({ "n": 99 }));
		record.commit();
	}
}

#[cfg(test)]
mod desync_test {
	use super::super::{Config, Encoder};
	use super::*;
	use crate::Compression;
	use serde_json::{Value, json};

	fn deflate() -> Config {
		Config {
			compression: Compression::Deflate,
		}
	}

	/// A compressed record that never reached the wire leaves the window ahead of the consumer, and a
	/// log has no keyframe to resynchronize on. Continuing would emit frames nothing can decode, so
	/// the encoder refuses until the caller rolls a new group and resets.
	#[test]
	fn an_uncommitted_compressed_record_stops_the_encoder() {
		let mut encoder = Encoder::<Value>::new(deflate());
		encoder.encode(&json!({ "n": 0 })).unwrap().commit();

		// This one fails to write, so the caller never commits it.
		drop(encoder.encode(&json!({ "n": 1 })).unwrap());

		assert!(matches!(encoder.encode(&json!({ "n": 2 })), Err(crate::Error::Desync)));

		// Rolling a new group gives the consumer a cold window too, so the reset clears it.
		encoder.reset();
		let record = encoder.encode(&json!({ "n": 2 })).unwrap();
		let mut decoder = Decoder::<Value>::new(deflate());
		assert_eq!(decoder.decode(record.payload()).unwrap(), json!({ "n": 2 }));
		record.commit();
	}

	/// Without compression a record carries no shared state, so a dropped one leaves a gap in the log
	/// rather than an undecodable stream, and the encoder keeps going.
	#[test]
	fn an_uncommitted_plaintext_record_does_not_stop_the_encoder() {
		let mut encoder = Encoder::<Value>::new(Config::default());
		drop(encoder.encode(&json!({ "n": 0 })).unwrap());

		let record = encoder
			.encode(&json!({ "n": 1 }))
			.expect("plaintext records are independent");
		let mut decoder = Decoder::<Value>::new(Config::default());
		assert_eq!(decoder.decode(record.payload()).unwrap(), json!({ "n": 1 }));
		record.commit();
	}
}
