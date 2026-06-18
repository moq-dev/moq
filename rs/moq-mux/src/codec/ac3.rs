//! Dolby Digital (AC-3).
//!
//! Broadcast audio carried verbatim: each sync frame is published whole. The
//! header is parsed only to find frame boundaries and the catalog config
//! (sample rate, channels); the audio is never decoded.

use super::legacy;

pub(crate) static DESCRIPTOR: legacy::Descriptor = legacy::Descriptor {
	track_suffix: ".ac3",
	codec: hang::catalog::AudioCodec::Ac3,
	min_header_len: 7,
	parse: parse_header,
};

// Bitrates in kbps indexed by frmsizecod >> 1 (A/52 Table 5.18).
const BITRATE: [u32; 19] = [
	32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 576, 640,
];

// Full-bandwidth channels per acmod: 1+1, 1/0, 2/0, 3/0, 2/1, 3/1, 2/2, 3/2.
const CHANNELS: [u32; 8] = [2, 1, 2, 3, 3, 4, 4, 5];

// An AC-3 sync frame is always 6 audio blocks of 256 samples.
const SAMPLES_PER_FRAME: u64 = 1536;

/// Parse an AC-3 sync frame header from the start of `data` (needs >= 7 bytes).
pub(crate) fn parse_header(data: &[u8]) -> legacy::Result<legacy::Header> {
	if data.len() < 7 {
		return Err(legacy::Error::Ac3HeaderTooShort);
	}
	if !(data[0] == 0x0B && data[1] == 0x77) {
		return Err(legacy::Error::Ac3MissingSyncWord);
	}

	let fscod = data[4] >> 6;
	let frmsizecod = (data[4] & 0x3F) as usize;
	if frmsizecod > 37 {
		return Err(legacy::Error::Ac3InvalidFrameSizeCode);
	}
	let bitrate_kbps = BITRATE[frmsizecod >> 1] as usize;

	// bsid > 8 is E-AC-3 or a low-sample-rate variant, neither parsed here.
	let bsid = data[5] >> 3;
	if bsid > 8 {
		return Err(legacy::Error::Ac3UnsupportedBsid(bsid));
	}

	// At 44.1 kHz the frame doesn't divide evenly, so the low frmsizecod bit
	// selects the padded size (A/52 Table 5.18).
	let (sample_rate, len) = match fscod {
		0b00 => (48000, 4 * bitrate_kbps),
		0b01 => (44100, 2 * (320 * bitrate_kbps / 147 + (frmsizecod & 1))),
		0b10 => (32000, 6 * bitrate_kbps),
		_ => return Err(legacy::Error::Ac3ReservedSampleRate),
	};

	// acmod decides which mix-level fields precede lfeon; skip them bit by bit.
	// Worst case (3/2) lands lfeon on the last bit of byte 6, so it never
	// crosses into byte 7.
	let acmod = data[6] >> 5;
	let mut bit = 3;
	if acmod & 0x01 != 0 && acmod != 0x01 {
		bit += 2; // cmixlev
	}
	if acmod & 0x04 != 0 {
		bit += 2; // surmixlev
	}
	if acmod == 0x02 {
		bit += 2; // dsurmod
	}
	let lfeon = (data[6] >> (7 - bit)) & 0x01;
	let channel_count = CHANNELS[acmod as usize] + lfeon as u32;

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
	fn parses_5_1_sync_frame() {
		// 48 kHz, 384 kbps (frmsizecod 28): 4 * 384 = 1536 bytes. acmod 3/2 with
		// lfeon on the last bit of byte 6: 5 full channels + LFE = 6.
		let h = parse_header(&[0x0B, 0x77, 0x00, 0x00, 0x1C, 0x40, 0xE1]).unwrap();
		assert_eq!(
			(h.len, h.sample_rate, h.channel_count, h.samples),
			(1536, 48_000, 6, 1536)
		);
	}

	#[test]
	fn rejects_eac3_bsid() {
		// bsid 16 is E-AC-3, which has its own parser and stream type (0x87).
		assert!(parse_header(&[0x0B, 0x77, 0x00, 0x00, 0x1C, 0x80, 0xE1]).is_err());
	}
}
