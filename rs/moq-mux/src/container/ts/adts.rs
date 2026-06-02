//! ADTS framing for AAC.
//!
//! TS carries AAC as ADTS: every raw AAC frame is prefixed by a 7-byte header
//! (9 with CRC). The moq-mux AAC codec layer works in raw AAC plus an
//! AudioSpecificConfig, so import strips ADTS headers (and synthesizes a
//! config from the first one) while export re-frames raw AAC into ADTS.

use anyhow::Context;

/// ISO 14496-3 sampling-frequency-index table.
const SAMPLE_RATES: [u32; 13] = [
	96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];

/// Fields decoded from an ADTS fixed+variable header.
#[derive(Clone, Copy, Debug)]
pub(super) struct Header {
	/// audioObjectType (ADTS `profile` + 1). AAC-LC is 2.
	pub object_type: u8,
	pub sample_rate: u32,
	pub channel_count: u32,
	/// Total access-unit length, header included.
	pub frame_len: usize,
	/// Header length: 7 without CRC, 9 with.
	pub header_len: usize,
}

impl Header {
	/// Parse the ADTS header at the start of `data`.
	pub fn parse(data: &[u8]) -> anyhow::Result<Self> {
		anyhow::ensure!(data.len() >= 7, "ADTS header truncated");
		anyhow::ensure!(data[0] == 0xFF && (data[1] & 0xF0) == 0xF0, "missing ADTS syncword");

		let protection_absent = data[1] & 0x01;
		let profile = (data[2] >> 6) & 0x03;
		let freq_index = (data[2] >> 2) & 0x0F;
		let channel_config = ((data[2] & 0x01) << 2) | ((data[3] >> 6) & 0x03);
		let frame_len =
			((data[3] as usize & 0x03) << 11) | ((data[4] as usize) << 3) | ((data[5] as usize >> 5) & 0x07);

		let sample_rate = SAMPLE_RATES
			.get(freq_index as usize)
			.copied()
			.context("reserved ADTS sample rate index")?;
		let header_len = if protection_absent == 1 { 7 } else { 9 };
		anyhow::ensure!(frame_len >= header_len, "ADTS frame length smaller than its header");

		Ok(Self {
			object_type: profile + 1,
			sample_rate,
			channel_count: channel_count_from_config(channel_config),
			frame_len,
			header_len,
		})
	}
}

/// Build a 7-byte ADTS header (no CRC) for a raw AAC frame of `raw_len` bytes.
pub(super) fn write_header(
	object_type: u8,
	sample_rate: u32,
	channel_count: u32,
	raw_len: usize,
) -> anyhow::Result<[u8; 7]> {
	// ADTS `profile` is the 2-bit audioObjectType - 1.
	let profile = object_type.saturating_sub(1) & 0x03;
	let freq_index = freq_index_from_rate(sample_rate)?;
	let channel_config = channel_config_from_count(channel_count);

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

/// Map an AAC `channel_config` (ISO 14496-3 Table 1.19) to a channel count.
fn channel_count_from_config(channel_config: u8) -> u32 {
	match channel_config {
		1..=6 => channel_config as u32,
		7 => 8,
		_ => 2,
	}
}

/// Inverse of [`channel_count_from_config`].
fn channel_config_from_count(channel_count: u32) -> u8 {
	match channel_count {
		1..=6 => channel_count as u8,
		8 => 7,
		_ => 2,
	}
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
		assert_eq!(parsed.channel_count, 2);
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
	fn frame_len_for_5_1() {
		let header = write_header(2, 44_100, 6, 512).unwrap();
		let parsed = Header::parse(&header).unwrap();
		assert_eq!(parsed.channel_count, 6);
		assert_eq!(parsed.sample_rate, 44_100);
		assert_eq!(parsed.frame_len, 519);
	}
}
