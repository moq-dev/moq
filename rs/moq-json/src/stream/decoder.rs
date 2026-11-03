//! The track-free half of stream consuming: frame payloads in, records out.

use std::cell::RefCell;
use std::marker::PhantomData;

use serde::de::DeserializeOwned;

use super::Config;
use crate::Result;

/// Decodes JSON records from frame payloads.
///
/// The track-free core of [`Consumer`](super::Consumer), and the mirror of
/// [`Encoder`](super::Encoder).
pub struct Decoder<T> {
	/// Reused key buffers for refusing a record that repeats a key.
	check: RefCell<crate::strict::Scratch>,
	_marker: PhantomData<fn() -> T>,
}

impl<T> Decoder<T> {
	/// Create a decoder.
	pub fn new(_config: Config) -> Self {
		Self {
			check: RefCell::default(),
			_marker: PhantomData,
		}
	}
}

impl<T: DeserializeOwned> Decoder<T> {
	/// Decode the next frame payload back into a record.
	pub fn decode(&mut self, payload: &[u8]) -> Result<T> {
		crate::strict::check(payload, &self.check)?;
		Ok(serde_json::from_slice(payload)?)
	}
}

#[cfg(test)]
mod test {
	use super::super::{Config, Encoder};
	use super::*;
	use serde_json::{Value, json};

	#[test]
	fn roundtrip_in_order() {
		let mut encoder = Encoder::<Value>::new(Config::default());
		let mut decoder = Decoder::<Value>::new(Config::default());
		for n in 0..20 {
			let value = json!({ "group": n, "pts": n * 2_000 });
			let record = encoder.encode(&value).unwrap();
			assert_eq!(decoder.decode(record.payload()).unwrap(), value);
			record.commit();
		}
	}

	#[test]
	fn duplicate_keys_are_refused() {
		let vectors = crate::strict::test::vectors();
		let mut decoder = Decoder::<Value>::new(Config::default());
		for payload in &vectors.refused {
			let err = decoder.decode(payload.as_bytes()).unwrap_err();
			assert!(matches!(err, crate::Error::DuplicateKey), "{payload}: {err}");
		}
		for payload in &vectors.accepted {
			decoder.decode(payload.as_bytes()).unwrap();
		}
	}

	/// A record carries no shared state, so a dropped one leaves a gap in the log rather than an
	/// undecodable stream, and the encoder keeps going.
	#[test]
	fn an_uncommitted_record_does_not_stop_the_encoder() {
		let mut encoder = Encoder::<Value>::new(Config::default());
		drop(encoder.encode(&json!({ "n": 0 })).unwrap());

		let record = encoder.encode(&json!({ "n": 1 })).expect("records are independent");
		let mut decoder = Decoder::<Value>::new(Config::default());
		assert_eq!(decoder.decode(record.payload()).unwrap(), json!({ "n": 1 }));
		record.commit();
	}
}
