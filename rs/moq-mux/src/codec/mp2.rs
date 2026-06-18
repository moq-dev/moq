//! MPEG-1/2 Audio Layer II (MP2).
//!
//! Broadcast audio carried verbatim: each frame is published whole, header
//! included (unlike AAC, where the ADTS framing header is stripped). The header
//! is parsed only to find frame boundaries and the catalog config (sample rate,
//! channels); the audio is never decoded.

use super::legacy;

pub(crate) static DESCRIPTOR: legacy::Descriptor = legacy::Descriptor {
	track_suffix: ".mp2",
	codec: hang::catalog::AudioCodec::Mp2,
	min_header_len: 4,
	parse: parse_header,
};

#[derive(Clone, Copy)]
enum Version {
	Mpeg1,
	Mpeg2,
}

// Layer II bitrate tables in kbps, indexed by the 4-bit bitrate field. Index 0 is
// "free format" and 15 is invalid; both map to 0 and are rejected.
const BITRATE_MPEG1_L2: [u32; 16] = [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 0];
const BITRATE_MPEG2_L2: [u32; 16] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0];

// Sample rates in Hz, indexed by the 2-bit field, per MPEG version (1 / 2).
const SAMPLE_RATE: [[u32; 3]; 2] = [[44100, 48000, 32000], [22050, 24000, 16000]];

// Layer II is always 1152 samples per frame, across MPEG versions.
const SAMPLES_PER_FRAME: u64 = 1152;

/// Parse a Layer II frame header from the start of `data` (needs >= 4 bytes).
pub(crate) fn parse_header(data: &[u8]) -> legacy::Result<legacy::Header> {
	if data.len() < 4 {
		return Err(legacy::Error::Mp2HeaderTooShort);
	}
	// Frame sync: 11 bits set (0xFFE).
	if !(data[0] == 0xFF && (data[1] & 0xE0) == 0xE0) {
		return Err(legacy::Error::Mp2MissingSync);
	}

	// 0b00 is the unofficial MPEG-2.5 extension: 13818-1 has no stream type for
	// it, so accepting it here would re-announce it as 0x04 on export and invent
	// wire semantics. Rejecting it keeps the export's sample-rate -> stream_type
	// derivation bijective (MPEG-1 >= 32 kHz <-> 0x03, MPEG-2 BC < 32 kHz <-> 0x04).
	let (version, sr_row) = match (data[1] >> 3) & 0x03 {
		0b11 => (Version::Mpeg1, 0),
		0b10 => (Version::Mpeg2, 1),
		_ => return Err(legacy::Error::Mp2ReservedVersion),
	};
	// Layer field 0b10 is Layer II.
	if (data[1] >> 1) & 0x03 != 0b10 {
		return Err(legacy::Error::Mp2NotLayerII);
	}

	let bitrate_index = (data[2] >> 4) & 0x0F;
	let sr_index = (data[2] >> 2) & 0x03;
	let padding = ((data[2] >> 1) & 0x01) as usize;

	if sr_index == 3 {
		return Err(legacy::Error::Mp2ReservedSampleRate);
	}
	let sample_rate = SAMPLE_RATE[sr_row][sr_index as usize];

	let bitrate_kbps = match version {
		Version::Mpeg1 => BITRATE_MPEG1_L2[bitrate_index as usize],
		Version::Mpeg2 => BITRATE_MPEG2_L2[bitrate_index as usize],
	};
	if bitrate_kbps == 0 {
		return Err(legacy::Error::Mp2InvalidBitrate);
	}

	// Layer II is always 1152 samples, so the frame is 144 * bitrate / sample_rate bytes.
	let len = (144 * bitrate_kbps * 1000 / sample_rate) as usize + padding;

	// Channel mode 0b11 is single channel (mono); stereo/joint/dual are 2 channels.
	let channel_count = if (data[3] >> 6) & 0x03 == 0b11 { 1 } else { 2 };

	Ok(legacy::Header {
		len,
		sample_rate,
		channel_count,
		samples: SAMPLES_PER_FRAME,
	})
}

#[cfg(test)]
mod test {
	use super::parse_header;

	#[test]
	fn parses_mpeg1_and_mpeg2() {
		// MPEG-1 Layer II, 32 kbps, 48 kHz, stereo: 144 * 32000 / 48000 = 96 bytes.
		let h = parse_header(&[0xFF, 0xFD, 0x14, 0x00]).unwrap();
		assert_eq!(
			(h.len, h.sample_rate, h.channel_count, h.samples),
			(96, 48_000, 2, 1152)
		);

		// MPEG-2 BC Layer II, 8 kbps, 16 kHz, mono: 144 * 8000 / 16000 = 72 bytes.
		let h = parse_header(&[0xFF, 0xF5, 0x18, 0xC0]).unwrap();
		assert_eq!(
			(h.len, h.sample_rate, h.channel_count, h.samples),
			(72, 16_000, 1, 1152)
		);
	}

	#[test]
	fn rejects_mpeg2_5_and_reserved_versions() {
		// MPEG-2.5 (version bits 0b00) has no TS stream type; 0b01 is reserved.
		assert!(parse_header(&[0xFF, 0xE5, 0x14, 0x00]).is_err());
		assert!(parse_header(&[0xFF, 0xED, 0x14, 0x00]).is_err());
	}
}
