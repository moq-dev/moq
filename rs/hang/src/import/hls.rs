//! HLS (HTTP Live Streaming) ingest built on top of fMP4.
//!
//! This module provides reusable logic to ingest HLS master/media playlists and
//! feed their fMP4 segments into a `hang` broadcast. It is designed to be
//! independent of any particular HTTP client; callers provide an implementation
//! of [`Fetcher`] to perform the actual network I/O.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use m3u8_rs::{
	AlternativeMedia, AlternativeMediaType, Map, MasterPlaylist, MediaPlaylist, MediaSegment, Resolution, VariantStream,
};
use reqwest::Client;
use tracing::{debug, info, warn};
use url::Url;

use crate::BroadcastProducer;
use crate::import::{Fmp4, Fmp4Config};

/// Configuration for the single-rendition HLS ingest loop.
#[derive(Clone)]
pub struct HlsConfig {
	/// The master or media playlist URL or file path to ingest.
	pub playlist: String,

	/// An optional HTTP client to use for fetching the playlist and segments.
	/// If not provided, a default client will be created.
	pub client: Option<Client>,

	/// Enable passthrough mode for CMAF fragment transport.
	/// When enabled, complete fMP4 fragments (moof+mdat) are transported directly
	/// instead of being decomposed into individual samples.
	pub passthrough: bool,
}

impl HlsConfig {
	pub fn new(playlist: String) -> Self {
		Self {
			playlist,
			client: None,
			passthrough: false,
		}
	}

	/// Parse the playlist string into a URL.
	/// If it starts with http:// or https://, parse as URL.
	/// Otherwise, treat as a file path and convert to file:// URL.
	fn parse_playlist(&self) -> anyhow::Result<Url> {
		if self.playlist.starts_with("http://") || self.playlist.starts_with("https://") {
			Url::parse(&self.playlist).context("invalid playlist URL")
		} else {
			let path = PathBuf::from(&self.playlist);
			let absolute = if path.is_absolute() {
				path
			} else {
				std::env::current_dir()?.join(path)
			};
			Url::from_file_path(&absolute).ok().context("invalid file path")
		}
	}
}

/// Result of a single ingest step.
struct StepOutcome {
	/// Number of media segments written during this step.
	pub wrote_segments: usize,
	/// Target segment duration (in seconds) from the playlist, if known.
	pub target_duration: Option<f32>,
}

/// HLS ingest that pulls an HLS media playlist and feeds the bytes into the fMP4 ingest.
///
/// Provides `init()` to prime the ingest with initial segments, and `service()`
/// to run the continuous ingest loop.
pub struct Hls {
	/// Broadcast that all CMAF importers write into.
	broadcast: BroadcastProducer,

	/// fMP4 importers for each discovered video rendition.
	/// Each importer feeds a separate MoQ track but shares the same catalog.
	video_importers: Vec<Fmp4>,

	/// fMP4 importer for the selected audio rendition, if any.
	audio_importer: Option<Fmp4>,

	client: Client,
	/// Parsed base URL for the playlist (file:// or http(s)://).
	base_url: Url,
	/// All discovered video variants (one per HLS rendition).
	video: Vec<TrackState>,
	/// Optional audio track shared across variants.
	audio: Option<TrackState>,
	passthrough: bool,
}

#[derive(Debug, Clone, Copy)]
enum TrackKind {
	Video(usize),
	Audio,
}

struct TrackState {
	playlist: Url,
	next_sequence: Option<u64>,
	init_ready: bool,
}

impl TrackState {
	fn new(playlist: Url) -> Self {
		Self {
			playlist,
			next_sequence: None,
			init_ready: false,
		}
	}
}

impl Hls {
	/// Create a new HLS ingest that will write into the given broadcast.
	pub fn new(broadcast: BroadcastProducer, cfg: HlsConfig) -> anyhow::Result<Self> {
		let base_url = cfg.parse_playlist()?;
		let client = cfg.client.unwrap_or_else(|| {
			Client::builder()
				.user_agent(concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")))
				.build()
				.unwrap()
		});
		let passthrough = cfg.passthrough;
		Ok(Self {
			broadcast,
			video_importers: Vec::new(),
			passthrough,
			audio_importer: None,
			client,
			base_url,
			video: Vec::new(),
			audio: None,
		})
	}

	/// Fetch the latest playlist, download the init segment, and prime the importer with a buffer of segments.
	///
	/// Returns the number of segments buffered during initialization.
	pub async fn init(&mut self) -> anyhow::Result<()> {
		let buffered = self.prime().await?;
		if buffered == 0 {
			warn!("HLS playlist had no new segments during init step");
		} else {
			info!(count = buffered, "buffered initial HLS segments");
		}
		Ok(())
	}

	/// Run the ingest loop until cancelled.
	pub async fn run(&mut self) -> anyhow::Result<()> {
		loop {
			let outcome = self.step().await?;
			let delay = self.refresh_delay(outcome.target_duration, outcome.wrote_segments);

			info!(
				wrote_segments = outcome.wrote_segments,
				target_duration = ?outcome.target_duration,
				delay_secs = delay.as_secs_f32(),
				"HLS ingest step complete"
			);

			tokio::time::sleep(delay).await;
		}
	}

	/// Internal: fetch the latest playlist, download the init segment, and buffer segments.
	async fn prime(&mut self) -> anyhow::Result<usize> {
		self.ensure_tracks().await?;

		let mut buffered = 0usize;
		const MAX_INIT_SEGMENTS: usize = 3; // Only process a few segments during init to avoid getting ahead of live stream

		// Prime all discovered video variants.
		//
		// Move the video track states out of `self` so we can safely mutate both
		// the ingest and the tracks without running into borrow checker issues.
		let video_tracks = std::mem::take(&mut self.video);
		for (index, mut track) in video_tracks.into_iter().enumerate() {
			let playlist = self.fetch_media_playlist(track.playlist.clone()).await?;
			let count = self
				.consume_segments(TrackKind::Video(index), &mut track, &playlist, Some(MAX_INIT_SEGMENTS))
				.await?;
			buffered += count;
			self.video.push(track);
		}

		// Prime the shared audio track, if any.
		if let Some(mut track) = self.audio.take() {
			let playlist = self.fetch_media_playlist(track.playlist.clone()).await?;
			let count = self
				.consume_segments(TrackKind::Audio, &mut track, &playlist, Some(MAX_INIT_SEGMENTS))
				.await?;
			buffered += count;
			self.audio = Some(track);
		}

		Ok(buffered)
	}

	/// Perform a single ingest step for all active tracks.
	///
	/// This fetches the current media playlists, consumes any fresh segments,
	/// and returns how many segments were written along with the target
	/// duration to guide scheduling of the next step.
	async fn step(&mut self) -> anyhow::Result<StepOutcome> {
		self.ensure_tracks().await?;

		let mut wrote = 0usize;
		let mut target_duration = None;

		// Ingest a step from all active video variants.
		let video_tracks = std::mem::take(&mut self.video);
		for (index, mut track) in video_tracks.into_iter().enumerate() {
			let playlist = self.fetch_media_playlist(track.playlist.clone()).await?;
			// Use the first video's target duration as the base.
			if target_duration.is_none() {
				target_duration = Some(playlist.target_duration);
			}
			let count = self
				.consume_segments(TrackKind::Video(index), &mut track, &playlist, None)
				.await?;
			wrote += count;
			self.video.push(track);
		}

		// Ingest from the shared audio track, if present.
		if let Some(mut track) = self.audio.take() {
			let playlist = self.fetch_media_playlist(track.playlist.clone()).await?;
			if target_duration.is_none() {
				target_duration = Some(playlist.target_duration);
			}
			let count = self
				.consume_segments(TrackKind::Audio, &mut track, &playlist, None)
				.await?;
			wrote += count;
			self.audio = Some(track);
		}

		Ok(StepOutcome {
			wrote_segments: wrote,
			target_duration,
		})
	}

	/// Compute the delay before the next ingest step should run.
	fn refresh_delay(&self, target_duration: Option<f32>, wrote_segments: usize) -> Duration {
		let base = target_duration
			.map(|dur| Duration::from_secs_f32(dur.max(0.5)))
			.unwrap_or_else(|| Duration::from_millis(500));
		if wrote_segments == 0 {
			return base / 2;
		}

		base
	}

	async fn fetch_media_playlist(&self, url: Url) -> anyhow::Result<MediaPlaylist> {
		let body = self.fetch_bytes(url).await?;

		// Nom errors take ownership of the input, so we need to stringify any error messages.
		let playlist = m3u8_rs::parse_media_playlist_res(&body)
			.map_err(|e| anyhow::anyhow!("failed to parse media playlist: {}", e))?;

		Ok(playlist)
	}

	async fn ensure_tracks(&mut self) -> anyhow::Result<()> {
		// Tracks already discovered.
		if !self.video.is_empty() {
			return Ok(());
		}

		let body = self.fetch_bytes(self.base_url.clone()).await?;
		if let Ok((_, master)) = m3u8_rs::parse_master_playlist(&body) {
			let variants = select_variants(&master);
			anyhow::ensure!(!variants.is_empty(), "no usable variants found in master playlist");

			// Create a video track state for every usable variant.
			for variant in &variants {
				let video_url = resolve_uri(&self.base_url, &variant.uri)?;
				self.video.push(TrackState::new(video_url));
			}

			// Choose an audio rendition based on the first variant with an audio group.
			if let Some(group_id) = variants.iter().find_map(|v| v.audio.as_deref()) {
				if let Some(audio_tag) = select_audio(&master, group_id) {
					if let Some(uri) = &audio_tag.uri {
						let audio_url = resolve_uri(&self.base_url, uri)?;
						self.audio = Some(TrackState::new(audio_url));
					} else {
						warn!(%group_id, "audio rendition missing URI");
					}
				} else {
					warn!(%group_id, "audio group not found in master playlist");
				}
			}

			let audio_url = self.audio.as_ref().map(|a| a.playlist.to_string());
			info!(
				video_variants = variants.len(),
				audio = audio_url.as_deref().unwrap_or("none"),
				"selected master playlist renditions"
			);

			return Ok(());
		}

		// Fallback: treat the provided URL as a single media playlist.
		self.video.push(TrackState::new(self.base_url.clone()));
		Ok(())
	}

	async fn consume_segments(
		&mut self,
		kind: TrackKind,
		track: &mut TrackState,
		playlist: &MediaPlaylist,
		limit: Option<usize>,
	) -> anyhow::Result<usize> {
		self.ensure_init_segment(kind, track, playlist).await?;

		let next_seq = track.next_sequence.unwrap_or(0);
		let playlist_seq = playlist.media_sequence;
		let total_segments = playlist.segments.len();
		let last_playlist_seq = playlist_seq + total_segments as u64;

		let skip = if next_seq > last_playlist_seq {
			warn!(
				?kind,
				next_sequence = next_seq,
				playlist_sequence = playlist_seq,
				last_playlist_sequence = last_playlist_seq,
				"imported ahead of playlist, waiting for new segments"
			);
			total_segments
		} else if next_seq < playlist_seq {
			warn!(
				?kind,
				next_sequence = next_seq,
				playlist_sequence = playlist_seq,
				"next_sequence behind playlist, resetting to start of playlist"
			);
			track.next_sequence = None;
			0
		} else {
			(next_seq - playlist_seq) as usize
		};

		let available = total_segments.saturating_sub(skip);
		let to_process = match limit {
			Some(max) => available.min(max),
			None => available,
		};

		info!(
			?kind,
			playlist_sequence = playlist_seq,
			next_sequence = next_seq,
			skip = skip,
			total_segments = total_segments,
			to_process = to_process,
			"consuming HLS segments"
		);

		if to_process > 0 {
			let base_seq = playlist_seq + skip as u64;
			for (i, segment) in playlist.segments[skip..skip + to_process].iter().enumerate() {
				self.push_segment(kind, track, segment, base_seq + i as u64).await?;
			}
			info!(?kind, consumed = to_process, "consumed HLS segments");
		} else {
			debug!(?kind, "no fresh HLS segments available");
		}

		Ok(to_process)
	}

	async fn ensure_init_segment(
		&mut self,
		kind: TrackKind,
		track: &mut TrackState,
		playlist: &MediaPlaylist,
	) -> anyhow::Result<()> {
		if track.init_ready {
			return Ok(());
		}

		let map = self.find_map(playlist).context("playlist missing EXT-X-MAP")?;

		let url = resolve_uri(&track.playlist, &map.uri)?;
		let mut bytes = self.fetch_bytes(url).await?;
		let importer = match kind {
			TrackKind::Video(index) => self.ensure_video_importer_for(index),
			TrackKind::Audio => self.ensure_audio_importer(),
		};

		importer.decode(&mut bytes).context("init segment parse error")?;

		anyhow::ensure!(bytes.is_empty(), "init segment was not fully consumed");
		anyhow::ensure!(
			importer.is_initialized(),
			"init segment did not initialize the importer"
		);

		track.init_ready = true;
		info!(?kind, "loaded HLS init segment");
		Ok(())
	}

	async fn push_segment(
		&mut self,
		kind: TrackKind,
		track: &mut TrackState,
		segment: &MediaSegment,
		sequence: u64,
	) -> anyhow::Result<()> {
		anyhow::ensure!(!segment.uri.is_empty(), "encountered segment with empty URI");

		let url = resolve_uri(&track.playlist, &segment.uri)?;
		let mut bytes = self.fetch_bytes(url).await?;

		// Ensure the importer is initialized before processing fragments
		// Use track.init_ready to avoid borrowing issues
		if !track.init_ready {
			// Try to ensure init segment is processed
			let playlist = self.fetch_media_playlist(track.playlist.clone()).await?;
			self.ensure_init_segment(kind, track, &playlist).await?;
		}

		// Get importer after ensuring init segment
		let importer = match kind {
			TrackKind::Video(index) => self.ensure_video_importer_for(index),
			TrackKind::Audio => self.ensure_audio_importer(),
		};

		// Final check after ensuring init segment
		if !importer.is_initialized() {
			return Err(anyhow::anyhow!(
				"importer not initialized for {:?} after ensure_init_segment - init segment processing failed",
				kind
			));
		}

		importer.decode(&mut bytes).context("failed to parse media segment")?;
		track.next_sequence = Some(sequence + 1);

		Ok(())
	}

	fn find_map<'a>(&self, playlist: &'a MediaPlaylist) -> Option<&'a Map> {
		playlist.segments.iter().find_map(|segment| segment.map.as_ref())
	}

	async fn fetch_bytes(&self, url: Url) -> anyhow::Result<Bytes> {
		if url.scheme() == "file" {
			let path = url.to_file_path().ok().context("invalid file URL")?;
			let bytes = tokio::fs::read(&path).await.context("failed to read file")?;
			Ok(Bytes::from(bytes))
		} else {
			let response = self.client.get(url).send().await?;
			let response = response.error_for_status()?;
			let bytes = response.bytes().await.context("failed to read response body")?;
			Ok(bytes)
		}
	}

	/// Create or retrieve the fMP4 importer for a specific video rendition.
	///
	/// Each video variant gets its own importer so that their tracks remain
	/// independent while still contributing to the same shared catalog.
	fn ensure_video_importer_for(&mut self, index: usize) -> &mut Fmp4 {
		while self.video_importers.len() <= index {
			let importer = Fmp4::new(
				self.broadcast.clone(),
				Fmp4Config {
					passthrough: self.passthrough,
				},
			);
			self.video_importers.push(importer);
		}

		self.video_importers.get_mut(index).unwrap()
	}

	/// Create or retrieve the fMP4 importer for the audio rendition.
	fn ensure_audio_importer(&mut self) -> &mut Fmp4 {
		let passthrough = self.passthrough;
		self.audio_importer
			.get_or_insert_with(|| Fmp4::new(self.broadcast.clone(), Fmp4Config { passthrough }))
	}

	#[cfg(test)]
	fn has_video_importer(&self) -> bool {
		!self.video_importers.is_empty()
	}

	#[cfg(test)]
	fn has_audio_importer(&self) -> bool {
		self.audio_importer.is_some()
	}
}

fn select_audio<'a>(master: &'a MasterPlaylist, group_id: &str) -> Option<&'a AlternativeMedia> {
	let mut first = None;
	let mut default = None;

	for alternative in master
		.alternatives
		.iter()
		.filter(|alt| alt.media_type == AlternativeMediaType::Audio && alt.group_id == group_id)
	{
		if first.is_none() {
			first = Some(alternative);
		}
		if alternative.default {
			default = Some(alternative);
			break;
		}
	}

	default.or(first)
}

fn select_variants(master: &MasterPlaylist) -> Vec<&VariantStream> {
	// Helper to extract the first video codec token from the CODECS attribute.
	fn first_video_codec(variant: &VariantStream) -> Option<&str> {
		let codecs = variant.codecs.as_deref()?;
		codecs.split(',').map(|s| s.trim()).find(|s| !s.is_empty())
	}

	// Map codec strings into a coarse "family" so we can prefer H.264 over others.
	fn codec_family(codec: &str) -> Option<&'static str> {
		if codec.starts_with("avc1.") || codec.starts_with("avc3.") {
			Some("h264")
		} else {
			None
		}
	}

	// Consider only non-i-frame variants with a URI and a known codec family.
	let candidates: Vec<(&VariantStream, &str, &str)> = master
		.variants
		.iter()
		.filter(|variant| !variant.is_i_frame && !variant.uri.is_empty())
		.filter_map(|variant| {
			let codec = first_video_codec(variant)?;
			let family = codec_family(codec)?;
			Some((variant, codec, family))
		})
		.collect();

	if candidates.is_empty() {
		return Vec::new();
	}

	// Prefer families in this order, falling back to the first available.
	const FAMILY_PREFERENCE: &[&str] = &["h264"];

	let families_present: Vec<&str> = candidates.iter().map(|(_, _, fam)| *fam).collect();

	let target_family = FAMILY_PREFERENCE
		.iter()
		.find(|fav| families_present.iter().any(|fam| fam == *fav))
		.copied()
		.unwrap_or(families_present[0]);

	// Keep only variants in the chosen family.
	let family_variants: Vec<&VariantStream> = candidates
		.into_iter()
		.filter(|(_, _, fam)| *fam == target_family)
		.map(|(variant, _, _)| variant)
		.collect();

	// Deduplicate by resolution, keeping the lowest-bandwidth variant for each size.
	let mut by_resolution: HashMap<Option<Resolution>, &VariantStream> = HashMap::new();

	for variant in family_variants {
		let key = variant.resolution;
		let bandwidth = variant.average_bandwidth.unwrap_or(variant.bandwidth);

		match by_resolution.entry(key) {
			Entry::Vacant(entry) => {
				entry.insert(variant);
			}
			Entry::Occupied(mut entry) => {
				let existing = entry.get();
				let existing_bw = existing.average_bandwidth.unwrap_or(existing.bandwidth);
				if bandwidth < existing_bw {
					entry.insert(variant);
				}
			}
		}
	}

	by_resolution.values().cloned().collect()
}

fn resolve_uri(base: &Url, value: &str) -> std::result::Result<Url, url::ParseError> {
	if let Ok(url) = Url::parse(value) {
		return Ok(url);
	}

	base.join(value)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn hls_config_new_sets_fields() {
		let url = "https://example.com/stream.m3u8".to_string();
		let cfg = HlsConfig::new(url.clone());
		assert_eq!(cfg.playlist, url);
	}

	#[test]
	fn hls_ingest_starts_without_importers() {
		let broadcast = moq_lite::Broadcast::produce().producer.into();
		let url = "https://example.com/master.m3u8".to_string();
		let cfg = HlsConfig::new(url);
		let hls = Hls::new(broadcast, cfg).unwrap();

		assert!(!hls.has_video_importer());
		assert!(!hls.has_audio_importer());
	}
}
