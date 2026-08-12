//! Derivative catalog construction: pick the source rendition, size the ladder
//! against it, and fill the output catalog with rung + passthrough entries.

use hang::catalog::{AV1, Video, VideoCodec, VideoConfig};
use moq_net::PathRelativeOwned;

use crate::{Error, Rung};

/// A rung resolved against the source: concrete geometry and encoder settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Resolved {
	/// The rendition/track name, e.g. `video/360p`.
	pub name: String,
	/// The output resolution, derived from the source aspect ratio.
	pub size: moq_video::Size,
	pub bitrate: u64,
	pub framerate: u32,
}

/// Pick the rendition to transcode from: the highest-resolution decodable
/// (H.264/H.265/AV1) rendition local to the source broadcast.
pub(crate) fn choose_source(video: &Video) -> Result<(String, VideoConfig), Error> {
	video
		.renditions
		.iter()
		// A rendition that itself lives in another broadcast can't be subscribed
		// through this one; composing relative references is a follow-up.
		.filter(|(_, config)| config.broadcast.is_none())
		.filter(|(_, config)| can_decode(config))
		.max_by_key(|(_, config)| (config.coded_height, config.coded_width, config.bitrate))
		.map(|(name, config)| (name.clone(), config.clone()))
		.ok_or(Error::NoSource)
}

fn can_decode(config: &VideoConfig) -> bool {
	match &config.codec {
		VideoCodec::H264(_) | VideoCodec::H265(_) => true,
		VideoCodec::AV1(av1) => is_supported_av1(av1),
		_ => false,
	}
}

fn is_supported_av1(av1: &AV1) -> bool {
	av1.bitdepth == 8 && !av1.mono_chrome && av1.chroma_subsampling_x && av1.chroma_subsampling_y
}

/// Resolve the configured rungs against the source: derive geometry from the
/// source aspect ratio and drop any rung that isn't strictly below the source.
pub(crate) fn resolve_rungs(rungs: &[Rung], source_name: &str, source: &VideoConfig) -> Result<Vec<Resolved>, Error> {
	let (source_width, source_height) = match (source.coded_width, source.coded_height) {
		(Some(w), Some(h)) if w > 0 && h > 0 => (w as u64, h as u64),
		_ => return Err(Error::SourceDimensions(source_name.to_string())),
	};
	let framerate = source
		.framerate
		.map(|f| f.round() as u32)
		.filter(|f| *f > 0)
		.unwrap_or(30);

	let mut resolved: Vec<Resolved> = Vec::new();
	for rung in rungs {
		let height = (rung.height & !1) as u64;
		if height == 0 || height > source_height {
			// Never upscale.
			continue;
		}
		// A same-height rung is only useful at a lower bitrate, and an unknown
		// source bitrate can't prove that.
		if height == source_height && source.bitrate.is_none() {
			continue;
		}
		if source.bitrate.is_some_and(|bitrate| rung.bitrate >= bitrate) {
			continue;
		}
		// Preserve the source aspect ratio, rounded to even for I420 chroma.
		let width = ((source_width * height + source_height / 2) / source_height) & !1;
		if width == 0 {
			continue;
		}

		let rung = Resolved {
			name: format!("video/{height}p"),
			size: moq_video::Size::new(width as u32, height as u32),
			bitrate: rung.bitrate,
			framerate,
		};
		// Duplicate heights in the config would collide on the track name.
		if resolved.iter().any(|other| other.name == rung.name) {
			continue;
		}
		resolved.push(rung);
	}
	Ok(resolved)
}

/// The catalog entry for a resolved rung.
///
/// Built from the ladder rather than the bitstream, via the same
/// [`Hint`](moq_video::encode::Hint) a `moq-video` publisher advertises its own track with, so the
/// catalog can be published before any encoder exists and stays deterministic.
pub(crate) fn rung_entry(rung: &Resolved, source: &VideoConfig) -> VideoConfig {
	let mut hint = moq_video::encode::Hint::new(moq_video::encode::Codec::H264);
	hint.size = Some(rung.size);
	hint.framerate = Some(rung.framerate);
	hint.bitrate = Some(rung.bitrate);

	let mut config = VideoConfig::from(&hint);
	config.optimize_for_latency = source.optimize_for_latency;
	config
}

/// Fill the derivative catalog: rung entries plus, when `source_rel` is set,
/// every source rendition referenced through it (so players fetch those tracks
/// from the source broadcast directly). Called again on each source catalog
/// update; the rung entries are fixed, the passthrough entries track the source.
pub(crate) fn populate(
	out: &mut moq_mux::catalog::hang::Catalog,
	source: &moq_mux::catalog::hang::Catalog,
	rungs: &[(String, VideoConfig)],
	source_rel: Option<&PathRelativeOwned>,
) -> Result<(), Error> {
	out.video = Video::default();
	out.audio = hang::catalog::Audio::default();

	// Display metadata applies to the rungs too (same picture, smaller).
	out.video.display = source.video.display.clone();
	out.video.rotation = source.video.rotation;
	out.video.flip = source.video.flip;

	for (name, config) in rungs {
		out.video.insert(name, config.clone())?;
	}

	let Some(rel) = source_rel else {
		return Ok(());
	};

	for (name, config) in &source.video.renditions {
		if config.broadcast.is_some() {
			// Already a reference into another broadcast; composing relative
			// paths is a follow-up.
			continue;
		}
		let mut config = config.clone();
		config.broadcast = Some(rel.clone());
		if out.video.insert(name, config).is_err() {
			tracing::warn!(rendition = %name, "source video rendition collides with a rung name; skipping");
		}
	}

	for (name, config) in &source.audio.renditions {
		if config.broadcast.is_some() {
			continue;
		}
		let mut config = config.clone();
		config.broadcast = Some(rel.clone());
		if out.audio.insert(name, config).is_err() {
			tracing::warn!(rendition = %name, "duplicate source audio rendition; skipping");
		}
	}

	Ok(())
}

#[cfg(test)]
mod tests {
	use hang::catalog::H264;

	use super::*;

	fn source(width: u32, height: u32, bitrate: Option<u64>) -> VideoConfig {
		let mut config = VideoConfig::new(H264 {
			inline: true,
			profile: 0x64,
			constraints: 0,
			level: 40,
		});
		config.coded_width = Some(width);
		config.coded_height = Some(height);
		config.bitrate = bitrate;
		config.framerate = Some(30.0);
		config
	}

	#[test]
	fn rungs_never_upscale() {
		let rungs = crate::Config::default().rungs;
		let resolved = resolve_rungs(&rungs, "video", &source(854, 480, Some(2_000_000))).unwrap();
		let names: Vec<_> = resolved.iter().map(|r| r.name.as_str()).collect();
		// A 480p source keeps only the strictly-lower rungs: the 480p rung is
		// admitted only because its bitrate (1.2M) undercuts the source (2M).
		assert_eq!(names, ["video/480p", "video/360p", "video/240p"]);
	}

	#[test]
	fn same_height_needs_lower_bitrate() {
		let rungs = vec![Rung::new(480, 1_200_000)];
		// Unknown source bitrate: a same-height rung can't prove it's below.
		assert!(
			resolve_rungs(&rungs, "video", &source(854, 480, None))
				.unwrap()
				.is_empty()
		);
		// Source bitrate below the rung: dropped too.
		assert!(
			resolve_rungs(&rungs, "video", &source(854, 480, Some(1_000_000)))
				.unwrap()
				.is_empty()
		);
	}

	#[test]
	fn rung_geometry_follows_source_aspect() {
		let resolved = resolve_rungs(
			&[Rung::new(360, 600_000)],
			"video",
			&source(1920, 1080, Some(6_000_000)),
		)
		.unwrap();
		assert_eq!(resolved.len(), 1);
		assert_eq!(resolved[0].size, moq_video::Size::new(640, 360));

		// Vertical video: aspect preserved, width rounded to even.
		let resolved = resolve_rungs(
			&[Rung::new(360, 600_000)],
			"video",
			&source(1080, 1920, Some(6_000_000)),
		)
		.unwrap();
		assert_eq!(resolved[0].size, moq_video::Size::new(202, 360));
	}

	#[test]
	fn source_needs_dimensions() {
		let mut config = source(0, 0, Some(1_000_000));
		config.coded_width = None;
		config.coded_height = None;
		assert!(matches!(
			resolve_rungs(&[Rung::new(360, 600_000)], "video", &config),
			Err(Error::SourceDimensions(_))
		));
	}

	/// A rung's entry describes the rung, not the source: the codec string's level and every
	/// dimension come from what this rung will encode, since a player picks between rungs on
	/// exactly those fields before a single frame exists.
	#[test]
	fn rung_entry_describes_the_rung() {
		let rung = Resolved {
			name: "video/360p".to_string(),
			size: moq_video::Size::new(640, 360),
			bitrate: 600_000,
			framerate: 30,
		};
		let mut source = source(1920, 1080, Some(6_000_000));
		source.optimize_for_latency = Some(true);

		let entry = rung_entry(&rung, &source);
		// 640x360 @ 30fps is 920 MBs * 30 = 27600 MB/s: past level 2.2's 20250, so level 3.0.
		assert_eq!(entry.codec.to_string(), "avc3.64001e");
		assert_eq!(entry.coded_width, Some(640));
		assert_eq!(entry.coded_height, Some(360));
		assert_eq!(entry.bitrate, Some(600_000));
		assert_eq!(entry.framerate, Some(30.0));
		// Inherited from the source: latency is a property of the stream, not the ladder.
		assert_eq!(entry.optimize_for_latency, Some(true));
	}

	#[test]
	fn chooses_highest_local_rendition() {
		let mut video = Video::default();
		video.insert("low", source(640, 360, None)).unwrap();
		video.insert("high", source(1920, 1080, None)).unwrap();
		let mut remote = source(3840, 2160, None);
		remote.broadcast = Some(PathRelativeOwned::from("../other".to_string()));
		video.insert("remote", remote).unwrap();

		let (name, config) = choose_source(&video).unwrap();
		assert_eq!(name, "high");
		assert_eq!(config.coded_height, Some(1080));
	}

	#[test]
	fn chooses_av1_source() {
		let mut video = Video::default();
		let mut av1 = VideoConfig::new(hang::catalog::AV1::default());
		av1.coded_width = Some(1920);
		av1.coded_height = Some(1080);
		video.insert("av1", av1).unwrap();

		let (name, config) = choose_source(&video).unwrap();
		assert_eq!(name, "av1");
		assert!(matches!(config.codec, VideoCodec::AV1(_)));
	}

	#[test]
	fn skips_unsupported_av1_source() {
		let mut video = Video::default();
		let mut av1 = VideoConfig::new(hang::catalog::AV1 {
			bitdepth: 10,
			..hang::catalog::AV1::default()
		});
		av1.coded_width = Some(3840);
		av1.coded_height = Some(2160);
		video.insert("av1", av1).unwrap();
		video.insert("h264", source(1920, 1080, None)).unwrap();

		let (name, config) = choose_source(&video).unwrap();
		assert_eq!(name, "h264");
		assert!(matches!(config.codec, VideoCodec::H264(_)));
	}
}
