//! Derivative catalog construction: pick the source rendition, size the ladder
//! against it, and fill the output catalog with rung + passthrough entries.

use hang::catalog::{AV1, Video, VideoCodec, VideoConfig};
use moq_net::path::RelativeOwned;
use moq_video::decode::Codec;

use crate::{Error, Ladder};

/// A rung resolved against the source: concrete geometry and encoder settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Resolved {
	/// The rendition/track name serving this incarnation, e.g. `video/360p`.
	///
	/// Later incarnations of the same configured height carry a revision suffix
	/// (`video/360p.2`); see [`Names`].
	pub name: String,
	/// The configured height this rung serves, which the name is derived from.
	pub height: u32,
	/// The output resolution, derived from the source aspect ratio.
	pub size: moq_video::Size,
	pub bitrate: moq_net::bandwidth::Rate,
	pub framerate: Option<moq_video::Rate>,
}

impl Resolved {
	/// Whether `other` resolves to exactly this picture, whatever it is named.
	pub fn same_shape(&self, other: &Self) -> bool {
		self.height == other.height
			&& self.size == other.size
			&& self.bitrate == other.bitrate
			&& self.framerate == other.framerate
	}
}

/// The track names a ladder has handed out, so it never hands one out twice.
///
/// A retired rung ends its track for good, and a clean end is terminal: a relay
/// keeps the finished logical track (`broadcast::Consumer::track_inner` only
/// drops an *aborted* one) and serves its EOF to every later subscriber, so a
/// request for that name never reaches the transcoder again. A rung re-resolved
/// under a new picture is therefore published under a name the ladder has never
/// used, rather than reusing the one it just finished.
#[derive(Default)]
pub(crate) struct Names {
	/// The revision last issued per configured height. The first is the bare
	/// `video/360p`, so a suffix only ever appears on a rung that was resolved
	/// again.
	revisions: std::collections::HashMap<u32, u32>,
}

impl Names {
	/// A name for a fresh incarnation of `height`.
	pub fn mint(&mut self, height: u32) -> String {
		let revision = self.revisions.entry(height).and_modify(|r| *r += 1).or_insert(1);
		match *revision {
			1 => format!("video/{height}p"),
			revision => format!("video/{height}p.{revision}"),
		}
	}
}

/// A resolved rung together with the catalog entry advertising it.
///
/// The two are built and replaced as a unit, so an entry can never describe a
/// geometry the rung no longer encodes.
#[derive(Clone, Debug)]
pub(crate) struct Published {
	pub rung: Resolved,
	pub entry: VideoConfig,
}

/// Which codecs the configured decoder opens on this host, each probed once.
///
/// A rendition's codec string says what a decoder must handle, not whether this
/// host has one: a software-only build decodes H.264 and nothing else, and a
/// backend forced by name may not be here at all. Only opening a decoder tells,
/// so each codec is opened (and dropped) the first time a candidate needs it and
/// the answer kept for every later snapshot.
pub(crate) struct Decoders {
	config: moq_video::decode::Config,
	/// Each codec probed so far, and whether its decoder opened.
	probed: Vec<(Codec, bool)>,
}

impl Decoders {
	pub fn new(config: moq_video::decode::Config) -> Self {
		Self {
			config,
			probed: Vec::new(),
		}
	}

	/// A host that decodes exactly `codecs`, for tests that must not depend on
	/// the machine they run on. Nothing is opened until a refusal asks why.
	#[cfg(test)]
	fn assume(codecs: &[Codec]) -> Self {
		let probed = [Codec::H264, Codec::H265, Codec::Av1]
			.into_iter()
			.map(|codec| (codec, codecs.contains(&codec)))
			.collect();
		Self {
			config: moq_video::decode::Config::new(),
			probed,
		}
	}

	/// Whether `rendition` (in `codec`) has a decoder on this host.
	async fn probe(&mut self, codec: Codec, rendition: &VideoConfig) -> bool {
		if let Some((_, decodes)) = self.probed.iter().find(|(probed, _)| *probed == codec) {
			return *decodes;
		}

		let decodes = match self.open(rendition).await {
			Ok(()) => true,
			Err(err) => {
				tracing::warn!(?codec, %err, "no decoder for this codec; its renditions will not be transcoded");
				false
			}
		};
		self.probed.push((codec, decodes));
		decodes
	}

	/// Why `rendition`'s decoder refused to open. Only the verdict is cached, so
	/// this opens it once more, on a path that has nothing left to serve.
	///
	/// `Ok(())` means that reopen succeeded. The first failure was transient, so
	/// the cached refusal is cleared and the caller selects this rendition
	/// instead of waiting on a catalog that will not change.
	async fn refusal(&mut self, rendition: &VideoConfig) -> Result<(), Error> {
		match self.open(rendition).await {
			Err(err) => Err(err.into()),
			Ok(()) => {
				if let Some(codec) = codec(rendition)
					&& let Some((_, decodes)) = self.probed.iter_mut().find(|(probed, _)| *probed == codec)
				{
					*decodes = true;
				}
				Ok(())
			}
		}
	}

	/// Open and drop a decoder for `rendition`.
	async fn open(&self, rendition: &VideoConfig) -> Result<(), moq_video::Error> {
		// Parameter sets in band, so the probe asks about the backend and not about
		// this rendition's description: a malformed one belongs to the stream and
		// fails when the rendition is decoded, not as a verdict on the whole codec.
		let mut config = rendition.clone();
		config.description = None;
		match &mut config.codec {
			VideoCodec::H264(h264) => h264.inline = true,
			VideoCodec::H265(h265) => h265.in_band = true,
			_ => {}
		}
		moq_video::decode::Sink::open(&config, &self.config).await.map(drop)
	}
}

/// Pick the rendition to transcode from: the highest-resolution rendition local
/// to the source broadcast that this host can decode.
///
/// [`Error::NoSource`] means wait for a later snapshot. Any other error means
/// nothing on offer can be decoded here, and is why the tallest one refused.
pub(crate) async fn choose_source(video: &Video, decoders: &mut Decoders) -> Result<(String, VideoConfig), Error> {
	let mut candidates: Vec<_> = video
		.renditions
		.iter()
		// A rendition that itself lives in another broadcast can't be subscribed
		// through this one; composing relative references is a follow-up.
		.filter(|(_, config)| config.broadcast.is_none())
		.filter_map(|(name, config)| Some((name, config, codec(config)?)))
		.collect();
	// Largest first, ties going to the last name as they always have. A rendition
	// without dimensions sorts after every one with them: it can't be chosen yet,
	// but it can still keep the transcoder waiting.
	candidates.reverse();
	candidates.sort_by_key(|(_, config, _)| {
		std::cmp::Reverse((
			dimensions(config).is_some(),
			config.coded_height,
			config.coded_width,
			config.bitrate,
		))
	});

	let mut refused = None;
	for (name, config, codec) in candidates {
		match decoders.probe(codec, config).await {
			true if dimensions(config).is_some() => return Ok((name.clone(), config.clone())),
			// A publisher can advertise its codec before it knows its picture: a capture
			// whose camera hasn't been opened publishes a rendition with no dimensions,
			// and the first keyframe fills them in. There is no ladder to derive from
			// that yet, but it will be usable, so wait for it rather than refuse.
			true => return Err(Error::NoSource),
			false => {
				refused.get_or_insert((name, config));
			}
		}
	}

	let Some((name, config)) = refused else {
		return Err(Error::NoSource);
	};
	match decoders.refusal(config).await {
		Err(err) => {
			tracing::warn!(rendition = %name, "no source rendition can be decoded on this host");
			Err(err)
		}
		// The picture is known, so this is the source. A missing size still has
		// to wait, but the codec is cached as decodable for the next snapshot.
		Ok(()) if dimensions(config).is_some() => Ok((name.clone(), config.clone())),
		Ok(()) => Err(Error::NoSource),
	}
}

/// Re-pick the source rendition for a new snapshot, preferring the one already
/// chosen while it is still on offer.
///
/// Running [`choose_source`] afresh on every snapshot would hand the ladder to a
/// taller rendition the moment a publisher advertises one, retiring every rung
/// that was serving. Sticking to the current name follows the picture it now
/// carries (the point of this path) without treating a momentary catalog edit as
/// a source switch.
pub(crate) async fn follow_source(
	video: &Video,
	current: &str,
	decoders: &mut Decoders,
) -> Result<(String, VideoConfig), Error> {
	if let Some(config) = video.renditions.get(current)
		&& config.broadcast.is_none()
		&& dimensions(config).is_some()
		&& let Some(codec) = codec(config)
		// The name can stay while the codec changes under it.
		&& decoders.probe(codec, config).await
	{
		return Ok((current.to_string(), config.clone()));
	}
	choose_source(video, decoders).await
}

/// Whether a rung decoding `old` can keep decoding `new` untouched.
///
/// Decoders are opened from the codec, container, and any out-of-band codec
/// description. They read geometry changes from the bitstream, so a source that
/// only resized is still the same stream: the shared decode and every rung that
/// still fits carry on, and only the ladder moves.
pub(crate) fn same_stream(old: &VideoConfig, new: &VideoConfig) -> bool {
	old.codec == new.codec && old.container == new.container && old.description == new.description
}

/// The source geometry a ladder can be sized against, if it's known at all.
fn dimensions(config: &VideoConfig) -> Option<(u64, u64)> {
	match (config.coded_width, config.coded_height) {
		(Some(w), Some(h)) if w > 0 && h > 0 => Some((w as u64, h as u64)),
		_ => None,
	}
}

/// The decoder a rendition needs, if `moq-video` has one for its codec at all.
fn codec(config: &VideoConfig) -> Option<Codec> {
	match &config.codec {
		VideoCodec::H264(_) => Some(Codec::H264),
		VideoCodec::H265(_) => Some(Codec::H265),
		VideoCodec::AV1(av1) if is_supported_av1(av1) => Some(Codec::Av1),
		_ => None,
	}
}

fn is_supported_av1(av1: &AV1) -> bool {
	av1.bitdepth == 8 && !av1.mono_chrome && av1.chroma_subsampling_x && av1.chroma_subsampling_y
}

/// Resolve the configured rungs against the source: derive geometry from the
/// source aspect ratio and drop any rung that isn't strictly below the source.
///
/// The names are placeholders: [`Names`] decides whether a rung keeps the name
/// its predecessor was serving or takes a fresh one.
pub(crate) fn resolve_rungs(ladder: &Ladder, source_name: &str, source: &VideoConfig) -> Result<Vec<Resolved>, Error> {
	let Some((source_width, source_height)) = dimensions(source) else {
		return Err(Error::SourceDimensions(source_name.to_string()));
	};
	let framerate = source.framerate.map(moq_video::Rate::from_f64).transpose()?;

	let mut resolved: Vec<Resolved> = Vec::new();
	for rung in ladder.rungs() {
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

		let height = height as u32;
		resolved.push(Resolved {
			name: format!("video/{height}p"),
			height,
			size: moq_video::Size::new(width as u32, height),
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
	let encode_rate = rung.framerate.unwrap_or(moq_video::Rate::new(30, 1).unwrap());
	let mut config = moq_video::encode::Config::new(rung.size.width, rung.size.height, encode_rate);
	config.bitrate = Some(rung.bitrate);
	config.kind = encoder.clone();

	let mut entry = config.probe().await?;
	entry.framerate = rung.framerate.map(moq_video::Rate::as_f64);
	// A property of the source rather than the ladder: every rung shows the same picture.
	entry.optimize_for_latency = source.optimize_for_latency;
	Ok(entry)
}

/// Rungs inherit the state of the rendition the pipeline actually decodes.
pub(crate) fn inherit_stalled(rungs: &mut [Published], source: &VideoConfig) {
	for published in rungs {
		published.entry.stalled = source.stalled;
	}
}

/// Fill the derivative catalog: rung entries plus, when `source_rel` is set,
/// every source rendition referenced through it (so players fetch those tracks
/// from the source broadcast directly). Called again on each source catalog
/// update, with whatever the ladder resolved to for that snapshot.
pub(crate) fn populate(
	out: &mut moq_mux::catalog::hang::Catalog,
	source: &moq_mux::catalog::hang::Catalog,
	rungs: &[Published],
	source_rel: Option<&RelativeOwned>,
) -> Result<(), Error> {
	out.video = Video::default();
	out.audio = hang::catalog::Audio::default();
	// A derivative does not synthesize its own archive: keep the child's, including a
	// live-only timeline, so replay/store discovery survives composition. The clock goes with
	// it: the derivative republishes the child's timeline, so its timestamps only mean
	// something under the child's wall mapping.
	out.archive = source.archive.clone();
	out.clock = source.clock;

	// Display metadata applies to the rungs too (same picture, smaller).
	out.video.display = source.video.display.clone();
	out.video.rotation = source.video.rotation;
	out.video.flip = source.video.flip;

	for published in rungs {
		out.video.insert(&published.rung.name, published.entry.clone())?;
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
	use crate::Rung;

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
	fn rungs_inherit_a_stalled_source() {
		let mut source_catalog = moq_mux::catalog::hang::Catalog::default();
		let mut src = source(1280, 720, Some(2_500_000));
		src.stalled = Some(true);
		source_catalog.video.insert("video", src).unwrap();

		let mut published = [Published {
			rung: Resolved {
				name: "video/360p".into(),
				height: 360,
				size: moq_video::Size::new(640, 360),
				bitrate: moq_net::bandwidth::Rate::from_bps(600_000),
				framerate: Some(moq_video::Rate::new(30, 1).unwrap()),
			},
			entry: source(640, 360, Some(600_000)),
		}];

		inherit_stalled(&mut published, &source_catalog.video.renditions["video"]);
		let mut out = moq_mux::catalog::hang::Catalog::default();
		populate(&mut out, &source_catalog, &published, None).unwrap();
		assert_eq!(
			out.video.renditions.get("video/360p").and_then(|c| c.stalled),
			Some(true)
		);

		// A different local rendition may stay stalled after the selected input recovers.
		let healthy = source(1920, 1080, Some(5_000_000));
		source_catalog.video.insert("healthy", healthy.clone()).unwrap();
		inherit_stalled(&mut published, &healthy);
		populate(&mut out, &source_catalog, &published, None).unwrap();
		assert_eq!(out.video.renditions["video/360p"].stalled, None);
	}

	#[test]
	fn rungs_never_upscale() {
		let rungs = crate::Config::default().ladder;
		let resolved = resolve_rungs(&rungs, "video", &source(854, 480, Some(2_000_000))).unwrap();
		let names: Vec<_> = resolved.iter().map(|r| r.name.as_str()).collect();
		// A 480p source keeps only the strictly-lower rungs: the 480p rung is
		// admitted only because its bitrate (1.2M) undercuts the source (2M).
		assert_eq!(names, ["video/240p", "video/360p", "video/480p"]);
	}

	#[test]
	fn fractional_and_unknown_source_rates_stay_explicit() {
		let ladder = crate::Config::default().ladder;
		let mut fractional = source(1280, 720, Some(2_500_000));
		fractional.framerate = Some(30_000.0 / 1_001.0);
		let resolved = resolve_rungs(&ladder, "video", &fractional).unwrap();
		assert_eq!(
			resolved[0].framerate,
			Some(moq_video::Rate::new(30_000, 1_001).unwrap())
		);

		fractional.framerate = None;
		let resolved = resolve_rungs(&ladder, "video", &fractional).unwrap();
		assert_eq!(resolved[0].framerate, None);
	}

	#[test]
	fn filtering_preserves_order_across_source_changes() {
		let ladder = Ladder::new([
			Rung::new(720, moq_net::bandwidth::Rate::from_bps(2_500_000)),
			Rung::new(241, moq_net::bandwidth::Rate::from_bps(350_000)),
			Rung::new(480, moq_net::bandwidth::Rate::from_bps(1_200_000)),
			Rung::new(360, moq_net::bandwidth::Rate::from_bps(600_000)),
		])
		.unwrap();
		for (picture, expected) in [
			(source(1920, 1080, None), vec![240, 360, 480, 720]),
			(source(1280, 720, Some(1_000_000)), vec![240, 360]),
			(source(320, 180, None), vec![]),
			(source(1920, 1080, Some(6_000_000)), vec![240, 360, 480, 720]),
		] {
			let resolved = resolve_rungs(&ladder, "video", &picture).unwrap();
			assert_eq!(resolved.iter().map(|rung| rung.height).collect::<Vec<_>>(), expected);
			assert!(resolved.windows(2).all(|pair| pair[0].bitrate < pair[1].bitrate));
		}
	}

	#[test]
	fn same_height_needs_lower_bitrate() {
		let rungs = Ladder::new([Rung::new(480, moq_net::bandwidth::Rate::from_bps(1_200_000))]).unwrap();
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
			&Ladder::new([Rung::new(360, moq_net::bandwidth::Rate::from_bps(600_000))]).unwrap(),
			"video",
			&source(1920, 1080, Some(6_000_000)),
		)
		.unwrap();
		assert_eq!(resolved.len(), 1);
		assert_eq!(resolved[0].size, moq_video::Size::new(640, 360));

		// Vertical video: aspect preserved, width rounded to even.
		let resolved = resolve_rungs(
			&Ladder::new([Rung::new(360, moq_net::bandwidth::Rate::from_bps(600_000))]).unwrap(),
			"video",
			&source(1080, 1920, Some(6_000_000)),
		)
		.unwrap();
		assert_eq!(resolved[0].size, moq_video::Size::new(202, 360));
	}

	/// A publisher that advertises its codec before opening its camera has no dimensions yet, and
	/// `run` keeps waiting for a snapshot it can use. Choosing that rendition instead would fail on
	/// geometry and terminate the transcode before any rung could create the demand that opens the
	/// camera in the first place.
	/// Every codec the tests build a rendition in.
	fn decodes_all() -> Decoders {
		Decoders::assume(&[Codec::H264, Codec::H265, Codec::Av1])
	}

	/// An H.265 rendition at `width`x`height`.
	fn hevc(width: u32, height: u32) -> VideoConfig {
		let mut config = VideoConfig::new(hang::catalog::H265 {
			in_band: true,
			profile_space: 0,
			profile_idc: 1,
			profile_compatibility_flags: [0x60, 0, 0, 0],
			tier_flag: false,
			level_idc: 120,
			constraint_flags: [0x90, 0, 0, 0, 0, 0],
		});
		config.coded_width = Some(width);
		config.coded_height = Some(height);
		config
	}

	#[tokio::test]
	async fn dimensionless_rendition_is_not_a_source_yet() {
		let mut provisional = source(0, 0, None);
		provisional.coded_width = None;
		provisional.coded_height = None;

		let mut video = Video::default();
		video.renditions.insert("video".to_string(), provisional.clone());
		assert!(matches!(
			choose_source(&video, &mut decodes_all()).await,
			Err(Error::NoSource)
		));

		// The keyframe fills the geometry in, and the same rendition becomes usable.
		video.renditions.insert("video".to_string(), source(1920, 1080, None));
		let (name, chosen) = choose_source(&video, &mut decodes_all()).await.unwrap();
		assert_eq!(name, "video");
		assert_eq!(chosen.coded_width, Some(1920));
	}

	#[test]
	fn source_needs_dimensions() {
		let mut config = source(0, 0, Some(1_000_000));
		config.coded_width = None;
		config.coded_height = None;
		assert!(matches!(
			resolve_rungs(
				&Ladder::new([Rung::new(360, moq_net::bandwidth::Rate::from_bps(600_000))]).unwrap(),
				"video",
				&config
			),
			Err(Error::SourceDimensions(_))
		));
	}

	/// An out-of-band parameter-set change changes what the decoder injects ahead
	/// of every keyframe, even when the codec string and container stay the same.
	#[test]
	fn a_new_description_is_a_new_decode_stream() {
		let mut before = source(1920, 1080, Some(6_000_000));
		let VideoCodec::H264(h264) = &mut before.codec else {
			unreachable!()
		};
		h264.inline = false;
		before.description = Some(bytes::Bytes::from_static(b"old avcC"));

		let mut after = before.clone();
		after.description = Some(bytes::Bytes::from_static(b"new avcC"));

		assert!(!same_stream(&before, &after));
	}

	/// A rung's entry describes the rung, not the source: the codec string's level and every
	/// dimension come from what this rung will encode, since a player picks between rungs on
	/// exactly those fields before a single frame exists.
	#[tokio::test]
	async fn rung_entry_describes_the_rung() {
		let rung = Resolved {
			name: "video/360p".to_string(),
			height: 360,
			size: moq_video::Size::new(640, 360),
			bitrate: moq_net::bandwidth::Rate::from_bps(600_000),
			framerate: Some(moq_video::Rate::new(30, 1).unwrap()),
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

	#[tokio::test]
	async fn an_unknown_rate_stays_unknown_after_probe() {
		let rung = Resolved {
			name: "video/360p".to_string(),
			height: 360,
			size: moq_video::Size::new(640, 360),
			bitrate: moq_net::bandwidth::Rate::from_bps(600_000),
			framerate: None,
		};
		let entry = rung_entry(
			&rung,
			&source(1920, 1080, Some(6_000_000)),
			&moq_video::encode::Kind::Software,
		)
		.await
		.unwrap();
		assert_eq!(entry.framerate, None);
	}

	/// A name is handed out once and never again: a retired rung's track ends for
	/// good, so its replacement has to be a name no subscriber can already hold a
	/// finished copy of.
	#[test]
	fn names_are_never_reused() {
		let mut names = Names::default();
		assert_eq!(names.mint(360), "video/360p");
		assert_eq!(names.mint(120), "video/120p");
		assert_eq!(names.mint(360), "video/360p.2");
		assert_eq!(names.mint(360), "video/360p.3");
		assert_eq!(names.mint(120), "video/120p.2");
	}

	#[tokio::test]
	async fn chooses_highest_local_rendition() {
		let mut video = Video::default();
		video.insert("low", source(640, 360, None)).unwrap();
		video.insert("high", source(1920, 1080, None)).unwrap();
		let mut remote = source(3840, 2160, None);
		remote.broadcast = Some(RelativeOwned::from("./other".to_string()));
		video.insert("remote", remote).unwrap();

		let (name, config) = choose_source(&video, &mut decodes_all()).await.unwrap();
		assert_eq!(name, "high");
		assert_eq!(config.coded_height, Some(1080));
	}

	#[tokio::test]
	async fn chooses_av1_source() {
		let mut video = Video::default();
		let mut av1 = VideoConfig::new(hang::catalog::AV1::default());
		av1.coded_width = Some(1920);
		av1.coded_height = Some(1080);
		video.insert("av1", av1).unwrap();

		let (name, config) = choose_source(&video, &mut decodes_all()).await.unwrap();
		assert_eq!(name, "av1");
		assert!(matches!(config.codec, VideoCodec::AV1(_)));
	}

	#[tokio::test]
	async fn skips_unsupported_av1_source() {
		let mut video = Video::default();
		let mut av1 = VideoConfig::new(hang::catalog::AV1 {
			bitdepth: 10,
			..hang::catalog::AV1::default()
		});
		av1.coded_width = Some(3840);
		av1.coded_height = Some(2160);
		video.insert("av1", av1).unwrap();
		video.insert("h264", source(1920, 1080, None)).unwrap();

		let (name, config) = choose_source(&video, &mut decodes_all()).await.unwrap();
		assert_eq!(name, "h264");
		assert!(matches!(config.codec, VideoCodec::H264(_)));
	}

	/// A software-only host decodes H.264 and nothing else, so a taller H.265
	/// rendition loses to it rather than failing once a rung opens its decoder.
	#[tokio::test]
	async fn skips_a_rendition_this_host_cannot_decode() {
		let mut video = Video::default();
		video.insert("hevc", hevc(1920, 1080)).unwrap();
		video.insert("avc", source(640, 360, None)).unwrap();
		let mut av1 = VideoConfig::new(hang::catalog::AV1::default());
		av1.coded_width = Some(3840);
		av1.coded_height = Some(2160);
		video.insert("av1", av1).unwrap();

		let mut decoders = Decoders::assume(&[Codec::H264]);
		let (name, _) = choose_source(&video, &mut decoders).await.unwrap();
		assert_eq!(name, "avc");

		// Hardware that decodes H.265 keeps the usual pick of the tallest it can.
		let (name, _) = choose_source(&video, &mut Decoders::assume(&[Codec::H264, Codec::H265]))
			.await
			.unwrap();
		assert_eq!(name, "hevc");
	}

	/// Nothing on offer decodes here: a refusal carrying why the tallest
	/// rendition's decoder refused, rather than waiting on a complete catalog.
	#[tokio::test]
	async fn refuses_when_no_rendition_decodes() {
		let mut video = Video::default();
		video.insert("small", hevc(640, 360)).unwrap();
		video.insert("large", hevc(1920, 1080)).unwrap();

		let mut config = moq_video::decode::Config::new();
		config.kind = moq_video::decode::Kind::Named("missing".to_string());
		match choose_source(&video, &mut Decoders::new(config)).await {
			Err(Error::Video(moq_video::Error::UnknownDecoder { name, codec, .. })) => {
				assert_eq!(name, "missing");
				assert_eq!(codec, Codec::H265);
			}
			other => panic!("expected the decoder's refusal, got {other:?}"),
		}
	}

	/// A probe can fail once and succeed when the refusal path opens the decoder
	/// again. That rendition is the source. Returning `NoSource` would leave
	/// `run` waiting on a catalog a static source never updates, with the stale
	/// refusal still cached.
	#[tokio::test]
	async fn a_reopen_that_succeeds_is_the_source() {
		let mut video = Video::default();
		video.insert("avc", source(640, 360, None)).unwrap();

		// Cached as refused without opening, the shape of a probe that failed once.
		let mut decoders = Decoders::assume(&[]);
		decoders.config.kind = moq_video::decode::Kind::Software;
		let (name, _) = choose_source(&video, &mut decoders).await.unwrap();
		assert_eq!(name, "avc");
		assert!(decoders.probe(Codec::H264, &source(640, 360, None)).await);
	}

	/// A decodable rendition still waiting on its first keyframe will be usable,
	/// so an undecodable one that already knows its picture is no reason to refuse.
	/// A simulcast publisher fills in each rendition's geometry separately.
	#[tokio::test]
	async fn a_pending_decodable_rendition_defers_the_refusal() {
		let mut pending = source(0, 0, None);
		pending.coded_width = None;
		pending.coded_height = None;

		let mut video = Video::default();
		video.insert("hevc", hevc(1920, 1080)).unwrap();
		video.insert("avc", pending).unwrap();

		let mut decoders = Decoders::assume(&[Codec::H264]);
		assert!(matches!(
			choose_source(&video, &mut decoders).await,
			Err(Error::NoSource)
		));

		video.renditions.insert("avc".to_string(), source(640, 360, None));
		let (name, _) = choose_source(&video, &mut decoders).await.unwrap();
		assert_eq!(name, "avc");
	}

	/// The current source stays while it is still decodable, even when a taller
	/// rendition appears, and is replaced once its codec changes to one this host
	/// can't decode.
	#[tokio::test]
	async fn follow_keeps_a_valid_source() {
		let mut video = Video::default();
		video.insert("avc", source(640, 360, None)).unwrap();
		let mut decoders = Decoders::assume(&[Codec::H264]);

		video.insert("tall", source(1920, 1080, None)).unwrap();
		video.insert("hevc", hevc(3840, 2160)).unwrap();
		let (name, _) = follow_source(&video, "avc", &mut decoders).await.unwrap();
		assert_eq!(name, "avc");

		video.renditions.insert("avc".to_string(), hevc(640, 360));
		let (name, _) = follow_source(&video, "avc", &mut decoders).await.unwrap();
		assert_eq!(name, "tall");
	}

	/// Probing opens a real decoder, so each codec is opened once per transcoder
	/// rather than once per catalog edit.
	#[tokio::test]
	async fn each_codec_is_probed_once() {
		let mut config = moq_video::decode::Config::new();
		config.kind = moq_video::decode::Kind::Named("missing".to_string());
		let mut decoders = Decoders::new(config);

		// Every catalog edit reshapes the rendition; the H.265 verdict stands.
		for height in [360, 720, 1080] {
			assert!(!decoders.probe(Codec::H265, &hevc(height * 16 / 9, height)).await);
		}
		assert_eq!(decoders.probed, [(Codec::H265, false)]);
	}

	#[test]
	fn populate_preserves_the_child_archive() {
		let mut child = moq_mux::catalog::hang::Catalog::<()>::default();
		child
			.video
			.insert("video", source(1920, 1080, Some(6_000_000)))
			.unwrap();
		let mut archive = hang::catalog::Archive::new();
		archive
			.timelines
			.insert("video".to_string(), "video.timeline.z".to_string());
		archive.replay = Some(RelativeOwned::from("./recordings/clip".to_string()));
		archive.version = Some(hang::catalog::Archive::VERSION);
		child.archive = Some(archive.clone());
		let clock = hang::catalog::Clock::new(moq_net::Timestamp::from_micros(1_751_846_400_000_000).unwrap()).unwrap();
		child.clock = Some(clock);

		let mut out = moq_mux::catalog::hang::Catalog::<()>::default();
		populate(&mut out, &child, &[], None).unwrap();
		assert_eq!(out.archive, Some(archive), "a derivative keeps the child's archive");
		assert_eq!(out.clock, Some(clock), "a derivative keeps the child's clock");
	}
}
