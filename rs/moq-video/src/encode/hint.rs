//! What a video track advertises before its own bitstream says otherwise.

use super::encoder::{Codec, Config, default_bitrate};
use crate::Size;

/// The geometry the codec string's level is computed from when the caller hints none.
///
/// Deliberately modest. The level has to be one every decoder accepts, because a capability probe
/// that rejects it drops the rendition, and dropping it withholds the subscription the first
/// keyframe is waiting on. Under-claiming costs nothing: a decoder configures from the in-band
/// SPS, and the first keyframe replaces the whole codec string anyway.
const FALLBACK_SIZE: Size = Size {
	width: 1280,
	height: 720,
};

/// The framerate the level is computed from when the caller hints none. See [`FALLBACK_SIZE`].
const FALLBACK_FRAMERATE: u32 = 30;

/// What a track's catalog rendition advertises before the encoder resolves it from the bitstream.
///
/// A track's rendition is published as soon as the track exists, so a subscriber can discover it
/// (and subscribe) before anything has been encoded. That is what an on-demand encoder needs: it
/// only encodes while someone is watching, and without this nobody can watch until it encodes.
///
/// Only [`codec`](Self::codec) is required, and it is the field that does the work. The rest fill
/// gaps the bitstream leaves and are refined the moment it reveals otherwise: the first keyframe's
/// parameter sets replace the codec string and the dimensions, so an approximate hint is corrected
/// within a keyframe rather than standing forever.
///
/// Build one from an [`Encoder`](super::Encoder)'s [`Config`] (`(&config).into()`), which fills
/// every field, or from a bare [`Codec`] (`codec.into()`) when that's all you have. It converts
/// into the [`VideoHint`](moq_mux::catalog::VideoHint) an importer overlays onto every config it
/// resolves.
///
/// `#[non_exhaustive]`: build via [`Hint::new`] and set the optional fields, so new ones don't
/// break callers.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Hint {
	/// The codec the track carries. Fixes the shape of the codec string (`avc3` / `hev1`).
	pub codec: Codec,
	/// The encoded resolution, if known before the first frame.
	pub size: Option<Size>,
	/// The encoded framerate in frames per second. Never revealed by an H.264/H.265 keyframe, so a
	/// catalog only carries it when hinted.
	pub framerate: Option<u32>,
	/// The target bitrate in bits per second. Detected from the track over time, so hinting it just
	/// makes the first snapshot accurate.
	pub bitrate: Option<u64>,
}

impl Hint {
	/// A hint carrying nothing but the codec: enough to publish the rendition up front.
	pub fn new(codec: Codec) -> Self {
		Self {
			codec,
			size: None,
			framerate: None,
			bitrate: None,
		}
	}

	/// The `level_idc` the advertised codec string carries: the lowest level of this hint's codec
	/// that fits the geometry, or the level the fallback geometry resolves to when it hints none.
	pub fn level(&self) -> u8 {
		let size = self.size.unwrap_or(FALLBACK_SIZE);
		let framerate = self.framerate.unwrap_or(FALLBACK_FRAMERATE);
		let bitrate = self.bitrate.unwrap_or_else(|| default_bitrate(size, framerate));

		match self.codec {
			Codec::H264 => h264_level(size, framerate, bitrate),
			Codec::H265 => h265_level(size, framerate, bitrate),
		}
	}

	/// The codec string to advertise, computed from the hint rather than the bitstream.
	///
	/// In-band parameter sets (avc3 / hev1), matching what every [`Encoder`](super::Encoder)
	/// backend emits, and the *least* capable profile any of them emits: H.264 Constrained Baseline
	/// (openh264, the software fallback; VideoToolbox and Media Foundation emit High, VAAPI Main)
	/// and HEVC Main (all of them). Claiming less is the safe direction here, because this string
	/// only stands until the first keyframe: a decoder that rejects it drops the rendition and
	/// never subscribes, which strands the encoder that was waiting for a subscriber, while one
	/// that accepts it gets the stream's real profile a keyframe later.
	fn codec_string(&self) -> hang::catalog::VideoCodec {
		match self.codec {
			Codec::H264 => hang::catalog::H264 {
				inline: true,
				profile: 0x42,
				// constraint_set0/1/2: the constrained-baseline subset every profile can decode.
				constraints: 0xe0,
				level: self.level(),
			}
			.into(),
			Codec::H265 => hang::catalog::H265 {
				in_band: true,
				profile_space: 0,
				// Main profile, whose compatibility flags render as "6".
				profile_idc: 1,
				profile_compatibility_flags: [0x60, 0, 0, 0],
				// Main tier ("L"), the only one our backends ask for.
				tier_flag: false,
				level_idc: self.level(),
				// Progressive, non-packed, frame-only: what a live encoder emits.
				constraint_flags: [0xb0, 0, 0, 0, 0, 0],
			}
			.into(),
		}
	}
}

impl From<Codec> for Hint {
	fn from(codec: Codec) -> Self {
		Self::new(codec)
	}
}

/// Everything an encoder is configured with that the catalog also carries.
impl From<&Config> for Hint {
	fn from(config: &Config) -> Self {
		Self {
			codec: config.codec,
			size: Some(config.size()),
			framerate: Some(config.framerate),
			// The rate the encoder actually opens at, not the caller's `None`.
			bitrate: Some(config.resolved_bitrate()),
		}
	}
}

/// The overlay a [`moq_mux`] importer applies to every config it resolves, filling only what the
/// bitstream leaves absent.
impl From<&Hint> for moq_mux::catalog::VideoHint {
	fn from(hint: &Hint) -> Self {
		let mut out = moq_mux::catalog::VideoHint::default();
		out.codec = Some(hint.codec_string());
		out.coded_width = hint.size.map(|size| size.width);
		out.coded_height = hint.size.map(|size| size.height);
		out.framerate = hint.framerate.map(f64::from);
		out.bitrate = hint.bitrate;
		out
	}
}

/// The smallest H.264 level (Table A-1) that fits the geometry, frame rate, and bitrate, as a
/// `level_idc` (level 3.1 -> 31, printed `1f` in the codec string).
pub fn h264_level(size: Size, framerate: u32, bitrate: u64) -> u8 {
	// (level_idc, MaxMBPS, MaxFS, MaxBR in kbit/s at the Baseline/Main factor).
	const LEVELS: &[(u8, u64, u64, u64)] = &[
		(10, 1_485, 99, 64),
		(11, 3_000, 396, 192),
		(12, 6_000, 396, 384),
		(13, 11_880, 396, 768),
		(20, 11_880, 396, 2_000),
		(21, 19_800, 792, 4_000),
		(22, 20_250, 1_620, 4_000),
		(30, 40_500, 1_620, 10_000),
		(31, 108_000, 3_600, 14_000),
		(32, 216_000, 5_120, 20_000),
		(40, 245_760, 8_192, 20_000),
		(41, 245_760, 8_192, 50_000),
		(42, 522_240, 8_704, 50_000),
		(50, 589_824, 22_080, 135_000),
		(51, 983_040, 36_864, 240_000),
		(52, 2_073_600, 36_864, 240_000),
	];

	let width = size.width.div_ceil(16) as u64;
	let height = size.height.div_ceil(16) as u64;
	let macroblocks = width * height;
	let macroblocks_per_sec = macroblocks * framerate as u64;
	for &(idc, max_mbps, max_fs, max_br) in LEVELS {
		// Annex A caps each axis at sqrt(MaxFS * 8) macroblocks on top of the frame area, so an
		// extreme aspect ratio needs a higher level than its area alone implies. Squared to keep
		// this in integers.
		if width * width > max_fs * 8 || height * height > max_fs * 8 {
			continue;
		}
		// High profile raises the bitrate cap by cpbBrVclFactor 1250/1000.
		if macroblocks <= max_fs && macroblocks_per_sec <= max_mbps && bitrate <= max_br * 1250 {
			return idc;
		}
	}
	// Beyond the table: claim the top level rather than an impossible one.
	52
}

/// The smallest HEVC level (Table A.8 / A.9, Main tier) that fits the geometry, frame rate, and
/// bitrate, as a `level_idc` (level 3.1 -> 93, printed `L93` in the codec string).
pub fn h265_level(size: Size, framerate: u32, bitrate: u64) -> u8 {
	// (level_idc, MaxLumaPs, MaxLumaSr, MaxBR in kbit/s at the Main tier).
	const LEVELS: &[(u8, u64, u64, u64)] = &[
		(30, 36_864, 552_960, 128),
		(60, 122_880, 3_686_400, 1_500),
		(63, 245_760, 7_372_800, 3_000),
		(90, 552_960, 16_588_800, 6_000),
		(93, 983_040, 33_177_600, 10_000),
		(120, 2_228_224, 66_846_720, 12_000),
		(123, 2_228_224, 133_693_440, 20_000),
		(150, 8_912_896, 267_386_880, 25_000),
		(153, 8_912_896, 534_773_760, 40_000),
		(156, 8_912_896, 1_069_547_520, 60_000),
		(180, 35_651_584, 1_069_547_520, 60_000),
		(183, 35_651_584, 2_139_095_040, 120_000),
		(186, 35_651_584, 4_278_190_080, 240_000),
	];

	let (width, height) = (size.width as u64, size.height as u64);
	let samples = size.pixels();
	let samples_per_sec = samples * framerate as u64;
	for &(idc, max_luma_ps, max_luma_sr, max_br) in LEVELS {
		// Table A.8 caps each axis at sqrt(MaxLumaPs * 8) samples on top of the picture area, the
		// HEVC counterpart of Annex A's per-axis macroblock limit. Squared to stay in integers.
		if width * width > max_luma_ps * 8 || height * height > max_luma_ps * 8 {
			continue;
		}
		if samples <= max_luma_ps && samples_per_sec <= max_luma_sr && bitrate <= max_br * 1000 {
			return idc;
		}
	}
	// Beyond the table: claim the top level rather than an impossible one.
	186
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The rendition a hint publishes, as an importer would.
	fn rendition(hint: &Hint) -> hang::catalog::VideoConfig {
		moq_mux::catalog::VideoHint::from(hint)
			.to_config()
			.expect("a hint always carries a codec")
	}

	/// The whole point of the hint: a codec string a decoder capability probe accepts, published
	/// before anything is encoded. A rendition the probe rejects is dropped by the player, which
	/// withholds the subscription an on-demand encoder is waiting for.
	///
	/// So the profile is the *least* any backend emits (openh264 leaves it at Constrained
	/// Baseline; VAAPI writes Main; VideoToolbox and Media Foundation ask for High), not the most.
	/// Claiming High here would exclude a decoder that could have played the software fallback's
	/// output, and an excluded decoder never subscribes, so the SPS that would have corrected the
	/// claim never arrives.
	#[test]
	fn a_bare_codec_advertises_a_playable_string() {
		// Both at level 3.1: what the fallback geometry resolves to, and a floor no decoder refuses.
		assert_eq!(rendition(&Hint::new(Codec::H264)).codec.to_string(), "avc3.42e01f");
		assert_eq!(rendition(&Hint::new(Codec::H265)).codec.to_string(), "hev1.1.6.L93.B0");
	}

	/// An unhinted geometry must not reach the catalog as a fabricated resolution: it only feeds
	/// the level, which the first keyframe replaces anyway.
	#[test]
	fn an_unhinted_geometry_stays_absent() {
		let config = rendition(&Hint::new(Codec::H264));
		assert_eq!(config.coded_width, None);
		assert_eq!(config.coded_height, None);
		assert_eq!(config.framerate, None);
		assert_eq!(config.bitrate, None);
	}

	/// An encoder's config fills every field, so the first catalog snapshot describes the stream
	/// that is about to arrive rather than a stub.
	#[test]
	fn an_encoder_config_fills_the_rendition() {
		let mut source = Config::new(1920, 1080, 30);
		source.bitrate = Some(6_000_000);
		let hint = Hint::from(&source);

		let config = rendition(&hint);
		assert_eq!(
			config.codec.to_string(),
			"avc3.42e028",
			"1080p30 at 6 Mbps is level 4.0"
		);
		assert_eq!(config.coded_width, Some(1920));
		assert_eq!(config.coded_height, Some(1080));
		assert_eq!(config.framerate, Some(30.0));
		assert_eq!(config.bitrate, Some(6_000_000));
		assert_eq!(config.container, hang::catalog::Container::Legacy);
	}

	/// A hinted bitrate is the encoder's resolved target, not the caller's `None`.
	#[test]
	fn an_unset_bitrate_resolves_to_the_encoder_default() {
		let source = Config::new(320, 240, 30);
		assert_eq!(Hint::from(&source).bitrate, Some(source.resolved_bitrate()));
	}

	/// The importer overlay carries the same fields, since it is what publishes the rendition.
	#[test]
	fn the_importer_overlay_matches() {
		let hint = Hint::from(&Config::new(640, 480, 15));
		let overlay: moq_mux::catalog::VideoHint = (&hint).into();

		assert!(
			overlay.codec.is_some(),
			"a codec-less overlay publishes nothing up front"
		);
		assert_eq!(overlay.coded_width, Some(640));
		assert_eq!(overlay.coded_height, Some(480));
		assert_eq!(overlay.framerate, Some(15.0));
	}

	/// Levels scale with what the encoder is actually doing, so a big stream isn't advertised as a
	/// small one (and a decoder isn't asked for more than the stream needs).
	#[test]
	fn levels_track_the_geometry() {
		let avc = |width, height, framerate, bitrate| h264_level(Size::new(width, height), framerate, bitrate);
		assert_eq!(avc(320, 240, 30, 500_000), 13, "QVGA30");
		assert_eq!(avc(1280, 720, 30, 2_000_000), 31, "720p30");
		assert_eq!(avc(1920, 1080, 60, 8_000_000), 42, "1080p60");
		assert_eq!(avc(3840, 2160, 30, 20_000_000), 51, "4K30");

		// Past the table: claim the top level rather than an impossible one.
		assert_eq!(avc(8192, 8192, 120, u64::MAX), 52);

		// An extreme aspect ratio needs more than its area implies: 3840x32 is only 480
		// macroblocks (level 2.1 by area) but 240 of them wide, past level 2.1's sqrt(MaxFS * 8)
		// per-axis cap of ~79. Level 4.0 is the first row wide enough.
		assert_eq!(avc(3840, 32, 30, 500_000), 40, "wide and short");
		assert_eq!(avc(32, 3840, 30, 500_000), 40, "tall and narrow");

		let hevc = |width, height, framerate, bitrate| h265_level(Size::new(width, height), framerate, bitrate);
		assert_eq!(hevc(1280, 720, 30, 2_000_000), 93, "720p30");
		assert_eq!(hevc(3840, 2160, 30, 20_000_000), 150, "4K30");
		assert_eq!(hevc(16384, 16384, 120, u64::MAX), 186);
		// The same per-axis cap, on sqrt(MaxLumaPs * 8) luma samples.
		assert_eq!(hevc(3840, 32, 30, 500_000), 120, "wide and short");
	}
}
