//! Derivative catalog construction: pick the source rendition, size the ladder
//! against it, and fill the output catalog with rung + passthrough entries.

use hang::catalog::{AV1, Video, VideoCodec, VideoConfig};
use moq_net::PathRelativeOwned;

use crate::{Error, Ladder};

/// A rung resolved against the source: concrete geometry and encoder settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Resolved {
	/// The rendition/track name, e.g. `video/360p`.
	pub name: String,
	/// The output resolution, derived from the source aspect ratio.
	pub size: moq_video::Size,
	pub bitrate: moq_net::bandwidth::Rate,
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
		// A publisher can advertise its codec before it knows its picture: a capture whose camera
		// hasn't been opened publishes a rendition with no dimensions, and the first keyframe fills
		// them in. There is no ladder to derive from that yet, so leave it for a later snapshot
		// rather than choosing it and failing on geometry.
		.filter(|(_, config)| dimensions(config).is_some())
		.max_by_key(|(_, config)| (config.coded_height, config.coded_width, config.bitrate))
		.map(|(name, config)| (name.clone(), config.clone()))
		.ok_or(Error::NoSource)
}

/// The source geometry a ladder can be sized against, if it's known at all.
fn dimensions(config: &VideoConfig) -> Option<(u64, u64)> {
	match (config.coded_width, config.coded_height) {
		(Some(w), Some(h)) if w > 0 && h > 0 => Some((w as u64, h as u64)),
		_ => None,
	}
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

/// Resolve the configured ladder against the source: derive geometry from the
/// source aspect ratio and drop any rung that isn't strictly below the source.
///
/// Dropping a rung never reorders the rest, so the result keeps the ladder's
/// order: lowest rendition first, each one the next above its predecessor.
pub(crate) fn resolve_rungs(ladder: &Ladder, source_name: &str, source: &VideoConfig) -> Result<Vec<Resolved>, Error> {
	let Some((source_width, source_height)) = dimensions(source) else {
		return Err(Error::SourceDimensions(source_name.to_string()));
	};
	let framerate = source
		.framerate
		.map(|f| f.round() as u32)
		.filter(|f| *f > 0)
		.unwrap_or(30);

	let mut resolved: Vec<Resolved> = Vec::new();
	for rung in ladder.rungs() {
		// Even, non-zero, and unique across the ladder: `Ladder::new` guarantees
		// all three, so the track names below cannot collide.
		let height = rung.height as u64;
		if height > source_height {
			// Never upscale.
			continue;
		}
		// A same-height rung is only useful at a lower bitrate, and an unknown
		// source bitrate can't prove that.
		if height == source_height && source.bitrate.is_none() {
			continue;
		}
		if source.bitrate.is_some_and(|bitrate| rung.bitrate.as_bps() >= bitrate) {
			continue;
		}
		// Preserve the source aspect ratio, rounded to even for I420 chroma.
		let width = ((source_width * height + source_height / 2) / source_height) & !1;
		if width == 0 {
			continue;
		}

		resolved.push(Resolved {
			name: format!("video/{height}p"),
			size: moq_video::Size::new(width as u32, height as u32),
			bitrate: rung.bitrate,
			framerate,
		});
	}
	Ok(resolved)
}

/// The catalog entry for a resolved rung, probed from a throwaway encoder at the rung's geometry.
///
/// The ladder is published before any rung has been encoded (a rung is only encoded once someone
/// asks for it), and nothing ever refines these entries from a bitstream the way an importer would.
/// So the codec string has to be right the first time: it is read back out of the encoder that will
/// serve the rung rather than guessed from the ladder.
pub(crate) async fn rung_entry(
	rung: &Resolved,
	source: &VideoConfig,
	encoder: &moq_video::encode::Kind,
) -> Result<VideoConfig, Error> {
	let mut config = moq_video::encode::Config::new(rung.size.width, rung.size.height, rung.framerate);
	config.bitrate = Some(rung.bitrate);
	config.kind = encoder.clone();

	let mut entry = config.probe().await?;
	// A property of the source rather than the ladder: every rung shows the same picture.
	entry.optimize_for_latency = source.optimize_for_latency;
	Ok(entry)
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

	/// A ladder from `(height, bps)` pairs, in any order.
	fn ladder<const N: usize>(rungs: [(u32, u64); N]) -> Ladder {
		Ladder::new(
			rungs
				.into_iter()
				.map(|(height, bps)| crate::Rung::new(height, moq_net::bandwidth::Rate::from_bps(bps))),
		)
		.unwrap()
	}

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
		assert_eq!(names, ["video/240p", "video/360p", "video/480p"]);
	}

	/// Filtering against the source removes rungs from the middle of the ladder,
	/// and the survivors have to stay ranked: the rung before each one is still
	/// the next rendition down, which is what "the next lower rendition" means
	/// everywhere downstream.
	#[test]
	fn resolution_keeps_the_ladder_ordered() {
		// Written top-down and with a gap the source will punch out: the 1080p
		// rung is above a 720p source, and the 480p rung's 3M ceiling is over the
		// source's 2.5M.
		let rungs = ladder([(1080, 5_000_000), (480, 3_000_000), (360, 600_000), (240, 350_000)]);
		let resolved = resolve_rungs(&rungs, "video", &source(1280, 720, Some(2_500_000))).unwrap();

		let names: Vec<_> = resolved.iter().map(|r| r.name.as_str()).collect();
		assert_eq!(names, ["video/240p", "video/360p"]);
		for pair in resolved.windows(2) {
			assert!(pair[0].bitrate < pair[1].bitrate);
			assert!(pair[0].size.height < pair[1].size.height);
		}
	}

	#[test]
	fn same_height_needs_lower_bitrate() {
		let rungs = ladder([(480, 1_200_000)]);
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
		let resolved = resolve_rungs(&ladder([(360, 600_000)]), "video", &source(1920, 1080, Some(6_000_000))).unwrap();
		assert_eq!(resolved.len(), 1);
		assert_eq!(resolved[0].size, moq_video::Size::new(640, 360));

		// Vertical video: aspect preserved, width rounded to even.
		let resolved = resolve_rungs(&ladder([(360, 600_000)]), "video", &source(1080, 1920, Some(6_000_000))).unwrap();
		assert_eq!(resolved[0].size, moq_video::Size::new(202, 360));
	}

	/// A publisher that advertises its codec before opening its camera has no dimensions yet, and
	/// `run` keeps waiting for a snapshot it can use. Choosing that rendition instead would fail on
	/// geometry and terminate the transcode before any rung could create the demand that opens the
	/// camera in the first place.
	#[test]
	fn dimensionless_rendition_is_not_a_source_yet() {
		let mut provisional = source(0, 0, None);
		provisional.coded_width = None;
		provisional.coded_height = None;

		let mut video = Video::default();
		video.renditions.insert("video".to_string(), provisional.clone());
		assert!(matches!(choose_source(&video), Err(Error::NoSource)));

		// The keyframe fills the geometry in, and the same rendition becomes usable.
		video.renditions.insert("video".to_string(), source(1920, 1080, None));
		let (name, chosen) = choose_source(&video).unwrap();
		assert_eq!(name, "video");
		assert_eq!(chosen.coded_width, Some(1920));
	}

	/// The ladder's order is a property of the configuration, not of the source,
	/// so it is settled long before any dimensions are known. A source that has
	/// not published its geometry yet is refused outright rather than resolved
	/// into a ladder ranked on guesses.
	#[test]
	fn source_needs_dimensions() {
		let rungs = ladder([(360, 600_000), (240, 350_000)]);
		let heights: Vec<_> = rungs.rungs().iter().map(|rung| rung.height).collect();
		assert_eq!(heights, [240, 360]);

		let mut config = source(0, 0, Some(1_000_000));
		config.coded_width = None;
		config.coded_height = None;
		assert!(matches!(
			resolve_rungs(&rungs, "video", &config),
			Err(Error::SourceDimensions(_))
		));

		// The keyframe fills the geometry in and the same ladder resolves, in the
		// order it already had.
		let resolved = resolve_rungs(&rungs, "video", &source(1280, 720, Some(2_500_000))).unwrap();
		let names: Vec<_> = resolved.iter().map(|r| r.name.as_str()).collect();
		assert_eq!(names, ["video/240p", "video/360p"]);
	}

	/// A rung's entry describes the rung, not the source: the codec string's level and every
	/// dimension come from what this rung will encode, since a player picks between rungs on
	/// exactly those fields before a single frame exists.
	#[tokio::test]
	async fn rung_entry_describes_the_rung() {
		let rung = Resolved {
			name: "video/360p".to_string(),
			size: moq_video::Size::new(640, 360),
			bitrate: moq_net::bandwidth::Rate::from_bps(600_000),
			framerate: 30,
		};
		let mut source = source(1920, 1080, Some(6_000_000));
		source.optimize_for_latency = Some(true);

		// Software (openh264) so the probe is deterministic and never touches a hardware backend.
		let entry = rung_entry(&rung, &source, &moq_video::encode::Kind::Software)
			.await
			.unwrap();

		// Read out of the encoder that will serve this rung, so the entry describes the rung rather
		// than the source: a player picks between rungs on exactly these fields, before a single
		// frame of any of them exists.
		let hang::catalog::VideoCodec::H264(h264) = &entry.codec else {
			panic!("expected H.264, got {}", entry.codec)
		};
		assert!(h264.inline, "an avc3 rung carries its parameter sets in band");
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
		remote.broadcast = Some(PathRelativeOwned::from("./other".to_string()));
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
