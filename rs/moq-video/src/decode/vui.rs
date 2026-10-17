//! The color space an H.264 or H.265 sequence signals in its VUI.
//!
//! Most hardware decoders hand back pictures without saying what space they
//! are in, though the stream says so in every keyframe's SPS. Reading it here
//! labels every backend's output the same way, so a transcode re-encodes with
//! the source's own space rather than the guess for whatever size it scaled to.

use bytes::Bytes;
use moq_mux::codec::annexb::NalIterator;

use super::Codec;
use crate::color::{CodePoints, Range};
use crate::{Color, Size};

/// The space the SPS in a keyframe's access unit signals for the pictures that
/// follow it.
///
/// `None` when the access unit carries no SPS, so the previous answer stands.
/// `Some(None)` when the SPS signals a space [`Color`] cannot hold (an RGB or
/// YCgCo matrix, a gamma curve), which leaves the pictures unlabelled rather
/// than claiming one they are not in. Several SPSs that disagree are
/// `Some(None)` too: which one the slices use takes the PPS to know.
pub(super) fn signalled(codec: Codec, access_unit: &Bytes) -> Option<Option<Color>> {
	if !matches!(codec, Codec::H264 | Codec::H265) {
		return None;
	}
	let mut buf = access_unit.clone();
	let mut nals = NalIterator::new(&mut buf);
	let mut signalled = None;
	for nal in nals.by_ref() {
		// The iterator repeats an error rather than advancing past it, and a
		// malformed access unit is the backend's to reject.
		let Ok(nal) = nal else { return signalled };
		merge(&mut signalled, sps(codec, &nal));
	}
	if let Ok(Some(nal)) = nals.flush() {
		merge(&mut signalled, sps(codec, &nal));
	}
	signalled
}

/// Fold one NAL unit's answer into the access unit's.
fn merge(signalled: &mut Option<Option<Color>>, nal: Option<Option<Color>>) {
	*signalled = match (*signalled, nal) {
		(previous, None) => previous,
		(None, Some(color)) => Some(color),
		(Some(previous), Some(color)) if previous == color => Some(color),
		(Some(_), Some(_)) => Some(None),
	};
}

/// The space one NAL unit signals, when it is an SPS.
///
/// An SPS this cannot read (cameras ship H.265 colour fields the parser
/// refuses) leaves the pictures unlabelled rather than keeping the previous
/// space, which would claim one the stream may no longer be in.
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
	let Ok(sps) = SeqParameterSet::from_bits(nal.rbsp_bits()) else {
		return Some(None);
	};
	let Ok((width, height)) = sps.pixel_dimensions() else {
		return Some(None);
	};
	// Absent, the range flag is 0 (limited) and the description unspecified (E.2.1).
	let signal = sps
		.vui_parameters
		.as_ref()
		.and_then(|vui| vui.video_signal_type.as_ref());
	let range = range(signal.is_some_and(|signal| signal.video_full_range_flag));
	let codes = signal
		.and_then(|signal| signal.colour_description.as_ref())
		.map(|description| CodePoints {
			primaries: description.colour_primaries,
			transfer: description.transfer_characteristics,
			matrix: description.matrix_coefficients,
		});
	Some(Color::from_code_points(codes, range, Size::new(width, height)))
}

fn h265(nal: &[u8]) -> Option<Option<Color>> {
	use scuffle_h265::{NALUnitType, SpsNALUnit};

	// The type is the six bits after the forbidden zero bit (H.265 7.3.1.2).
	if NALUnitType::from((nal.first()? >> 1) & 0x3f) != NALUnitType::SpsNut {
		return None;
	}
	let Ok(sps) = SpsNALUnit::parse(&mut &nal[..]) else {
		return Some(None);
	};
	let sps = sps.rbsp;
	let Some(size) = h265_size(&sps) else {
		return Some(None);
	};
	// Absent, the range flag is 0 (limited) and the description unspecified
	// (E.3.1); scuffle_h265 fills an absent description with 2 itself.
	let Some(vui) = &sps.vui_parameters else {
		return Some(Color::from_code_points(None, Range::Limited, size));
	};
	let signal = &vui.video_signal_type;
	let codes = CodePoints {
		primaries: signal.colour_primaries,
		transfer: signal.transfer_characteristics,
		matrix: signal.matrix_coeffs,
	};
	Some(Color::from_code_points(
		Some(codes),
		range(signal.video_full_range_flag),
		size,
	))
}

/// The picture size inside the conformance window, or `None` for a window
/// that crops past the picture.
///
/// `cropped_width` and `cropped_height` subtract unchecked, so they would
/// overflow on such a window.
fn h265_size(sps: &scuffle_h265::SpsRbsp) -> Option<Size> {
	let window = &sps.conformance_window;
	let crop = |size: u64, sub: u8, start: u64, end: u64| {
		let offset = start.checked_add(end)?.checked_mul(sub.into())?;
		u32::try_from(size.checked_sub(offset)?).ok()
	};
	let width = crop(
		sps.pic_width_in_luma_samples.get(),
		sps.sub_width_c(),
		window.conf_win_left_offset,
		window.conf_win_right_offset,
	)?;
	let height = crop(
		sps.pic_height_in_luma_samples.get(),
		sps.sub_height_c(),
		window.conf_win_top_offset,
		window.conf_win_bottom_offset,
	)?;
	Some(Size::new(width, height))
}

fn range(full: bool) -> Range {
	match full {
		true => Range::Full,
		false => Range::Limited,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn other_codecs_and_nals_signal_nothing() {
		// A lone H.264 PPS: no SPS, so the previous answer stands.
		let pps = Bytes::from_static(&[0, 0, 0, 1, 0x68, 0xce, 0x38, 0x80]);
		assert_eq!(signalled(Codec::H264, &pps), None);
		assert_eq!(signalled(Codec::Vp9, &pps), None);
		// Not Annex-B at all: the start-code scan gives up rather than spinning.
		assert_eq!(signalled(Codec::H264, &Bytes::from_static(b"not annex b")), None);
	}

	/// Two SPSs that disagree leave the pictures unlabelled, since which one the
	/// slices use takes the PPS to know; two that agree keep their space.
	#[test]
	#[cfg(feature = "openh264")]
	fn disagreeing_sps_clear_the_label() {
		use crate::encode::{Config, Encoder, Kind};
		use crate::{Frame, I420, Surface};

		let size = Size::new(64, 64);
		let keyframe = |color: Color| {
			let mut encoder = Encoder::new(&Config {
				kind: Kind::Software,
				color: Some(color),
				..Config::new(size.width, size.height, crate::Rate::new(30, 1).unwrap())
			})
			.unwrap();
			encoder.cut().unwrap();
			let i420 = I420::new(size, vec![0x80; I420::len(size).unwrap()]).unwrap();
			let frame = Frame::new(
				Surface::I420(i420.with_color(color)),
				moq_net::Timestamp::from_micros(0).unwrap(),
			);
			encoder.encode(&frame).unwrap().remove(0).payload
		};
		let both = |first: Color, second: Color| {
			let mut unit = keyframe(first).to_vec();
			unit.extend_from_slice(&keyframe(second));
			signalled(Codec::H264, &Bytes::from(unit))
		};
		assert_eq!(both(Color::BT709, Color::BT601), Some(None));
		assert_eq!(both(Color::BT709, Color::BT709), Some(Some(Color::BT709)));
	}

	/// A camera's H.265 SPS with zeroed colour fields, which the parser refuses
	/// (moq-mux's `SPS_ZERO_COLOUR`), leaves pictures unlabelled rather than
	/// keeping whatever space came before.
	#[test]
	fn an_unreadable_sps_clears_the_label() {
		let mut unit = vec![0, 0, 0, 1];
		unit.extend_from_slice(&[
			0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00,
			0x96, 0xa0, 0x03, 0xc0, 0x80, 0x11, 0x07, 0xcb, 0x8a, 0xad, 0x3b, 0xa2, 0x4b, 0xb9, 0x08, 0x00, 0x00, 0x03,
			0x00, 0x20, 0x05, 0x26, 0x5c, 0x00, 0x33, 0x7f, 0x98, 0x01,
		]);
		assert_eq!(signalled(Codec::H265, &Bytes::from(unit)), Some(None));
	}

	/// A conformance window past the picture is refused rather than wrapping
	/// (moq-mux's `SPS_NVENC`, a 1280x720 hevc_nvenc SPS, with its window
	/// widened).
	#[test]
	fn a_window_past_the_picture_is_refused() {
		let nal: &[u8] = &[
			0x42, 0x01, 0x01, 0x01, 0x60, 0x00, 0x00, 0x03, 0x00, 0x90, 0x00, 0x00, 0x03, 0x00, 0x00, 0x03, 0x00, 0x78,
			0xa0, 0x02, 0x80, 0x80, 0x2e, 0x1f, 0x13, 0x96, 0x5d, 0x29, 0x08, 0x46, 0x45, 0xd5, 0x0c, 0x04, 0x04, 0x00,
			0x00, 0x03, 0x00, 0x04, 0x00, 0x00, 0x03, 0x00, 0xf1, 0x80, 0x17, 0x79, 0x44, 0x00, 0x02, 0xdc, 0x6c, 0x00,
			0x00, 0x2d, 0xc6, 0xc0, 0x40,
		];
		let sps = scuffle_h265::SpsNALUnit::parse(&mut &nal[..]).unwrap().rbsp;
		assert_eq!(h265_size(&sps), Some(Size::new(1280, 720)));

		let width = sps.pic_width_in_luma_samples.get() / u64::from(sps.sub_width_c());
		for (right, bottom) in [(width + 1, 0), (0, u64::MAX)] {
			let mut sps = sps.clone();
			sps.conformance_window.conf_win_right_offset = right;
			sps.conformance_window.conf_win_bottom_offset = bottom;
			assert_eq!(h265_size(&sps), None);
		}
	}
}
