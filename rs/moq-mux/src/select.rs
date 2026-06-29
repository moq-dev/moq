//! Track selection.
//!
//! [`Broadcast`] picks which renditions of a broadcast to keep. It is purely
//! additive: a default [`Broadcast`] selects *nothing*, and you opt a role in with
//! [`video`](Broadcast::video) / [`audio`](Broadcast::audio). Within an opted-in
//! role, an empty field matches everything; listing values keeps renditions matching
//! any one of them (a union within a field, intersected across fields).
//!
//! The same [`Broadcast`] drives selection at either end of the pipeline: narrowing
//! a published catalog on the consume side (see [`catalog::Select`](crate::catalog::Select)),
//! or choosing which tracks to publish on the import side.

use hang::catalog::{AudioCodecKind, AudioConfig, VideoCodecKind, VideoConfig};

use crate::catalog::hang::{Catalog, CatalogExt};

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

	/// Drop every rendition from `catalog` that isn't selected.
	pub fn retain<E: CatalogExt>(&self, catalog: &mut Catalog<E>) {
		match &self.video {
			Some(video) => catalog
				.video
				.renditions
				.retain(|name, config| video.matches(name, config)),
			None => catalog.video.renditions.clear(),
		}
		match &self.audio {
			Some(audio) => catalog
				.audio
				.renditions
				.retain(|name, config| audio.matches(name, config)),
			None => catalog.audio.renditions.clear(),
		}
	}
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
		(self.name.is_empty() || self.name.iter().any(|n| n == name))
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
		(self.name.is_empty() || self.name.iter().any(|n| n == name))
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
		Broadcast::default().retain(&mut catalog);
		assert!(catalog.video.renditions.is_empty());
		assert!(catalog.audio.renditions.is_empty());
	}

	#[test]
	fn empty_axis_keeps_everything() {
		let mut catalog = catalog(vec![h264("lo"), h264("hi")], vec![opus("a")]);
		Broadcast::default().video(Video::default()).retain(&mut catalog);
		assert_eq!(video_names(&catalog), vec!["hi", "lo"]);
		// Audio was never selected, so it's dropped.
		assert!(catalog.audio.renditions.is_empty());
	}

	#[test]
	fn video_only_drops_audio() {
		let mut catalog = catalog(vec![h264("v")], vec![opus("a")]);
		Broadcast::default().video(Video::default()).retain(&mut catalog);
		assert_eq!(video_names(&catalog), vec!["v"]);
		assert!(catalog.audio.renditions.is_empty());
	}

	#[test]
	fn name_matches_exact() {
		let mut catalog = catalog(vec![h264("lo"), h264("hi")], vec![]);
		Broadcast::default()
			.video(Video::default().name("hi"))
			.retain(&mut catalog);
		assert_eq!(video_names(&catalog), vec!["hi"]);
	}

	#[test]
	fn names_union_within_field() {
		let mut catalog = catalog(vec![], vec![opus("en"), opus("es"), opus("fr")]);
		Broadcast::default()
			.audio(Audio::default().name("en").name("es"))
			.retain(&mut catalog);
		assert_eq!(audio_names(&catalog), vec!["en", "es"]);
	}

	#[test]
	fn codec_filters_within_selected_axis() {
		let mut catalog = catalog(vec![h264("a"), vp9("b"), h264("c")], vec![]);
		Broadcast::default()
			.video(Video::default().codec(VideoCodecKind::H264))
			.retain(&mut catalog);
		assert_eq!(video_names(&catalog), vec!["a", "c"]);
	}

	#[test]
	fn codecs_union_within_field() {
		let mut catalog = catalog(vec![h264("a"), vp9("b")], vec![]);
		Broadcast::default()
			.video(Video::default().codec(VideoCodecKind::H264).codec(VideoCodecKind::VP9))
			.retain(&mut catalog);
		assert_eq!(video_names(&catalog), vec!["a", "b"]);
	}

	#[test]
	fn name_and_codec_intersect() {
		let mut catalog = catalog(vec![h264("hi"), vp9("hi2"), h264("lo")], vec![]);
		// name in {hi, hi2} AND codec H264 -> only "hi".
		Broadcast::default()
			.video(Video::default().name("hi").name("hi2").codec(VideoCodecKind::H264))
			.retain(&mut catalog);
		assert_eq!(video_names(&catalog), vec!["hi"]);
	}
}
