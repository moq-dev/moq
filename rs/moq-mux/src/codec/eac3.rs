//! Dolby Digital Plus (E-AC-3 / Enhanced AC-3).
//!
//! Broadcast audio carried verbatim: each sync frame is published whole. The
//! header is parsed only to find frame boundaries and the catalog config
//! (sample rate, channels); the audio is never decoded.
//!
//! Scope: a single independent substream (strmtyp 0 or 2, substreamid 0), the
//! shape broadcast encoders emit for up to 5.1. Dependent substreams (7.1 and
//! beyond) and additional substream ids are rejected explicitly.

use super::legacy;

pub(crate) static DESCRIPTOR: legacy::Descriptor = legacy::Descriptor {
	track_suffix: ".eac3",
	codec: hang::catalog::AudioCodec::Ec3,
	min_header_len: 6,
	parse: parse_header,
};

const SAMPLE_RATE: [u32; 3] = [48000, 44100, 32000];
// Reduced rates selected by fscod2 when fscod is 0b11.
const SAMPLE_RATE_REDUCED: [u32; 3] = [24000, 22050, 16000];
// Audio blocks per frame, indexed by numblkscod; each block is 256 samples.
const BLOCKS: [u64; 4] = [1, 2, 3, 6];
// Full-bandwidth channels per acmod: 1+1, 1/0, 2/0, 3/0, 2/1, 3/1, 2/2, 3/2.
const CHANNELS: [u32; 8] = [2, 1, 2, 3, 3, 4, 4, 5];

/// Parse an E-AC-3 sync frame header from the start of `data` (needs >= 6 bytes).
pub(crate) fn parse_header(data: &[u8]) -> legacy::Result<legacy::Header> {
	if data.len() < 6 {
		return Err(legacy::Error::Eac3HeaderTooShort);
	}
	if !(data[0] == 0x0B && data[1] == 0x77) {
		return Err(legacy::Error::Eac3MissingSyncWord);
	}

	// bsid 11..=16 is E-AC-3; plain AC-3 (bsid <= 8) is routed by stream_type and
	// never reaches this parser.
	let bsid = data[5] >> 3;
	if !(11..=16).contains(&bsid) {
		return Err(legacy::Error::Eac3NotEac3Bsid(bsid));
	}

	// A dependent substream (strmtyp 1) extends a prior program to 7.1+, and a
	// nonzero substreamid carries additional programs in the same PID. Either
	// would make this track's channel count a lie, so they are rejected rather
	// than mis-described.
	let strmtyp = data[2] >> 6;
	if strmtyp == 3 {
		return Err(legacy::Error::Eac3ReservedStreamType);
	}
	if strmtyp == 1 {
		return Err(legacy::Error::Eac3DependentSubstream);
	}
	let substreamid = (data[2] >> 3) & 0x07;
	if substreamid != 0 {
		return Err(legacy::Error::Eac3AdditionalSubstream(substreamid));
	}

	let frmsiz = (((data[2] & 0x07) as usize) << 8) | data[3] as usize;
	let len = (frmsiz + 1) * 2;
	// frmsiz is a raw field; corrupt input can declare a "frame" shorter than the
	// header just parsed, which would surface later as a confusing sync error.
	if len < 6 {
		return Err(legacy::Error::Eac3FrameShorterThanHeader(len));
	}

	let fscod = data[4] >> 6;
	let (sample_rate, blocks) = if fscod == 0b11 {
		let fscod2 = (data[4] >> 4) & 0x03;
		if fscod2 == 3 {
			return Err(legacy::Error::Eac3ReservedSampleRate);
		}
		// Reduced rates always run 6 blocks.
		(SAMPLE_RATE_REDUCED[fscod2 as usize], 6)
	} else {
		(SAMPLE_RATE[fscod as usize], BLOCKS[((data[4] >> 4) & 0x03) as usize])
	};

	let acmod = (data[4] >> 1) & 0x07;
	let lfeon = data[4] & 0x01;

	Ok(legacy::Header {
		len,
		sample_rate,
		channel_count: CHANNELS[acmod as usize] + lfeon as u32,
		samples: 256 * blocks,
	})
}

#[cfg(test)]
mod test {
	use super::parse_header;

	#[test]
	fn parses_independent_substream() {
		// strmtyp 0, substreamid 0, frmsiz 255 (512 bytes), 48 kHz, 6 blocks,
		// acmod 3/2 + lfeon = 6 channels, 1536 samples.
		let h = parse_header(&[0x0B, 0x77, 0x00, 0xFF, 0x3F, 0x80]).unwrap();
		assert_eq!(
			(h.len, h.sample_rate, h.channel_count, h.samples),
			(512, 48_000, 6, 1536)
		);

		// Reduced rate: fscod 0b11 + fscod2 0 selects 24 kHz, always 6 blocks.
		let h = parse_header(&[0x0B, 0x77, 0x00, 0xFF, 0xCF, 0x80]).unwrap();
		assert_eq!(
			(h.len, h.sample_rate, h.channel_count, h.samples),
			(512, 24_000, 6, 1536)
		);
	}

	#[test]
	fn rejects_reserved_codes() {
		// strmtyp 3 is reserved.
		let err = parse_header(&[0x0B, 0x77, 0xC0, 0xFF, 0x3F, 0x80]).unwrap_err();
		assert!(err.to_string().contains("reserved E-AC-3 stream type"), "{err}");

		// fscod 0b11 + fscod2 0b11 is a reserved sample-rate code.
		let err = parse_header(&[0x0B, 0x77, 0x00, 0xFF, 0xFF, 0x80]).unwrap_err();
		assert!(err.to_string().contains("reserved E-AC-3 sample-rate code"), "{err}");

		// frmsiz 1 declares a 4-byte "frame", shorter than the 6-byte header.
		let err = parse_header(&[0x0B, 0x77, 0x00, 0x01, 0x3F, 0x80]).unwrap_err();
		assert!(err.to_string().contains("shorter than its header"), "{err}");
	}

	#[test]
	fn rejects_out_of_scope_substreams() {
		// Dependent substream (strmtyp 1): a 7.1+ extension of a prior program,
		// not a standalone track. The error must say so; this is the first thing
		// a real 7.1 feed will hit.
		let err = parse_header(&[0x0B, 0x77, 0x40, 0xFF, 0x3F, 0x80]).unwrap_err();
		assert!(err.to_string().contains("dependent substream"), "{err}");

		// A second program multiplexed in the same PID (substreamid 1).
		let err = parse_header(&[0x0B, 0x77, 0x08, 0xFF, 0x3F, 0x80]).unwrap_err();
		assert!(err.to_string().contains("additional substream"), "{err}");

		// Plain AC-3 bsid (8): wrong bitstream for this parser.
		let err = parse_header(&[0x0B, 0x77, 0x00, 0xFF, 0x3F, 0x40]).unwrap_err();
		assert!(err.to_string().contains("bsid"), "{err}");
	}
}
