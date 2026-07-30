//! Export: serve a MoQ broadcast as HLS, fetching media on demand.
//!
//! A [`Broadcaster`] subscribes to one broadcast's catalog and its single timeline track (see
//! [`hang::timeline`]). That is the *only* standing traffic: playlists are a pure function of
//! the timeline records (each names a complete aligned segment with per-track group ranges),
//! and media bytes move only when an HTTP client requests a segment, which FETCHes exactly
//! the groups that segment covers from the relay cache and transmuxes them to CMAF. A
//! broadcast whose catalog advertises no timeline can't be served this way and is skipped.
//!
//! The same machinery serves two kinds of consumer:
//!
//! * the HTTP serve path (pull): [`Broadcaster::rendition`] / [`Broadcaster::master_playlist`]
//!   and the crate-internal `Rendition::playlist` / `Rendition::segment`, rendered/fetched per
//!   request (that pull surface is gated behind the `server` feature); and
//! * a recorder (push): the [`renditions::Consumer`] and [`segments::Consumer`] cursors, which
//!   yield every rendition and every finalized segment in order, for mirroring a broadcast to
//!   storage.

mod master;
mod playlist;
mod rendition;

pub mod renditions;
pub mod segments;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use moq_mux::catalog::{self, CatalogFormat, Stream};

pub(crate) use playlist::render_media;
pub use rendition::{Kind, Rendition};

/// How long to wait before retrying the initial catalog subscription.
const CATALOG_RETRY: Duration = Duration::from_millis(250);

/// Export tuning shared across renditions.
///
/// Construct via [`Config::default`] and set the fields you need, so new options
/// stay additive.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// Minimum duration of media listed in each rendition's playlist window. Older timeline
	/// records are evicted once the remaining segments still cover this span; keep it within
	/// the relay's group-cache retention, since segments are fetched from there on request.
	pub window: Duration,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			window: Duration::from_secs(16),
		}
	}
}

/// All renditions of one broadcast, kept in sync with its catalog.
pub struct Broadcaster {
	// Read only by the serve path's `is_closed`/`closed` (server-gated); a recording consumer
	// observes close through its `renditions()` cursor ending instead.
	#[cfg_attr(not(feature = "server"), allow(dead_code))]
	broadcast: moq_net::broadcast::Consumer,
	/// The current rendition set, reconciled from the catalog by the watcher task.
	renditions: renditions::Producer,
	/// Aborts the catalog watcher when the broadcaster is dropped; `Drop` then retires the
	/// renditions themselves (see [`renditions::Producer::clear`]). Set once, right after
	/// construction.
	watcher: Mutex<Option<tokio::task::JoinHandle<()>>>,
	/// The broadcast's timeline watcher, spawned by the catalog watcher once the catalog
	/// advertises the timeline track. On drop it is aborted only when no cursor still needs
	/// its records (see `Drop`).
	timeline_watcher: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl Broadcaster {
	/// Resolve `source`'s catalog broadcast and start tracking its renditions.
	pub async fn new(source: moq_mux::Source, config: Config) -> crate::Result<Arc<Self>> {
		let broadcast = source.broadcast().await?;
		let renditions = renditions::Producer::new(config.window);
		let timeline_watcher = Arc::new(Mutex::new(None));
		let broadcaster = Arc::new(Self {
			broadcast: broadcast.clone(),
			renditions: renditions.clone(),
			watcher: Mutex::new(None),
			timeline_watcher: timeline_watcher.clone(),
		});
		// The watcher owns its own producer clone; the `Broadcaster`'s `Drop` aborts it so the
		// standing catalog subscription stops when nobody's serving from this broadcaster.
		let watcher = tokio::spawn(watch_catalog(source, broadcast, renditions, timeline_watcher));
		*broadcaster.watcher.lock().unwrap() = Some(watcher);
		Ok(broadcaster)
	}

	/// Whether the source broadcast has closed (ended or dropped).
	#[cfg(feature = "server")]
	pub(crate) fn is_closed(&self) -> bool {
		self.broadcast.is_closed()
	}

	/// Resolve once the source broadcast closes, so the server can evict a dead broadcaster.
	#[cfg(feature = "server")]
	pub(crate) async fn closed(&self) {
		self.broadcast.closed().await;
	}

	/// Look up a rendition by kind and name. Video and audio are separate axes, so a
	/// video and an audio rendition may share a name without colliding.
	pub fn rendition(&self, kind: Kind, name: &str) -> Option<Arc<Rendition>> {
		self.renditions.get(kind, name)
	}

	/// A cursor over rendition add/remove events, for a consumer that mirrors or records the
	/// whole broadcast. It replays the current renditions, then yields changes, and returns
	/// `None` once the source closes.
	pub fn renditions(&self) -> renditions::Consumer {
		self.renditions.subscribe()
	}

	/// Resolve once at least one rendition has been discovered. How long to wait is the
	/// caller's policy: wrap this in a timeout (or select against it) as needed.
	pub async fn ready(&self) {
		self.renditions.ready().await;
	}

	/// Render the multivariant (master) playlist from the current renditions.
	///
	/// `query` is an optional query string (without the leading `?`, e.g. `jwt=<token>`)
	/// appended to every child media-playlist URL, so a credential the master was fetched
	/// with propagates to the rendition playlists a stock player loads next. It is an
	/// argument rather than a [`Config`] field because one broadcaster fans out to viewers
	/// holding different tokens; a credential in `Config` would embed one viewer's in
	/// another's playlist.
	pub fn master_playlist(&self, query: Option<&str>) -> String {
		let mut video = Vec::new();
		let mut audio = Vec::new();
		for rendition in self.renditions.snapshot() {
			match rendition.kind {
				Kind::Video => video.push(master::VideoVariant {
					name: rendition.name.clone(),
					bandwidth: rendition.bandwidth,
					width: rendition.width,
					height: rendition.height,
					codec: rendition.codec.clone(),
				}),
				Kind::Audio => audio.push(master::AudioVariant {
					name: rendition.name.clone(),
					bandwidth: rendition.bandwidth,
					codec: rendition.codec.clone(),
				}),
			}
		}
		master::render_master(&video, &audio, query)
	}

	/// Whether the current catalog contains no servable renditions (serve path).
	#[cfg_attr(not(feature = "server"), allow(dead_code))]
	pub(crate) fn is_empty(&self) -> bool {
		self.renditions.is_empty()
	}
}

impl Drop for Broadcaster {
	fn drop(&mut self) {
		if let Some(watcher) = self.watcher.lock().unwrap().take() {
			watcher.abort();
		}
		// A recording cursor (holding an `Arc<Rendition>`) must keep receiving timeline
		// records after this broadcaster is gone, or its final segments would be truncated;
		// the timeline watcher then ends on its own when the timeline (or broadcast) does.
		// With no such holder, abort it now so teardown can't leak a standing subscription.
		let held = self.renditions.any_held_externally();
		// Release the map either way: a surviving `renditions::Consumer` would otherwise pin
		// every `Rendition` forever.
		self.renditions.clear();
		if !held && let Some(watcher) = self.timeline_watcher.lock().unwrap().take() {
			watcher.abort();
		}
	}
}

async fn watch_catalog(
	source: moq_mux::Source,
	broadcast: moq_net::broadcast::Consumer,
	renditions: renditions::Producer,
	timeline_watcher: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
) {
	let mut consumer = loop {
		match catalog::Consumer::<()>::new(&broadcast, CatalogFormat::Hang).await {
			Ok(consumer) => break consumer,
			Err(err) => {
				tracing::warn!(%err, "failed to subscribe to broadcast catalog, retrying");
				tokio::select! {
					_ = tokio::time::sleep(CATALOG_RETRY) => {}
					_ = kio::wait(|waiter| broadcast.poll_closed(waiter)) => {
						renditions.close();
						return;
					}
				}
			}
		}
	};

	let mut timeline_started = false;
	loop {
		match kio::wait(|waiter| consumer.poll_next(waiter)).await {
			Ok(Some(catalog)) => {
				renditions.sync(&source, &catalog);
				// The broadcast has one timeline; subscribe once it's advertised and fan its
				// records out to every rendition.
				if !timeline_started && let Some(section) = catalog.timeline.clone() {
					timeline_started = true;
					let watcher = tokio::spawn(watch_timeline(broadcast.clone(), section, renditions.fanout()));
					*timeline_watcher.lock().unwrap() = Some(watcher);
				}
			}
			Ok(None) => break,
			Err(err) => {
				tracing::warn!(%err, "broadcast catalog stream ended with error");
				break;
			}
		}
	}

	// The source is done (or errored): let recording cursors finish.
	renditions.close();
}

/// The broadcast's timeline watcher: read the single timeline track and fan each record out
/// to every rendition's window.
async fn watch_timeline(
	broadcast: moq_net::broadcast::Consumer,
	section: hang::catalog::Timeline,
	renditions: renditions::Fanout,
) {
	match watch(&broadcast, &section, &renditions).await {
		// The timeline finished cleanly: the publisher is done, so every window can end
		// (ENDLIST) and recording cursors drain to completion.
		Ok(()) => renditions.end_windows(),
		// A transient error (subscription reset, relay hiccup): don't mark the windows ended;
		// the serve path keeps serving the frozen windows.
		Err(err) => {
			tracing::warn!(track = %section.track, %err, "timeline watcher error; leaving the playlists live")
		}
	}
	// The timeline stream is over either way: close so recording cursors terminate instead of
	// parking forever (the serve path still reads the last windows).
	renditions.close_windows();
}

async fn watch(
	broadcast: &moq_net::broadcast::Consumer,
	section: &hang::catalog::Timeline,
	renditions: &renditions::Fanout,
) -> crate::Result<()> {
	let mut timeline = moq_mux::timeline::Consumer::<()>::subscribe(broadcast, section).await?;
	while let Some(entry) = timeline.next().await? {
		renditions.push(entry);
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	fn frame(micros: u64, keyframe: bool) -> moq_mux::container::Frame {
		moq_mux::container::Frame {
			timestamp: moq_net::Timestamp::from_micros(micros).unwrap(),
			payload: bytes::Bytes::from_static(&[0xDE, 0xAD, 0xBE, 0xEF]),
			keyframe,
			duration: None,
		}
	}

	// Let the origin's spawned attach task run so a created broadcast is routable.
	async fn settle() {
		for _ in 0..10 {
			tokio::task::yield_now().await;
		}
	}

	// The whole fetch-on-demand path in process: a broadcast publishes media through the
	// catalog (which records the timeline), the Broadcaster renders playlists from the
	// timeline alone, and a segment request fetches and transmuxes exactly its groups.
	#[tokio::test]
	async fn serves_playlist_and_segments_from_the_timeline() {
		let origin = moq_net::Origin::random().produce();
		let mut broadcast = origin
			.create_broadcast("live", moq_net::broadcast::Route::new().with_announce(true))
			.expect("publish allowed");
		settle().await;
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();

		let reserved = catalog.reserve();
		let mut registration = reserved.video("video0");
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		config.framerate = Some(30.0);
		registration.set(config);
		drop(reserved);

		// Three GOPs, 2s apart: groups 0 and 1 are complete, group 2 is the live edge.
		let track = broadcast.create_track("video0", None).unwrap();
		let mut media = catalog
			.media_producer(track, moq_mux::catalog::hang::Container::Legacy)
			.unwrap();
		media.write(frame(0, true)).unwrap();
		media.write(frame(1_000_000, false)).unwrap();
		media.write(frame(2_000_000, true)).unwrap();
		media.write(frame(3_000_000, false)).unwrap();
		media.write(frame(4_000_000, true)).unwrap();

		let source = moq_mux::Source::new(origin.consume(), "live");
		let broadcaster = Broadcaster::new(source, Config::default()).await.unwrap();
		let _ = tokio::time::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let rendition = broadcaster
			.rendition(Kind::Video, "video0")
			.expect("rendition discovered from the catalog");
		let _ = tokio::time::timeout(Duration::from_secs(5), rendition.playable()).await;

		let master = broadcaster.master_playlist(None);
		assert!(master.contains("video/video0/media.m3u8"), "master lists the rendition");

		let playlist = rendition.playlist();
		assert_eq!(playlist.segments.len(), 2, "the live-edge group is not listed");
		assert_eq!(playlist.segments[0].segment, 0);
		assert_eq!(playlist.segments[0].duration, 2.0);
		assert_eq!(playlist.segments[1].segment, 1);
		assert_eq!(
			playlist.target_duration, 2,
			"the observed segment duration, in whole seconds"
		);
		assert!(!playlist.finished);

		let rendered = rendition.media_playlist(None).expect("playable");
		assert!(rendered.contains("#EXT-X-MAP:URI=\"init.mp4\"\n"));
		assert!(rendered.contains("seg/0.m4s\n"));
		assert!(rendered.contains("seg/1.m4s\n"));

		// The same render, but carrying a credential into every child URL.
		let signed = rendition.media_playlist(Some("jwt=abc.def")).expect("playable");
		assert!(signed.contains("#EXT-X-MAP:URI=\"init.mp4?jwt=abc.def\"\n"));
		assert!(signed.contains("seg/0.m4s?jwt=abc.def\n"));

		let init = rendition.init().await.unwrap().expect("init segment");
		assert_eq!(&init[4..8], b"ftyp");

		let segment = rendition.segment(0).await.unwrap().expect("segment fetched on demand");
		assert_eq!(&segment[4..8], b"moof", "a fetched group transmuxes to moof+mdat");

		// The live-edge group isn't a segment yet, and unknown groups miss.
		assert!(rendition.segment(2).await.unwrap().is_none());
		assert!(rendition.segment(99).await.unwrap().is_none());

		// Keep the publisher alive for the whole test.
		drop((media, registration, broadcast));
	}

	// The property HLS needs from the timeline rework: audio and video renditions share one
	// segment numbering, cut at the same boundaries. Video is one group per segment; an audio
	// segment packs every audio group inside the video segment's span.
	// Most publishers declare no durationMax (a real-time GOP can be minutes long), so the
	// exporter has to derive EXT-X-TARGETDURATION from the segments it has: a playlist whose
	// EXTINF exceeds its target duration is invalid.
	#[tokio::test]
	async fn target_duration_covers_the_window_without_a_declared_bound() {
		let origin = moq_net::Origin::random().produce();
		let mut broadcast = origin
			.create_broadcast("live", moq_net::broadcast::Route::new().with_announce(true))
			.expect("publish allowed");
		settle().await;
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();

		let reserved = catalog.reserve();
		let mut registration = reserved.video("video0");
		registration.set(hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8));
		drop(reserved);

		// 3s GOPs against the default 1s minimum: every segment is one whole GOP.
		let track = broadcast.create_track("video0", None).unwrap();
		let mut media = catalog
			.media_producer(track, moq_mux::catalog::hang::Container::Legacy)
			.unwrap();
		media.write(frame(0, true)).unwrap();
		media.write(frame(3_000_000, true)).unwrap();
		media.write(frame(6_000_000, true)).unwrap();

		let source = moq_mux::Source::new(origin.consume(), "live");
		let broadcaster = Broadcaster::new(source, Config::default()).await.unwrap();
		let _ = tokio::time::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let rendition = broadcaster.rendition(Kind::Video, "video0").expect("rendition");
		let _ = tokio::time::timeout(Duration::from_secs(5), rendition.playable()).await;

		let playlist = rendition.playlist();
		assert_eq!(playlist.segments[0].duration, 3.0);
		assert_eq!(
			playlist.target_duration, 3,
			"no bound was declared, so the target duration must still cover the 3s segment"
		);
	}

	#[tokio::test]
	async fn audio_and_video_segments_are_aligned() {
		let origin = moq_net::Origin::random().produce();
		let mut broadcast = origin
			.create_broadcast("live", moq_net::broadcast::Route::new().with_announce(true))
			.expect("publish allowed");
		settle().await;
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();

		let reserved = catalog.reserve();
		let mut video_registration = reserved.video("video0");
		let mut video_config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		video_config.framerate = Some(30.0);
		video_registration.set(video_config);
		let mut audio_registration = reserved.audio("audio0");
		let audio_config = hang::catalog::AudioConfig::new(hang::catalog::AudioCodec::Opus, 48_000, 2);
		audio_registration.set(audio_config);
		drop(reserved);

		let video_track = broadcast.create_track("video0", None).unwrap();
		let mut video = catalog
			.media_producer(video_track, moq_mux::catalog::hang::Container::Legacy)
			.unwrap();
		let audio_track = broadcast.create_track("audio0", None).unwrap();
		let mut audio = catalog
			.media_producer(audio_track, moq_mux::catalog::hang::Container::Legacy)
			.unwrap();

		// Interleaved by pts, as a muxer would demux them: video keyframes every 2s open the
		// segments; audio publishes a 500ms group each cut.
		let mut audio_write = |micros: u64| {
			let keyframe = audio.needs_keyframe();
			audio.write(frame(micros, keyframe)).unwrap();
			audio.cut(None).unwrap();
		};
		video.write(frame(0, true)).unwrap(); // segment 0
		for micros in [0u64, 500_000, 1_000_000, 1_500_000] {
			audio_write(micros); // audio groups 0..=3 pack into segment 0
		}
		video.write(frame(2_000_000, true)).unwrap(); // segment 1
		for micros in [2_000_000u64, 2_500_000, 3_000_000, 3_500_000] {
			audio_write(micros); // audio groups 4..=7 pack into segment 1
		}
		video.write(frame(4_000_000, true)).unwrap(); // segment 2 (live edge)
		audio_write(4_000_000); // audio group 8 opens its slice of segment 2

		let source = moq_mux::Source::new(origin.consume(), "live");
		let broadcaster = Broadcaster::new(source, Config::default()).await.unwrap();
		let _ = tokio::time::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let video_rendition = broadcaster.rendition(Kind::Video, "video0").expect("video discovered");
		let audio_rendition = broadcaster.rendition(Kind::Audio, "audio0").expect("audio discovered");
		let _ = tokio::time::timeout(Duration::from_secs(5), video_rendition.playable()).await;
		let _ = tokio::time::timeout(Duration::from_secs(5), audio_rendition.playable()).await;

		// Both playlists list the same segment numbers over the same spans.
		let video_playlist = video_rendition.playlist();
		let audio_playlist = audio_rendition.playlist();
		assert_eq!(video_playlist.media_sequence, audio_playlist.media_sequence);
		let video_segments: Vec<u64> = video_playlist.segments.iter().map(|s| s.segment).collect();
		let audio_segments: Vec<u64> = audio_playlist.segments.iter().map(|s| s.segment).collect();
		assert_eq!(video_segments, vec![0, 1]);
		assert_eq!(audio_segments, vec![0, 1], "audio lists the same aligned segments");
		assert_eq!(audio_playlist.segments[0].duration, 2.0, "cut at the video boundary");

		// The same URI names the same span of content time on either rendition.
		let rendered = audio_rendition.media_playlist(None).expect("playable");
		assert!(rendered.contains("seg/0.m4s\n"));
		assert!(rendered.contains("seg/1.m4s\n"));

		// A video segment is one group; the matching audio segment transmuxes its four groups.
		let video_segment = video_rendition.segment(1).await.unwrap().expect("video segment");
		assert_eq!(&video_segment[4..8], b"moof");
		let audio_segment = audio_rendition.segment(1).await.unwrap().expect("audio segment");
		assert_eq!(&audio_segment[4..8], b"moof");
		assert!(
			audio_segment.len() > video_segment.len(),
			"the audio segment packs multiple groups into one fragment"
		);

		drop((video, audio, video_registration, audio_registration, broadcast));
	}

	// Dropping the broadcaster must not truncate a recording in progress. A cursor holds its
	// rendition alive, so the rendition's own watcher still ends the timeline with `end()` --
	// which is what promotes the live-edge record into the final segment. Force-closing the
	// rendition here instead would race that (`end()` no-ops on a closed channel) and silently
	// drop the last segment of every recording.
	#[tokio::test]
	async fn dropping_the_broadcaster_keeps_a_cursor_drainable() {
		let origin = moq_net::Origin::random().produce();
		let mut broadcast = origin
			.create_broadcast("live", moq_net::broadcast::Route::new().with_announce(true))
			.expect("publish allowed");
		settle().await;
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();

		let reserved = catalog.reserve();
		let mut registration = reserved.video("video0");
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		config.framerate = Some(30.0);
		registration.set(config);
		drop(reserved);

		// Two GOPs: group 0 is complete, while group 1 stays at the live edge until the
		// publisher finishes.
		let track = broadcast.create_track("video0", None).unwrap();
		let mut media = catalog
			.media_producer(track, moq_mux::catalog::hang::Container::Legacy)
			.unwrap();
		media.write(frame(0, true)).unwrap();
		media.write(frame(2_000_000, true)).unwrap();

		let source = moq_mux::Source::new(origin.consume(), "live");
		let broadcaster = Broadcaster::new(source, Config::default()).await.unwrap();
		let _ = tokio::time::timeout(Duration::from_secs(5), broadcaster.ready()).await;
		let rendition = broadcaster
			.rendition(Kind::Video, "video0")
			.expect("rendition discovered");
		let mut segments = rendition.segments();

		// The recorder drops the broadcaster before finalizing its uploads.
		drop(broadcaster);

		// Finish the source after teardown, forcing the broadcaster's drop to land before the
		// rendition watcher's clean-end path.
		media.finish().unwrap();
		catalog.finish().unwrap();

		let mut groups = Vec::new();
		while let Some(segment) = tokio::time::timeout(Duration::from_secs(5), segments.next())
			.await
			.expect("the cursor drains without parking")
			.unwrap()
		{
			groups.push(segment.segment);
		}
		assert!(
			groups.contains(&1),
			"the final segment survives dropping the broadcaster, got {groups:?}"
		);

		drop((catalog, media, registration, broadcast, rendition));
	}

	// A rendition the catalog drops must end its segment cursors. The cursor holds an
	// `Arc<Rendition>`, so without an explicit close the rendition (and its timeline
	// subscription) would stay alive and the cursor would park at the live edge forever.
	#[tokio::test]
	async fn removing_a_rendition_ends_its_segment_cursor() {
		let origin = moq_net::Origin::random().produce();
		let broadcast = origin
			.create_broadcast("live", moq_net::broadcast::Route::new().with_announce(true))
			.expect("publish allowed");
		let source = moq_mux::Source::new(origin.consume(), "live");

		// Drive the producer directly so the catalog can be reconciled synchronously.
		let renditions = renditions::Producer::new(Config::default().window);
		let media = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		let mut catalog = moq_mux::catalog::hang::Catalog::default();
		catalog.video.renditions.insert("video".to_string(), media);
		catalog.timeline = Some(hang::catalog::Timeline::new(hang::timeline::DEFAULT_NAME));
		renditions.sync(&source, &catalog);

		let rendition = renditions.get(Kind::Video, "video").expect("rendition synced");
		let mut segments = rendition.segments();

		// The catalog drops the rendition: its cursor must run dry rather than park.
		let empty = moq_mux::catalog::hang::Catalog {
			timeline: Some(hang::catalog::Timeline::new(hang::timeline::DEFAULT_NAME)),
			..Default::default()
		};
		renditions.sync(&source, &empty);
		let ended = tokio::time::timeout(Duration::from_secs(5), segments.next())
			.await
			.expect("a removed rendition's cursor ends instead of parking")
			.unwrap();
		assert!(ended.is_none(), "no segments remained to drain");

		drop((broadcast, rendition));
	}

	// Dropping the broadcaster must release its renditions even when a cursor (and an
	// `Arc<Rendition>` it was handed) outlives it. The map lives in shared state, so without an
	// explicit teardown those would pin every rendition's timeline watcher -- and the standing
	// source subscription it holds -- for as long as the consumer lived.
	#[tokio::test]
	async fn dropping_the_broadcaster_releases_its_renditions() {
		let origin = moq_net::Origin::random().produce();
		let mut broadcast = origin
			.create_broadcast("live", moq_net::broadcast::Route::new().with_announce(true))
			.expect("publish allowed");
		settle().await;
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();

		let reserved = catalog.reserve();
		let mut registration = reserved.video("video0");
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		config.framerate = Some(30.0);
		registration.set(config);
		drop(reserved);

		let track = broadcast.create_track("video0", None).unwrap();
		let mut media = catalog
			.media_producer(track, moq_mux::catalog::hang::Container::Legacy)
			.unwrap();
		media.write(frame(0, true)).unwrap();
		media.write(frame(2_000_000, true)).unwrap();

		let source = moq_mux::Source::new(origin.consume(), "live");
		let broadcaster = Broadcaster::new(source, Config::default()).await.unwrap();

		let mut renditions = broadcaster.renditions();
		let added = tokio::time::timeout(Duration::from_secs(5), renditions.next())
			.await
			.expect("a rendition is discovered");
		let Some(renditions::Event::Added(_rendition)) = added else {
			panic!("expected an Added event");
		};

		// The cursor and the handed-out rendition both outlive the broadcaster.
		drop(broadcaster);

		let next = tokio::time::timeout(Duration::from_secs(5), renditions.next())
			.await
			.expect("the cursor resolves after the broadcaster drops");
		assert!(
			matches!(next, Some(renditions::Event::Removed { .. })),
			"dropping the broadcaster releases its renditions to a live cursor"
		);

		// ...and the cursor then reaches a terminal state rather than parking. Releasing the
		// renditions is only safe if consumers actually finish.
		let ended = tokio::time::timeout(Duration::from_secs(5), renditions.next())
			.await
			.expect("the cursor terminates after the broadcaster drops");
		assert!(ended.is_none(), "the cursor ends once the broadcaster is gone");

		drop((catalog, media, registration, broadcast));
	}

	// The record path: the renditions cursor yields the video rendition, its segment cursor
	// yields each finalized segment WITH its media, and closing the publisher drains the final
	// segment and then ends both cursors.
	#[tokio::test]
	async fn record_cursors_yield_renditions_and_segments() {
		let origin = moq_net::Origin::random().produce();
		let mut broadcast = origin
			.create_broadcast("live", moq_net::broadcast::Route::new().with_announce(true))
			.expect("publish allowed");
		settle().await;
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();

		let reserved = catalog.reserve();
		let mut registration = reserved.video("video0");
		let mut config = hang::catalog::VideoConfig::new(hang::catalog::VideoCodec::VP8);
		config.framerate = Some(30.0);
		registration.set(config);
		drop(reserved);

		// Groups 0 and 1 are complete; group 2 is the live edge until the publisher drops.
		let track = broadcast.create_track("video0", None).unwrap();
		let mut media = catalog
			.media_producer(track, moq_mux::catalog::hang::Container::Legacy)
			.unwrap();
		media.write(frame(0, true)).unwrap();
		media.write(frame(2_000_000, true)).unwrap();
		media.write(frame(4_000_000, true)).unwrap();

		let source = moq_mux::Source::new(origin.consume(), "live");
		let broadcaster = Broadcaster::new(source, Config::default()).await.unwrap();

		let mut renditions = broadcaster.renditions();
		let rendition = match tokio::time::timeout(Duration::from_secs(5), renditions.next())
			.await
			.expect("a rendition is discovered")
		{
			Some(renditions::Event::Added(rendition)) => rendition,
			other => panic!("expected an Added event, got {:?}", other.is_some()),
		};
		assert_eq!(rendition.kind, Kind::Video);
		assert_eq!(rendition.name, "video0");

		let mut segments = rendition.segments();
		assert!(segments.init().await.unwrap().is_some(), "init is buildable");

		let first = tokio::time::timeout(Duration::from_secs(5), segments.next())
			.await
			.expect("first segment finalizes")
			.unwrap()
			.expect("a segment, not end");
		assert_eq!(first.segment, 0);
		assert_eq!(&first.media[4..8], b"moof", "the segment carries its transmuxed media");
		assert_eq!(first.duration, 2.0);
		assert!(!first.discontinuity, "a clean start is not a discontinuity");

		let second = segments.next().await.unwrap().expect("second segment");
		assert_eq!(second.segment, 1);
		assert!(!second.discontinuity, "consecutive segments are continuous");

		// Tear down the publisher mid-group. The track ends abruptly (the cursor drains the
		// segments it already saw and ends; the still-open live-edge group is NOT finalized,
		// since a reset can't vouch that its media is complete), while finishing the broadcast
		// ends it promptly instead of lingering for a reconnect. (Clean-end finalization of the
		// live edge is covered by segments::tests::next_after_walks_finalized_segments.)
		drop((catalog, media, registration));
		broadcast.finish();

		let end = tokio::time::timeout(Duration::from_secs(5), segments.next())
			.await
			.expect("segment cursor resolves after the source is lost")
			.unwrap();
		assert!(
			end.is_none(),
			"an abrupt end does not finalize the open live-edge group"
		);

		// The renditions cursor also ends once the source closes.
		let ended = tokio::time::timeout(Duration::from_secs(5), renditions.next())
			.await
			.expect("renditions cursor resolves");
		assert!(ended.is_none(), "renditions cursor ends when the broadcast closes");
	}
}
