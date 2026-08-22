use crate::coding::*;

use super::{Message, Version};

/// Sent to probe the available bitrate and round-trip time.
///
/// Lite03+. Lite04 adds the `rtt` field.
/// On the wire, 0 means unknown (None). Some(0) is rounded up to Some(1).
#[derive(Clone, Debug)]
pub struct Probe {
	pub bitrate: Option<u64>,
	pub rtt: Option<u64>,
}

impl Message for Probe {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => {
				return Err(DecodeError::Version);
			}
			_ => {}
		}

		// 0 means unknown, the same as RTT below. A publisher whose transport
		// exposes no congestion controller reports the RTT half alone.
		let bitrate = match u64::decode(r, version)? {
			0 => None,
			v => Some(v),
		};
		let rtt = match version.has_probe_rtt() {
			false => None,
			true => match u64::decode(r, version)? {
				0 => None,
				v => Some(v),
			},
		};

		Ok(Self { bitrate, rtt })
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => {
				return Err(EncodeError::Version);
			}
			_ => {}
		}

		// 0 means unknown; round Some(0) up to 1.
		let wire = self.bitrate.map(|v| v.max(1)).unwrap_or(0);
		wire.encode(w, version)?;
		if version.has_probe_rtt() {
			let wire = self.rtt.map(|v| v.max(1)).unwrap_or(0);
			wire.encode(w, version)?;
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn round_trip(msg: &Probe, version: Version) -> Probe {
		let mut buf = bytes::BytesMut::new();
		msg.encode(&mut buf, version).unwrap();
		let mut slice = &buf[..];
		let got = Probe::decode(&mut slice, version).unwrap();
		assert!(bytes::Buf::remaining(&slice) == 0, "trailing bytes after decode");
		got
	}

	/// Both fields use 0 for unknown, independently, so a publisher that can
	/// measure only one still has a legal message to send.
	#[test]
	fn each_field_is_independently_unknown() {
		for (bitrate, rtt) in [
			(Some(1_000_000), Some(40)),
			(None, Some(40)),
			(Some(1_000_000), None),
			(None, None),
		] {
			let msg = Probe { bitrate, rtt };
			let got = round_trip(&msg, Version::Lite05);
			assert_eq!(got.bitrate, bitrate);
			assert_eq!(got.rtt, rtt);
		}
	}

	/// A measured zero is indistinguishable from unknown on the wire, so it rounds
	/// up to the smallest value that still reads as a measurement.
	#[test]
	fn measured_zero_rounds_up() {
		let got = round_trip(
			&Probe {
				bitrate: Some(0),
				rtt: Some(0),
			},
			Version::Lite05,
		);
		assert_eq!(got.bitrate, Some(1));
		assert_eq!(got.rtt, Some(1));
	}

	/// Lite03 predates the RTT field; the bitrate half still round-trips.
	#[test]
	fn lite03_carries_bitrate_only() {
		let got = round_trip(
			&Probe {
				bitrate: Some(1_000_000),
				rtt: Some(40),
			},
			Version::Lite03,
		);
		assert_eq!(got.bitrate, Some(1_000_000));
		assert_eq!(got.rtt, None);
	}
}
