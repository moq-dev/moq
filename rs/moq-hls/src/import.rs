//! HLS import: pull an HLS master/media playlist and publish it into MoQ.
//!
//! Watches an HLS master or media playlist, downloads each fMP4 segment as it
//! appears, and feeds it through moq-mux's fMP4 importer (which publishes a
//! `hang` broadcast + catalog). Classic HLS only for now (no LL-HLS partial
//! segments on the import side).

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::io::SeekFrom;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use m3u8_rs::{
	AlternativeMedia, AlternativeMediaType, ByteRange, Map, MasterPlaylist, MediaPlaylist, MediaSegment, Resolution,
	VariantStream,
};
use moq_mux::catalog::Producer as CatalogProducer;
use moq_mux::container::fmp4::Import as Fmp4;
use moq_mux::select;
use rand::RngExt;
use reqwest::Client;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tracing::{debug, info, warn};
use url::Url;

use crate::error::redact;
use crate::{Error, Result, SequenceKind};

/// Per-request timeout for the default HTTP client (playlist + segment fetches).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Backoff bounds for retrying a failed import step, so a transient upstream error (a 503, a
/// dropped connection) doesn't tear down the whole import.
///
/// The budget is short on purpose: a failure that clears within it was transient, and one that
/// doesn't is an outage the caller should hear about rather than one the import papers over while
/// publishing nothing. No error is classified; the budget is the filter.
const STEP_RETRY_MIN: Duration = Duration::from_secs(1);
const STEP_RETRY_MAX: Duration = Duration::from_secs(5);
const STEP_RETRY_BUDGET: Duration = Duration::from_secs(10);

/// How far back from the live edge to start when (re-)anchoring to a playlist window.
///
/// A live window is commonly 30s or more of media. Starting at its oldest segment would
/// publish that much stale media before reaching live, so join a few segments back
/// instead: enough to prime a player's buffer, without the lag.
const ANCHOR_SEGMENTS: usize = 3;

/// How many consecutive failed steps retire a rendition (see [`TrackState::evict`]).
///
/// Enough to ride out a transient fetch error or a stale playlist, short enough that a
/// genuinely dead variant leaves the catalog within a few seconds.
const MAX_RENDITION_FAILURES: usize = 3;

/// Configuration for the HLS import loop.
#[derive(Clone)]
#[non_exhaustive]
pub struct Config {
	/// The master or media playlist URL to import. Local files use a `file://` URL.
	/// Every resource and redirect hop must share its scheme, host, and port.
	pub playlist: Url,

	/// Republish each fMP4 fragment whole as CMAF instead of splitting it into one Legacy frame per
	/// sample. See [`fmp4::Import::with_passthrough`](moq_mux::container::fmp4::Import::with_passthrough).
	pub passthrough: bool,
}

impl Config {
	/// Create an import configuration for `playlist`.
	pub fn new(playlist: Url) -> Self {
		Self {
			playlist,
			passthrough: false,
		}
	}
}

/// Result of a single import step.
struct StepOutcome {
	/// Number of media segments written during this step.
	wrote_segments: usize,
	/// Target segment duration (in seconds) from the playlist, if known.
	target_duration: Option<u64>,
	/// Renditions that ingested without error, however many segments each wrote.
	///
	/// A live playlist with no new segments yet still counts: that's a healthy rendition with
	/// nothing to add this pass, not a failure. Counting segments instead would read a quiet
	/// playlist as a dead one.
	ok: usize,
	/// A rendition failure from this step, if any.
	///
	/// [`OnError::Warn`] keeps the other renditions going after one fails, so a step can report
	/// `Ok` having imported nothing at all. The loop needs to tell that apart from a quiet playlist
	/// with no new segments, or it treats a permanently broken source as steady progress.
	///
	/// When several renditions fail, this is the one whose failure another pass could still clear,
	/// so a single permanently-dead variant doesn't end an import the others could still serve.
	failed: Option<Error>,
	/// Every rendition's playlist carries `EXT-X-ENDLIST` and its last segment is imported.
	ended: bool,
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
				.ok_or_else(|| Error::MissingByteRangeOffset { url: redact(&url) })?,
		};
		let end = start
			.checked_add(range.length)
			.filter(|_| range.length > 0)
			.ok_or_else(|| Error::InvalidByteRange {
				url: redact(&url),
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

/// Whether `url` shares `entry`'s scheme, host, and port.
///
/// `file:` URLs have opaque origins in [`Url::origin`], so compare the parts directly.
fn same_origin(entry: &Url, url: &Url) -> bool {
	entry.scheme() == url.scheme()
		&& entry.host() == url.host()
		&& entry.port_or_known_default() == url.port_or_known_default()
}

/// Fetches playlists and segments over HTTP or from the filesystem, applying HLS byte
/// ranges (and validating that the server honored them).
struct Fetcher {
	client: Client,
	/// The caller's playlist URL. A playlist is untrusted, so every fetch stays on its origin.
	entry: Url,
}

impl Fetcher {
	fn new(config: &Config) -> Result<Self> {
		let entry = config.playlist.clone();
		if !matches!(entry.scheme(), "file" | "http" | "https") {
			return Err(Error::ResourceRefused);
		}

		// An intermediate hop is already a request to its origin, so check each one before
		// following it. A stopped hop reaches `fetch` as a redirect response, refused there.
		let origin = entry.clone();
		let follow = reqwest::redirect::Policy::default();
		let redirect = reqwest::redirect::Policy::custom(move |attempt| {
			if same_origin(&origin, attempt.url()) {
				follow.redirect(attempt)
			} else {
				attempt.stop()
			}
		});

		let client = Client::builder()
			.user_agent(concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION")))
			// Bound playlist/segment fetches so a stuck request can't wedge `run()`.
			.timeout(REQUEST_TIMEOUT)
			.redirect(redirect)
			.build()?;
		Ok(Self { client, entry })
	}

	/// Fetch and parse a media playlist, with the URL its relative URIs resolve against.
	async fn media_playlist(&self, url: Url) -> Result<(MediaPlaylist, Url)> {
		let (body, base) = self.playlist(url).await?;

		// Nom errors take ownership of the input, so we need to stringify any error messages.
		let playlist = m3u8_rs::parse_media_playlist_res(&body)
			.map_err(|e| Error::ParsePlaylist(e.map(|e| e.code).to_string()))?;

		Ok((playlist, base))
	}

	/// Fetch a playlist, with the URL its relative URIs resolve against: where any redirects ended.
	async fn playlist(&self, url: Url) -> Result<(Bytes, Url)> {
		self.get(&Resource { url, range: None }).await
	}

	async fn fetch(&self, resource: &Resource) -> Result<Bytes> {
		Ok(self.get(resource).await?.0)
	}

	/// Fetch `resource`, with the URL that finally served it after any redirects.
	async fn get(&self, resource: &Resource) -> Result<(Bytes, Url)> {
		let url = &resource.url;
		if !same_origin(&self.entry, url) {
			return Err(Error::ResourceRefused);
		}
		if url.scheme() == "file" {
			let path = url.to_file_path().map_err(|_| Error::InvalidFileUrl)?;
			let Some(range) = resource.range else {
				let bytes = tokio::fs::read(&path).await.map(Bytes::from).map_err(Error::from)?;
				return Ok((bytes, url.clone()));
			};

			let mut file = tokio::fs::File::open(&path).await.map_err(Error::from)?;
			file.seek(SeekFrom::Start(range.start)).await.map_err(Error::from)?;
			let mut bytes = Vec::new();
			file.take(range.length)
				.read_to_end(&mut bytes)
				.await
				.map_err(Error::from)?;
			Ok((
				Self::validate_range_length(url, range, Bytes::from(bytes))?,
				url.clone(),
			))
		} else {
			let response = self.request(resource).send().await?;
			if response.status().is_redirection() {
				return Err(Error::ResourceRefused);
			}
			let mut response = response.error_for_status()?;
			let served = response.url().clone();
			let Some(range) = resource.range else {
				return Ok((response.bytes().await?, served));
			};

			// An origin that ignores `Range` sends the whole resource, which would cost every
			// earlier byte of the file for each segment.
			if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
				return Err(Error::ByteRangeIgnored {
					url: redact(url),
					status: response.status().as_u16(),
				});
			}
			Self::validate_content_range(url, range, response.headers())?;

			// Stream the body so an overrun fails without buffering the excess.
			let mut body = BytesMut::new();
			while let Some(chunk) = response.chunk().await? {
				let read = body.len() + chunk.len();
				if !u64::try_from(read).is_ok_and(|read| read <= range.length) {
					return Err(Error::ByteRangeLengthMismatch {
						url: redact(url),
						expected: range.length,
						actual: read,
					});
				}
				body.extend_from_slice(&chunk);
			}
			Ok((Self::validate_range_length(url, range, body.freeze())?, served))
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

	fn validate_range_length(url: &Url, range: ResolvedRange, bytes: Bytes) -> Result<Bytes> {
		if u64::try_from(bytes.len()).ok() != Some(range.length) {
			return Err(Error::ByteRangeLengthMismatch {
				url: redact(url),
				expected: range.length,
				actual: bytes.len(),
			});
		}
		Ok(bytes)
	}

	fn validate_content_range(url: &Url, range: ResolvedRange, headers: &reqwest::header::HeaderMap) -> Result<()> {
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
				url: redact(url),
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
	broadcast: moq_net::broadcast::Producer,
	catalog: CatalogProducer,
	/// The playlist's one timestamp base. Every rendition, and every importer replacing one on a
	/// new `EXT-X-MAP`, shifts by its offset: separate offsets would shift each by its own first
	/// fragment's arrival.
	timebase: moq_mux::catalog::Timebase,
	/// Republish fragments whole rather than unwrapping their samples.
	passthrough: bool,
}

impl Sink {
	fn new(broadcast: moq_net::broadcast::Producer, catalog: CatalogProducer, passthrough: bool) -> Self {
		let timebase = catalog.timebase();
		Self {
			broadcast,
			catalog,
			timebase,
			passthrough,
		}
	}

	/// Mint an fMP4 importer that publishes only the roles in `select`.
	fn importer(&self, select: &select::Broadcast) -> Fmp4 {
		// A reservation (not a bare clone) so this init's tracks publish together.
		// The pass holds a separate reservation across every rendition; this one
		// only covers the tracks this init segment declares.
		Fmp4::new(self.broadcast.clone(), self.timebase.reserve())
			.with_select(select.clone())
			.with_passthrough(self.passthrough)
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
	/// Consecutive failed ingest attempts, reset by any successful one. See
	/// [`MAX_RENDITION_FAILURES`].
	failures: usize,
	/// The playlist carries `EXT-X-ENDLIST` and its last segment is imported: nothing follows.
	ended: bool,
	/// This track has joined the playlist while it was live. Unlike `next_sequence`, eviction
	/// does not forget it, so a playlist that ends while the rendition recovers still finishes.
	followed: bool,
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
			failures: 0,
			ended: false,
			followed: false,
		}
	}

	/// Give up on this rendition's current importer generation after repeated failures.
	///
	/// Dropping the importer ends its tracks and retires their catalog entries and HLS records, so
	/// players stop selecting a rendition that no longer advances; a later successful step rebuilds
	/// it from the init segment.
	fn evict(&mut self) {
		self.importer = None;
		self.map = None;
		self.reanchor();
	}

	/// Fetch this track's current media playlist and consume any fresh segments,
	/// recording the playlist's target duration if not already known.
	async fn ingest(&mut self, fetcher: &Fetcher, target_duration: &mut Option<u64>) -> Result<usize> {
		// Reloads request the listed URL, so a redirect that moves is followed anew each time.
		let (playlist, base) = fetcher.media_playlist(self.playlist.clone()).await?;
		let end = sequence_end(&playlist)?;
		if target_duration.is_none() {
			*target_duration = Some(playlist.target_duration);
		}

		let consumed = self.consume_segments(fetcher, &playlist, &base).await?;
		self.ended = playlist.end_list && self.next_sequence.is_none_or(|next| next == end);
		Ok(consumed)
	}

	/// Where in `playlist.segments` this track resumes, re-anchoring (and warning) if the
	/// upstream window moved out from under us.
	fn anchor(&mut self, playlist: &MediaPlaylist) -> Result<usize> {
		let playlist_seq = playlist.media_sequence;
		let last_playlist_seq = sequence_end(playlist)?;

		// First step: starting mid-window is normal, so no warning.
		let Some(next) = self.next_sequence else {
			return self.join(playlist);
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
			return self.join(playlist);
		}

		if next < playlist_seq {
			warn!(
				label = %self.label,
				next_sequence = next,
				playlist_sequence = playlist_seq,
				"fell behind the playlist window, resuming from its oldest segment"
			);
			self.reanchor();
			return Ok(0);
		}

		Ok((next - playlist_seq) as usize)
	}

	/// Where this track joins `playlist` when it has no position in it yet.
	///
	/// The window is joined near its edge (see [`ANCHOR_SEGMENTS`]). A playlist that had already
	/// ended before this track ever saw it live is refused: joining its tail would end the import
	/// cleanly having published an arbitrary suffix of the recording, and replaying it whole would
	/// burst it out faster than real time, withholding the catalog until the last segment
	/// downloads. One that ended after it was followed live, even while this track had no position
	/// in it (it was empty, or the rendition was evicted), is the live stream ending.
	fn join(&mut self, playlist: &MediaPlaylist) -> Result<usize> {
		if !playlist.end_list {
			self.followed = true;
		} else if !self.followed {
			return Err(Error::PlaylistEnded);
		}
		Ok(playlist.segments.len().saturating_sub(ANCHOR_SEGMENTS))
	}

	/// Forget where we were, so the next pushed segment re-bases the MoQ group sequence.
	/// The byte-range cursor needs no reset: `consume_segments` rebuilds it from the
	/// playlist prefix on every step.
	fn reanchor(&mut self) {
		self.next_sequence = None;
	}

	/// Import `playlist`'s fresh segments, resolving its relative URIs against `base`.
	async fn consume_segments(&mut self, fetcher: &Fetcher, playlist: &MediaPlaylist, base: &Url) -> Result<usize> {
		let playlist_seq = playlist.media_sequence;
		let total_segments = playlist.segments.len();

		let skip = self.anchor(playlist)?;
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
				let url = resolve_uri(base, &segment.uri)?;
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
				self.ensure_map(fetcher, map, base).await?;
			}
			self.push_segment(fetcher, segment, base, sequence, discontinuity_seq)
				.await?;
		}

		info!(label = %self.label, consumed = to_process, "consumed HLS segments");
		Ok(to_process)
	}

	/// Point this track at `map`'s init segment, replacing the importer if it changed.
	async fn ensure_map(&mut self, fetcher: &Fetcher, map: &Map, base: &Url) -> Result<()> {
		let url = resolve_uri(base, &map.uri)?;
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

		// The old generation must finish before retiring its catalog entries, so a subscriber
		// draining its final group sees a clean end instead of a dropped producer.
		if let Some(previous) = self.importer.as_mut() {
			previous.finish()?;
		}
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
		base: &Url,
		sequence: u64,
		discontinuity_sequence: u64,
	) -> Result<()> {
		if segment.uri.is_empty() {
			return Err(Error::EmptySegmentUri);
		}

		let url = resolve_uri(base, &segment.uri)?;
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
		// Only a discontinuity starts a new media timeline; a sequence skip must keep advancing.
		if self
			.next_discontinuity
			.is_some_and(|previous| previous != discontinuity_sequence)
		{
			importer.discontinuity()?;
		}
		if reanchored {
			importer.seek(group_sequence)?;
		}
		// Every playlist entry is a source segment boundary, so declare it even when the media
		// carries no styp. Every rendition declares the same ones; the HLS segmenter drops a cut
		// that would land inside its minimum segment duration, so the duplicates cost nothing.
		importer.cut()?;
		importer.decode(&bytes).map_err(|err| match err {
			// Timestamp resets are unsupported: the published tracks never rewind, and no later
			// pass can import this segment, so it is refused rather than retried.
			moq_mux::Error::TimestampRewind(_)
			| moq_mux::Error::Cmaf(moq_mux::container::fmp4::Error::NonMonotonicDecodeTime { .. }) => Error::Rewind {
				rendition: self.label.clone(),
				source: err,
			},
			err => err.into(),
		})?;

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
	/// The configured master or media playlist URL (file:// or http(s)://).
	playlist: Url,
	/// All discovered video variants (one per HLS rendition).
	video: Vec<TrackState>,
	/// Optional audio track shared across variants.
	audio: Option<TrackState>,
}

impl Import {
	/// Create a new HLS import that will write into the given broadcast.
	pub fn new(broadcast: moq_net::broadcast::Producer, catalog: CatalogProducer, cfg: Config) -> Result<Self> {
		let fetcher = Fetcher::new(&cfg)?;
		Ok(Self {
			sink: Sink::new(broadcast, catalog, cfg.passthrough),
			fetcher,
			playlist: cfg.playlist,
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

	/// Run the import loop until the playlist ends or the import fails.
	///
	/// The playlist ends cleanly once every rendition's playlist carries `EXT-X-ENDLIST` and its
	/// last segment is imported; this then returns `Ok`, and [`finish`](Self::finish) ends the
	/// broadcast. A playlist that never ends runs until cancelled. Import follows live playlists
	/// only: one that has already ended (`EXT-X-ENDLIST`) when a rendition first sees it fails with
	/// [`Error::PlaylistEnded`], rather than importing its tail and ending as if it were whole.
	///
	/// An `EXT-X-DISCONTINUITY` keeps the source's timestamps: a forward jump continues the same
	/// tracks, and a changed `EXT-X-MAP` starts a fresh generation of them. Timestamps that go
	/// backwards on the same tracks, such as an encoder restart that reuses its init segment,
	/// end the import with [`Error::Rewind`] instead of publishing a group that rewinds.
	///
	/// A failed step is logged and retried with escalating backoff, and the import ends with the
	/// last error once the backoff budget is spent, so a broken source surfaces within seconds
	/// instead of looping forever. A settled answer ends the import immediately: an HTTP status
	/// the origin actually sent, such as a `404` playlist, or [`Error::PlaylistEnded`], since no
	/// amount of waiting changes either.
	pub async fn run(&mut self) -> Result<()> {
		let mut delay = STEP_RETRY_MIN;
		let mut deadline = moq_clock::now() + STEP_RETRY_BUDGET;

		loop {
			// A step where *nothing* ingested while a rendition was failing is a failed pass wearing
			// an `Ok`: `step` swallows per-rendition errors so one bad variant doesn't drop the rest.
			// Letting it through would reset the backoff every pass, so a source that returns 404
			// forever would spin at the refresh cadence while publishing nothing.
			//
			// Keyed on renditions that ingested, not on segments written: a healthy live playlist
			// with nothing new this pass writes no segments, and reading that as a failure would end
			// a multi-rendition import over one dead variant the others were covering for.
			let stepped = match self.step(OnError::Warn).await {
				Ok(StepOutcome {
					ok: 0,
					failed: Some(err),
					..
				}) => Err(err),
				stepped => stepped,
			};

			let outcome = match stepped {
				Ok(outcome) => {
					// The source is alive, so the earlier failures no longer describe it.
					delay = STEP_RETRY_MIN;
					deadline = moq_clock::now() + STEP_RETRY_BUDGET;
					outcome
				}
				Err(err @ Error::ResourceRefused) => return Err(err),
				// A settled answer, such as a 404 playlist, is not going to become a 200 on the next
				// pass. Everything else rides the backoff budget.
				Err(err) if err.settled() => {
					return Err(err);
				}
				// The source rewound its timestamps, which no later pass can import.
				Err(err @ Error::Rewind { .. }) => return Err(err),
				Err(err) => {
					let now = moq_clock::now();
					if now >= deadline {
						return Err(err);
					}

					warn!(%err, "HLS import step failed, retrying");
					// Jittered so a fleet of importers pointed at one origin doesn't retry in
					// lockstep, and never past the deadline the budget promised.
					let wait = delay
						.mul_f64(0.5 + rand::rng().random::<f64>() / 2.0)
						.min(deadline - now);
					delay = (delay * 2).min(STEP_RETRY_MAX);
					moq_clock::sleep(wait).await;
					continue;
				}
			};
			if outcome.ended {
				info!("HLS playlist ended");
				return Ok(());
			}
			let delay = refresh_delay(outcome.target_duration, outcome.wrote_segments);

			debug!(
				wrote_segments = outcome.wrote_segments,
				target_duration = ?outcome.target_duration,
				delay_secs = delay.as_secs_f32(),
				"HLS import step complete"
			);

			moq_clock::sleep(delay).await;
		}
	}

	/// Finish every rendition's tracks and the catalog once [`run`](Self::run) returned `Ok`.
	///
	/// The finished tracks stay servable while the import is held, so hand it and the
	/// broadcast to a [`moq_mux::Linger`] to keep them up for late subscribers.
	pub fn finish(&mut self) -> Result<()> {
		for track in self.video.iter_mut().chain(self.audio.iter_mut()) {
			if let Some(importer) = &mut track.importer {
				importer.finish()?;
			}
		}
		self.sink.catalog.finish()?;
		Ok(())
	}

	/// Abort every rendition's importer and the catalog with `err` so subscribers see the real
	/// cause.
	///
	/// Call this when the import is torn down with a known error; simply dropping the
	/// [`Import`] instead lets its tracks end as [`moq_net::Error::Dropped`]. Consumes
	/// the import: every rendition is dead afterwards.
	pub fn abort(mut self, err: moq_net::Error) {
		for track in &mut self.video {
			if let Some(importer) = track.importer.take() {
				importer.abort(err.clone());
			}
		}
		if let Some(track) = &mut self.audio
			&& let Some(importer) = track.importer.take()
		{
			importer.abort(err.clone());
		}
		self.sink.catalog.clone().abort(err);
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

		// Hold the catalog across the pass. A rendition reserves only once its init
		// segment loads, and that reservation ends at its first frame, before the next
		// rendition is ingested. Without this, the first rendition publishes alone.
		// The hold ends with the pass: a rendition that has no segments yet, or whose
		// init fetch fails, never reserves, and must not withhold the catalog for the
		// rest of the import. Every pass holds, not only the first, because a quiet
		// playlist publishes nothing and the first catalog may be a later pass.
		let _catalog = self.sink.catalog.reserve();

		let mut wrote_segments = 0;
		let mut target_duration = None;
		let mut failed = None;
		let mut ok = 0;

		for track in self.video.iter_mut().chain(self.audio.iter_mut()) {
			// Nothing follows an ended rendition, so polling it again could only fail and evict
			// a finished rendition while its siblings are still catching up.
			if track.ended {
				continue;
			}

			match track.ingest(&self.fetcher, &mut target_duration).await {
				Ok(count) => {
					track.failures = 0;
					wrote_segments += count;
					ok += 1;
				}
				// Admission is settled by the caller's input policy, not rendition health.
				Err(err @ Error::ResourceRefused) => return Err(err),
				Err(err) => match on_error {
					OnError::Fail => return Err(err),
					// A playlist that ended before this rendition saw it live is refused input, not a
					// dead variant the others can cover for: importing the rest would publish a
					// broadcast silently missing it.
					OnError::Warn if matches!(err, Error::PlaylistEnded) => return Err(err),
					// A rewind ends the import: no sibling covers for it, and evicting the
					// rendition would rejoin it as fresh tracks, accepting the reset after all.
					OnError::Warn if matches!(err, Error::Rewind { .. }) => return Err(err),
					// Keep the other renditions going: one bad variant or segment shouldn't
					// drop the rest or abort the whole step.
					OnError::Warn => {
						track.failures += 1;
						if track.failures >= MAX_RENDITION_FAILURES && track.importer.is_some() {
							warn!(label = %track.label, %err, failures = track.failures, "rendition import keeps failing, dropping it until it recovers");
							track.evict();
						} else {
							warn!(label = %track.label, %err, "rendition import step failed, will retry");
						}
						// Prefer a failure another pass could still clear, so a single
						// permanently-dead variant doesn't end an import the others could still
						// serve. Keeping whichever error came last instead would make the outcome
						// depend on rendition order.
						if !err.settled() || failed.is_none() {
							failed = Some(err);
						}
					}
				},
			}
		}

		let ended = self.video.iter().chain(self.audio.iter()).all(|track| track.ended);
		Ok(StepOutcome {
			wrote_segments,
			target_duration,
			failed,
			ok,
			ended,
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

		let (body, base) = self.fetcher.playlist(self.playlist.clone()).await?;
		if !m3u8_rs::is_master_playlist(&body) {
			// Fallback: treat the provided URL as a single (muxed) media playlist.
			let track = self.track("video[0]", self.playlist.clone(), select_muxed());
			self.video.push(track);
			return Ok(());
		}

		let master = m3u8_rs::parse_master_playlist_res(&body)
			.map_err(|e| Error::ParsePlaylist(e.map(|e| e.code).to_string()))?;
		let variants = select_variants(&master);
		if variants.is_empty() {
			return Err(Error::NoVariants);
		}

		// Choose an audio rendition first, so the video variants below know whether
		// they need to drop their muxed audio.
		if let Some(group_id) = variants.iter().find_map(|v| v.audio.as_deref()) {
			if let Some(audio_tag) = select_audio(&master, group_id) {
				if let Some(uri) = &audio_tag.uri {
					let audio_url = resolve_uri(&base, uri)?;
					// Boundaries follow the video variant; audio only reports its groups.
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
			let video_url = resolve_uri(&base, &variant.uri)?;
			let track = self.track(format!("video[{index}]"), video_url, variant_select.clone());
			self.video.push(track);
		}

		let audio_url = self.audio.as_ref().map(|a| redact(&a.playlist).to_string());
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
		// Widened so a hostile RESOLUTION neither panics nor wraps into a false rank.
		let pixels = variant
			.resolution
			.map(|r| u128::from(r.width) * u128::from(r.height))
			.unwrap_or(0);
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

/// One past the media sequence of the playlist's last segment, rejecting a window that wraps u64.
///
/// Every sequence a segment of the window carries is below this, so validating it once keeps
/// the per-segment arithmetic in range.
fn sequence_end(playlist: &MediaPlaylist) -> Result<u64> {
	playlist
		.media_sequence
		.checked_add(playlist.segments.len() as u64)
		.ok_or(Error::SequenceOverflow {
			kind: SequenceKind::Media,
			value: playlist.media_sequence,
		})
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
	use std::path::{Path, PathBuf};
	use std::sync::Arc;
	use std::sync::atomic::{AtomicUsize, Ordering};
	use std::task::Poll;
	use tokio::io::AsyncWriteExt as _;
	use tokio::net::TcpListener;
	use tokio::sync::Notify;

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

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = CatalogProducer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let cfg = Config::new(Url::from_file_path(&playlist_path).unwrap());
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

	fn http_response(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
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
		response
	}

	/// Answer one connection per canned response, in order, returning each request.
	fn serve(listener: TcpListener, responses: Vec<Vec<u8>>) -> tokio::task::JoinHandle<Vec<Vec<u8>>> {
		tokio::spawn(async move {
			let mut requests = Vec::new();
			for response in responses {
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
				requests.push(request);
			}
			requests
		})
	}

	async fn serve_response(
		status: &str,
		headers: &[(&str, &str)],
		body: &[u8],
	) -> (Url, tokio::task::JoinHandle<Vec<Vec<u8>>>) {
		let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
		let address = listener.local_addr().unwrap();
		let server = serve(listener, vec![http_response(status, headers, body)]);
		(Url::parse(&format!("http://{address}/media.mp4")).unwrap(), server)
	}

	fn sink() -> Sink {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = CatalogProducer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		Sink::new(broadcast, catalog, false)
	}

	fn track_state() -> TrackState {
		TrackState::new(
			"video[0]".to_string(),
			Url::parse("https://example.com/media.m3u8").unwrap(),
			select_muxed(),
			sink(),
		)
	}

	fn fetcher(entry: &Url) -> Fetcher {
		Fetcher::new(&Config::new(entry.clone())).unwrap()
	}

	/// `url` carrying a credential in every part a signed URL might put one.
	fn signed(url: &Url) -> Url {
		let mut url = url.clone();
		url.set_username("alice").unwrap();
		url.set_password(Some("hunter2")).unwrap();
		url.set_query(Some("jwt=s3cret"));
		url.set_fragment(Some("f4ag"));
		url
	}

	/// Fails if any rendering of `err` carries a credential from [`signed`], as text or as the
	/// byte list a `Debug`-printed `&[u8]` renders.
	fn assert_redacted(err: &Error) {
		let printed = format!("{err} {err:#} {err:?}");
		for fixture in ["alice", "hunter2", "jwt", "s3cret", "f4ag"] {
			let bytes = format!("{:?}", fixture.as_bytes());
			let bytes = bytes.trim_matches(['[', ']']);
			assert!(
				!printed.contains(fixture) && !printed.contains(bytes),
				"error leaked {fixture}: {printed}"
			);
		}
	}

	#[test]
	fn same_origin_references_are_admitted() {
		let entry = Url::parse("https://example.com/live/master.m3u8").unwrap();
		for reference in [
			"video.m3u8",
			"../a.mp4",
			"/a.mp4",
			"//example.com/a.mp4",
			"https://example.com:443/a.mp4",
			"HTTPS://EXAMPLE.COM/a.mp4",
		] {
			let url = resolve_uri(&entry, reference).unwrap();
			assert!(same_origin(&entry, &url), "{reference}");
		}
	}

	#[tokio::test]
	async fn cross_origin_references_are_refused() {
		let dir = temp_dir();
		let path = dir.join("resource");
		std::fs::write(&path, b"sentinel").unwrap();
		let local = Url::from_file_path(dir.join("media.m3u8")).unwrap();
		let remote = Url::parse("https://example.com/live/media.m3u8").unwrap();
		let file = Url::from_file_path(&path).unwrap();
		let mut downgraded = remote.clone();
		downgraded.set_scheme("http").unwrap();
		for (entry, reference) in [
			(&remote, "https://other.example/live/a.mp4"),
			(&remote, "//other.example/live/a.mp4"),
			(&remote, downgraded.as_str()),
			(&remote, "https://example.com:8443/live/a.mp4"),
			(&remote, file.as_str()),
			(&local, "https://example.com/live/a.mp4"),
			(&local, "//example.com/live/a.mp4"),
		] {
			// Refused before any I/O: none of these hosts resolve, and the file exists.
			let url = resolve_uri(entry, reference).unwrap();
			let fetcher = fetcher(entry);
			assert!(
				matches!(fetcher.playlist(url.clone()).await, Err(Error::ResourceRefused)),
				"{reference} from {entry}"
			);
			assert!(matches!(
				fetcher.fetch(&ranged_resource(url)).await,
				Err(Error::ResourceRefused)
			));
		}
	}

	#[tokio::test]
	async fn redirect_hops_stay_on_the_playlist_origin() {
		// A same-origin hop is followed.
		let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
		let entry = Url::parse(&format!("http://{}/media.m3u8", listener.local_addr().unwrap())).unwrap();
		let server = serve(
			listener,
			vec![
				http_response("302 Found", &[("Location", "/moved.m3u8")], b""),
				http_response("200 OK", &[], b"moved"),
			],
		);
		let (body, base) = fetcher(&entry).playlist(entry.clone()).await.unwrap();
		assert_eq!(body, b"moved"[..]);
		assert_eq!(base, entry.join("/moved.m3u8").unwrap());
		server.await.unwrap();

		// A later hop to another origin is refused before it is requested.
		let other = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
		other.set_nonblocking(true).unwrap();
		let location = format!("http://{}/media.m3u8", other.local_addr().unwrap());
		let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
		let entry = Url::parse(&format!("http://{}/media.m3u8", listener.local_addr().unwrap())).unwrap();
		let server = serve(
			listener,
			vec![
				http_response("302 Found", &[("Location", "/hop.m3u8")], b""),
				http_response("302 Found", &[("Location", &location)], b""),
			],
		);
		assert!(matches!(
			fetcher(&entry).playlist(entry.clone()).await,
			Err(Error::ResourceRefused)
		));
		server.await.unwrap();
		assert_eq!(other.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
	}

	#[tokio::test]
	async fn redirected_master_resolves_against_its_final_url() {
		let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
		let origin = format!("http://{}", listener.local_addr().unwrap());
		let master = "#EXTM3U\n\
			#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aac\",NAME=\"en\",DEFAULT=YES,URI=\"audio.m3u8\"\n\
			#EXT-X-STREAM-INF:BANDWIDTH=1000,CODECS=\"avc1.4d401f,mp4a.40.2\",AUDIO=\"aac\"\nvideo.m3u8\n";
		let server = serve(
			listener,
			vec![
				http_response("302 Found", &[("Location", "/new/master.m3u8")], b""),
				http_response("200 OK", &[], master.as_bytes()),
			],
		);
		let sink = sink();
		let cfg = Config::new(Url::parse(&format!("{origin}/old/master.m3u8")).unwrap());
		let mut import = Import::new(sink.broadcast, sink.catalog, cfg).unwrap();
		import.ensure_tracks().await.unwrap();
		server.await.unwrap();

		assert_eq!(import.video[0].playlist.as_str(), format!("{origin}/new/video.m3u8"));
		assert_eq!(
			import.audio.unwrap().playlist.as_str(),
			format!("{origin}/new/audio.m3u8")
		);
	}

	#[tokio::test]
	async fn redirected_media_playlist_resolves_against_its_final_url() {
		let (init, fragments) = fmp4_parts(1);
		let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
		let origin = format!("http://{}", listener.local_addr().unwrap());
		let media = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:1,\n0.m4s\n";
		let moved = http_response("302 Found", &[("Location", "/new/video.m3u8")], b"");
		let server = serve(
			listener,
			vec![
				moved.clone(),
				http_response("200 OK", &[], media.as_bytes()),
				http_response("200 OK", &[], &init),
				http_response("200 OK", &[], &fragments[0]),
				// The reload asks the listed URL again, so a moved redirect is followed anew.
				moved,
				http_response("200 OK", &[], media.as_bytes()),
			],
		);
		let entry = Url::parse(&format!("{origin}/old/video.m3u8")).unwrap();
		let fetcher = fetcher(&entry);
		let mut track = TrackState::new("video[0]".to_string(), entry, select_muxed(), sink());
		let mut target_duration = None;
		assert_eq!(track.ingest(&fetcher, &mut target_duration).await.unwrap(), 1);
		assert_eq!(track.ingest(&fetcher, &mut target_duration).await.unwrap(), 0);

		let paths: Vec<_> = server
			.await
			.unwrap()
			.iter()
			.map(|r| request_path(r).unwrap().to_string())
			.collect();
		assert_eq!(
			paths,
			[
				"/old/video.m3u8",
				"/new/video.m3u8",
				"/new/init.mp4",
				"/new/0.m4s",
				"/old/video.m3u8",
				"/new/video.m3u8",
			]
		);
	}

	#[tokio::test]
	async fn admission_failure_ends_import_without_retrying() {
		tokio::time::pause();
		let sink = sink();
		let mut import = Import::new(
			sink.broadcast.clone(),
			sink.catalog.clone(),
			Config::new(Url::parse("https://example.com/master.m3u8").unwrap()),
		)
		.unwrap();
		import.video.push(TrackState::new(
			"video[0]".to_string(),
			Url::parse("file:///refused.m3u8").unwrap(),
			select_muxed(),
			sink,
		));
		assert!(matches!(
			futures::FutureExt::now_or_never(import.run()),
			Some(Err(Error::ResourceRefused))
		));
	}

	#[tokio::test]
	async fn local_import_allows_local_resources() {
		let dir = temp_dir();
		let path = dir.join("resource");
		std::fs::write(&path, b"sentinel").unwrap();
		let url = Url::from_file_path(&path).unwrap();
		let fetcher = Fetcher::new(&Config::new(url.clone())).unwrap();
		assert_eq!(
			fetcher.playlist(url.clone()).await.unwrap(),
			(Bytes::from_static(b"sentinel"), url.clone())
		);
		assert_eq!(fetcher.fetch(&ranged_resource(url)).await.unwrap(), b"nti"[..]);
	}

	#[tokio::test]
	async fn unsupported_resources_are_refused() {
		let entry = Url::parse("https://example.com/master.m3u8").unwrap();
		for scheme in ["ftp", "data", "custom"] {
			let url = Url::parse(&format!("{scheme}://example.com/resource?token=secret")).unwrap();
			assert!(matches!(
				Fetcher::new(&Config::new(url.clone())),
				Err(Error::ResourceRefused)
			));
			let error = fetcher(&entry).playlist(url).await.unwrap_err();
			assert!(matches!(error, Error::ResourceRefused));
			assert!(!format!("{error} {error:?}").contains("secret"));
		}
	}

	fn ranged_resource(url: Url) -> Resource {
		Resource {
			url,
			range: Some(ResolvedRange { start: 2, length: 3 }),
		}
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

	/// A declared area past `u64` still ranks by its true size: 2^32 x 2^32 would wrap to 0.
	#[test]
	fn select_variants_ranks_an_enormous_resolution_first() {
		let master = b"#EXTM3U\n\
			#EXT-X-STREAM-INF:BANDWIDTH=5000000,RESOLUTION=1920x1080,CODECS=\"avc1.4d401f\"\nlarge.m3u8\n\
			#EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=4294967296x4294967296,CODECS=\"avc1.4d401f\"\nhuge.m3u8\n";
		let (_, master) = m3u8_rs::parse_master_playlist(master).unwrap();
		let uris: Vec<&str> = select_variants(&master).iter().map(|v| v.uri.as_str()).collect();
		assert_eq!(uris, ["huge.m3u8", "large.m3u8"]);
	}

	/// A `len`-segment window starting at `media_sequence`, ended with `EXT-X-ENDLIST` or not.
	fn window(media_sequence: u64, len: usize, end_list: bool) -> MediaPlaylist {
		MediaPlaylist {
			media_sequence,
			segments: vec![MediaSegment::default(); len],
			end_list,
			..Default::default()
		}
	}

	/// Joining a live window must start near its edge, not replay the whole thing.
	#[test]
	fn first_anchor_joins_near_the_live_edge() {
		let mut track = track_state();
		assert_eq!(track.anchor(&window(100, 10, false)).unwrap(), 10 - ANCHOR_SEGMENTS);
	}

	/// A window shorter than the rewind is consumed whole rather than skipped.
	#[test]
	fn first_anchor_takes_a_short_window_whole() {
		let mut track = track_state();
		assert_eq!(track.anchor(&window(0, 2, false)).unwrap(), 0);
	}

	/// A playlist that has already ended is refused however short, since importing any part of
	/// it would end cleanly on a recording the import never followed.
	#[test]
	fn first_anchor_refuses_an_ended_playlist() {
		for len in [2, 10] {
			let mut track = track_state();
			let err = track.anchor(&window(100, len, true)).unwrap_err();
			assert!(matches!(err, Error::PlaylistEnded), "{err:?}");
		}
	}

	/// Joining an empty live window is still a join: the segment it gains next is imported, even
	/// when the playlist ends in the same reload.
	#[test]
	fn an_empty_live_join_resumes_at_its_first_segment() {
		let mut track = track_state();
		assert_eq!(track.anchor(&window(100, 0, false)).unwrap(), 0);
		assert_eq!(track.anchor(&window(100, 1, true)).unwrap(), 0);
	}

	#[test]
	fn contiguous_anchor_resumes_where_it_left_off() {
		let mut track = track_state();
		track.next_sequence = Some(105);
		assert_eq!(track.anchor(&window(100, 10, false)).unwrap(), 5);
		// Still anchored: no re-base.
		assert_eq!(track.next_sequence, Some(105));
	}

	/// A followed playlist that ends keeps importing from where it left off, through its end.
	#[test]
	fn contiguous_anchor_follows_a_playlist_into_its_end() {
		let mut track = track_state();
		track.next_sequence = Some(105);
		assert_eq!(track.anchor(&window(100, 10, true)).unwrap(), 5);
	}

	/// An upstream sequence reset leaves us "ahead"; rejoin near the live edge.
	#[test]
	fn anchor_ahead_of_playlist_rejoins_near_the_live_edge() {
		let mut track = track_state();
		track.next_sequence = Some(500);
		assert_eq!(track.anchor(&window(100, 10, false)).unwrap(), 10 - ANCHOR_SEGMENTS);
		assert!(track.next_sequence.is_none());
	}

	/// A followed playlist that ends while the track has no position in it, after an upstream
	/// reset or an eviction, is rejoined near its edge and finishes: the live stream ended.
	#[test]
	fn a_followed_playlist_that_ends_is_rejoined() {
		let mut track = track_state();
		assert_eq!(track.anchor(&window(100, 10, false)).unwrap(), 10 - ANCHOR_SEGMENTS);
		track.next_sequence = Some(500);
		assert_eq!(track.anchor(&window(100, 10, true)).unwrap(), 10 - ANCHOR_SEGMENTS);

		let mut track = track_state();
		assert_eq!(track.anchor(&window(100, 10, false)).unwrap(), 10 - ANCHOR_SEGMENTS);
		track.next_sequence = Some(110);
		track.evict();
		assert_eq!(track.anchor(&window(105, 10, true)).unwrap(), 10 - ANCHOR_SEGMENTS);
	}

	/// Falling out of the window loses media either way; take everything still there.
	#[test]
	fn anchor_behind_playlist_resumes_from_the_oldest_segment() {
		let mut track = track_state();
		track.next_sequence = Some(5);
		assert_eq!(track.anchor(&window(100, 10, false)).unwrap(), 0);
		assert!(track.next_sequence.is_none());
	}

	/// A window whose sequence runs past `u64` is refused before anything is anchored.
	#[test]
	fn anchor_refuses_a_window_past_the_last_sequence() {
		let mut track = track_state();
		let err = track.anchor(&window(u64::MAX, 1, false)).unwrap_err();
		assert!(matches!(
			err,
			Error::SequenceOverflow {
				kind: SequenceKind::Media,
				value: u64::MAX,
			}
		));
		assert!(!track.followed);
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

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = CatalogProducer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let cfg = Config::new(Url::from_file_path(&path).unwrap());
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
				signed(&Url::parse("https://example.com/b.mp4").unwrap()),
				Some(&ByteRange {
					length: 5,
					offset: None,
				}),
			)
			.unwrap_err();

		assert!(matches!(err, Error::MissingByteRangeOffset { .. }));
		assert_redacted(&err);
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
	fn empty_byte_range_is_refused() {
		let err = RangeCursor::default()
			.resolve(
				signed(&Url::parse("https://example.com/media.mp4").unwrap()),
				Some(&ByteRange {
					length: 0,
					offset: Some(10),
				}),
			)
			.unwrap_err();

		assert!(matches!(
			err,
			Error::InvalidByteRange {
				start: 10,
				length: 0,
				..
			}
		));
		assert_redacted(&err);
	}

	#[tokio::test]
	async fn playlist_parse_error_omits_the_playlist() {
		let dir = temp_dir();
		let path = dir.join("media.m3u8");
		std::fs::write(&path, "#EXTINF:1,\nseg.m4s?jwt=s3cret\n").unwrap();
		let url = Url::from_file_path(&path).unwrap();

		let err = fetcher(&url).media_playlist(url).await.unwrap_err();

		assert!(matches!(err, Error::ParsePlaylist(_)));
		assert_redacted(&err);
	}

	#[tokio::test]
	async fn ranged_http_resource_accepts_matching_partial_response() {
		let (url, server) = serve_response("206 Partial Content", &[("Content-Range", "bytes 2-4/6")], b"cde").await;

		let bytes = fetcher(&url).fetch(&ranged_resource(url.clone())).await.unwrap();
		let request = String::from_utf8(server.await.unwrap().remove(0)).unwrap();

		assert_eq!(bytes, b"cde".as_slice());
		assert!(
			request
				.lines()
				.any(|line| line.eq_ignore_ascii_case("range: bytes=2-4"))
		);
	}

	#[tokio::test]
	async fn ranged_http_resource_refuses_full_response() {
		let (url, server) = serve_response("200 OK", &[], b"abcdef").await;

		let err = fetcher(&url).fetch(&ranged_resource(signed(&url))).await.unwrap_err();
		server.await.unwrap();

		assert!(matches!(err, Error::ByteRangeIgnored { status: 200, .. }));
		assert_redacted(&err);
	}

	#[tokio::test]
	async fn ranged_http_resource_refuses_overrun_before_the_body_ends() {
		// The body promises far more than the range and stays open after the first bytes, so
		// only a fetch that refuses mid-stream returns before the request timeout.
		let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
		let url = Url::parse(&format!("http://{}/media.mp4", listener.local_addr().unwrap())).unwrap();
		let server = tokio::spawn(async move {
			let (mut stream, _) = listener.accept().await.unwrap();
			let mut request = [0; 1024];
			let _ = stream.read(&mut request).await.unwrap();
			let head = "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 2-4/6\r\nContent-Length: 1000000\r\n\r\n";
			stream.write_all(head.as_bytes()).await.unwrap();
			stream.write_all(b"cdefgh").await.unwrap();
			// Hold the connection until the client hangs up.
			let _ = stream.read(&mut request).await;
		});

		let err = fetcher(&url).fetch(&ranged_resource(signed(&url))).await.unwrap_err();
		server.await.unwrap();

		assert!(matches!(err, Error::ByteRangeLengthMismatch { expected: 3, .. }));
		assert_redacted(&err);
	}

	#[tokio::test]
	async fn ranged_http_resource_rejects_wrong_response_length() {
		let (url, server) = serve_response("206 Partial Content", &[("Content-Range", "bytes 2-4/6")], b"cd").await;

		let err = fetcher(&url).fetch(&ranged_resource(signed(&url))).await.unwrap_err();
		server.await.unwrap();

		assert!(matches!(
			err,
			Error::ByteRangeLengthMismatch {
				expected: 3,
				actual: 2,
				..
			}
		));
		assert_redacted(&err);
	}

	#[tokio::test]
	async fn ranged_http_resource_rejects_mismatched_content_range() {
		let (url, server) = serve_response("206 Partial Content", &[("Content-Range", "bytes 3-5/6")], b"def").await;

		let err = fetcher(&url).fetch(&ranged_resource(signed(&url))).await.unwrap_err();
		server.await.unwrap();

		assert!(matches!(err, Error::ByteRangeResponseMismatch { start: 2, end: 4, .. }));
		assert_redacted(&err);
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

	/// One file holding the fixture's init and four segments that each open a group, and a playlist
	/// listing the first `count` of them, ended with `EXT-X-ENDLIST` or not. The file is the same
	/// whatever `count`, so rewriting the playlist models a live window growing.
	fn recording(count: usize, end_list: bool) -> (Vec<u8>, String) {
		// The fixture interleaves per-track moofs, so only fragments 0, 1, 2 and 4 open a group.
		let (init, fragments) = fmp4_parts(5);
		let segments: Vec<&Vec<u8>> = [0, 1, 2, 4].iter().map(|&k| &fragments[k]).collect();
		assert!(count <= segments.len());

		let mut resource = init.clone();
		let mut playlist = format!(
			"#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"media.mp4\",BYTERANGE=\"{}@0\"\n",
			init.len()
		);
		for (index, segment) in segments.iter().enumerate() {
			if index < count {
				let range = format!("{}@{}", segment.len(), resource.len());
				playlist.push_str(&format!("#EXTINF:1,\n#EXT-X-BYTERANGE:{range}\nmedia.mp4\n"));
			}
			resource.extend_from_slice(segment);
		}
		if end_list {
			playlist.push_str("#EXT-X-ENDLIST\n");
		}
		(resource, playlist)
	}

	/// A playlist that has already ended when the import joins it is refused before anything is
	/// published, rather than importing its last few segments and ending as if they were the whole
	/// recording. The refusal is settled, so `run` reports it without spending the retry budget.
	#[tokio::test]
	async fn an_initially_ended_playlist_is_refused() {
		tokio::time::pause();
		let (resource, playlist) = recording(4, true);
		let (mut import, catalog) = write_import(&temp_dir(), &resource, &playlist);

		assert!(matches!(import.init().await, Err(Error::PlaylistEnded)));
		assert!(import.video[0].importer.is_none(), "nothing was imported");
		assert!(catalog.snapshot().video.renditions.is_empty());

		let run = moq_clock::timeout(Duration::from_secs(5), import.run())
			.await
			.expect("a settled refusal skips the retry budget");
		assert!(matches!(run, Err(Error::PlaylistEnded)));
	}

	/// One rendition that had already ended is refused even while another is live and ingesting,
	/// rather than importing a broadcast without it until the live one ends.
	#[tokio::test]
	async fn an_ended_rendition_beside_a_live_one_is_refused() {
		tokio::time::pause();
		let dir = temp_dir();
		let (resource, live) = recording(3, false);
		let (_, ended) = recording(4, true);
		std::fs::write(dir.join("media.mp4"), &resource).unwrap();
		std::fs::write(dir.join("live.m3u8"), live).unwrap();
		std::fs::write(dir.join("ended.m3u8"), ended).unwrap();
		let master = "#EXTM3U\n\
			#EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=640x360,CODECS=\"avc1.4d401f\"\nlive.m3u8\n\
			#EXT-X-STREAM-INF:BANDWIDTH=3000000,RESOLUTION=1280x720,CODECS=\"avc1.4d401f\"\nended.m3u8\n";
		std::fs::write(dir.join("master.m3u8"), master).unwrap();

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = CatalogProducer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let cfg = Config::new(Url::from_file_path(dir.join("master.m3u8")).unwrap());
		let mut import = Import::new(broadcast, catalog, cfg).unwrap();

		let run = moq_clock::timeout(Duration::from_secs(5), import.run())
			.await
			.expect("the ended rendition ends the import");
		assert!(matches!(run, Err(Error::PlaylistEnded)), "{run:?}");
	}

	/// A followed playlist that gains `EXT-X-ENDLIST` ends the import once its last segment is in,
	/// including one published alongside the end, and the finished import keeps serving it,
	/// catalog included, while it is held.
	#[tokio::test]
	async fn a_playlist_that_ends_later_finishes_the_import() {
		tokio::time::pause();
		let (resource, playlist) = recording(3, false);
		let dir = temp_dir();
		std::fs::write(dir.join("media.mp4"), &resource).unwrap();
		std::fs::write(dir.join("media.m3u8"), playlist).unwrap();

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = CatalogProducer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let playlist = Url::from_file_path(dir.join("media.m3u8")).unwrap();
		let mut import = Import::new(broadcast, catalog, Config::new(playlist)).unwrap();

		import.init().await.unwrap();
		assert_eq!(
			import.video[0].next_sequence,
			Some(3),
			"the live window was joined whole"
		);
		assert!(!import.video[0].ended);

		let (_, ended) = recording(4, true);
		std::fs::write(dir.join("media.m3u8"), ended).unwrap();
		moq_clock::timeout(Duration::from_secs(10), import.run())
			.await
			.expect("an ended playlist ends the import")
			.unwrap();
		assert_eq!(import.video[0].next_sequence, Some(4), "the final segment was imported");
		import.finish().unwrap();
		let mut listed = consumer.track("catalog.json").unwrap().subscribe(None).await.unwrap();
		assert!(
			listed.recv_group().await.unwrap().is_some(),
			"the late subscriber reads the catalog"
		);
	}

	/// A rendition that has stopped making progress is dropped after a few consecutive
	/// failures, which closes its recorders and retires its catalog entries.
	#[tokio::test]
	async fn a_persistently_failing_rendition_is_evicted() {
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
		let dir = temp_dir();
		let (mut import, _catalog) = write_import(&dir, &resource, &playlist);

		import.init().await.unwrap();
		assert!(import.video[0].importer.is_some(), "the rendition imported once");

		// The source goes away underneath us.
		std::fs::remove_file(dir.join("media.m3u8")).unwrap();

		for _ in 0..MAX_RENDITION_FAILURES - 1 {
			import.step(OnError::Warn).await.unwrap();
			assert!(
				import.video[0].importer.is_some(),
				"a transient failure keeps the rendition"
			);
		}

		import.step(OnError::Warn).await.unwrap();
		assert!(import.video[0].importer.is_none(), "the dead rendition is retired");
	}

	/// An ended rendition is never fetched again, so losing its playlist while its siblings
	/// are still running cannot evict the finished rendition.
	#[tokio::test]
	async fn an_ended_rendition_is_not_evicted() {
		let (resource, playlist) = recording(2, false);
		let dir = temp_dir();
		let (mut import, _catalog) = write_import(&dir, &resource, &playlist);

		import.init().await.unwrap();
		let (_, ended) = recording(2, true);
		std::fs::write(dir.join("media.m3u8"), ended).unwrap();
		assert!(import.step(OnError::Warn).await.unwrap().ended);
		assert!(import.video[0].ended, "the rendition imported its last segment");

		std::fs::remove_file(dir.join("media.m3u8")).unwrap();

		for _ in 0..MAX_RENDITION_FAILURES {
			assert!(import.step(OnError::Warn).await.unwrap().ended);
		}
		assert!(import.video[0].importer.is_some(), "the ended rendition is kept");
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

	/// The first frame of every buffered group, without waiting for another source event.
	fn group_stamps(subscriber: &mut moq_net::track::Ordered) -> (Vec<(u64, u128)>, Option<moq_net::Error>) {
		let mut out = Vec::new();
		loop {
			let mut group = match subscriber.poll_next_group(&kio::Waiter::noop()) {
				Poll::Ready(Ok(Some(group))) => group,
				Poll::Pending | Poll::Ready(Ok(None)) => break,
				Poll::Ready(Err(error)) => return (out, Some(error)),
			};
			let frame = match group.poll_read_frame(&kio::Waiter::noop()) {
				Poll::Ready(Ok(Some(frame))) => frame,
				Poll::Ready(Err(error)) => return (out, Some(error)),
				other => panic!("reading a published group's first frame: {other:?}"),
			};
			out.push((group.sequence, frame.timestamp.unwrap().as_micros()));
		}
		(out, None)
	}

	struct Discontinuity {
		result: Result<usize>,
		before: String,
		after: String,
		old_groups: Vec<(u64, u128)>,
		terminal: Option<moq_net::Error>,
		new_groups: Vec<(u64, u128)>,
		/// The first generation's HLS record track has ended.
		old_records_ended: bool,
	}

	/// Whether `subscriber` has ended, after draining the groups it already holds.
	fn ended(subscriber: &mut moq_net::track::Ordered) -> bool {
		loop {
			match subscriber.poll_next_group(&kio::Waiter::noop()) {
				Poll::Ready(Ok(Some(_))) => continue,
				Poll::Ready(Ok(None)) => return true,
				Poll::Pending => return false,
				Poll::Ready(Err(error)) => panic!("the record track failed: {error:?}"),
			}
		}
	}

	/// A one-track fMP4 muxer for synthesized VP8 video or Opus audio.
	fn synth(video: bool) -> moq_mux::container::fmp4::Muxer {
		use hang::catalog::{AudioCodec, AudioConfig, VideoCodec, VideoConfig};
		use moq_mux::container::fmp4::Muxer;

		if video {
			let mut config = VideoConfig::new(VideoCodec::VP8);
			config.coded_width = Some(320);
			config.coded_height = Some(240);
			config.framerate = Some(50.0);
			Muxer::video(&config).unwrap()
		} else {
			Muxer::audio(&AudioConfig::new(AudioCodec::Opus, 48_000, 2)).unwrap()
		}
	}

	/// Write `muxer`'s init segment to `init.mp4` in `dir`, and one keyframe at each of `stamps`
	/// (in microseconds) to `{prefix}{index}.m4s`.
	fn write_synth(dir: &Path, muxer: &moq_mux::container::fmp4::Muxer, prefix: &str, stamps: &[u64]) {
		std::fs::write(dir.join(format!("{prefix}init.mp4")), muxer.init().unwrap().unwrap()).unwrap();
		for (index, &micros) in stamps.iter().enumerate() {
			let frame = moq_mux::container::Frame {
				timestamp: moq_net::Timestamp::from_micros(micros).unwrap(),
				duration: Some(moq_net::Timestamp::from_micros(20_000).unwrap()),
				payload: Bytes::from_static(&[0x78, 0x00, 0x00, 0x00]),
				keyframe: true,
			};
			let fragment = muxer.fragment(index as u32, &[frame], None).unwrap();
			std::fs::write(dir.join(format!("{prefix}{index}.m4s")), fragment).unwrap();
		}
	}

	/// Exercise the playlist/map path with real fMP4 produced by the exporter's muxer.
	///
	/// The second segment, stamped `after_us`, follows a 5 s one behind the playlist `tags`.
	async fn discontinuity(video: bool, passthrough: bool, tags: &str, after_us: u64) -> Discontinuity {
		let dir = temp_dir();
		let muxer = synth(video);
		write_synth(&dir, &muxer, "", &[5_000_000, after_us]);
		std::fs::copy(dir.join("init.mp4"), dir.join("other.mp4")).unwrap();

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = CatalogProducer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let sink = Sink::new(broadcast, catalog.clone(), passthrough);
		let mut track = TrackState::new(
			if video { "video" } else { "audio" }.to_string(),
			Url::from_file_path(dir.join("media.m3u8")).unwrap(),
			if video {
				select_video_only()
			} else {
				select_audio_only()
			},
			sink,
		);
		let before = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:1,\n0.m4s\n";
		let before = m3u8_rs::parse_media_playlist_res(before.as_bytes()).unwrap();
		let local = fetcher(&track.playlist);
		let base = track.playlist.clone();
		track.consume_segments(&local, &before, &base).await.unwrap();
		let name = |snapshot: &moq_mux::catalog::hang::Catalog| {
			if video {
				snapshot.video.renditions.keys().next()
			} else {
				snapshot.audio.renditions.keys().next()
			}
			.unwrap()
			.clone()
		};
		let before_name = name(&catalog.snapshot());
		let replay = moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(3600));
		let mut old = consumer
			.track(&before_name)
			.unwrap()
			.subscribe(replay.clone())
			.await
			.unwrap()
			.ordered();
		let mut old_records = consumer
			.track(&hang::hls::default_name(&before_name))
			.unwrap()
			.subscribe(replay.clone())
			.await
			.unwrap()
			.ordered();
		let after = format!(
			"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:1,\n0.m4s\n{tags}#EXTINF:1,\n1.m4s\n"
		);
		let after = m3u8_rs::parse_media_playlist_res(after.as_bytes()).unwrap();
		let result = track.consume_segments(&local, &after, &base).await;
		let after_name = name(&catalog.snapshot());
		let (old_groups, terminal) = group_stamps(&mut old);
		let old_records_ended = ended(&mut old_records);
		let new_groups = if before_name != after_name {
			let mut new = consumer
				.track(&after_name)
				.unwrap()
				.subscribe(replay)
				.await
				.unwrap()
				.ordered();
			let (groups, error) = group_stamps(&mut new);
			assert!(error.is_none(), "new generation failed: {error:?}");
			groups
		} else {
			Vec::new()
		};
		std::fs::remove_dir_all(dir).unwrap();
		Discontinuity {
			result,
			before: before_name,
			after: after_name,
			old_groups,
			terminal,
			new_groups,
			old_records_ended,
		}
	}

	#[tokio::test]
	async fn forward_discontinuity_preserves_source_time_and_identity() {
		for video in [false, true] {
			for passthrough in [false, true] {
				let case = discontinuity(video, passthrough, DISCONTINUITY, 7_000_000).await;
				case.result.unwrap();
				assert!(case.terminal.is_none(), "{terminal:?}", terminal = case.terminal);
				assert_eq!(case.before, case.after);
				assert_eq!(
					case.old_groups,
					[(0, 5_000_000), (moq_sequence(1, 1).unwrap(), 7_000_000)]
				);
			}
		}
	}

	const DISCONTINUITY: &str = "#EXT-X-DISCONTINUITY\n";

	/// A timestamp reset under the same init segment is refused as a rewind, unwrapped or
	/// passthrough, with or without the discontinuity the source should have signalled, and
	/// without publishing anything for it: the track keeps its media and its name, and no group
	/// (not even an aborted one) opens for the refused segment.
	#[tokio::test]
	async fn timestamp_reset_is_refused() {
		for video in [false, true] {
			for passthrough in [false, true] {
				for tags in [DISCONTINUITY, ""] {
					let case = discontinuity(video, passthrough, tags, 0).await;
					let label = format!("video={video} passthrough={passthrough} tags={tags:?}");
					assert!(
						matches!(
							case.result,
							Err(Error::Rewind {
								source: moq_mux::Error::TimestampRewind(_)
									| moq_mux::Error::Cmaf(
										moq_mux::container::fmp4::Error::NonMonotonicDecodeTime { .. }
									),
								..
							})
						),
						"{label}: {:?}",
						case.result
					);
					assert_eq!(case.before, case.after, "{label}");
					assert!(case.terminal.is_none(), "{label}: {:?}", case.terminal);
					assert_eq!(case.old_groups, [(0, 5_000_000)], "{label}");
				}
			}
		}
	}

	/// A rewind ends the import at once, even while a sibling rendition is healthy: it is not
	/// retried, and the rendition is not evicted and rejoined as fresh tracks.
	#[tokio::test]
	async fn a_rewind_ends_the_import() {
		let dir = temp_dir();
		write_synth(&dir, &synth(true), "video", &[5_000_000, 7_000_000]);
		write_synth(&dir, &synth(false), "audio", &[5_000_000, 0]);
		let master = "#EXTM3U\n\
			#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"en\",DEFAULT=YES,URI=\"audio.m3u8\"\n\
			#EXT-X-STREAM-INF:BANDWIDTH=1000000,CODECS=\"avc1.4d401f,opus\",AUDIO=\"aud\"\n\
			video.m3u8\n";
		std::fs::write(dir.join("master.m3u8"), master).unwrap();
		let playlist = |prefix: &str, segments: &str| {
			let body = format!("#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"{prefix}init.mp4\"\n{segments}");
			std::fs::write(dir.join(format!("{prefix}.m3u8")), body).unwrap();
		};
		playlist("video", "#EXTINF:1,\nvideo0.m4s\n");
		playlist("audio", "#EXTINF:1,\naudio0.m4s\n");

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = CatalogProducer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let cfg = Config::new(Url::from_file_path(dir.join("master.m3u8")).unwrap());
		let mut import = Import::new(broadcast, catalog, cfg).unwrap();
		import.init().await.unwrap();

		// Video moves forward while audio restarts at zero under the same init segment.
		playlist(
			"video",
			"#EXTINF:1,\nvideo0.m4s\n#EXT-X-DISCONTINUITY\n#EXTINF:1,\nvideo1.m4s\n",
		);
		playlist(
			"audio",
			"#EXTINF:1,\naudio0.m4s\n#EXT-X-DISCONTINUITY\n#EXTINF:1,\naudio1.m4s\n",
		);
		let run = moq_clock::timeout(Duration::from_secs(5), import.run())
			.await
			.expect("a rewind is not retried");
		assert!(
			matches!(&run, Err(Error::Rewind { rendition, .. }) if rendition == "audio"),
			"{run:?}"
		);
		std::fs::remove_dir_all(dir).unwrap();
	}

	/// A different `EXT-X-MAP` resource, even with identical bytes.
	const CHANGED_MAP: &str = "#EXT-X-DISCONTINUITY\n#EXT-X-MAP:URI=\"other.mp4\"\n";

	#[tokio::test]
	async fn changed_map_discontinuity_publishes_a_fresh_identity() {
		for video in [false, true] {
			for passthrough in [false, true] {
				for after_us in [0, 7_000_000] {
					let case = discontinuity(video, passthrough, CHANGED_MAP, after_us).await;
					case.result.unwrap();
					assert!(case.terminal.is_none(), "{terminal:?}", terminal = case.terminal);
					assert_ne!(case.before, case.after);
					assert!(case.old_records_ended, "the replaced generation retires its records");
					assert_eq!(case.old_groups, [(0, 5_000_000)]);
					assert_eq!(case.new_groups, [(moq_sequence(1, 1).unwrap(), u128::from(after_us))]);
				}
			}
		}
	}

	/// Every frame timestamp `name` carries so far, in microseconds.
	async fn stamps(consumer: &moq_net::broadcast::Consumer, name: &str) -> Vec<u128> {
		let replay = moq_net::track::Subscription::default().with_max_delay(std::time::Duration::from_secs(3600));
		let mut track = consumer.track(name).unwrap().subscribe(replay).await.unwrap();
		let wait = std::time::Duration::from_millis(100);
		let mut out = Vec::new();
		while let Ok(Ok(Some(mut group))) = moq_clock::timeout(wait, track.recv_group()).await {
			while let Ok(Ok(Some(frame))) = moq_clock::timeout(wait, group.read_frame()).await {
				out.push(frame.timestamp.unwrap().as_micros());
			}
		}
		out
	}

	/// Every frame timestamp a catalog's renditions carry so far, in microseconds.
	async fn published(consumer: &moq_net::broadcast::Consumer, catalog: &CatalogProducer) -> Vec<Vec<u128>> {
		let snapshot = catalog.snapshot();
		let mut out = Vec::new();
		for name in snapshot.video.renditions.keys().chain(snapshot.audio.renditions.keys()) {
			out.push(stamps(consumer, name).await);
		}
		out
	}

	/// The same fragment imported alone, which keeps its timestamps verbatim.
	async fn alone(init: &[u8], fragment: &[u8]) -> Vec<Vec<u128>> {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = CatalogProducer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let mut import = Fmp4::new(broadcast, catalog.reserve()).with_select(select_muxed());
		import.decode(init).unwrap();
		import.decode(fragment).unwrap();
		published(&consumer, &catalog).await
	}

	/// The import's timebase never withholds the catalog, and an importer replacing a rendition on a
	/// new `EXT-X-MAP` after that catalog is out shifts by the same offset as the first one: none,
	/// since the first placed the clock. A fresh offset would shift it by its own arrival.
	#[tokio::test]
	async fn a_replacement_importer_keeps_the_offset() {
		let (init, fragments) = fmp4_parts(2);
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let consumer = broadcast.consume();
		let catalog = CatalogProducer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let mut catalogs = catalog.consume().unwrap();
		let sink = Sink::new(broadcast, catalog.clone(), false);

		let mut first = sink.importer(&select_muxed());
		first.decode(&init).unwrap();
		first.decode(&fragments[0]).unwrap();
		assert!(
			matches!(catalogs.poll_next(&kio::Waiter::noop()), Poll::Ready(Ok(Some(_)))),
			"the initial catalog is out while the timebase lives"
		);
		let before = published(&consumer, &catalog).await;

		// Let the clock run on, so a fresh offset could not land back on the source's timestamps.
		moq_clock::sleep(std::time::Duration::from_millis(50)).await;
		let mut second = sink.importer(&select_muxed());
		second.decode(&init).unwrap();
		drop(first);
		second.decode(&fragments[1]).unwrap();
		let after = published(&consumer, &catalog).await;

		assert_eq!(
			before,
			alone(&init, &fragments[0]).await,
			"the first importer stays verbatim"
		);
		assert_eq!(
			after,
			alone(&init, &fragments[1]).await,
			"the replacement keeps that offset"
		);
		assert!(after.iter().any(|stamps| !stamps.is_empty()), "{after:?}");
	}

	/// Resolve `ensure_tracks` against a master playlist written to a temp file.
	async fn discover(master_body: &str) -> Import {
		let dir = temp_dir();
		let path = dir.join("master.m3u8");
		std::fs::write(&path, master_body).unwrap();

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = CatalogProducer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let cfg = Config::new(Url::from_file_path(&path).unwrap());
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

	#[tokio::test]
	#[tracing_test::traced_test]
	async fn discovery_logs_redact_playlist_credentials() {
		let audio = signed(&Url::parse("https://example.com/audio.m3u8").unwrap());
		let master = format!(
			"#EXTM3U\n\
			#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"en\",URI=\"{audio}\"\n\
			#EXT-X-STREAM-INF:BANDWIDTH=1000000,CODECS=\"avc1.4d401f,mp4a.40.2\",AUDIO=\"aud\"\n\
			video.m3u8?jwt=s3cret#f4ag\n"
		);
		let hls = discover(&master).await;
		assert_eq!(hls.audio.as_ref().unwrap().playlist, audio);

		assert!(logs_contain("selected master playlist renditions"));
		assert!(logs_contain("audio=\"https://example.com/audio.m3u8\""));
		for credential in ["alice", "hunter2", "jwt", "s3cret", "f4ag"] {
			assert!(!logs_contain(credential), "log leaked {credential}");
		}
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

	/// The catalog consumer jumps to the newest snapshot, so this looks while the second
	/// rendition's playlist is still withheld: on a pass that publishes early, that read is
	/// already the first catalog, and it must list every rendition.
	#[tokio::test]
	async fn first_catalog_lists_every_rendition() {
		let (init, fragments) = fmp4_parts(2);
		let mut media = init.clone();
		media.extend_from_slice(&fragments[0]);
		media.extend_from_slice(&fragments[1]);
		let playlist = format!(
			"#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"media.mp4\",BYTERANGE=\"{}@0\"\n#EXTINF:1,\n#EXT-X-BYTERANGE:{}@{}\nmedia.mp4\n#EXTINF:1,\n#EXT-X-BYTERANGE:{}\nmedia.mp4\n",
			init.len(),
			fragments[0].len(),
			init.len(),
			fragments[1].len()
		);
		let master = "\
#EXTM3U
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"en\",DEFAULT=YES,URI=\"audio.m3u8\"
#EXT-X-STREAM-INF:BANDWIDTH=1000000,CODECS=\"avc1.4d401f,mp4a.40.2\",AUDIO=\"aud\"
video.m3u8
";

		let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
		let address = listener.local_addr().unwrap();
		let arrived = Arc::new(Notify::new());
		let release = Arc::new(Notify::new());
		let server = serve_gated(
			listener,
			vec![
				("/master.m3u8".to_string(), master.as_bytes().to_vec()),
				("/video.m3u8".to_string(), playlist.as_bytes().to_vec()),
				("/audio.m3u8".to_string(), playlist.into_bytes()),
				("/media.mp4".to_string(), media),
			],
			"/audio.m3u8".to_string(),
			arrived.clone(),
			release.clone(),
		);

		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = CatalogProducer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let mut consumer = catalog.consume().unwrap();
		let cfg = Config::new(Url::parse(&format!("http://{address}/master.m3u8")).unwrap());
		let mut import = Import::new(broadcast, catalog, cfg).unwrap();

		// Keep the import alive across the read: dropping it tears the catalog track down,
		// and the consumer would see that instead of the snapshot.
		let importing = tokio::spawn(async move {
			let result = import.init().await;
			(result, import)
		});

		// Video has finished ingesting: its playlist, init, and segments all precede this fetch.
		arrived.notified().await;
		let published_early = match consumer.poll_next(&kio::Waiter::noop()) {
			Poll::Pending => false,
			Poll::Ready(Ok(Some(snapshot))) => {
				assert!(
					!snapshot.video.renditions.is_empty() && !snapshot.audio.renditions.is_empty(),
					"first catalog listed video {} and audio {}",
					snapshot.video.renditions.len(),
					snapshot.audio.renditions.len()
				);
				true
			}
			other => panic!("catalog consumer failed before the second rendition: {other:?}"),
		};

		release.notify_one();
		let (result, _import) = importing.await.unwrap();
		result.unwrap();

		if !published_early {
			let snapshot = match consumer.poll_next(&kio::Waiter::noop()) {
				Poll::Ready(Ok(Some(snapshot))) => snapshot,
				other => panic!("expected the first catalog, got {other:?}"),
			};
			assert!(!snapshot.video.renditions.is_empty(), "video rendition missing");
			assert!(!snapshot.audio.renditions.is_empty(), "audio rendition missing");
		}
		server.abort();
	}

	/// Answer `routes` over HTTP/1.1. `gate` is held until `release`, which is how the test
	/// observes the catalog after one rendition and before the next.
	fn serve_gated(
		listener: TcpListener,
		routes: Vec<(String, Vec<u8>)>,
		gate: String,
		arrived: Arc<Notify>,
		release: Arc<Notify>,
	) -> tokio::task::JoinHandle<()> {
		tokio::spawn(async move {
			loop {
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
				if request.is_empty() {
					continue;
				}

				let path = request_path(&request).unwrap_or("");
				if path == gate {
					arrived.notify_one();
					release.notified().await;
				}
				let body = routes
					.iter()
					.find(|(name, _)| name == path)
					.map(|(_, body)| body.as_slice())
					.unwrap_or_else(|| panic!("unexpected request for {path}: {}", String::from_utf8_lossy(&request)));
				let response = match request_range(&request) {
					Some((start, end)) => http_response(
						"206 Partial Content",
						&[("Content-Range", &format!("bytes {start}-{end}/{}", body.len()))],
						&body[start..=end],
					),
					None => http_response("200 OK", &[], body),
				};
				stream.write_all(&response).await.unwrap();
			}
		})
	}

	/// The inclusive `bytes=start-end` a request asked for, if any.
	fn request_range(request: &[u8]) -> Option<(usize, usize)> {
		let request = std::str::from_utf8(request).ok()?;
		let range = request.lines().find_map(|line| {
			let (name, value) = line.split_once(':')?;
			name.eq_ignore_ascii_case("range").then(|| value.trim())
		})?;
		let (start, end) = range.strip_prefix("bytes=")?.split_once('-')?;
		Some((start.parse().ok()?, end.parse().ok()?))
	}

	fn request_path(request: &[u8]) -> Option<&str> {
		let request = std::str::from_utf8(request).ok()?;
		let target = request.lines().next()?.split_whitespace().nth(1)?;
		Some(target.split('?').next().unwrap_or(target))
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
