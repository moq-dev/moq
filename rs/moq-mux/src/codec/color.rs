//! The catalog colour every video source resolves to.
//!
//! Codec syntax (H.264 and H.265 VUI, AV1 colour config, the VP9 codec string) and container boxes
//! (ISOBMFF `colr`, Matroska `Colour`) all signal ITU-T H.273 code points; [`cicp`] turns any of
//! them into a [`Color`]. The bitstream outranks the container: the encoder signalled it and it
//! travels with the elementary stream, while a muxer adds the box and may get it wrong. So an
//! importer resolves the bitstream's colour first and [`fill`]s only its gaps from the container.

use hang::catalog::{Chromaticity, Color, ContentLight, Mastering, Range};

/// The colour a set of H.273 code points signals, leaving each unspecified or reserved one absent.
///
/// `None` when nothing is left, so a source that signals nothing adds no catalog field.
pub(crate) fn cicp(primaries: u8, transfer: u8, matrix: u8, full_range: Option<bool>) -> Option<Color> {
	// Only H.273's registered values, never 2 (unspecified). Cameras ship zeroed and other reserved
	// descriptions, so a reserved value reads as unsignalled, letting a container fill it, rather
	// than refusing the stream.
	let registered = |code: u8, values: &[std::ops::RangeInclusive<u8>]| {
		values.iter().any(|range| range.contains(&code)).then_some(code)
	};
	let mut color = Color::default();
	color.primaries = registered(primaries, &[1..=1, 4..=12, 22..=22]);
	color.transfer = registered(transfer, &[1..=1, 4..=18]);
	color.matrix = registered(matrix, &[0..=1, 4..=17]);
	color.range = full_range.map(|full| if full { Range::Full } else { Range::Limited });
	(color != Color::default()).then_some(color)
}

/// Fill the absent fields of `color` from `fallback`, a lower-precedence source named `source`.
///
/// A field both set keeps `color`'s value and logs the disagreement, so a conflict between the
/// bitstream and its container is diagnosable rather than invisible.
pub(crate) fn fill(color: &mut Option<Color>, fallback: Option<&Color>, source: &str) {
	let Some(fallback) = fallback else {
		return;
	};
	let color = color.get_or_insert_with(Color::default);
	fn field<T: PartialEq + Clone + std::fmt::Debug>(
		name: &str,
		source: &str,
		slot: &mut Option<T>,
		other: &Option<T>,
	) {
		match (&*slot, other) {
			(None, _) => *slot = other.clone(),
			(Some(kept), Some(other)) if kept != other => {
				tracing::warn!(
					field = name,
					?kept,
					?other,
					source,
					"colour disagrees with the stream; keeping the stream's"
				);
			}
			_ => {}
		}
	}
	field("primaries", source, &mut color.primaries, &fallback.primaries);
	field("transfer", source, &mut color.transfer, &fallback.transfer);
	field("matrix", source, &mut color.matrix, &fallback.matrix);
	field("range", source, &mut color.range, &fallback.range);
	field("mastering", source, &mut color.mastering, &fallback.mastering);
	field(
		"content_light",
		source,
		&mut color.content_light,
		&fallback.content_light,
	);
}

/// The static HDR metadata an access unit carries in band.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Hdr {
	pub mastering: Option<Mastering>,
	pub content_light: Option<ContentLight>,
}

impl Hdr {
	/// Keep any field `other` sets, so an access unit carrying only one message keeps the other.
	pub(crate) fn update(&mut self, other: Hdr) {
		self.mastering = other.mastering.or(self.mastering);
		self.content_light = other.content_light.or(self.content_light);
	}

	/// Set these fields on `color`; they come from the bitstream, so they replace a hint.
	pub(crate) fn apply(&self, color: &mut Option<Color>) {
		if *self == Self::default() {
			return;
		}
		let color = color.get_or_insert_with(Color::default);
		color.mastering = self.mastering.or(color.mastering);
		color.content_light = self.content_light.or(color.content_light);
	}
}

/// The HDR metadata in an H.264 or H.265 access unit's SEI NAL units, which are Annex-B, or
/// length-prefixed when `length_size` is set. `header` is the codec's NAL header length and `sei`
/// picks the SEI NAL units by their first byte.
pub(crate) fn access_unit(
	payload: &bytes::Bytes,
	length_size: Option<usize>,
	header: usize,
	sei: impl Fn(u8) -> bool,
) -> Hdr {
	let mut hdr = Hdr::default();
	let mut scan = |nal: &[u8]| {
		if nal.len() > header
			&& sei(nal[0])
			&& let Some(rbsp) = crate::codec::annexb::rbsp_bytes(nal, header)
		{
			hdr.update(self::sei(&rbsp));
		}
	};
	match length_size {
		Some(size) => {
			let Ok(nals) = crate::codec::annexb::length_prefixed_nals(payload, size) else {
				return hdr;
			};
			nals.map_while(Result::ok).for_each(&mut scan);
		}
		None => {
			let mut buf = payload.clone();
			let mut nals = crate::codec::annexb::NalIterator::new(&mut buf);
			while let Some(Ok(nal)) = nals.next() {
				scan(&nal);
			}
			if let Ok(Some(nal)) = nals.flush() {
				scan(&nal);
			}
		}
	}
	hdr
}

const SEI_MASTERING_DISPLAY: u32 = 137;
const SEI_CONTENT_LIGHT: u32 = 144;

/// The HDR messages in an H.264 or H.265 SEI RBSP (the NAL payload after its header, emulation
/// prevention removed). Both codecs share the `sei_message` syntax and these two payloads
/// (ITU-T H.264 D.1.29 and D.1.31, H.265 D.2.28 and D.2.35). A malformed message ends the scan.
pub(crate) fn sei(rbsp: &[u8]) -> Hdr {
	let mut hdr = Hdr::default();
	let mut buf = rbsp;
	// Each message needs at least a type and size byte; the last byte is the RBSP stop bit.
	while buf.len() > 2 {
		let Some(kind) = sei_value(&mut buf) else { break };
		let Some(size) = sei_value(&mut buf) else { break };
		let Some((payload, rest)) = buf.split_at_checked(size as usize) else {
			break;
		};
		buf = rest;
		match kind {
			SEI_MASTERING_DISPLAY if payload.len() >= 24 => hdr.mastering = Some(sei_mastering(payload)),
			SEI_CONTENT_LIGHT if payload.len() >= 4 => {
				hdr.content_light = Some(ContentLight {
					max_cll: u16::from_be_bytes([payload[0], payload[1]]),
					max_fall: u16::from_be_bytes([payload[2], payload[3]]),
				})
			}
			_ => {}
		}
	}
	hdr
}

const AV1_METADATA_HDR_CLL: u64 = 1;
const AV1_METADATA_HDR_MDCV: u64 = 2;

/// The HDR metadata in an AV1 metadata OBU's payload (AV1 5.8.1, 6.7.3 and 6.7.4): content light
/// levels, or a mastering display whose chromaticities are 0.16 fixed point and whose luminances
/// are 24.8 (max) and 18.14 (min) fixed point. Anything else, or a short payload, is ignored.
pub(crate) fn av1_metadata(payload: &[u8]) -> Hdr {
	let mut hdr = Hdr::default();
	let mut buf = payload;
	let Some(kind) = leb128(&mut buf) else {
		return hdr;
	};
	let u16_at = |i: usize| f64::from(u16::from_be_bytes([buf[i], buf[i + 1]]));
	let u32_at = |i: usize| f64::from(u32::from_be_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]));
	match kind {
		AV1_METADATA_HDR_CLL if buf.len() >= 4 => {
			hdr.content_light = Some(ContentLight {
				max_cll: u16_at(0) as u16,
				max_fall: u16_at(2) as u16,
			})
		}
		AV1_METADATA_HDR_MDCV if buf.len() >= 24 => {
			let xy = |i: usize| Chromaticity {
				x: u16_at(i) / 65_536.0,
				y: u16_at(i + 2) / 65_536.0,
			};
			hdr.mastering = Some(Mastering {
				red: xy(0),
				green: xy(4),
				blue: xy(8),
				white: xy(12),
				max_luminance: u32_at(16) / 256.0,
				min_luminance: u32_at(20) / 16_384.0,
			})
		}
		_ => {}
	}
	hdr
}

/// An AV1 `leb128()` value.
fn leb128(buf: &mut &[u8]) -> Option<u64> {
	let mut value = 0u64;
	for i in 0..8 {
		let (&byte, rest) = buf.split_first()?;
		*buf = rest;
		value |= u64::from(byte & 0x7F) << (7 * i);
		if byte & 0x80 == 0 {
			return Some(value);
		}
	}
	None
}

/// An SEI `payloadType` or `payloadSize`: a run of 0xFF bytes, each adding 255, then the remainder.
fn sei_value(buf: &mut &[u8]) -> Option<u32> {
	let mut value = 0u32;
	loop {
		let (&byte, rest) = buf.split_first()?;
		*buf = rest;
		value = value.checked_add(byte.into())?;
		if byte != 0xFF {
			return Some(value);
		}
	}
}

/// A `mastering_display_colour_volume` payload. Chromaticities count 0.00002 and luminance
/// 0.0001 cd/m². The primaries are conventionally green, blue, red, which libavcodec assumes too.
fn sei_mastering(p: &[u8]) -> Mastering {
	let u16_at = |i: usize| u16::from_be_bytes([p[i], p[i + 1]]);
	let u32_at = |i: usize| u32::from_be_bytes([p[i], p[i + 1], p[i + 2], p[i + 3]]);
	let xy = |i: usize| Chromaticity {
		x: f64::from(u16_at(i)) / 50_000.0,
		y: f64::from(u16_at(i + 2)) / 50_000.0,
	};
	Mastering {
		green: xy(0),
		blue: xy(4),
		red: xy(8),
		white: xy(12),
		max_luminance: f64::from(u32_at(16)) / 10_000.0,
		min_luminance: f64::from(u32_at(20)) / 10_000.0,
	}
}

#[cfg(test)]
pub(crate) mod test {
	use super::*;

	/// The HDR10 mastering display and light levels the x264 and x265 SEI fixtures carry.
	pub(crate) fn hdr10() -> Hdr {
		Hdr {
			mastering: Some(Mastering {
				red: Chromaticity { x: 0.708, y: 0.292 },
				green: Chromaticity { x: 0.17, y: 0.797 },
				blue: Chromaticity { x: 0.131, y: 0.046 },
				white: Chromaticity { x: 0.3127, y: 0.329 },
				max_luminance: 1000.0,
				min_luminance: 0.005,
			}),
			content_light: Some(ContentLight {
				max_cll: 1000,
				max_fall: 400,
			}),
		}
	}

	#[test]
	fn cicp_drops_unspecified_and_reserved() {
		assert_eq!(cicp(2, 2, 2, None), None);
		assert_eq!(cicp(0, 0, 3, None), None);
		assert_eq!(cicp(255, 19, 18, None), None, "past the registered values");
		for matrix in 15..=17 {
			// IPT-C2, YCgCo-Re, and YCgCo-Ro, registered since H.273 V3.
			assert_eq!(cicp(2, 2, matrix, None).unwrap().matrix, Some(matrix));
		}
		let color = cicp(9, 16, 0, Some(false)).unwrap();
		assert_eq!(
			(color.primaries, color.transfer, color.matrix, color.range),
			(Some(9), Some(16), Some(0), Some(hang::catalog::Range::Limited))
		);
	}

	#[test]
	fn fill_keeps_the_stream_and_fills_its_gaps() {
		let mut color = cicp(1, 2, 1, None);
		fill(&mut color, cicp(9, 1, 9, Some(true)).as_ref(), "test");
		let color = color.unwrap();
		assert_eq!(color.primaries, Some(1), "the stream wins a conflict");
		assert_eq!(color.matrix, Some(1));
		assert_eq!(color.transfer, Some(1), "the container fills an unspecified field");
		assert_eq!(
			color.range,
			Some(hang::catalog::Range::Full),
			"the container fills an unsignalled range"
		);
	}

	#[test]
	fn fill_from_nothing_adds_nothing() {
		let mut color = None;
		fill(&mut color, None, "test");
		assert_eq!(color, None);
	}

	#[test]
	fn sei_reads_hdr10() {
		use crate::codec::h264::fixtures::{SEI_CONTENT_LIGHT, SEI_MASTERING};
		let mut hdr = Hdr::default();
		for nal in [SEI_MASTERING, SEI_CONTENT_LIGHT] {
			hdr.update(sei(&crate::codec::annexb::rbsp_bytes(nal, 1).unwrap()));
		}
		assert_eq!(hdr, hdr10());
	}

	#[test]
	fn access_unit_reads_both_shapes() {
		use crate::codec::h264::fixtures::{PPS, SEI_CONTENT_LIGHT, SEI_MASTERING, SPS_HDR10};
		let is_sei = |h: u8| h & 0x1f == 6;
		let nals = [SPS_HDR10, PPS, SEI_MASTERING, SEI_CONTENT_LIGHT];

		let mut annexb = Vec::new();
		let mut prefixed = Vec::new();
		for nal in nals {
			annexb.extend_from_slice(&[0, 0, 0, 1]);
			annexb.extend_from_slice(nal);
			prefixed.extend_from_slice(&(nal.len() as u32).to_be_bytes());
			prefixed.extend_from_slice(nal);
		}
		assert_eq!(access_unit(&annexb.into(), None, 1, is_sei), hdr10());
		assert_eq!(access_unit(&prefixed.into(), Some(4), 1, is_sei), hdr10());
	}

	#[test]
	fn av1_metadata_reads_hdr10() {
		let mut mdcv = vec![2];
		for v in [46_399u16, 19_136, 11_141, 52_232, 8_585, 3_014, 20_493, 21_561] {
			mdcv.extend_from_slice(&v.to_be_bytes());
		}
		mdcv.extend_from_slice(&(1000u32 << 8).to_be_bytes());
		mdcv.extend_from_slice(&82u32.to_be_bytes());
		let mastering = av1_metadata(&mdcv).mastering.unwrap();
		assert!((mastering.red.x - 0.708).abs() < 1e-4);
		assert!((mastering.green.y - 0.797).abs() < 1e-4);
		assert!((mastering.white.x - 0.3127).abs() < 1e-4);
		assert_eq!(mastering.max_luminance, 1000.0);
		assert!((mastering.min_luminance - 0.005).abs() < 1e-4);

		let cll = [1, 0x03, 0xE8, 0x01, 0x90];
		assert_eq!(
			av1_metadata(&cll),
			Hdr {
				content_light: hdr10().content_light,
				..Default::default()
			}
		);
		assert_eq!(av1_metadata(&[2, 0, 1]), Hdr::default(), "a short payload is ignored");
	}

	#[test]
	fn sei_ignores_other_and_truncated_messages() {
		// A user-data message, then a mastering display cut short.
		let rbsp = [5, 2, 0xAA, 0xBB, 137, 24, 0, 1, 0x80];
		assert_eq!(sei(&rbsp), Hdr::default());
	}
}
