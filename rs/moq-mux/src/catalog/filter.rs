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

/// Shared state behind a [`Filter`].
///
/// `epoch` advances on every setter so [`Filter::poll_next`] can tell whether
/// the criteria changed since the last emit.
#[derive(Debug, Default, Clone)]
struct FilterState {
	video: Option<FilterVideo>,
	audio: Option<FilterAudio>,
	epoch: u64,
}

/// A [`Stream`] that drops renditions failing a [`FilterVideo`] / [`FilterAudio`].
///
/// Selection criteria live behind a [`conducer::Producer`], so calls to
/// [`set_video`](Self::set_video) / [`set_audio`](Self::set_audio) wake any
/// pending `poll_next` instead of silently waiting for the next upstream
/// snapshot.
pub struct Filter<S: Stream> {
	inner: S,
	state: conducer::Producer<FilterState>,
	state_consumer: conducer::Consumer<FilterState>,
	/// Last raw snapshot from `inner`, retained so a setter between snapshots
	/// can re-apply without polling upstream.
	last_input: Option<Catalog>,
	/// Epoch we already emitted against.
	last_epoch: u64,
	/// True once `inner` has handed us a snapshot we haven't emitted yet.
	fresh_input: bool,
}

impl<S: Stream> Filter<S> {
	pub fn new(inner: S) -> Self {
		let state = conducer::Producer::new(FilterState::default());
		let state_consumer = state.consume();
		Self {
			inner,
			state,
			state_consumer,
			last_input: None,
			last_epoch: 0,
			fresh_input: false,
		}
	}

	/// Set or clear the video filter. Pass `None` to clear.
	pub fn set_video(&mut self, filter: impl Into<Option<FilterVideo>>) {
		self.update(|s| s.video = filter.into());
	}

	/// Set or clear the audio filter. Pass `None` to clear.
	pub fn set_audio(&mut self, filter: impl Into<Option<FilterAudio>>) {
		self.update(|s| s.audio = filter.into());
	}

	fn update(&self, f: impl FnOnce(&mut FilterState)) {
		// `write()` only errors when the producer is closed, which can't happen
		// while `self` holds the only producer handle.
		let Ok(mut state) = self.state.write() else {
			return;
		};
		f(&mut state);
		state.epoch = state.epoch.wrapping_add(1);
		// Mut::drop wakes the paired consumer waiters here.
	}
}

impl<S: Stream> Stream for Filter<S> {
	fn poll_next(&mut self, waiter: &conducer::Waiter) -> Poll<crate::Result<Option<Catalog>>> {
		let inner_eof = loop {
			match self.inner.poll_next(waiter)? {
				Poll::Ready(Some(snapshot)) => {
					self.last_input = Some(snapshot);
					self.fresh_input = true;
				}
				Poll::Ready(None) => break true,
				Poll::Pending => break false,
			}
		};

		let last_epoch = self.last_epoch;
		let fresh_input = self.fresh_input;
		let last_input = self.last_input.clone();

		let polled = self.state_consumer.poll(waiter, |state| {
			let filter_changed = state.epoch != last_epoch;
			if !fresh_input && !filter_changed {
				return Poll::Pending;
			}
			let Some(input) = last_input.clone() else {
				return Poll::Pending;
			};
			let emit = apply(input, state.video.as_ref(), state.audio.as_ref());
			Poll::Ready((emit, state.epoch))
		});

		match polled {
			Poll::Ready(Ok((emit, epoch))) => {
				self.last_epoch = epoch;
				self.fresh_input = false;
				Poll::Ready(Ok(Some(emit)))
			}
			Poll::Ready(Err(_)) => Poll::Ready(Ok(None)),
			Poll::Pending => {
				if inner_eof && self.last_input.is_none() {
					Poll::Ready(Ok(None))
				} else {
					Poll::Pending
				}
			}
		}
	}
}

/// Apply the active video / audio filters to a raw snapshot, dropping
/// renditions that don't match. Axes with no filter pass through unchanged.
fn apply(mut catalog: Catalog, video: Option<&FilterVideo>, audio: Option<&FilterAudio>) -> Catalog {
	if let Some(filter) = video {
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
	if let Some(filter) = audio {
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

#[cfg(test)]
mod test {
	use std::collections::BTreeMap;

	use hang::catalog::{AudioCodec, AudioConfig, Container, H264, VideoConfig};

	use super::*;

	struct Once(Option<Catalog>);

	impl Stream for Once {
		fn poll_next(&mut self, _: &conducer::Waiter) -> Poll<crate::Result<Option<Catalog>>> {
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
