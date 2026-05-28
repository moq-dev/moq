//! Soft-match rendition target.
//!
//! [`Target`] wraps any [`Stream`] and reduces each axis (video / audio) to at
//! most one rendition by ranking the input against constraints like maximum
//! width, height, pixels, or bitrate. The ranking algorithm is a Rust port of
//! [js/watch's `#select`](js/watch/src/video/source.ts).

use std::collections::BTreeMap;
use std::task::Poll;

use hang::Catalog;
use hang::catalog::{AudioConfig, VideoConfig};

use super::Stream;

/// Soft-match constraints for the video rendition.
///
/// Each `Option` is a *maximum* the selection will try to stay under. When a
/// rendition fits every active maximum, the largest such rendition wins; if
/// nothing fits, the algorithm degrades to the smallest over-budget rendition
/// (per constraint) and intersects across constraints.
#[derive(Debug, Default, Clone)]
pub struct TargetVideo {
	pub width: Option<u32>,
	pub height: Option<u32>,
	pub pixels: Option<u32>,
	pub bitrate: Option<u64>,
}

/// Soft-match constraints for the audio rendition.
#[derive(Debug, Default, Clone)]
pub struct TargetAudio {
	pub bitrate: Option<u64>,
}

/// Shared state behind a [`Target`].
///
/// `epoch` advances on every setter so [`Target::poll_next`] can tell whether
/// the criteria changed since the last emit without diffing the structs.
#[derive(Debug, Default, Clone)]
struct TargetState {
	video: Option<TargetVideo>,
	audio: Option<TargetAudio>,
	epoch: u64,
}

/// A [`Stream`] that picks one rendition per axis from the inner snapshot.
///
/// Selection criteria live behind a [`conducer::Producer`], so calls to
/// [`set_video`](Self::set_video) / [`set_audio`](Self::set_audio) wake any
/// pending `poll_next` instead of silently waiting for the next upstream
/// snapshot. That makes the type usable as the foothold for bandwidth-driven
/// ABR retargeting.
pub struct Target<S: Stream> {
	inner: S,
	state: conducer::Producer<TargetState>,
	state_consumer: conducer::Consumer<TargetState>,
	/// Last raw snapshot from `inner`, retained so a target change between
	/// snapshots can be re-applied without polling upstream.
	last_input: Option<Catalog>,
	/// Epoch we already emitted against. If `state.epoch` advances past this
	/// while `last_input` is `Some`, the next poll re-emits.
	last_epoch: u64,
	/// True once `inner` has handed us a snapshot we haven't emitted yet.
	fresh_input: bool,
}

impl<S: Stream> Target<S> {
	pub fn new(inner: S) -> Self {
		let state = conducer::Producer::new(TargetState::default());
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

	/// Set or clear the video target. Pass `None` to keep every rendition.
	pub fn set_video(&mut self, target: impl Into<Option<TargetVideo>>) {
		self.update(|s| s.video = target.into());
	}

	/// Set or clear the audio target. Pass `None` to keep every rendition.
	pub fn set_audio(&mut self, target: impl Into<Option<TargetAudio>>) {
		self.update(|s| s.audio = target.into());
	}

	fn update(&self, f: impl FnOnce(&mut TargetState)) {
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

impl<S: Stream> Stream for Target<S> {
	fn poll_next(&mut self, waiter: &conducer::Waiter) -> Poll<crate::Result<Option<Catalog>>> {
		// Drain inner: the latest snapshot wins. `poll_next` registers the
		// waiter on its own Pending branch.
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

		// Snapshot the fields the inner closure needs so it can borrow them
		// without colliding with the `&self.state_consumer` receiver.
		let last_epoch = self.last_epoch;
		let fresh_input = self.fresh_input;
		let last_input = self.last_input.clone();

		let polled = self.state_consumer.poll(waiter, |state| {
			let target_changed = state.epoch != last_epoch;
			if !fresh_input && !target_changed {
				// Nothing new from inner and nothing new from caller: register
				// the waiter on this consumer so the next setter wakes us.
				return Poll::Pending;
			}
			let Some(input) = last_input.clone() else {
				// Caller already retargeted, but no upstream snapshot yet to apply.
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
			Poll::Ready(Err(_)) => {
				// Producer dropped (impossible while Self holds it); treat as EOF.
				Poll::Ready(Ok(None))
			}
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

/// Apply the active video / audio targets to a raw snapshot, narrowing each
/// axis to at most one rendition. Axes with no target pass through unchanged.
fn apply(mut catalog: Catalog, video: Option<&TargetVideo>, audio: Option<&TargetAudio>) -> Catalog {
	if let Some(target) = video {
		if let Some(name) = select_video(&catalog.video.renditions, target) {
			let mut kept = BTreeMap::new();
			if let Some(config) = catalog.video.renditions.remove(&name) {
				kept.insert(name, config);
			}
			catalog.video.renditions = kept;
		} else {
			catalog.video.renditions.clear();
		}
	}

	if let Some(target) = audio {
		if let Some(name) = select_audio(&catalog.audio.renditions, target) {
			let mut kept = BTreeMap::new();
			if let Some(config) = catalog.audio.renditions.remove(&name) {
				kept.insert(name, config);
			}
			catalog.audio.renditions = kept;
		} else {
			catalog.audio.renditions.clear();
		}
	}

	catalog
}

/// Run all active video rankings and return the highest-ranked rendition
/// present in every ranking, or `None` if the intersection is empty.
fn select_video(renditions: &BTreeMap<String, VideoConfig>, target: &TargetVideo) -> Option<String> {
	if renditions.is_empty() {
		return None;
	}
	if renditions.len() == 1 {
		return renditions.keys().next().cloned();
	}

	let mut rankings: Vec<Vec<String>> = Vec::new();
	if let Some(max) = target.pixels {
		rankings.push(by_pixels(renditions, max));
	}
	if target.width.is_some() || target.height.is_some() {
		rankings.push(by_dimensions(renditions, target.width, target.height));
	}
	if let Some(max) = target.bitrate {
		rankings.push(by_video_bitrate(renditions, max));
	}

	if rankings.is_empty() {
		return Some(best_video(renditions));
	}

	intersect_rankings(rankings)
}

fn select_audio(renditions: &BTreeMap<String, AudioConfig>, target: &TargetAudio) -> Option<String> {
	if renditions.is_empty() {
		return None;
	}
	if renditions.len() == 1 {
		return renditions.keys().next().cloned();
	}

	let mut rankings: Vec<Vec<String>> = Vec::new();
	if let Some(max) = target.bitrate {
		rankings.push(by_audio_bitrate(renditions, max));
	}

	if rankings.is_empty() {
		return Some(best_audio(renditions));
	}

	intersect_rankings(rankings)
}

/// Pick the first name from `rankings[0]` that appears in every other ranking.
fn intersect_rankings(rankings: Vec<Vec<String>>) -> Option<String> {
	use std::collections::HashSet;
	let sets: Vec<HashSet<&String>> = rankings.iter().map(|r| r.iter().collect()).collect();
	for name in &rankings[0] {
		if sets.iter().all(|s| s.contains(name)) {
			return Some(name.clone());
		}
	}
	tracing::warn!("conflicting rendition targets, no rendition satisfies all criteria");
	None
}

/// Rank by area, largest-first within budget; fall back to single smallest
/// over-budget if nothing fits. Renditions without resolution metadata are
/// returned unranked when no rendition has any metadata at all (mirrors the JS).
fn by_pixels(renditions: &BTreeMap<String, VideoConfig>, max: u32) -> Vec<String> {
	let mut within: Vec<(String, u32)> = Vec::new();
	let mut rest: Vec<(String, u32)> = Vec::new();

	for (name, config) in renditions {
		if let (Some(w), Some(h)) = (config.coded_width, config.coded_height) {
			let size = w.saturating_mul(h);
			if size <= max {
				within.push((name.clone(), size));
			} else {
				rest.push((name.clone(), size));
			}
		}
	}

	within.sort_by_key(|b| std::cmp::Reverse(b.1));
	if !within.is_empty() {
		return within.into_iter().map(|(n, _)| n).collect();
	}

	rest.sort_by_key(|a| a.1);
	if let Some(smallest) = rest.into_iter().next() {
		return vec![smallest.0];
	}

	renditions.keys().cloned().collect()
}

fn by_dimensions(renditions: &BTreeMap<String, VideoConfig>, width: Option<u32>, height: Option<u32>) -> Vec<String> {
	let mut within: Vec<(String, u32)> = Vec::new();
	let mut rest: Vec<(String, u32)> = Vec::new();

	for (name, config) in renditions {
		let (Some(w), Some(h)) = (config.coded_width, config.coded_height) else {
			continue;
		};
		let size = w.saturating_mul(h);
		let fits_w = width.is_none_or(|cap| w <= cap);
		let fits_h = height.is_none_or(|cap| h <= cap);
		if fits_w && fits_h {
			within.push((name.clone(), size));
		} else {
			rest.push((name.clone(), size));
		}
	}

	within.sort_by_key(|b| std::cmp::Reverse(b.1));
	if !within.is_empty() {
		return within.into_iter().map(|(n, _)| n).collect();
	}

	rest.sort_by_key(|a| a.1);
	if let Some(smallest) = rest.into_iter().next() {
		return vec![smallest.0];
	}

	renditions.keys().cloned().collect()
}

fn by_video_bitrate(renditions: &BTreeMap<String, VideoConfig>, max: u64) -> Vec<String> {
	let mut within: Vec<(String, u64)> = Vec::new();
	let mut rest: Vec<(String, u64)> = Vec::new();
	for (name, config) in renditions {
		if let Some(b) = config.bitrate {
			if b <= max {
				within.push((name.clone(), b));
			} else {
				rest.push((name.clone(), b));
			}
		}
	}
	within.sort_by_key(|b| std::cmp::Reverse(b.1));
	if !within.is_empty() {
		return within.into_iter().map(|(n, _)| n).collect();
	}
	rest.sort_by_key(|a| a.1);
	if let Some(smallest) = rest.into_iter().next() {
		return vec![smallest.0];
	}
	renditions.keys().cloned().collect()
}

fn by_audio_bitrate(renditions: &BTreeMap<String, AudioConfig>, max: u64) -> Vec<String> {
	let mut within: Vec<(String, u64)> = Vec::new();
	let mut rest: Vec<(String, u64)> = Vec::new();
	for (name, config) in renditions {
		if let Some(b) = config.bitrate {
			if b <= max {
				within.push((name.clone(), b));
			} else {
				rest.push((name.clone(), b));
			}
		}
	}
	within.sort_by_key(|b| std::cmp::Reverse(b.1));
	if !within.is_empty() {
		return within.into_iter().map(|(n, _)| n).collect();
	}
	rest.sort_by_key(|a| a.1);
	if let Some(smallest) = rest.into_iter().next() {
		return vec![smallest.0];
	}
	renditions.keys().cloned().collect()
}

/// With no constraints, prefer the largest resolution then the highest bitrate.
fn best_video(renditions: &BTreeMap<String, VideoConfig>) -> String {
	renditions
		.iter()
		.max_by_key(|(_, c)| {
			let area = c.coded_width.unwrap_or(0).saturating_mul(c.coded_height.unwrap_or(0)) as u64;
			(area, c.bitrate.unwrap_or(0))
		})
		.map(|(n, _)| n.clone())
		.expect("renditions non-empty checked by caller")
}

fn best_audio(renditions: &BTreeMap<String, AudioConfig>) -> String {
	renditions
		.iter()
		.max_by_key(|(_, c)| c.bitrate.unwrap_or(0))
		.map(|(n, _)| n.clone())
		.expect("renditions non-empty checked by caller")
}

#[cfg(test)]
mod test {
	use hang::catalog::{Container, H264, VideoConfig};

	use super::*;

	fn vid(name: &str, w: u32, h: u32, bitrate: u64) -> (String, VideoConfig) {
		let mut config = VideoConfig::new(H264 {
			profile: 0x42,
			constraints: 0,
			level: 0x1e,
			inline: false,
		});
		config.coded_width = Some(w);
		config.coded_height = Some(h);
		config.bitrate = Some(bitrate);
		config.framerate = Some(30.0);
		config.container = Container::Legacy;
		(name.to_string(), config)
	}

	fn map(items: Vec<(String, VideoConfig)>) -> BTreeMap<String, VideoConfig> {
		BTreeMap::from_iter(items)
	}

	#[test]
	fn pick_largest_under_width_cap() {
		let renditions = map(vec![
			vid("sd", 640, 360, 500_000),
			vid("hd", 1280, 720, 2_500_000),
			vid("fhd", 1920, 1080, 6_000_000),
		]);
		let target = TargetVideo {
			width: Some(1280),
			..Default::default()
		};
		assert_eq!(select_video(&renditions, &target).as_deref(), Some("hd"));
	}

	#[test]
	fn pick_largest_under_bitrate_cap() {
		let renditions = map(vec![
			vid("sd", 640, 360, 500_000),
			vid("hd", 1280, 720, 2_500_000),
			vid("fhd", 1920, 1080, 6_000_000),
		]);
		let target = TargetVideo {
			bitrate: Some(3_000_000),
			..Default::default()
		};
		assert_eq!(select_video(&renditions, &target).as_deref(), Some("hd"));
	}

	#[test]
	fn degrade_to_smallest_over_budget() {
		let renditions = map(vec![vid("hd", 1280, 720, 2_500_000), vid("fhd", 1920, 1080, 6_000_000)]);
		let target = TargetVideo {
			bitrate: Some(100_000),
			..Default::default()
		};
		assert_eq!(select_video(&renditions, &target).as_deref(), Some("hd"));
	}

	#[test]
	fn no_constraints_picks_largest() {
		let renditions = map(vec![
			vid("sd", 640, 360, 500_000),
			vid("hd", 1280, 720, 2_500_000),
			vid("fhd", 1920, 1080, 6_000_000),
		]);
		let target = TargetVideo::default();
		assert_eq!(select_video(&renditions, &target).as_deref(), Some("fhd"));
	}

	#[test]
	fn width_and_bitrate_intersect() {
		let renditions = map(vec![
			vid("sd", 640, 360, 500_000),
			vid("hd", 1280, 720, 2_500_000),
			vid("fhd", 1920, 1080, 6_000_000),
		]);
		let target = TargetVideo {
			width: Some(1920),
			bitrate: Some(1_000_000),
			..Default::default()
		};
		// width allows all, bitrate allows only sd.
		assert_eq!(select_video(&renditions, &target).as_deref(), Some("sd"));
	}
}
