use std::num::NonZeroU64;

/// A bitrate cap on one direction of a session, in bits per second.
///
/// Positive, and at most 2^53 - 1 so every JSON reader holds it exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct Rate(NonZeroU64);

/// A bitrate that is zero or past JSON's exact integer range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("rate must be a positive integer of at most 9007199254740991 bits per second")]
pub struct InvalidRate;

impl Rate {
	/// The largest rate JSON carries exactly: 2^53 - 1 bits per second.
	pub const MAX: Self = Self(NonZeroU64::new((1 << 53) - 1).expect("non-zero"));

	/// A cap of `bps` bits per second.
	pub fn from_bps(bps: u64) -> Result<Self, InvalidRate> {
		match NonZeroU64::new(bps) {
			Some(bps) if bps <= Self::MAX.0 => Ok(Self(bps)),
			_ => Err(InvalidRate),
		}
	}

	/// This cap in bits per second.
	pub fn as_bps(self) -> u64 {
		self.0.get()
	}
}

impl TryFrom<u64> for Rate {
	type Error = InvalidRate;

	fn try_from(bps: u64) -> Result<Self, Self::Error> {
		Self::from_bps(bps)
	}
}

impl From<Rate> for u64 {
	fn from(rate: Rate) -> Self {
		rate.as_bps()
	}
}

impl std::fmt::Display for Rate {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}bps", self.0)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn refuses_zero_and_inexact_rates() {
		assert_eq!(Rate::from_bps(0), Err(InvalidRate));
		assert_eq!(Rate::from_bps(1 << 53), Err(InvalidRate));
		assert_eq!(Rate::from_bps((1 << 53) - 1), Ok(Rate::MAX));
		for json in ["0", "-1", "1.5", "9007199254740992", "null", "\"8000\""] {
			assert!(serde_json::from_str::<Rate>(json).is_err(), "{json}");
		}
		assert_eq!(serde_json::from_str::<Rate>("8000").unwrap().as_bps(), 8000);
	}

	/// The vectors `js/auth` reads too, so both languages agree on the capped wire shape.
	#[test]
	fn shared_wire_vectors() {
		let vectors: serde_json::Value = serde_json::from_str(include_str!("../rate.json")).unwrap();
		for row in vectors["valid"].as_array().unwrap() {
			let claims: crate::Claims = serde_json::from_value(row["input"].clone()).unwrap();
			claims.validate().unwrap();
			assert_eq!(serde_json::to_value(&claims).unwrap(), row["claims"]);
			let grant: crate::Grant = serde_json::from_value(row["input"].clone()).unwrap();
			grant.validate().unwrap();
			assert_eq!(serde_json::to_value(&grant).unwrap(), row["grant"]);
		}
		for input in vectors["invalid"].as_array().unwrap() {
			assert!(
				serde_json::from_value::<crate::Claims>(input.clone()).is_err(),
				"{input}"
			);
			assert!(
				serde_json::from_value::<crate::Grant>(input.clone()).is_err(),
				"{input}"
			);
		}
	}

	/// A cap survives signing and rebasing, and a key scope bounds paths, never bitrate.
	#[test]
	fn caps_survive_signing_but_scopes_refuse_them() {
		let wire = serde_json::json!({
			"root": "room",
			"publish": {"path": ["alice/**"], "rate": 8000},
			"subscribe": {"path": ["bob/**"], "rate": 16000},
		});
		assert!(serde_json::from_value::<crate::Scope>(wire.clone()).is_err());
		let claims: crate::Claims = serde_json::from_value(wire).unwrap();
		let key = crate::Key::generate(crate::Algorithm::HS256, None).unwrap();
		let claims = key.verify(&key.sign(&claims).unwrap()).unwrap();
		assert_eq!(claims.publish_rate, Some(Rate::from_bps(8000).unwrap()));
		assert_eq!(claims.subscribe_rate, Some(Rate::from_bps(16000).unwrap()));
		let scope: crate::Scope =
			serde_json::from_str(r#"{"root":"room","publish":["**"],"subscribe":["**"]}"#).unwrap();
		assert!(scope.allows(&claims));
	}
}
