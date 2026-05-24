//! Hard-match rendition filter.
//!
//! [`Filter`] wraps any [`Stream`] and drops renditions that don't satisfy a
//! [`FilterVideo`] / [`FilterAudio`]. Matching is exact: a `name` constraint
//! keeps only the rendition with that key, a `codec` constraint keeps only
//! renditions whose codec family matches. Multiple constraints intersect.

use std::task::Poll;

use hang::Catalog;
use hang::catalog::{AudioCodecKind, VideoCodecKind};

use super::Stream;

/// Hard-match criteria for video renditions.
#[derive(Debug, Default, Clone)]
pub struct FilterVideo {
	/// Keep only the rendition with this exact name.
	pub name: Option<String>,
	/// Keep only renditions whose codec family matches.
	pub codec: Option<VideoCodecKind>,
}

/// Hard-match criteria for audio renditions.
#[derive(Debug, Default, Clone)]
pub struct FilterAudio {
	/// Keep only the rendition with this exact name.
	pub name: Option<String>,
	/// Keep only renditions whose codec family matches.
	pub codec: Option<AudioCodecKind>,
}

/// A [`Stream`] that drops renditions failing a [`FilterVideo`] / [`FilterAudio`].
pub struct Filter<S: Stream> {
	inner: S,
	video: Option<FilterVideo>,
	audio: Option<FilterAudio>,
	/// Last raw snapshot from `inner`, kept so retargeting via `set_*` can re-emit
	/// without polling upstream (the foothold for future ABR retargeting).
	last_input: Option<Catalog>,
	/// True if `set_*` has been called since the last emit and we still owe a
	/// re-evaluated snapshot derived from `last_input`.
	dirty: bool,
}

impl<S: Stream> Filter<S> {
	pub fn new(inner: S) -> Self {
		Self {
			inner,
			video: None,
			audio: None,
			last_input: None,
			dirty: false,
		}
	}

	/// Set or clear the video filter. Pass `None` to clear.
	pub fn set_video(&mut self, filter: impl Into<Option<FilterVideo>>) {
		self.video = filter.into();
		self.dirty = self.last_input.is_some();
	}

	/// Set or clear the audio filter. Pass `None` to clear.
	pub fn set_audio(&mut self, filter: impl Into<Option<FilterAudio>>) {
		self.audio = filter.into();
		self.dirty = self.last_input.is_some();
	}

	fn apply(&self, mut catalog: Catalog) -> Catalog {
		if let Some(filter) = &self.video {
			catalog.video.renditions.retain(|name, config| {
				if let Some(want) = &filter.name
					&& want != name
				{
					return false;
				}
				if let Some(want) = filter.codec
					&& config.codec.kind() != want
				{
					return false;
				}
				true
			});
		}
		if let Some(filter) = &self.audio {
			catalog.audio.renditions.retain(|name, config| {
				if let Some(want) = &filter.name
					&& want != name
				{
					return false;
				}
				if let Some(want) = filter.codec
					&& config.codec.kind() != want
				{
					return false;
				}
				true
			});
		}
		catalog
	}
}

impl<S: Stream> Stream for Filter<S> {
	fn poll_next(&mut self, waiter: &conducer::Waiter) -> Poll<anyhow::Result<Option<Catalog>>> {
		if self.dirty {
			self.dirty = false;
			if let Some(snapshot) = self.last_input.clone() {
				return Poll::Ready(Ok(Some(self.apply(snapshot))));
			}
		}

		match self.inner.poll_next(waiter)? {
			Poll::Ready(Some(snapshot)) => {
				self.last_input = Some(snapshot.clone());
				Poll::Ready(Ok(Some(self.apply(snapshot))))
			}
			Poll::Ready(None) => Poll::Ready(Ok(None)),
			Poll::Pending => Poll::Pending,
		}
	}
}

#[cfg(test)]
mod test {
	use std::collections::BTreeMap;

	use hang::catalog::{AudioCodec, AudioConfig, Container, H264, VideoConfig};

	use super::*;

	struct Once(Option<Catalog>);

	impl Stream for Once {
		fn poll_next(&mut self, _: &conducer::Waiter) -> Poll<anyhow::Result<Option<Catalog>>> {
			Poll::Ready(Ok(self.0.take()))
		}
	}

	fn h264(name: &str) -> (String, VideoConfig) {
		let mut config = VideoConfig::new(H264 {
			profile: 0x42,
			constraints: 0,
			level: 0x1e,
			inline: false,
		});
		config.coded_width = Some(640);
		config.coded_height = Some(360);
		config.bitrate = Some(500_000);
		config.framerate = Some(30.0);
		config.container = Container::Legacy;
		(name.to_string(), config)
	}

	fn opus(name: &str) -> (String, AudioConfig) {
		let mut config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		config.bitrate = Some(128_000);
		config.container = Container::Legacy;
		(name.to_string(), config)
	}

	fn catalog_with(video: Vec<(String, VideoConfig)>, audio: Vec<(String, AudioConfig)>) -> Catalog {
		let mut c = Catalog::default();
		c.video.renditions = BTreeMap::from_iter(video);
		c.audio.renditions = BTreeMap::from_iter(audio);
		c
	}

	#[test]
	fn codec_filter_keeps_matching() {
		let mut hd = h264("hd");
		hd.1.codec = hang::catalog::VP9 {
			profile: 0,
			level: 10,
			bit_depth: 8,
			chroma_subsampling: 1,
			color_primaries: 1,
			transfer_characteristics: 1,
			matrix_coefficients: 1,
			full_range: false,
		}
		.into();
		let snapshot = catalog_with(vec![h264("lo"), hd], vec![]);

		let mut f = Filter::new(Once(Some(snapshot)));
		f.set_video(FilterVideo {
			codec: Some(VideoCodecKind::H264),
			..Default::default()
		});

		let out = match f.poll_next(&conducer::Waiter::noop()) {
			Poll::Ready(Ok(Some(c))) => c,
			other => panic!("expected snapshot, got {other:?}"),
		};
		assert_eq!(out.video.renditions.keys().collect::<Vec<_>>(), vec!["lo"]);
	}

	#[test]
	fn name_filter_exact() {
		let snapshot = catalog_with(vec![h264("lo"), h264("hi")], vec![]);
		let mut f = Filter::new(Once(Some(snapshot)));
		f.set_video(FilterVideo {
			name: Some("hi".into()),
			..Default::default()
		});
		let out = match f.poll_next(&conducer::Waiter::noop()) {
			Poll::Ready(Ok(Some(c))) => c,
			other => panic!("got {other:?}"),
		};
		assert_eq!(out.video.renditions.keys().collect::<Vec<_>>(), vec!["hi"]);
	}

	#[test]
	fn audio_filter_independent_of_video() {
		let snapshot = catalog_with(vec![h264("hi")], vec![opus("en"), opus("es")]);
		let mut f = Filter::new(Once(Some(snapshot)));
		f.set_audio(FilterAudio {
			name: Some("es".into()),
			..Default::default()
		});
		let out = match f.poll_next(&conducer::Waiter::noop()) {
			Poll::Ready(Ok(Some(c))) => c,
			other => panic!("got {other:?}"),
		};
		assert_eq!(out.video.renditions.keys().collect::<Vec<_>>(), vec!["hi"]);
		assert_eq!(out.audio.renditions.keys().collect::<Vec<_>>(), vec!["es"]);
	}

	#[test]
	fn set_video_after_snapshot_reemits() {
		let snapshot = catalog_with(vec![h264("lo"), h264("hi")], vec![]);
		let mut f = Filter::new(Once(Some(snapshot)));

		let first = match f.poll_next(&conducer::Waiter::noop()) {
			Poll::Ready(Ok(Some(c))) => c,
			other => panic!("got {other:?}"),
		};
		assert_eq!(first.video.renditions.len(), 2);

		f.set_video(FilterVideo {
			name: Some("hi".into()),
			..Default::default()
		});

		let again = match f.poll_next(&conducer::Waiter::noop()) {
			Poll::Ready(Ok(Some(c))) => c,
			other => panic!("expected re-emit, got {other:?}"),
		};
		assert_eq!(again.video.renditions.keys().collect::<Vec<_>>(), vec!["hi"]);
	}
}
