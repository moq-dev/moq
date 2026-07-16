//! HLS import: pull an HLS master/media playlist and publish it into MoQ.
//!
//! Watches an HLS master or media playlist, downloads each fMP4 segment as it
//! appears, and feeds it through moq-mux's fMP4 importer (which publishes a
//! `hang` broadcast + catalog). Classic HLS only for now (no LL-HLS partial
//! segments on the import side).

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::io::SeekFrom;
use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use m3u8_rs::{
	AlternativeMedia, AlternativeMediaType, ByteRange, Map, MasterPlaylist, MediaPlaylist, MediaSegment, Resolution,
	VariantStream,
};
use moq_mux::catalog::Producer as CatalogProducer;
use moq_mux::container::fmp4::Import as Fmp4;
use moq_mux::select;
use reqwest::Client;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tracing::{debug, info, warn};
use url::Url;

use crate::{Error, Result, SequenceKind};

/// Per-request timeout for the default HTTP client (playlist + segment fetches).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Backoff before retrying after a failed import step, so a transient upstream
/// error (a 5xx, a truncated segment) doesn't tear down the whole import.
const ERROR_BACKOFF: Duration = Duration::from_secs(1);

/// How far back from the live edge to start when (re-)anchoring to a playlist window.
///
/// A live window is commonly 30s or more of media. Starting at its oldest segment would
/// publish that much stale media before reaching live, so join a few segments back
/// instead: enough to prime a player's buffer, without the lag.
const ANCHOR_SEGMENTS: usize = 3;

/// Configuration for the HLS import loop.
#[derive(Clone)]
pub struct Config {
	/// The master or media playlist URL or file path to import.
	pub playlist: String,

	/// HTTP client used to fetch the playlist and segments, for example one carrying
	/// credentials for an authenticated origin. Defaults to a plain client with a 30
	/// second per-request timeout.
	///
	/// This is a [`reqwest::Client`], re-exported as [`crate::reqwest`]; a major version
	/// bump of that dependency is a breaking change for this field.
	pub client: Option<Client>,
}

impl Config {
	/// Create an import configuration for `playlist` using the default HTTP client.
	pub fn new(playlist: String) -> Self {
		Self { playlist, client: None }
	}

	/// Parse the playlist string into a URL.
	/// If it starts with http:// or https://, parse as URL.
	/// Otherwise, treat as a file path and convert to file:// URL.
	fn parse_playlist(&self) -> Result<Url> {
		if self.playlist.starts_with("http://") || self.playlist.starts_with("https://") {
			Url::parse(&self.playlist).map_err(|_| Error::InvalidPlaylistUrl)
		} else {
			let path = PathBuf::from(&self.playlist);
			let absolute = if path.is_absolute() {
				path
			} else {
				std::env::current_dir()?.join(path)
			};
			Url::from_file_path(&absolute).map_err(|_| Error::InvalidFilePath)
		}
	}
}

/// Result of a single import step.
struct StepOutcome {
	/// Number of media segments written during this step.
	wrote_segments: usize,
	/// Target segment duration (in seconds) from the playlist, if known.
	target_duration: Option<u64>,
}

/// What a step does when a rendition fails.
#[derive(Clone, Copy)]
enum OnError {
	/// Fail the whole step. Used at startup, where a rendition that can't be imported
	/// means a broken import, and reporting it beats signalling readiness for nothing.
	Fail,
	/// Log it and keep the other renditions going. Used by the steady-state loop, where a
	/// transient upstream error must not tear the import down.
	Warn,
}

/// A resource to fetch: a URL, optionally narrowed to a resolved byte range.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Resource {
	url: Url,
	range: Option<ResolvedRange>,
}

/// An HLS byte range with its offset resolved (HLS allows omitting it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ResolvedRange {
	start: u64,
	length: u64,
}

/// Tracks the end of the previous range so an offset-less `EXT-X-BYTERANGE` can chain
/// onto it, as HLS requires.
#[derive(Clone, Debug, Default)]
struct RangeCursor {
	previous: Option<(Url, u64)>,
}

impl RangeCursor {
	fn resolve(&mut self, url: Url, range: Option<&ByteRange>) -> Result<Resource> {
		let Some(range) = range else {
			self.previous = None;
			return Ok(Resource { url, range: None });
		};

		let start = match range.offset {
			Some(offset) => offset,
			None => self
				.previous
				.as_ref()
				.filter(|(previous_url, _)| previous_url == &url)
				.map(|(_, end)| *end)
				.ok_or_else(|| Error::MissingByteRangeOffset { url: url.clone() })?,
		};
		let end = start
			.checked_add(range.length)
			.filter(|_| range.length > 0)
			.ok_or_else(|| Error::InvalidByteRange {
				url: url.clone(),
				start,
				length: range.length,
			})?;

		self.previous = Some((url.clone(), end));
		Ok(Resource {
			url,
			range: Some(ResolvedRange {
				start,
				length: range.length,
			}),
		})
	}
}

fn resolve_map(url: Url, range: Option<&ByteRange>) -> Result<Resource> {
	// EXT-X-MAP ranges require explicit offsets; they do not chain like media ranges.
	RangeCursor::default().resolve(url, range)
}

/// Fetches playlists and segments over HTTP or from the filesystem, applying HLS byte
/// ranges (and validating that the server honored them).
struct Fetcher {
	client: Client,
}

impl Fetcher {
	fn new(client: Option<Client>) -> Result<Self> {
		let client = match client {
			Some(client) => client,
			None => Client::builder()
				.user_agent(concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")))
				// Bound playlist/segment fetches so a stuck request can't wedge `run()`.
				.timeout(REQUEST_TIMEOUT)
				.build()?,
		};
		Ok(Self { client })
	}

	async fn media_playlist(&self, url: Url) -> Result<MediaPlaylist> {
		let body = self.fetch_url(url).await?;

		// Nom errors take ownership of the input, so we need to stringify any error messages.
		let playlist = m3u8_rs::parse_media_playlist_res(&body).map_err(|e| Error::ParsePlaylist(e.to_string()))?;

		Ok(playlist)
	}

	async fn fetch_url(&self, url: Url) -> Result<Bytes> {
		self.fetch(&Resource { url, range: None }).await
	}

	async fn fetch(&self, resource: &Resource) -> Result<Bytes> {
		let url = &resource.url;
		if url.scheme() == "file" {
			let path = url.to_file_path().map_err(|_| Error::InvalidFileUrl)?;
			let Some(range) = resource.range else {
				return tokio::fs::read(&path).await.map(Bytes::from).map_err(Error::from);
			};

			let mut file = tokio::fs::File::open(&path).await.map_err(Error::from)?;
			file.seek(SeekFrom::Start(range.start)).await.map_err(Error::from)?;
			let mut bytes = Vec::new();
			file.take(range.length)
				.read_to_end(&mut bytes)
				.await
				.map_err(Error::from)?;
			self.validate_range_length(resource, Bytes::from(bytes))
		} else {
			let response = self.request(resource).send().await.map_err(Error::from)?;
			let response = response.error_for_status().map_err(Error::from)?;
			let partial = response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
			if partial {
				self.validate_content_range(resource, response.headers())?;
			}
			let bytes = response.bytes().await.map_err(Error::from)?;
			if resource.range.is_some() && !partial {
				self.slice_full_response(resource, bytes)
			} else {
				self.validate_range_length(resource, bytes)
			}
		}
	}

	fn request(&self, resource: &Resource) -> reqwest::RequestBuilder {
		let mut request = self.client.get(resource.url.clone());
		if let Some(range) = resource.range {
			let end = range.start + range.length - 1;
			request = request.header(reqwest::header::RANGE, format!("bytes={}-{}", range.start, end));
		}
		request
	}

	fn slice_full_response(&self, resource: &Resource, bytes: Bytes) -> Result<Bytes> {
		let Some(range) = resource.range else {
			return Ok(bytes);
		};
		let start = usize::try_from(range.start).ok();
		let end = range
			.start
			.checked_add(range.length)
			.and_then(|end| usize::try_from(end).ok());
		match start
			.zip(end)
			.and_then(|(start, end)| bytes.get(start..end).map(|_| (start, end)))
		{
			Some((start, end)) => Ok(bytes.slice(start..end)),
			None => Err(Error::ByteRangeLengthMismatch {
				url: resource.url.clone(),
				expected: range.length,
				actual: bytes.len().saturating_sub(start.unwrap_or(bytes.len())),
			}),
		}
	}

	fn validate_range_length(&self, resource: &Resource, bytes: Bytes) -> Result<Bytes> {
		let Some(range) = resource.range else {
			return Ok(bytes);
		};
		if u64::try_from(bytes.len()).ok() != Some(range.length) {
			return Err(Error::ByteRangeLengthMismatch {
				url: resource.url.clone(),
				expected: range.length,
				actual: bytes.len(),
			});
		}
		Ok(bytes)
	}

	fn validate_content_range(&self, resource: &Resource, headers: &reqwest::header::HeaderMap) -> Result<()> {
		let Some(range) = resource.range else {
			return Ok(());
		};
		let end = range.start + range.length - 1;
		let actual = headers
			.get(reqwest::header::CONTENT_RANGE)
			.and_then(|value| value.to_str().ok())
			.unwrap_or("<missing>");
		let bounds = actual
			.split_once(' ')
			.filter(|(unit, _)| unit.eq_ignore_ascii_case("bytes"))
			.and_then(|(_, value)| value.split_once('/'))
			.and_then(|(bounds, _)| bounds.split_once('-'))
			.and_then(|(start, end)| Some((start.parse().ok()?, end.parse().ok()?)));
		if bounds != Some((range.start, end)) {
			return Err(Error::ByteRangeResponseMismatch {
				url: resource.url.clone(),
				start: range.start,
				end,
			});
		}
		Ok(())
	}
}

/// The publish side every rendition writes into: one broadcast and its shared catalog.
#[derive(Clone)]
struct Sink {
	broadcast: moq_net::BroadcastProducer,
	catalog: CatalogProducer,
}

impl Sink {
	/// Mint an fMP4 importer that publishes only the roles in `select`.
	fn importer(&self, select: &select::Broadcast) -> Fmp4 {
		Fmp4::new(self.broadcast.clone(), self.catalog.clone()).with_select(select.clone())
	}
}

/// One HLS rendition being imported: its playlist cursor plus the fMP4 importer that
/// publishes it. The importer lives here (rather than on [`Import`]) so a rendition owns
/// everything it needs to make progress.
struct TrackState {
	/// Human-readable name for logs, e.g. `video[0]` or `audio`.
	label: String,
	playlist: Url,
	/// Which roles this playlist's importer publishes (a muxed variant alongside a
	/// separate audio rendition publishes video only).
	select: select::Broadcast,
	sink: Sink,
	/// The importer for the current init segment generation, minted by [`Self::ensure_map`].
	importer: Option<Fmp4>,
	next_sequence: Option<u64>,
	next_discontinuity: Option<u64>,
	/// The `EXT-X-MAP` resource the current `importer` was initialized from.
	map: Option<Resource>,
	media_range: RangeCursor,
}

impl TrackState {
	fn new(label: String, playlist: Url, select: select::Broadcast, sink: Sink) -> Self {
		Self {
			label,
			playlist,
			select,
			sink,
			importer: None,
			next_sequence: None,
			next_discontinuity: None,
			map: None,
			media_range: RangeCursor::default(),
		}
	}

	/// Fetch this track's current media playlist and consume any fresh segments,
	/// recording the playlist's target duration if not already known.
	async fn ingest(&mut self, fetcher: &Fetcher, target_duration: &mut Option<u64>) -> Result<usize> {
		let playlist = fetcher.media_playlist(self.playlist.clone()).await?;
		if target_duration.is_none() {
			*target_duration = Some(playlist.target_duration);
		}
		self.consume_segments(fetcher, &playlist).await
	}

	/// Where in `playlist.segments` this track resumes, re-anchoring (and warning) if the
	/// upstream window moved out from under us.
	fn anchor(&mut self, playlist_seq: u64, last_playlist_seq: u64, total_segments: usize) -> usize {
		// Joining a live window: start near its edge rather than replaying the whole thing.
		let live_edge = total_segments.saturating_sub(ANCHOR_SEGMENTS);

		// First step: starting mid-window is normal, so no warning.
		let Some(next) = self.next_sequence else {
			return live_edge;
		};

		if next > last_playlist_seq {
			warn!(
				label = %self.label,
				next_sequence = next,
				playlist_sequence = playlist_seq,
				last_playlist_sequence = last_playlist_seq,
				"imported ahead of playlist (upstream sequence reset?), re-anchoring near the live edge"
			);
			self.reanchor();
			return live_edge;
		}

		if next < playlist_seq {
			warn!(
				label = %self.label,
				next_sequence = next,
				playlist_sequence = playlist_seq,
				"fell behind the playlist window, resuming from its oldest segment"
			);
			self.reanchor();
			return 0;
		}

		(next - playlist_seq) as usize
	}

	/// Forget where we were, so the next pushed segment re-bases the MoQ group sequence.
	/// The byte-range cursor needs no reset: `consume_segments` rebuilds it from the
	/// playlist prefix on every step.
	fn reanchor(&mut self) {
		self.next_sequence = None;
	}

	async fn consume_segments(&mut self, fetcher: &Fetcher, playlist: &MediaPlaylist) -> Result<usize> {
		let playlist_seq = playlist.media_sequence;
		let total_segments = playlist.segments.len();
		let last_playlist_seq = playlist_seq + total_segments as u64;

		let skip = self.anchor(playlist_seq, last_playlist_seq, total_segments);
		let to_process = total_segments.saturating_sub(skip);

		debug!(
			label = %self.label,
			playlist_sequence = playlist_seq,
			next_sequence = ?self.next_sequence,
			skip,
			total_segments,
			to_process,
			"consuming HLS segments"
		);

		if to_process == 0 {
			return Ok(0);
		}

		// Replay the skipped prefix. HLS defines this state relative to the whole playlist
		// rather than to the segments we happen to download, so joining mid-window (which
		// we do by design) has to reconstruct it rather than start blank:
		//
		// - the discontinuity counter, which names the media timeline,
		// - the `EXT-X-MAP` still in effect, since it applies until the next one,
		// - the byte-range cursor an offset-less `EXT-X-BYTERANGE` chains from.
		//
		// Rebuilding from the playlist is idempotent, so a contiguous resume lands on
		// exactly the state it left off with.
		let mut discontinuity_seq = playlist.discontinuity_sequence;
		let mut map = None;
		self.media_range = RangeCursor::default();
		for segment in &playlist.segments[..skip] {
			if segment.discontinuity {
				discontinuity_seq = bump_discontinuity(discontinuity_seq)?;
			}
			if segment.map.is_some() {
				map = segment.map.as_ref();
			}
			if !segment.uri.is_empty() {
				let url = resolve_uri(&self.playlist, &segment.uri)?;
				self.media_range.resolve(url, segment.byte_range.as_ref())?;
			}
		}

		let base_seq = playlist_seq + skip as u64;

		for (i, segment) in playlist.segments[skip..].iter().enumerate() {
			let sequence = base_seq + i as u64;
			if segment.discontinuity {
				discontinuity_seq = bump_discontinuity(discontinuity_seq)?;
			}
			if segment.map.is_some() {
				map = segment.map.as_ref();
			}
			// m3u8-rs only attaches `EXT-X-MAP` to the segment directly after the tag, so
			// on every segment but that one this is the map carried down from the prefix.
			if let Some(map) = map {
				self.ensure_map(fetcher, map).await?;
			}
			self.push_segment(fetcher, segment, sequence, discontinuity_seq).await?;
		}

		info!(label = %self.label, consumed = to_process, "consumed HLS segments");
		Ok(to_process)
	}

	/// Point this track at `map`'s init segment, replacing the importer if it changed.
	async fn ensure_map(&mut self, fetcher: &Fetcher, map: &Map) -> Result<()> {
		let url = resolve_uri(&self.playlist, &map.uri)?;
		let resource = resolve_map(url, map.byte_range.as_ref())?;

		// The resolved resource IS the init segment's identity: same bytes, same generation.
		if self.map.as_ref() == Some(&resource) {
			return Ok(());
		}

		let bytes = fetcher.fetch(&resource).await?;
		let mut importer = self.sink.importer(&self.select);

		// The importer buffers internally, so a fully-parsed init segment leaves it
		// initialized; any trailing partial atom just waits for the next segment. A
		// segment that never yields a moov surfaces later as a decode error.
		importer.decode(&bytes)?;

		// A changed map starts a new track generation. The new importer publishes its
		// configuration first, then dropping the old importer retires its catalog entries.
		self.importer = Some(importer);
		self.map = Some(resource);
		self.next_discontinuity = None;
		info!(label = %self.label, "loaded HLS init segment generation");
		Ok(())
	}

	async fn push_segment(
		&mut self,
		fetcher: &Fetcher,
		segment: &MediaSegment,
		sequence: u64,
		discontinuity_sequence: u64,
	) -> Result<()> {
		if segment.uri.is_empty() {
			return Err(Error::EmptySegmentUri);
		}

		let url = resolve_uri(&self.playlist, &segment.uri)?;
		let mut range = self.media_range.clone();
		let resource = range.resolve(url, segment.byte_range.as_ref())?;
		let bytes = fetcher.fetch(&resource).await?;

		// HLS media sequence names the live window, while discontinuity sequence names a
		// new media timeline. Whenever we join, skip ahead, or cross a discontinuity, anchor
		// the MoQ group sequence to both so consumers do not wait on groups HLS has moved
		// past. Contiguous segments let the importer auto-increment instead; we still pack
		// (and so validate) the sequence on that path so media sequence can't silently
		// auto-increment into the discontinuity bits.
		let group_sequence = moq_sequence(discontinuity_sequence, sequence)?;
		let reanchored =
			self.next_sequence != Some(sequence) || self.next_discontinuity != Some(discontinuity_sequence);

		// `consume_segments` resolves the effective map before every segment, so a missing
		// importer means the playlist never carried an `EXT-X-MAP`.
		let importer = self.importer.as_mut().ok_or(Error::MissingMap)?;
		if reanchored {
			importer.seek(group_sequence)?;
		}
		importer.decode(&bytes)?;

		self.media_range = range;
		self.next_sequence = Some(sequence + 1);
		self.next_discontinuity = Some(discontinuity_sequence);

		Ok(())
	}
}

/// HLS import that pulls an HLS master or media playlist and feeds the bytes into the
/// fMP4 importer.
///
/// Provides `init()` to publish an initial batch of segments, and `run()` to run the
/// continuous import loop.
pub struct Import {
	sink: Sink,
	fetcher: Fetcher,
	/// Parsed base URL for the playlist (file:// or http(s)://).
	base_url: Url,
	/// All discovered video variants (one per HLS rendition).
	video: Vec<TrackState>,
	/// Optional audio track shared across variants.
	audio: Option<TrackState>,
}

impl Import {
	/// Create a new HLS import that will write into the given broadcast.
	pub fn new(broadcast: moq_net::BroadcastProducer, catalog: CatalogProducer, cfg: Config) -> Result<Self> {
		let base_url = cfg.parse_playlist()?;
		Ok(Self {
			sink: Sink { broadcast, catalog },
			fetcher: Fetcher::new(cfg.client)?,
			base_url,
			video: Vec::new(),
			audio: None,
		})
	}

	/// Discover the renditions and import the first batch of segments.
	///
	/// Lets a caller signal readiness once media is actually flowing, so unlike
	/// [`run`](Self::run) this fails rather than retries: a rendition that can't be
	/// imported at startup is a broken import, and saying so beats reporting ready and
	/// then warning forever. [`run`](Self::run) does the same work on its first iteration,
	/// so calling this first is optional.
	pub async fn init(&mut self) -> Result<()> {
		let wrote = self.step(OnError::Fail).await?.wrote_segments;
		if wrote == 0 {
			warn!("HLS playlist had no new segments during init step");
		} else {
			info!(count = wrote, "buffered initial HLS segments");
		}
		Ok(())
	}

	/// Run the import loop until cancelled.
	///
	/// A failed step (e.g. a transient playlist fetch error) is logged and
	/// retried after a short backoff rather than ending the import.
	pub async fn run(&mut self) -> Result<()> {
		loop {
			let outcome = match self.step(OnError::Warn).await {
				Ok(outcome) => outcome,
				Err(err) => {
					warn!(%err, "HLS import step failed, retrying");
					tokio::time::sleep(ERROR_BACKOFF).await;
					continue;
				}
			};
			let delay = refresh_delay(outcome.target_duration, outcome.wrote_segments);

			debug!(
				wrote_segments = outcome.wrote_segments,
				target_duration = ?outcome.target_duration,
				delay_secs = delay.as_secs_f32(),
				"HLS import step complete"
			);

			tokio::time::sleep(delay).await;
		}
	}

	/// Perform a single import step for all active tracks.
	///
	/// This fetches the current media playlists, consumes any fresh segments,
	/// and returns how many segments were written along with the target
	/// duration to guide scheduling of the next step.
	///
	/// `on_error` decides what a failing rendition costs: the whole step, or a log line.
	async fn step(&mut self, on_error: OnError) -> Result<StepOutcome> {
		self.ensure_tracks().await?;

		let mut wrote_segments = 0;
		let mut target_duration = None;

		for track in self.video.iter_mut().chain(self.audio.iter_mut()) {
			match track.ingest(&self.fetcher, &mut target_duration).await {
				Ok(count) => wrote_segments += count,
				Err(err) => match on_error {
					OnError::Fail => return Err(err),
					// Keep the other renditions going: one bad variant or segment shouldn't
					// drop the rest or abort the whole step.
					OnError::Warn => warn!(label = %track.label, %err, "rendition import step failed, will retry"),
				},
			}
		}

		Ok(StepOutcome {
			wrote_segments,
			target_duration,
		})
	}

	fn track(&self, label: impl Into<String>, playlist: Url, select: select::Broadcast) -> TrackState {
		TrackState::new(label.into(), playlist, select, self.sink.clone())
	}

	async fn ensure_tracks(&mut self) -> Result<()> {
		// Tracks already discovered.
		if !self.video.is_empty() {
			return Ok(());
		}

		let body = self.fetcher.fetch_url(self.base_url.clone()).await?;
		if !m3u8_rs::is_master_playlist(&body) {
			// Fallback: treat the provided URL as a single (muxed) media playlist.
			let track = self.track("video[0]", self.base_url.clone(), select_muxed());
			self.video.push(track);
			return Ok(());
		}

		let master = m3u8_rs::parse_master_playlist_res(&body).map_err(|e| Error::ParsePlaylist(e.to_string()))?;
		let variants = select_variants(&master);
		if variants.is_empty() {
			return Err(Error::NoVariants);
		}

		// Choose an audio rendition first, so the video variants below know whether
		// they need to drop their muxed audio.
		if let Some(group_id) = variants.iter().find_map(|v| v.audio.as_deref()) {
			if let Some(audio_tag) = select_audio(&master, group_id) {
				if let Some(uri) = &audio_tag.uri {
					let audio_url = resolve_uri(&self.base_url, uri)?;
					self.audio = Some(self.track("audio", audio_url, select_audio_only()));
				} else {
					warn!(%group_id, "audio rendition missing URI");
				}
			} else {
				warn!(%group_id, "audio group not found in master playlist");
			}
		}

		// With a separate audio rendition, the variants are muxed but should publish
		// video only so the audio isn't duplicated; otherwise import every track.
		let variant_select = if self.audio.is_some() {
			select_video_only()
		} else {
			select_muxed()
		};
		for (index, variant) in variants.iter().enumerate() {
			let video_url = resolve_uri(&self.base_url, &variant.uri)?;
			let track = self.track(format!("video[{index}]"), video_url, variant_select.clone());
			self.video.push(track);
		}

		let audio_url = self.audio.as_ref().map(|a| a.playlist.to_string());
		info!(
			video_variants = variants.len(),
			audio = audio_url.as_deref().unwrap_or("none"),
			"selected master playlist renditions"
		);

		Ok(())
	}
}

/// Compute the delay before the next import step should run.
fn refresh_delay(target_duration: Option<u64>, wrote_segments: usize) -> Duration {
	let base = target_duration
		.map(|dur| Duration::from_secs(dur.max(1)))
		.unwrap_or_else(|| Duration::from_millis(500));
	if wrote_segments == 0 {
		return base / 2;
	}

	base
}

/// Selection for a muxed rendition (the only source): publish every track.
fn select_muxed() -> select::Broadcast {
	select::Broadcast::default()
		.video(select::Video::default())
		.audio(select::Audio::default())
}

/// Selection for a video variant that has a separate audio rendition: video only.
fn select_video_only() -> select::Broadcast {
	select::Broadcast::default().video(select::Video::default())
}

/// Selection for a separate audio rendition: audio only.
fn select_audio_only() -> select::Broadcast {
	select::Broadcast::default().audio(select::Audio::default())
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
	// Map codec strings into a coarse "family" so we can prefer H.264 over others.
	fn codec_family(codec: &str) -> Option<&'static str> {
		if codec.starts_with("avc1.") || codec.starts_with("avc3.") {
			Some("h264")
		} else {
			None
		}
	}

	// Extract the first *video* codec token from the CODECS attribute. A list like
	// `mp4a.40.2,avc1.4d401f` (audio first) must still surface the video codec.
	fn first_video_codec(variant: &VariantStream) -> Option<&str> {
		let codecs = variant.codecs.as_deref()?;
		codecs
			.split(',')
			.map(|s| s.trim())
			.find(|codec| codec_family(codec).is_some())
	}

	fn bandwidth(variant: &VariantStream) -> u64 {
		variant.average_bandwidth.unwrap_or(variant.bandwidth)
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

	// Deduplicate by resolution, keeping the highest-bandwidth variant for each size:
	// same pixels, more bits is the better rendition to re-publish.
	let mut by_resolution: HashMap<Option<Resolution>, &VariantStream> = HashMap::new();

	for variant in family_variants {
		match by_resolution.entry(variant.resolution) {
			Entry::Vacant(entry) => {
				entry.insert(variant);
			}
			Entry::Occupied(mut entry) => {
				if bandwidth(variant) > bandwidth(entry.get()) {
					entry.insert(variant);
				}
			}
		}
	}

	// The map has no order of its own, so sort highest quality first: track names and
	// logs stay stable across runs.
	let mut selected: Vec<&VariantStream> = by_resolution.into_values().collect();
	selected.sort_by_key(|variant| {
		let pixels = variant.resolution.map(|r| r.width * r.height).unwrap_or(0);
		std::cmp::Reverse((pixels, bandwidth(variant)))
	});
	selected
}

fn resolve_uri(base: &Url, value: &str) -> std::result::Result<Url, url::ParseError> {
	if let Ok(url) = Url::parse(value) {
		return Ok(url);
	}

	base.join(value)
}

/// Advance the running discontinuity sequence, rejecting a u64 wrap on absurd input.
fn bump_discontinuity(sequence: u64) -> Result<u64> {
	sequence.checked_add(1).ok_or(Error::SequenceOverflow {
		kind: SequenceKind::Discontinuity,
		value: sequence,
	})
}

/// Pack HLS discontinuity + media sequence into a single MoQ group sequence.
///
/// HLS media sequence alone can rewind after an upstream reset, and discontinuity
/// sequence alone cannot order segments inside the same epoch. The lower 48 bits hold
/// the media sequence (ample for realistic playlists) while the upper 16 bits hold the
/// discontinuity sequence, so a new epoch always sorts after every segment of the last.
fn moq_sequence(discontinuity_sequence: u64, media_sequence: u64) -> Result<u64> {
	const MEDIA_BITS: u32 = 48;
	const MEDIA_MASK: u64 = (1u64 << MEDIA_BITS) - 1;
	const DISCONTINUITY_MASK: u64 = u64::MAX >> MEDIA_BITS;

	if media_sequence > MEDIA_MASK {
		return Err(Error::SequenceOverflow {
			kind: SequenceKind::Media,
			value: media_sequence,
		});
	}
	if discontinuity_sequence > DISCONTINUITY_MASK {
		return Err(Error::SequenceOverflow {
			kind: SequenceKind::Discontinuity,
			value: discontinuity_sequence,
		});
	}

	Ok((discontinuity_sequence << MEDIA_BITS) | media_sequence)
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::path::Path;
	use std::sync::atomic::{AtomicUsize, Ordering};
	use tokio::io::AsyncWriteExt as _;
	use tokio::net::TcpListener;

	static COUNTER: AtomicUsize = AtomicUsize::new(0);

	fn temp_dir() -> PathBuf {
		let n = COUNTER.fetch_add(1, Ordering::Relaxed);
		let dir = std::env::temp_dir().join(format!("moq-hls-test-{}-{n}", std::process::id()));
		std::fs::create_dir_all(&dir).unwrap();
		dir
	}

	fn write_import(dir: &Path, resource: &[u8], playlist: &str) -> (Import, CatalogProducer) {
		std::fs::write(dir.join("media.mp4"), resource).unwrap();
		let playlist_path = dir.join("media.m3u8");
		std::fs::write(&playlist_path, playlist).unwrap();

		let mut broadcast = moq_net::Broadcast::new().produce();
		let catalog = CatalogProducer::new(&mut broadcast).unwrap();
		let cfg = Config::new(playlist_path.to_string_lossy().into_owned());
		let import = Import::new(broadcast, catalog.clone(), cfg).unwrap();
		(import, catalog)
	}

	/// The init segment plus `count` media fragments carved out of the fMP4 fixture.
	fn fmp4_parts(count: usize) -> (Vec<u8>, Vec<Vec<u8>>) {
		let data = include_bytes!("../../moq-mux/src/container/fmp4/test_data/bbb.mp4");
		let mut moofs = Vec::new();
		let mut position = 0usize;
		while position + 8 <= data.len() {
			let size = u32::from_be_bytes(data[position..position + 4].try_into().unwrap()) as usize;
			if size < 8 || position + size > data.len() {
				break;
			}
			if &data[position + 4..position + 8] == b"moof" {
				moofs.push(position);
			}
			position += size;
		}
		// `windows(2)` yields one fragment per adjacent moof pair.
		assert!(moofs.len() > count, "fixture has too few fragments for {count}");

		let init = data[..moofs[0]].to_vec();
		let fragments = moofs
			.windows(2)
			.take(count)
			.map(|window| data[window[0]..window[1]].to_vec())
			.collect();
		(init, fragments)
	}

	async fn serve_response(
		status: &str,
		headers: &[(&str, &str)],
		body: &[u8],
	) -> (Url, tokio::task::JoinHandle<Vec<u8>>) {
		let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
		let address = listener.local_addr().unwrap();
		let mut response = format!(
			"HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
			body.len()
		);
		for (name, value) in headers {
			response.push_str(&format!("{name}: {value}\r\n"));
		}
		response.push_str("\r\n");
		let mut response = response.into_bytes();
		response.extend_from_slice(body);

		let server = tokio::spawn(async move {
			let (mut stream, _) = listener.accept().await.unwrap();
			let mut request = Vec::new();
			loop {
				let mut chunk = [0; 1024];
				let read = stream.read(&mut chunk).await.unwrap();
				if read == 0 {
					break;
				}
				request.extend_from_slice(&chunk[..read]);
				if request.windows(4).any(|window| window == b"\r\n\r\n") {
					break;
				}
			}
			stream.write_all(&response).await.unwrap();
			request
		});
		(Url::parse(&format!("http://{address}/media.mp4")).unwrap(), server)
	}

	fn sink() -> Sink {
		let mut broadcast = moq_net::Broadcast::new().produce();
		let catalog = CatalogProducer::new(&mut broadcast).unwrap();
		Sink { broadcast, catalog }
	}

	fn track_state() -> TrackState {
		TrackState::new(
			"video[0]".to_string(),
			Url::parse("https://example.com/media.m3u8").unwrap(),
			select_muxed(),
			sink(),
		)
	}

	fn fetcher() -> Fetcher {
		Fetcher::new(None).unwrap()
	}

	fn ranged_resource(url: Url) -> Resource {
		Resource {
			url,
			range: Some(ResolvedRange { start: 2, length: 3 }),
		}
	}

	#[test]
	fn hls_config_new_sets_fields() {
		let url = "https://example.com/stream.m3u8".to_string();
		let cfg = Config::new(url.clone());
		assert_eq!(cfg.playlist, url);
	}

	#[test]
	fn select_variants_handles_audio_first_codecs() {
		// CODECS lists the audio codec first; the video codec must still be found.
		let master = b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000000,CODECS=\"mp4a.40.2,avc1.4d401f\"\nvideo.m3u8\n";
		let (_, master) = m3u8_rs::parse_master_playlist(master).unwrap();
		let variants = select_variants(&master);
		assert_eq!(variants.len(), 1);
	}

	/// Same resolution at two bitrates: re-publish the better one, deterministically.
	#[test]
	fn select_variants_prefers_highest_bandwidth_per_resolution() {
		let master = b"#EXTM3U\n\
			#EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=1280x720,CODECS=\"avc1.4d401f\"\nlow.m3u8\n\
			#EXT-X-STREAM-INF:BANDWIDTH=3000000,RESOLUTION=1280x720,CODECS=\"avc1.4d401f\"\nhigh.m3u8\n";
		let (_, master) = m3u8_rs::parse_master_playlist(master).unwrap();
		let variants = select_variants(&master);
		assert_eq!(variants.len(), 1);
		assert_eq!(variants[0].uri, "high.m3u8");
	}

	/// The dedup map has no order; the output must still be stable (highest quality first).
	#[test]
	fn select_variants_orders_by_descending_quality() {
		let master = b"#EXTM3U\n\
			#EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=640x360,CODECS=\"avc1.4d401f\"\nsmall.m3u8\n\
			#EXT-X-STREAM-INF:BANDWIDTH=5000000,RESOLUTION=1920x1080,CODECS=\"avc1.4d401f\"\nlarge.m3u8\n\
			#EXT-X-STREAM-INF:BANDWIDTH=3000000,RESOLUTION=1280x720,CODECS=\"avc1.4d401f\"\nmedium.m3u8\n";
		let (_, master) = m3u8_rs::parse_master_playlist(master).unwrap();
		let uris: Vec<&str> = select_variants(&master).iter().map(|v| v.uri.as_str()).collect();
		assert_eq!(uris, ["large.m3u8", "medium.m3u8", "small.m3u8"]);
	}

	#[test]
	fn hls_import_starts_without_tracks() {
		let mut broadcast = moq_net::Broadcast::new().produce();
		let catalog = CatalogProducer::new(&mut broadcast).unwrap();
		let url = "https://example.com/master.m3u8".to_string();
		let cfg = Config::new(url);
		let hls = Import::new(broadcast, catalog, cfg).unwrap();

		assert!(hls.video.is_empty());
		assert!(hls.audio.is_none());
	}

	/// Joining a live window must start near its edge, not replay the whole thing.
	#[test]
	fn first_anchor_joins_near_the_live_edge() {
		let mut track = track_state();
		// A 10-segment window starting at media sequence 100.
		assert_eq!(track.anchor(100, 110, 10), 10 - ANCHOR_SEGMENTS);
	}

	/// A window shorter than the rewind is consumed whole rather than skipped.
	#[test]
	fn first_anchor_takes_a_short_window_whole() {
		let mut track = track_state();
		assert_eq!(track.anchor(0, 2, 2), 0);
	}

	#[test]
	fn contiguous_anchor_resumes_where_it_left_off() {
		let mut track = track_state();
		track.next_sequence = Some(105);
		assert_eq!(track.anchor(100, 110, 10), 5);
		// Still anchored: no re-base.
		assert_eq!(track.next_sequence, Some(105));
	}

	/// An upstream sequence reset leaves us "ahead"; rejoin near the live edge.
	#[test]
	fn anchor_ahead_of_playlist_rejoins_near_the_live_edge() {
		let mut track = track_state();
		track.next_sequence = Some(500);
		assert_eq!(track.anchor(100, 110, 10), 10 - ANCHOR_SEGMENTS);
		assert!(track.next_sequence.is_none());
	}

	/// Falling out of the window loses media either way; take everything still there.
	#[test]
	fn anchor_behind_playlist_resumes_from_the_oldest_segment() {
		let mut track = track_state();
		track.next_sequence = Some(5);
		assert_eq!(track.anchor(100, 110, 10), 0);
		assert!(track.next_sequence.is_none());
	}

	#[tokio::test]
	async fn variantless_master_is_not_treated_as_media() {
		let dir = temp_dir();
		let path = dir.join("master.m3u8");
		std::fs::write(
			&path,
			"#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"en\",URI=\"audio.m3u8\"\n",
		)
		.unwrap();

		let mut broadcast = moq_net::Broadcast::new().produce();
		let catalog = CatalogProducer::new(&mut broadcast).unwrap();
		let cfg = Config::new(path.to_string_lossy().into_owned());
		let mut import = Import::new(broadcast, catalog, cfg).unwrap();

		assert!(matches!(import.ensure_tracks().await, Err(Error::NoVariants)));
	}

	#[test]
	fn byte_ranges_advance_implicit_offsets_for_the_same_resource() {
		let url = Url::parse("https://example.com/media.mp4").unwrap();
		let mut cursor = RangeCursor::default();
		let first = cursor
			.resolve(
				url.clone(),
				Some(&ByteRange {
					length: 20,
					offset: Some(10),
				}),
			)
			.unwrap();
		let second = cursor
			.resolve(
				url,
				Some(&ByteRange {
					length: 5,
					offset: None,
				}),
			)
			.unwrap();

		assert_eq!(first.range.unwrap().start, 10);
		assert_eq!(second.range.unwrap().start, 30);
	}

	#[test]
	fn implicit_byte_range_rejects_a_different_resource() {
		let mut cursor = RangeCursor::default();
		cursor
			.resolve(
				Url::parse("https://example.com/a.mp4").unwrap(),
				Some(&ByteRange {
					length: 20,
					offset: Some(10),
				}),
			)
			.unwrap();
		let err = cursor
			.resolve(
				Url::parse("https://example.com/b.mp4").unwrap(),
				Some(&ByteRange {
					length: 5,
					offset: None,
				}),
			)
			.unwrap_err();

		assert!(matches!(err, Error::MissingByteRangeOffset { .. }));
	}

	#[test]
	fn map_byte_range_requires_an_explicit_offset() {
		let err = resolve_map(
			Url::parse("https://example.com/init.mp4").unwrap(),
			Some(&ByteRange {
				length: 20,
				offset: None,
			}),
		)
		.unwrap_err();

		assert!(matches!(err, Error::MissingByteRangeOffset { .. }));
	}

	#[test]
	fn ranged_resource_builds_http_range_request() {
		let url = Url::parse("https://example.com/media.mp4").unwrap();
		let request = fetcher()
			.request(&Resource {
				url,
				range: Some(ResolvedRange { start: 2, length: 3 }),
			})
			.build()
			.unwrap();

		assert_eq!(request.headers()[reqwest::header::RANGE], "bytes=2-4");
	}

	#[tokio::test]
	async fn ranged_http_resource_accepts_matching_partial_response() {
		let (url, server) = serve_response("206 Partial Content", &[("Content-Range", "bytes 2-4/6")], b"cde").await;

		let bytes = fetcher().fetch(&ranged_resource(url)).await.unwrap();
		let request = String::from_utf8(server.await.unwrap()).unwrap();

		assert_eq!(bytes, b"cde".as_slice());
		assert!(
			request
				.lines()
				.any(|line| line.eq_ignore_ascii_case("range: bytes=2-4"))
		);
	}

	#[tokio::test]
	async fn ranged_http_resource_slices_full_response() {
		let (url, server) = serve_response("200 OK", &[], b"abcdef").await;

		let bytes = fetcher().fetch(&ranged_resource(url)).await.unwrap();
		server.await.unwrap();

		assert_eq!(bytes, b"cde".as_slice());
	}

	#[tokio::test]
	async fn ranged_http_resource_rejects_wrong_response_length() {
		let (url, server) = serve_response("206 Partial Content", &[("Content-Range", "bytes 2-4/6")], b"cd").await;

		let err = fetcher().fetch(&ranged_resource(url)).await.unwrap_err();
		server.await.unwrap();

		assert!(matches!(
			err,
			Error::ByteRangeLengthMismatch {
				expected: 3,
				actual: 2,
				..
			}
		));
	}

	#[tokio::test]
	async fn ranged_http_resource_rejects_mismatched_content_range() {
		let (url, server) = serve_response("206 Partial Content", &[("Content-Range", "bytes 3-5/6")], b"def").await;

		let err = fetcher().fetch(&ranged_resource(url)).await.unwrap_err();
		server.await.unwrap();

		assert!(matches!(err, Error::ByteRangeResponseMismatch { start: 2, end: 4, .. }));
	}

	#[tokio::test]
	async fn imports_single_file_with_implicit_segment_ranges() {
		let (init, fragments) = fmp4_parts(2);
		let mut resource = init.clone();
		resource.extend_from_slice(&fragments[0]);
		resource.extend_from_slice(&fragments[1]);
		let playlist = format!(
			"#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"media.mp4\",BYTERANGE=\"{}@0\"\n#EXTINF:1,\n#EXT-X-BYTERANGE:{}@{}\nmedia.mp4\n#EXTINF:1,\n#EXT-X-BYTERANGE:{}\nmedia.mp4\n",
			init.len(),
			fragments[0].len(),
			init.len(),
			fragments[1].len()
		);
		let (mut import, catalog) = write_import(&temp_dir(), &resource, &playlist);

		import.init().await.unwrap();

		let snapshot = catalog.snapshot();
		assert_eq!(snapshot.video.renditions.len(), 1);
		assert_eq!(snapshot.audio.renditions.len(), 1);
		assert_eq!(import.video[0].next_sequence, Some(2));
	}

	/// A live window longer than `ANCHOR_SEGMENTS` is joined mid-playlist, which means the
	/// state HLS carries across the whole playlist has to be replayed from the skipped
	/// prefix. m3u8-rs attaches `EXT-X-MAP` only to the segment right after the tag, so
	/// without that replay the importer is never built and every segment fails with
	/// `MissingMap`; the offset-less byte ranges would not resolve either.
	#[tokio::test]
	async fn joins_mid_window_using_prefix_map_and_byte_ranges() {
		// Every HLS segment starts a decodable group, as a real one does. The fixture
		// interleaves per-track moofs, so only fragments 0, 1, 2 and 4 open a group;
		// fragment 3 continues an earlier one and is left out of the byte layout.
		let (init, fragments) = fmp4_parts(5);
		let segments: Vec<&Vec<u8>> = [0, 1, 2, 4].iter().map(|&k| &fragments[k]).collect();

		let mut resource = init.clone();
		for segment in &segments {
			resource.extend_from_slice(segment);
		}

		// Only the first segment carries an explicit offset; the rest chain off it, so
		// resolving a later segment depends on every earlier one.
		let mut playlist = format!(
			"#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"media.mp4\",BYTERANGE=\"{}@0\"\n#EXTINF:1,\n#EXT-X-BYTERANGE:{}@{}\nmedia.mp4\n",
			init.len(),
			segments[0].len(),
			init.len()
		);
		for segment in &segments[1..] {
			playlist.push_str(&format!("#EXTINF:1,\n#EXT-X-BYTERANGE:{}\nmedia.mp4\n", segment.len()));
		}

		let (mut import, catalog) = write_import(&temp_dir(), &resource, &playlist);

		import.init().await.unwrap();

		let track = &import.video[0];
		assert!(
			track.importer.is_some(),
			"the EXT-X-MAP from the skipped prefix must initialize the importer"
		);
		// Four segments and a three-segment rewind, so segment 0 is skipped: the one
		// carrying both the EXT-X-MAP and the only explicit byte offset.
		assert_eq!(track.next_sequence, Some(4));
		assert_eq!(catalog.snapshot().video.renditions.len(), 1);
	}

	#[tokio::test]
	async fn map_change_replaces_importer_and_catalog_generation() {
		let (init, fragments) = fmp4_parts(2);
		let second_init = init.len() + fragments[0].len();
		let second_fragment = second_init + init.len();
		let mut resource = init.clone();
		resource.extend_from_slice(&fragments[0]);
		resource.extend_from_slice(&init);
		resource.extend_from_slice(&fragments[1]);
		let playlist = format!(
			"#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"media.mp4\",BYTERANGE=\"{}@0\"\n#EXTINF:1,\n#EXT-X-BYTERANGE:{}@{}\nmedia.mp4\n#EXT-X-DISCONTINUITY\n#EXT-X-MAP:URI=\"media.mp4\",BYTERANGE=\"{}@{}\"\n#EXTINF:1,\n#EXT-X-BYTERANGE:{}@{}\nmedia.mp4\n",
			init.len(),
			fragments[0].len(),
			init.len(),
			init.len(),
			second_init,
			fragments[1].len(),
			second_fragment
		);
		let (mut import, catalog) = write_import(&temp_dir(), &resource, &playlist);

		import.init().await.unwrap();

		let snapshot = catalog.snapshot();
		assert_eq!(snapshot.video.renditions.len(), 1);
		assert_eq!(snapshot.audio.renditions.len(), 1);
		assert_eq!(
			import.video[0].map.as_ref().unwrap().range.unwrap().start,
			second_init as u64
		);
	}

	/// Resolve `ensure_tracks` against a master playlist written to a temp file.
	async fn discover(master_body: &str) -> Import {
		let dir = temp_dir();
		let path = dir.join("master.m3u8");
		std::fs::write(&path, master_body).unwrap();

		let mut broadcast = moq_net::Broadcast::new().produce();
		let catalog = CatalogProducer::new(&mut broadcast).unwrap();
		// `Config` takes a filesystem path for non-http inputs.
		let cfg = Config::new(path.to_str().unwrap().to_string());
		let mut hls = Import::new(broadcast, catalog, cfg).unwrap();
		hls.ensure_tracks().await.unwrap();
		hls
	}

	/// A master with a separate audio rendition: variants publish video only, the
	/// alternate rendition publishes audio only.
	#[tokio::test]
	async fn discover_splits_separate_audio() {
		let master = "#EXTM3U\n\
			#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"en\",URI=\"audio.m3u8\"\n\
			#EXT-X-STREAM-INF:BANDWIDTH=1000000,CODECS=\"avc1.4d401f,mp4a.40.2\",AUDIO=\"aud\"\n\
			video.m3u8\n";
		let hls = discover(master).await;

		assert_eq!(hls.video.len(), 1);
		assert!(hls.video[0].select.has_video() && !hls.video[0].select.has_audio());

		let audio = hls.audio.as_ref().expect("separate audio rendition");
		assert!(audio.select.has_audio() && !audio.select.has_video());
	}

	/// A master whose variant carries muxed A/V (no separate audio group) publishes
	/// every track.
	#[tokio::test]
	async fn discover_muxed_variant_keeps_both() {
		let master = "#EXTM3U\n\
			#EXT-X-STREAM-INF:BANDWIDTH=1000000,CODECS=\"avc1.4d401f\"\n\
			video.m3u8\n";
		let hls = discover(master).await;

		assert_eq!(hls.video.len(), 1);
		assert!(hls.video[0].select.has_video() && hls.video[0].select.has_audio());
		assert!(hls.audio.is_none());
	}

	#[test]
	fn moq_sequence_orders_discontinuities_after_media_sequence() {
		// A new epoch outranks every segment of the previous one, even a higher media seq.
		let last_of_epoch_0 = moq_sequence(0, u64::from(u32::MAX)).unwrap();
		let first_of_epoch_1 = moq_sequence(1, 0).unwrap();
		assert!(first_of_epoch_1 > last_of_epoch_0);
	}

	#[test]
	fn moq_sequence_preserves_media_order_within_epoch() {
		assert!(moq_sequence(3, 10).unwrap() > moq_sequence(3, 9).unwrap());
	}

	#[test]
	fn moq_sequence_rejects_unrepresentable_media_sequence() {
		let err = moq_sequence(0, 1u64 << 48).unwrap_err();
		assert!(matches!(
			err,
			Error::SequenceOverflow {
				kind: SequenceKind::Media,
				..
			}
		));
	}

	#[test]
	fn moq_sequence_rejects_unrepresentable_discontinuity_sequence() {
		let err = moq_sequence(1u64 << 16, 0).unwrap_err();
		assert!(matches!(
			err,
			Error::SequenceOverflow {
				kind: SequenceKind::Discontinuity,
				..
			}
		));
	}
}
