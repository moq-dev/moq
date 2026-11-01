//! ADTS framing for AAC.
//!
//! TS carries AAC as ADTS: every raw AAC frame is prefixed by a 7-byte header
//! (9 with CRC). The moq-mux AAC codec layer works in raw AAC plus an
//! AudioSpecificConfig, so import strips ADTS headers (and synthesizes a
//! config from the first one) while export re-frames raw AAC into ADTS.

use anyhow::Context;

use super::SAMPLE_RATES;

/// An invalid ADTS header or frame.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// The fixed header is incomplete.
	#[error("ADTS header truncated")]
	Truncated,
	/// The header does not start with an ADTS syncword.
	#[error("missing ADTS syncword")]
	Syncword,
	/// The header names a reserved sample rate.
	#[error("reserved ADTS sample rate index")]
	SampleRate,
	/// The declared frame cannot contain its header.
	#[error("ADTS frame length smaller than its header")]
	FrameLength,
	/// A packet does not contain exactly one complete ADTS frame.
	#[error("ADTS packet must contain exactly one complete frame")]
	PacketLength,
}

/// Bytes needed to attempt a header parse: the fixed header without a CRC.
pub(crate) const MIN_HEADER_LEN: usize = 7;

/// Fields decoded from an ADTS fixed+variable header.
#[derive(Clone, Copy, Debug)]
pub struct Header {
	/// audioObjectType (ADTS `profile` + 1). AAC-LC is 2.
	pub object_type: u8,
	/// The sampling rate signaled by the ADTS core.
	pub sample_rate: u32,
	/// channelConfiguration; 0 means a program config element in the raw data block describes the channels.
	pub channel_config: u8,
	/// Total access-unit length, header included.
	pub(crate) frame_len: usize,
	/// The number of raw AAC blocks carried by this frame.
	pub blocks: u8,
	/// Header length, including CRC and raw-block position fields when present.
	pub(crate) header_len: usize,
}

impl Header {
	/// Parse the ADTS header at the start of `data`.
	pub fn parse(data: &[u8]) -> Result<Self, Error> {
		if data.len() < MIN_HEADER_LEN {
			return Err(Error::Truncated);
		}
		if data[0] != 0xFF || data[1] & 0xF6 != 0xF0 {
			return Err(Error::Syncword);
		}

		let protection_absent = data[1] & 0x01;
		let profile = (data[2] >> 6) & 0x03;
		let freq_index = (data[2] >> 2) & 0x0F;
		let channel_config = ((data[2] & 0x01) << 2) | ((data[3] >> 6) & 0x03);
		let frame_len =
			((data[3] as usize & 0x03) << 11) | ((data[4] as usize) << 3) | ((data[5] as usize >> 5) & 0x07);

		let sample_rate = SAMPLE_RATES
			.get(freq_index as usize)
			.copied()
			.ok_or(Error::SampleRate)?;
		let blocks = 1 + (data[6] & 3);
		let header_len = if protection_absent == 1 {
			7
		} else {
			7 + 2 * usize::from(blocks)
		};
		if frame_len < header_len {
			return Err(Error::FrameLength);
		}

		Ok(Self {
			object_type: profile + 1,
			sample_rate,
			channel_config,
			frame_len,
			blocks,
			header_len,
		})
	}
	/// Borrow the payload after the header of one complete ADTS frame.
	pub fn payload<'a>(&self, packet: &'a [u8]) -> Result<&'a [u8], Error> {
		if packet.len() != self.frame_len || self.header_len > packet.len() {
			return Err(Error::PacketLength);
		}
		Ok(&packet[self.header_len..])
	}
}

/// Build a 7-byte ADTS header (no CRC) for a raw AAC frame of `raw_len` bytes.
pub(crate) fn write_header(
	object_type: u8,
	sample_rate: u32,
	channel_config: u8,
	raw_len: usize,
) -> anyhow::Result<[u8; 7]> {
	// ADTS `profile` is the 2-bit audioObjectType - 1, so only Main, LC, SSR, and LTP fit.
	anyhow::ensure!(
		(1..=4).contains(&object_type),
		"audioObjectType {object_type} not representable in ADTS"
	);
	let profile = object_type - 1;
	let freq_index = freq_index_from_rate(sample_rate)?;
	// ADTS has 3 bits for it; the higher configurations only fit an AudioSpecificConfig.
	anyhow::ensure!(
		channel_config < 8,
		"channelConfiguration {channel_config} not representable in ADTS"
	);

	let frame_len = raw_len + 7;
	anyhow::ensure!(frame_len < (1 << 13), "AAC frame too large for ADTS framing");

	let mut h = [0u8; 7];
	h[0] = 0xFF;
	// syncword high nibble + MPEG-4 + layer 0 + protection_absent (no CRC).
	h[1] = 0xF1;
	h[2] = (profile << 6) | ((freq_index & 0x0F) << 2) | ((channel_config >> 2) & 0x01);
	h[3] = ((channel_config & 0x03) << 6) | ((frame_len >> 11) as u8 & 0x03);
	h[4] = ((frame_len >> 3) & 0xFF) as u8;
	// low 3 bits of frame_len, then buffer fullness = 0x7FF (variable rate).
	h[5] = (((frame_len & 0x07) << 5) as u8) | 0x1F;
	h[6] = 0xFC;
	Ok(h)
}

fn freq_index_from_rate(sample_rate: u32) -> anyhow::Result<u8> {
	// ADTS has no explicit-rate escape (unlike AudioSpecificConfig), so a rate
	// outside the table can't be represented. Fail rather than mislabel it.
	SAMPLE_RATES
		.iter()
		.position(|&r| r == sample_rate)
		.map(|i| i as u8)
		.with_context(|| format!("sample rate {sample_rate} not representable in ADTS"))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn header_roundtrip() {
		// AAC-LC (object_type 2), 48 kHz, stereo, 100-byte raw frame.
		let header = write_header(2, 48_000, 2, 100).unwrap();
		let parsed = Header::parse(&header).unwrap();

		assert_eq!(parsed.object_type, 2);
		assert_eq!(parsed.sample_rate, 48_000);
		assert_eq!(parsed.channel_config, 2);
		assert_eq!(parsed.header_len, 7);
		assert_eq!(parsed.frame_len, 107, "frame_len includes the 7-byte header");
	}

	#[test]
	fn parse_rejects_bad_syncword() {
		let mut header = write_header(2, 44_100, 2, 10).unwrap();
		header[0] = 0x00;
		assert!(Header::parse(&header).is_err());
	}

	#[test]
	fn write_refuses_object_types_outside_the_profile_field() {
		for object_type in [0, 5, 29] {
			assert!(write_header(object_type, 48_000, 2, 10).is_err());
		}
		for object_type in 1..=4 {
			let header = write_header(object_type, 48_000, 2, 10).unwrap();
			assert_eq!(Header::parse(&header).unwrap().object_type, object_type);
		}
	}

	#[test]
	fn frame_len_for_5_1() {
		let header = write_header(2, 44_100, 6, 512).unwrap();
		let parsed = Header::parse(&header).unwrap();
		assert_eq!(parsed.channel_config, 6);
		assert_eq!(parsed.sample_rate, 44_100);
		assert_eq!(parsed.frame_len, 519);
	}

	#[test]
	fn a_packet_holds_one_whole_frame_including_its_crc() {
		for crc in [false, true] {
			let mut packet = write_header(2, 44_100, 1, 3 + usize::from(crc) * 2).unwrap().to_vec();
			if crc {
				packet[1] &= !1;
				packet.extend_from_slice(&[0, 0]);
			}
			packet.extend_from_slice(&[1, 2, 3]);
			let header = Header::parse(&packet).unwrap();
			assert_eq!(header.payload(&packet).unwrap(), [1, 2, 3]);
			for end in 0..packet.len() {
				assert!(header.payload(&packet[..end]).is_err());
			}
			packet.push(0);
			assert!(
				header.payload(&packet).is_err(),
				"trailing bytes are not part of the frame"
			);
		}
	}

	#[test]
	fn an_adts_syncword_has_zero_layer_bits() {
		let mut packet = write_header(2, 44_100, 1, 1).unwrap();
		for layer in 1..=3 {
			packet[1] = 0xf1 | layer << 1;
			assert!(matches!(Header::parse(&packet), Err(Error::Syncword)));
		}
	}

	#[test]
	fn protected_multi_block_frames_keep_their_position_table_in_the_header() {
		let mut packet = write_header(2, 44_100, 1, 11).unwrap().to_vec();
		packet[1] &= !1;
		packet[6] |= 3;
		packet.extend_from_slice(&[0; 8]);
		packet.extend_from_slice(&[1, 2, 3]);
		let header = Header::parse(&packet).unwrap();
		assert_eq!(header.blocks, 4);
		assert_eq!(header.payload(&packet).unwrap(), [1, 2, 3]);
	}
}
