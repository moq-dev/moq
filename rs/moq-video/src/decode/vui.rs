//! The color space an H.264 or H.265 sequence signals in its VUI.
//!
//! Most hardware decoders hand back pictures without saying what space they
//! are in, though the stream says so in every keyframe's SPS. Reading it here
//! labels every backend's output the same way, so a transcode re-encodes with
//! the source's own space rather than the guess for whatever size it scaled to.

use bytes::Bytes;
use moq_mux::codec::annexb::NalIterator;

use super::Codec;
use crate::{Color, Size};

/// The space the SPS in a keyframe's access unit signals for the pictures that
/// follow it.
///
/// `None` when the access unit carries no SPS, so the previous answer stands.
/// `Some(None)` when the SPS signals a space [`Color`] cannot hold (BT.2020, an
/// RGB or YCgCo matrix), which leaves the pictures unlabelled rather than
/// claiming one it is not.
pub(super) fn signalled(codec: Codec, access_unit: &Bytes) -> Option<Option<Color>> {
	if !matches!(codec, Codec::H264 | Codec::H265) {
		return None;
	}
	let mut buf = access_unit.clone();
	let mut nals = NalIterator::new(&mut buf);
	for nal in nals.by_ref() {
		// The iterator repeats an error rather than advancing past it, and a
		// malformed access unit is the backend's to reject.
		let Ok(nal) = nal else { return None };
		if let Some(color) = sps(codec, &nal) {
			return Some(color);
		}
	}
	sps(codec, &nals.flush().ok()??)
}

/// The space one NAL unit signals, when it is a parseable SPS.
fn sps(codec: Codec, nal: &[u8]) -> Option<Option<Color>> {
	match codec {
		Codec::H264 => h264(nal),
		Codec::H265 => h265(nal),
		_ => None,
	}
}

fn h264(nal: &[u8]) -> Option<Option<Color>> {
	use h264_reader::nal::sps::SeqParameterSet;
	use h264_reader::nal::{Nal, RefNal, UnitType};

	let nal = RefNal::new(nal, &[], true);
	if nal.header().ok()?.nal_unit_type() != UnitType::SeqParameterSet {
		return None;
	}
	let sps = SeqParameterSet::from_bits(nal.rbsp_bits()).ok()?;
	let (width, height) = sps.pixel_dimensions().ok()?;
	// Absent, the range flag is 0 (limited) and the matrix unspecified (E.2.1).
	let signal = sps
		.vui_parameters
		.as_ref()
		.and_then(|vui| vui.video_signal_type.as_ref());
	let full_range = signal.is_some_and(|signal| signal.video_full_range_flag);
	let matrix = signal
		.and_then(|signal| signal.colour_description.as_ref())
		.map(|description| description.matrix_coefficients);
	Some(color(full_range, matrix, Size::new(width, height)))
}

fn h265(nal: &[u8]) -> Option<Option<Color>> {
	use scuffle_h265::{NALUnitType, SpsNALUnit};

	// The type is the six bits after the forbidden zero bit (H.265 7.3.1.2).
	if NALUnitType::from((nal.first()? >> 1) & 0x3f) != NALUnitType::SpsNut {
		return None;
	}
	let sps = SpsNALUnit::parse(&mut &nal[..]).ok()?.rbsp;
	let size = Size::new(
		u32::try_from(sps.cropped_width()).ok()?,
		u32::try_from(sps.cropped_height()).ok()?,
	);
	// Absent, the range flag is 0 (limited) and the matrix unspecified (E.3.1).
	let (full_range, matrix) = match &sps.vui_parameters {
		Some(vui) => (
			vui.video_signal_type.video_full_range_flag,
			Some(vui.video_signal_type.matrix_coeffs),
		),
		None => (false, None),
	};
	Some(color(full_range, matrix, size))
}

/// Map a VUI's range flag and `matrix_coefficients` (ISO/IEC 23091-2) onto a
/// [`Color`], with an unspecified matrix taking the guess for the coded size.
///
/// The matrix alone decides between BT.601 and BT.709: [`Color`] folds the
/// primaries and transfer into it, as every encoder here signals them.
fn color(full_range: bool, matrix: Option<u8>, size: Size) -> Option<Color> {
	let bt709 = match matrix {
		// Unspecified, either by omission or by code point 2.
		None | Some(2) => matches!(Color::infer(size), Color::Bt709Limited | Color::Bt709Full),
		Some(1) => true,
		// BT.470 System B/G and SMPTE 170M define the same matrix.
		Some(5 | 6) => false,
		Some(_) => return None,
	};
	Some(match (bt709, full_range) {
		(false, false) => Color::Bt601Limited,
		(false, true) => Color::Bt601Full,
		(true, false) => Color::Bt709Limited,
		(true, true) => Color::Bt709Full,
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn matrix_and_range_map_onto_color() {
		let hd = Size::new(1920, 1080);
		let sd = Size::new(640, 480);
		assert_eq!(color(false, Some(1), sd), Some(Color::Bt709Limited));
		assert_eq!(color(true, Some(6), hd), Some(Color::Bt601Full));
		assert_eq!(color(false, Some(5), hd), Some(Color::Bt601Limited));
		// Unspecified takes the size guess, but keeps a signalled range.
		assert_eq!(color(false, None, hd), Some(Color::Bt709Limited));
		assert_eq!(color(true, Some(2), sd), Some(Color::Bt601Full));
		// BT.2020 non-constant luminance has no variant to land in.
		assert_eq!(color(false, Some(9), hd), None);
	}

	#[test]
	fn other_codecs_and_nals_signal_nothing() {
		// A lone H.264 PPS: no SPS, so the previous answer stands.
		let pps = Bytes::from_static(&[0, 0, 0, 1, 0x68, 0xce, 0x38, 0x80]);
		assert_eq!(signalled(Codec::H264, &pps), None);
		assert_eq!(signalled(Codec::Vp9, &pps), None);
		// Not Annex-B at all: the start-code scan gives up rather than spinning.
		assert_eq!(signalled(Codec::H264, &Bytes::from_static(b"not annex b")), None);
	}
}
