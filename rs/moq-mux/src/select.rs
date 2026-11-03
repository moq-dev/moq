//! Track selection.
//!
//! [`Broadcast`] picks which renditions of a broadcast to keep. It is purely
//! additive: a default [`Broadcast`] selects *nothing*, and you opt a role in with
//! [`video`](Broadcast::video) / [`audio`](Broadcast::audio). Within an opted-in
//! role, an empty field matches everything; listing values keeps renditions matching
//! any one of them (a union within a field, intersected across fields). A rendition whose
//! container `kind` this build does not recognize is never selected.
//!
//! A disabled rendition (catalog `enabled: false`) is not selected, with one exception: one
//! already [`Selected`] is held, still selected, while it stays in the catalog and no other
//! rendition of its role that this selection matches is enabled. So a muted speaker keeps its
//! listeners subscribed and its unmute plays at once, while a simulcast ladder, whose lowest rung
//! never disables, moves to an enabled rung. Selection runs on every snapshot, so a rendition
//! that re-enables is selected again.
//!
//! The same [`Broadcast`] drives selection at either end of the pipeline: narrowing
//! a published catalog on the consume side (see [`catalog::Select`](crate::catalog::Select)),
//! or choosing which tracks to publish on the import side.

use std::collections::{BTreeMap, BTreeSet};

use hang::catalog::{AudioCodecKind, AudioConfig, VideoCodecKind, VideoConfig};

use crate::catalog::hang::{Catalog, CatalogExt, supported};

/// Which renditions of a broadcast to keep.
///
/// Defaults to selecting nothing. Opt a role in with [`video`](Self::video) /
/// [`audio`](Self::audio); an unselected role is dropped entirely.
#[derive(Clone, Debug, Default)]
pub struct Broadcast {
	video: Option<Video>,
	audio: Option<Audio>,
}

impl Broadcast {
	/// Select video, narrowed by `video`. A default [`Video`] keeps every video rendition.
	pub fn video(mut self, video: Video) -> Self {
		self.video = Some(video);
		self
	}

	/// Select audio, narrowed by `audio`. A default [`Audio`] keeps every audio rendition.
	pub fn audio(mut self, audio: Audio) -> Self {
		self.audio = Some(audio);
		self
	}

	/// Whether any video is selected.
	pub fn has_video(&self) -> bool {
		self.video.is_some()
	}

	/// Whether any audio is selected.
	pub fn has_audio(&self) -> bool {
		self.audio.is_some()
	}

	/// Drop every rendition from `catalog` that isn't selected, holding a disabled one in
	/// `selected` (see the [module docs](self)).
	///
	/// Pass [`Selected::default`] to select from scratch, or the renditions already in use,
	/// such as [`Selected::of`] the previous snapshot this returned.
	pub fn retain<E: CatalogExt>(&self, catalog: &mut Catalog<E>, selected: &Selected) {
		match &self.video {
			Some(video) => hold(
				&mut catalog.video.renditions,
				|name, config| video.matches(name, config),
				|config| config.enabled,
				&selected.video,
			),
			None => catalog.video.renditions.clear(),
		}
		match &self.audio {
			Some(audio) => hold(
				&mut catalog.audio.renditions,
				|name, config| audio.matches(name, config),
				|config| config.enabled,
				&selected.audio,
			),
			None => catalog.audio.renditions.clear(),
		}
	}
}

/// The renditions a consumer already selected, by role and catalog name, which
/// [`Broadcast::retain`] holds while their publisher has them disabled.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selected {
	/// Selected video renditions.
	pub video: BTreeSet<String>,
	/// Selected audio renditions.
	pub audio: BTreeSet<String>,
}

impl Selected {
	/// Every rendition `catalog` lists, such as the snapshot [`Broadcast::retain`] last returned.
	pub fn of<E: CatalogExt>(catalog: &Catalog<E>) -> Self {
		Self {
			video: catalog.video.renditions.keys().cloned().collect(),
			audio: catalog.audio.renditions.keys().cloned().collect(),
		}
	}
}

/// Keep the renditions of one role that match, dropping the disabled ones unless none that
/// match is enabled, in which case those in `selected` stay.
fn hold<C>(
	renditions: &mut BTreeMap<String, C>,
	matches: impl Fn(&str, &C) -> bool,
	enabled: impl Fn(&C) -> bool,
	selected: &BTreeSet<String>,
) {
	renditions.retain(|name, config| matches(name, config));
	let alternative = renditions.values().any(&enabled);
	renditions.retain(|name, config| enabled(config) || (!alternative && selected.contains(name)));
}

/// Video rendition criteria. An empty field matches every rendition.
#[derive(Clone, Debug, Default)]
pub struct Video {
	name: Vec<String>,
	codec: Vec<VideoCodecKind>,
}

impl Video {
	/// Also accept the rendition with this exact name. Repeatable; empty = any name.
	pub fn name(mut self, name: impl Into<String>) -> Self {
		self.name.push(name.into());
		self
	}

	/// Also accept renditions of this codec. Repeatable; empty = any codec.
	pub fn codec(mut self, codec: VideoCodecKind) -> Self {
		self.codec.push(codec);
		self
	}

	fn matches(&self, name: &str, config: &VideoConfig) -> bool {
		supported(name, &config.container)
			&& (self.name.is_empty() || self.name.iter().any(|n| n == name))
			&& (self.codec.is_empty() || self.codec.contains(&config.codec.kind()))
	}
}

/// Audio rendition criteria. An empty field matches every rendition.
#[derive(Clone, Debug, Default)]
pub struct Audio {
	name: Vec<String>,
	codec: Vec<AudioCodecKind>,
}

impl Audio {
	/// Also accept the rendition with this exact name. Repeatable; empty = any name.
	pub fn name(mut self, name: impl Into<String>) -> Self {
		self.name.push(name.into());
		self
	}

	/// Also accept renditions of this codec. Repeatable; empty = any codec.
	pub fn codec(mut self, codec: AudioCodecKind) -> Self {
		self.codec.push(codec);
		self
	}

	fn matches(&self, name: &str, config: &AudioConfig) -> bool {
		supported(name, &config.container)
			&& (self.name.is_empty() || self.name.iter().any(|n| n == name))
			&& (self.codec.is_empty() || self.codec.contains(&config.codec.kind()))
	}
}

#[cfg(test)]
mod tests {
	use std::collections::BTreeMap;

	use hang::catalog::{AudioCodec, AudioConfig, Container, H264, VP9, VideoConfig};

	use super::*;

	fn h264(name: &str) -> (String, VideoConfig) {
		let mut config = VideoConfig::new(H264 {
			profile: 0x42,
			constraints: 0,
			level: 0x1e,
			inline: false,
		});
		config.container = Container::Legacy;
		(name.to_string(), config)
	}

	fn vp9(name: &str) -> (String, VideoConfig) {
		let mut config = VideoConfig::new(VP9 {
			profile: 0,
			level: 10,
			bit_depth: 8,
			chroma_subsampling: 1,
			color_primaries: 1,
			transfer_characteristics: 1,
			matrix_coefficients: 1,
			full_range: false,
		});
		config.container = Container::Legacy;
		(name.to_string(), config)
	}

	fn opus(name: &str) -> (String, AudioConfig) {
		let mut config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		config.container = Container::Legacy;
		(name.to_string(), config)
	}

	fn catalog(video: Vec<(String, VideoConfig)>, audio: Vec<(String, AudioConfig)>) -> Catalog {
		let mut catalog = Catalog::default();
		catalog.video.renditions = BTreeMap::from_iter(video);
		catalog.audio.renditions = BTreeMap::from_iter(audio);
		catalog
	}

	fn video_names(catalog: &Catalog) -> Vec<&str> {
		catalog.video.renditions.keys().map(String::as_str).collect()
	}

	fn audio_names(catalog: &Catalog) -> Vec<&str> {
		catalog.audio.renditions.keys().map(String::as_str).collect()
	}

	#[test]
	fn default_selects_nothing() {
		let mut catalog = catalog(vec![h264("v")], vec![opus("a")]);
		Broadcast::default().retain(&mut catalog, &Selected::default());
		assert!(catalog.video.renditions.is_empty());
		assert!(catalog.audio.renditions.is_empty());
	}

	#[test]
	fn empty_axis_keeps_everything() {
		let mut catalog = catalog(vec![h264("lo"), h264("hi")], vec![opus("a")]);
		Broadcast::default()
			.video(Video::default())
			.retain(&mut catalog, &Selected::default());
		assert_eq!(video_names(&catalog), vec!["hi", "lo"]);
		// Audio was never selected, so it's dropped.
		assert!(catalog.audio.renditions.is_empty());
	}

	#[test]
	fn name_matches_exact() {
		let mut catalog = catalog(vec![h264("lo"), h264("hi")], vec![]);
		Broadcast::default()
			.video(Video::default().name("hi"))
			.retain(&mut catalog, &Selected::default());
		assert_eq!(video_names(&catalog), vec!["hi"]);
	}

	#[test]
	fn names_union_within_field() {
		let mut catalog = catalog(vec![], vec![opus("en"), opus("es"), opus("fr")]);
		Broadcast::default()
			.audio(Audio::default().name("en").name("es"))
			.retain(&mut catalog, &Selected::default());
		assert_eq!(audio_names(&catalog), vec!["en", "es"]);
	}

	#[test]
	fn codec_filters_within_selected_axis() {
		let mut catalog = catalog(vec![h264("a"), vp9("b"), h264("c")], vec![]);
		Broadcast::default()
			.video(Video::default().codec(VideoCodecKind::H264))
			.retain(&mut catalog, &Selected::default());
		assert_eq!(video_names(&catalog), vec!["a", "c"]);
	}

	#[test]
	fn codecs_union_within_field() {
		let mut catalog = catalog(vec![h264("a"), vp9("b")], vec![]);
		Broadcast::default()
			.video(Video::default().codec(VideoCodecKind::H264).codec(VideoCodecKind::VP9))
			.retain(&mut catalog, &Selected::default());
		assert_eq!(video_names(&catalog), vec!["a", "b"]);
	}

	#[test]
	fn unknown_container_never_selected() {
		let (name, mut future) = h264("future");
		future.container = serde_json::from_str(r#"{"kind":"future"}"#).unwrap();

		let mut catalog = catalog(vec![h264("known"), (name, future)], vec![]);
		Broadcast::default()
			.video(Video::default())
			.retain(&mut catalog, &Selected::default());
		assert_eq!(video_names(&catalog), vec!["known"]);
	}

	fn off_video((name, mut config): (String, VideoConfig)) -> (String, VideoConfig) {
		config.enabled = false;
		(name, config)
	}

	fn off_audio((name, mut config): (String, AudioConfig)) -> (String, AudioConfig) {
		config.enabled = false;
		(name, config)
	}

	fn selected(video: &[&str], audio: &[&str]) -> Selected {
		Selected {
			video: video.iter().map(|name| name.to_string()).collect(),
			audio: audio.iter().map(|name| name.to_string()).collect(),
		}
	}

	fn everything() -> Broadcast {
		Broadcast::default().video(Video::default()).audio(Audio::default())
	}

	/// Selecting from scratch skips a disabled rendition, so a listener who joins during a
	/// pause subscribes on the unmute.
	#[test]
	fn disabled_not_selected_initially() {
		let mut catalog = catalog(vec![h264("lo"), off_video(h264("hi"))], vec![off_audio(opus("a"))]);
		everything().retain(&mut catalog, &Selected::default());
		assert_eq!(video_names(&catalog), vec!["lo"]);
		assert!(catalog.audio.renditions.is_empty());
	}

	/// A selected rendition stays selected while disabled, if nothing else of its role is enabled.
	#[test]
	fn disabled_selection_is_held() {
		let mut catalog = catalog(vec![off_video(h264("v"))], vec![off_audio(opus("a"))]);
		everything().retain(&mut catalog, &selected(&["v"], &["a"]));
		assert_eq!(video_names(&catalog), vec!["v"]);
		assert_eq!(audio_names(&catalog), vec!["a"]);
	}

	/// Only what was selected is held: a rendition that was disabled all along is not picked up.
	#[test]
	fn only_the_selection_is_held() {
		let mut catalog = catalog(vec![], vec![off_audio(opus("a")), off_audio(opus("b"))]);
		everything().retain(&mut catalog, &selected(&[], &["a"]));
		assert_eq!(audio_names(&catalog), vec!["a"]);
	}

	/// An enabled alternative wins, as a simulcast ladder's lowest rung does.
	#[test]
	fn enabled_alternative_wins() {
		let mut catalog = catalog(vec![h264("lo"), off_video(h264("hi"))], vec![]);
		everything().retain(&mut catalog, &selected(&["hi"], &[]));
		assert_eq!(video_names(&catalog), vec!["lo"]);
	}

	/// A rendition the selection doesn't match is no alternative.
	#[test]
	fn unmatched_rendition_is_no_alternative() {
		let mut catalog = catalog(vec![h264("lo"), off_video(h264("hi"))], vec![]);
		Broadcast::default()
			.video(Video::default().name("hi"))
			.retain(&mut catalog, &selected(&["hi"], &[]));
		assert_eq!(video_names(&catalog), vec!["hi"]);
	}

	/// A held rendition removed from the catalog is dropped.
	#[test]
	fn removed_selection_is_dropped() {
		let mut catalog = catalog(vec![], vec![off_audio(opus("b"))]);
		everything().retain(&mut catalog, &selected(&[], &["a"]));
		assert!(catalog.audio.renditions.is_empty());
	}

	/// Each role holds on its own: enabled video does not release held audio.
	#[test]
	fn roles_hold_independently() {
		let mut catalog = catalog(vec![h264("v")], vec![off_audio(opus("a"))]);
		everything().retain(&mut catalog, &selected(&["v"], &["a"]));
		assert_eq!(video_names(&catalog), vec!["v"]);
		assert_eq!(audio_names(&catalog), vec!["a"]);
	}

	#[test]
	fn name_and_codec_intersect() {
		let mut catalog = catalog(vec![h264("hi"), vp9("hi2"), h264("lo")], vec![]);
		// name in {hi, hi2} AND codec H264 -> only "hi".
		Broadcast::default()
			.video(Video::default().name("hi").name("hi2").codec(VideoCodecKind::H264))
			.retain(&mut catalog, &Selected::default());
		assert_eq!(video_names(&catalog), vec!["hi"]);
	}
}
