//! The catalog entry an MP4 sample entry describes, shared by the fMP4 and regular MP4 readers.

use bytes::BytesMut;
use hang::catalog::{AudioCodec, AudioConfig, Container, H264, H265, VP9, VideoCodec, VideoConfig};
use mp4_atom::Atom;

use super::Error;
use crate::Result;

/// The one sample entry a track's `stsd` lists.
///
/// A track joined from several encodes lists one per encode, and switches between them by sample,
/// which a catalog entry can't follow, so it is refused.
pub(crate) fn sample_entry(trak: &mp4_atom::Trak) -> Result<&mp4_atom::Codec> {
	match trak.mdia.minf.stbl.stsd.codecs.as_slice() {
		[] => Err(Error::MissingCodec.into()),
		[codec] => Ok(codec),
		_ => Err(Error::MultipleCodecs.into()),
	}
}

/// Whether [`video`] or [`audio`] describes `codec`, rather than refusing it as unknown or
/// unsupported. A malformed entry of a supported codec still fails there.
pub(crate) fn supported(codec: &mp4_atom::Codec) -> bool {
	match codec {
		mp4_atom::Codec::Avc1(_)
		| mp4_atom::Codec::Avc3(_)
		| mp4_atom::Codec::Hev1(_)
		| mp4_atom::Codec::Hvc1(_)
		| mp4_atom::Codec::Vp08(_)
		| mp4_atom::Codec::Vp09(_)
		| mp4_atom::Codec::Av01(_)
		| mp4_atom::Codec::Opus(_)
		| mp4_atom::Codec::Flac(_) => true,
		mp4_atom::Codec::Mp4a(mp4a) => mp4a.esds.es_desc.dec_config.object_type_indication == AAC,
		_ => false,
	}
}

/// The MPEG-4 Audio object type indication, which AAC's `esds` carries.
const AAC: u8 = 0x40;

/// The catalog entry a video sample entry describes, wrapped in `container`.
pub(crate) fn video(codec: &mp4_atom::Codec, container: Container) -> Result<VideoConfig> {
	let (mut config, colr, pasp, spatial) = match codec {
		mp4_atom::Codec::Avc1(avc1) => (
			h264(false, &avc1.avcc, &avc1.visual, container)?,
			avc1.colr.as_slice(),
			avc1.pasp.as_ref(),
			avc1.spatial.as_deref(),
		),
		mp4_atom::Codec::Avc3(avc3) => (
			h264(true, &avc3.avcc, &avc3.visual, container)?,
			avc3.colr.as_slice(),
			avc3.pasp.as_ref(),
			avc3.spatial.as_deref(),
		),
		mp4_atom::Codec::Hev1(hev1) => (
			h265(true, &hev1.hvcc, &hev1.visual, container)?,
			hev1.colr.as_slice(),
			hev1.pasp.as_ref(),
			hev1.spatial.as_deref(),
		),
		mp4_atom::Codec::Hvc1(hvc1) => (
			h265(false, &hvc1.hvcc, &hvc1.visual, container)?,
			hvc1.colr.as_slice(),
			hvc1.pasp.as_ref(),
			hvc1.spatial.as_deref(),
		),
		mp4_atom::Codec::Vp08(vp08) => {
			let mut config = VideoConfig::new(VideoCodec::VP8);
			// vpcC carries VP8's colour too; its range is always explicit.
			let vpcc = &vp08.vpcc;
			config.color = crate::codec::color::cicp(
				vpcc.color_primaries,
				vpcc.transfer_characteristics,
				vpcc.matrix_coefficients,
				Some(vpcc.video_full_range_flag),
			);
			config.coded_width = Some(vp08.visual.width as _);
			config.coded_height = Some(vp08.visual.height as _);
			config.container = container;
			(
				config,
				vp08.colr.as_slice(),
				vp08.pasp.as_ref(),
				vp08.spatial.as_deref(),
			)
		}
		mp4_atom::Codec::Vp09(vp09) => {
			// https://github.com/gpac/mp4box.js/blob/325741b592d910297bf609bc7c400fc76101077b/src/box-codecs.js#L238
			let vpcc = &vp09.vpcc;

			let vp9 = VP9 {
				profile: vpcc.profile,
				level: vpcc.level,
				bit_depth: vpcc.bit_depth,
				color_primaries: vpcc.color_primaries,
				chroma_subsampling: vpcc.chroma_subsampling,
				transfer_characteristics: vpcc.transfer_characteristics,
				matrix_coefficients: vpcc.matrix_coefficients,
				full_range: vpcc.video_full_range_flag,
			};
			let mut config = VideoConfig::new(vp9.clone());
			config.color = crate::codec::vp9::color(&vp9);
			config.coded_width = Some(vp09.visual.width as _);
			config.coded_height = Some(vp09.visual.height as _);
			config.container = container;
			(
				config,
				vp09.colr.as_slice(),
				vp09.pasp.as_ref(),
				vp09.spatial.as_deref(),
			)
		}
		mp4_atom::Codec::Av01(av01) => {
			let mut config = VideoConfig::new(crate::codec::av1::av1_from_av1c(&av01.av1c));
			config.color = crate::codec::av1::color(&av01.av1c.config_obus);
			config.coded_width = Some(av01.visual.width as _);
			config.coded_height = Some(av01.visual.height as _);
			config.container = container;
			(
				config,
				av01.colr.as_slice(),
				av01.pasp.as_ref(),
				av01.spatial.as_deref(),
			)
		}
		mp4_atom::Codec::Unknown(unknown, _) => return Err(Error::UnknownCodec(unknown.to_string()).into()),
		unsupported => return Err(Error::unsupported_codec(unsupported).into()),
	};
	// The codec config outranks the colr box a muxer added, which only fills its gaps.
	for entry in colr {
		let color = super::colr_color(entry)?;
		crate::codec::color::fill(&mut config.color, color.as_ref(), "colr");
	}
	(config.projection, config.stereo) = super::spatial(spatial)?;
	if let Some(pasp) = pasp {
		let width = config.coded_width.ok_or(Error::InvalidAspect("missing width"))?;
		let height = config.coded_height.ok_or(Error::InvalidAspect("missing height"))?;
		let aspect = crate::codec::video::reduce_aspect(
			u128::from(width) * u128::from(pasp.h_spacing),
			u128::from(height) * u128::from(pasp.v_spacing),
		)
		.ok_or(Error::InvalidAspect("invalid or unrepresentable pasp"))?;
		// Square samples add no catalog field, but can still contradict an SPS.
		if pasp.h_spacing != pasp.v_spacing || config.display_aspect_width.is_some() {
			crate::codec::video::fill_display_aspect(&mut config, aspect, "pasp");
		}
	}

	Ok(config)
}

/// The avcC stays the description even for avc3, where it may list no parameter
/// sets: it still carries the NAL length size the samples are framed with.
fn h264(inline: bool, avcc: &mp4_atom::Avcc, visual: &mp4_atom::Visual, container: Container) -> Result<VideoConfig> {
	let mut description = BytesMut::new();
	avcc.encode_body(&mut description).map_err(crate::Error::mp4)?;
	let description = description.freeze();

	// Start from everything the avcC's SPS signals, so no field is dropped by a copy list.
	let mut config = crate::codec::h264::config(&description)?;
	config.codec = H264 {
		profile: avcc.avc_profile_indication,
		constraints: avcc.profile_compatibility,
		level: avcc.avc_level_indication,
		inline,
	}
	.into();
	// The SPS says what the decoder outputs, where a sample entry can disagree, as one declaring
	// 640x359 for 640x360 pictures does. The entry stands in when no SPS parses.
	config.coded_width = config.coded_width.or(Some(visual.width.into()));
	config.coded_height = config.coded_height.or(Some(visual.height.into()));
	config.description = Some(description);
	config.container = container;
	Ok(config)
}

fn h265(in_band: bool, hvcc: &mp4_atom::Hvcc, visual: &mp4_atom::Visual, container: Container) -> Result<VideoConfig> {
	let mut description = BytesMut::new();
	hvcc.encode_body(&mut description).map_err(crate::Error::mp4)?;

	let mut config = VideoConfig::new(H265 {
		in_band,
		profile_space: hvcc.general_profile_space,
		profile_idc: hvcc.general_profile_idc,
		profile_compatibility_flags: hvcc.general_profile_compatibility_flags,
		tier_flag: hvcc.general_tier_flag,
		level_idc: hvcc.general_level_idc,
		constraint_flags: hvcc.general_constraint_indicator_flags,
	});
	config.description = Some(description.freeze());
	config.coded_width = Some(visual.width as _);
	config.coded_height = Some(visual.height as _);
	crate::codec::h265::apply_hvcc_sps(hvcc, &mut config)?;
	config.container = container;
	Ok(config)
}

/// The catalog entry an audio sample entry describes, wrapped in `container`.
pub(crate) fn audio(codec: &mp4_atom::Codec, container: Container) -> Result<AudioConfig> {
	let config = match codec {
		mp4_atom::Codec::Mp4a(mp4a) => {
			let desc = &mp4a.esds.es_desc.dec_config;

			// TODO Also support mp4a.67
			if desc.object_type_indication != AAC {
				return Err(Error::UnsupportedMpeg2.into());
			}

			// The descriptor's bitrate is optional: a 0 means "unknown", so leave the field unset
			// and let the frame-size detector fill it instead of publishing a bogus 0.
			let bitrate = desc.avg_bitrate.max(desc.max_bitrate);
			let dec_specific = desc.dec_specific.as_ref().ok_or(Error::MissingDecoderSpecific)?;

			// The AudioSpecificConfig, kept whole so SBR/PS extensions reach the decoder, also
			// holds the rate and channels: the sample entry's 16-bit fields can't hold 96 kHz,
			// and describe the core rather than what SBR and PS decode to.
			let mut config = crate::codec::aac::config(&dec_specific.raw)?;
			if bitrate > 0 {
				config.bitrate = Some(bitrate.into());
			}
			config.container = container;
			config
		}
		mp4_atom::Codec::Opus(opus) => {
			// dOps carries the OpusHead fields in ISOBMFF byte order; republish them as the
			// OpusHead description so decoders apply its pre-skip, gain, and channel mapping.
			let dops = &opus.dops;
			let mut head = crate::codec::opus::Config::new(dops.input_sample_rate, dops.output_channel_count as u32)
				.with_pre_skip(dops.pre_skip);
			head.output_gain = dops.output_gain;
			head.mapping = dops.channel_mapping.as_ref().map(super::opus_mapping).transpose()?;

			// The shared conversion sets the decoder's 48 kHz output rate and Opus warmup,
			// whatever the sample entry or the informational input rate claims.
			let description = head.encode()?;
			let mut config: AudioConfig = head.into();
			config.description = Some(description);
			config.container = container;
			config
		}
		mp4_atom::Codec::Flac(flac) => {
			// Rate/channels come from STREAMINFO, not the sample entry's `Audio`
			// box: the latter's sample rate is 16.16 fixed point and can't carry
			// FLAC's high rates (96k/192k). STREAMINFO is also where the decoder
			// description comes from.
			let info = flac
				.dfla
				.blocks
				.iter()
				.find_map(|block| match block {
					mp4_atom::FlacMetadataBlock::StreamInfo {
						minimum_block_size,
						maximum_block_size,
						minimum_frame_size,
						maximum_frame_size,
						sample_rate,
						num_channels_minus_one,
						bits_per_sample_minus_one,
						number_of_interchannel_samples,
						md5_checksum,
					} => Some(crate::codec::flac::Config {
						min_block_size: *minimum_block_size,
						max_block_size: *maximum_block_size,
						min_frame_size: u32::from(*minimum_frame_size),
						max_frame_size: u32::from(*maximum_frame_size),
						sample_rate: *sample_rate,
						channel_count: *num_channels_minus_one as u32 + 1,
						bits_per_sample: *bits_per_sample_minus_one as u32 + 1,
						total_samples: *number_of_interchannel_samples,
						md5: md5_checksum.as_slice().try_into().unwrap_or([0; 16]),
					}),
					_ => None,
				})
				.ok_or(crate::codec::flac::Error::MissingStreamInfo)?;

			let mut config = AudioConfig::new(AudioCodec::Flac, info.sample_rate, info.channel_count);
			config.description = Some(info.description());
			config.container = container;
			config
		}
		mp4_atom::Codec::Unknown(unknown, _) => return Err(Error::UnknownCodec(unknown.to_string()).into()),
		unsupported => return Err(Error::unsupported_codec(unsupported).into()),
	};

	Ok(config)
}
