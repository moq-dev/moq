//! Per-broadcast egress source for the RTP-out paths.
//!
//! Counterpart to [`crate::ingest::IngestSink`]. Holds a
//! [`moq_net::broadcast::Consumer`] and follows its catalog; on each
//! `MediaAdded` event the session loop calls [`EgressSource::on_track`]
//! which spawns a pump task that serves the best enabled rendition in the
//! line's codec, re-picks as the catalog changes, and feeds RTP-ready frames
//! back to the session loop via an mpsc channel.
//!
//! Used by `server subscribe` (WHEP server) and `client publish` (WHIP
//! client). SDP negotiation lives in the matching modules; this file is
//! transport-agnostic.

use std::time::{Duration, Instant};

use bytes::Bytes;
use hang::catalog::{AudioCodec, VideoCodecKind};
use moq_mux::catalog::Stream;
use moq_mux::catalog::hang::Catalog;
use str0m::format::Codec;
use str0m::media::{Frequency, MediaTime, Mid, Pt};
use tokio::sync::{mpsc, watch};

use crate::{Error, Result, codec};

/// One frame waiting to be written into str0m's [`Writer`](str0m::media::Writer).
///
/// Pump tasks build these and send them down the channel; the session loop
/// receives them and calls `rtc.writer(mid).write(pt, wallclock, time, payload)`.
pub struct WriteRequest {
	/// Negotiated media line to write to.
	pub mid: Mid,
	/// Negotiated RTP payload type.
	pub pt: Pt,
	/// Presentation timestamp in the negotiated RTP clock domain.
	pub time: MediaTime,
	/// Complete encoded media frame.
	pub payload: Bytes,
}

/// Maps the shared MoQ presentation timeline to str0m's wallclock.
///
/// The first observed frame supplies an initial epoch. Later frames can prove
/// that epoch too recent when buffered media arrives faster than real time. In
/// that case the anchor moves earlier so no observed frame maps into the future.
/// Arrival delays never move it later, so dequeue jitter cannot become a
/// permanent difference between audio and video sender reports.
#[derive(Default)]
pub(crate) struct EgressClock {
	anchor: Option<(Duration, Instant)>,
}

impl EgressClock {
	/// Return the production wallclock corresponding to a presentation timestamp.
	pub(crate) fn wallclock(&mut self, time: MediaTime, now: Instant) -> Instant {
		let presentation = Duration::from(time);
		let Some((anchor_presentation, anchor_wallclock)) = self.anchor else {
			self.anchor = Some((presentation, now));
			return now;
		};

		if presentation >= anchor_presentation {
			let delta = presentation - anchor_presentation;
			let Some(mapped) = anchor_wallclock.checked_add(delta) else {
				self.anchor = Some((presentation, now));
				return now;
			};
			if mapped > now {
				// A catch-up burst revealed that the previous anchor was too recent.
				// Tighten it to the newest constraint. This anchor is shared by every
				// track, so equal presentation times map to equal wallclocks.
				self.anchor = Some((presentation, now));
				now
			} else {
				mapped
			}
		} else {
			anchor_wallclock
				.checked_sub(anchor_presentation - presentation)
				.unwrap_or(now)
		}
	}
}

/// Holds the export source and catalog, and spawns per-rendition pump tasks.
pub struct EgressSource {
	/// The catalog broadcast plus optional origin context, so a rendition referencing a
	/// sibling broadcast (its catalog `broadcast` field) resolves against the origin.
	source: moq_mux::Source,
	/// The latest catalog snapshot, followed for the session's life so each pump serves the
	/// best enabled rendition. Its sender drops when the catalog ends.
	catalog: watch::Receiver<Catalog>,
	writes_tx: mpsc::Sender<WriteRequest>,
	writes_rx: Option<mpsc::Receiver<WriteRequest>>,
}

impl EgressSource {
	/// Subscribe to the broadcast's catalog and wait for the first snapshot.
	///
	/// The [`moq_mux::Source`] carries the origin and catalog-broadcast path, so a
	/// rendition whose catalog `broadcast` field references a sibling broadcast is
	/// resolved against the same origin.
	///
	/// The session loop drives the pumps via the returned channel; the
	/// caller hands `EgressSource` to [`Session::egress`](crate::session::Session::egress)
	/// which takes the receiver via [`Self::take_writes`].
	pub async fn new(source: moq_mux::Source) -> Result<Self> {
		let mut consumer = source.catalog::<()>(moq_mux::catalog::CatalogFormat::Hang).await?;
		let catalog = consumer.next().await?.ok_or(Error::CatalogClosed)?;
		let (catalog_tx, catalog) = watch::channel(catalog);
		tokio::spawn(follow_catalog(consumer, catalog_tx));

		let (tx, rx) = mpsc::channel(64);
		Ok(Self {
			source,
			catalog,
			writes_tx: tx,
			writes_rx: Some(rx),
		})
	}

	/// One-shot extractor for the write-request receiver. The session loop
	/// awaits on this to forward frames into str0m.
	pub fn take_writes(&mut self) -> mpsc::Receiver<WriteRequest> {
		self.writes_rx.take().expect("EgressSource writes_rx already taken")
	}

	/// Spawn a pump task for a newly added (sendonly) media line.
	///
	/// `mid` and `pt` come from str0m's negotiated state; `clock_rate` is
	/// the codec's negotiated RTP clock. The pump serves the best enabled catalog
	/// rendition in `codec` and forwards every frame as a [`WriteRequest`].
	pub fn on_track(&mut self, mid: Mid, codec: Codec, pt: Pt, clock_rate: Frequency) -> Result<()> {
		let Some(target) = Target::new(codec) else {
			tracing::warn!(?codec, "unsupported egress codec; egress track ignored");
			return Ok(());
		};
		let pump = Pump {
			mid,
			pt,
			clock_rate,
			target,
			source: self.source.clone(),
			catalog: self.catalog.clone(),
			tx: self.writes_tx.clone(),
			playhead: None,
		};
		// Subscribing blocks on SUBSCRIBE_OK, so keep it inside the pump task to keep this
		// str0m callback non-blocking.
		tokio::spawn(pump.run());
		Ok(())
	}

	/// Codecs present in the catalog, used by the SDP-offer side
	/// (`client publish`) to declare what we have: the union of audio and video
	/// codecs across all renditions, disabled ones included since they may re-enable.
	/// The SDP codec list stays fixed for the session.
	pub fn catalog_codecs(&self) -> Vec<Codec> {
		let catalog = self.catalog.borrow();
		let mut out = Vec::new();
		if catalog
			.audio
			.renditions
			.values()
			.any(|r| matches!(r.codec, AudioCodec::Opus) && valid_reference(&self.source, r.broadcast.as_ref()))
		{
			out.push(Codec::Opus);
		}
		for rendition in catalog.video.renditions.values() {
			if !valid_reference(&self.source, rendition.broadcast.as_ref()) {
				continue;
			}
			let codec = match rendition.codec.kind() {
				VideoCodecKind::H264 => Some(Codec::H264),
				VideoCodecKind::H265 => Some(Codec::H265),
				VideoCodecKind::VP8 => Some(Codec::Vp8),
				VideoCodecKind::VP9 => Some(Codec::Vp9),
				VideoCodecKind::AV1 => Some(Codec::Av1),
				_ => None,
			};
			if let Some(c) = codec
				&& !out.contains(&c)
			{
				out.push(c);
			}
		}
		out
	}
}

/// Publish each catalog snapshot until the catalog ends or every pump is gone.
async fn follow_catalog(mut consumer: moq_mux::catalog::Consumer<()>, tx: watch::Sender<Catalog>) {
	loop {
		tokio::select! {
			next = consumer.next() => match next {
				Ok(Some(catalog)) => {
					tx.send_replace(catalog);
				}
				Ok(None) => return,
				Err(err) => {
					tracing::warn!(%err, "egress catalog failed");
					return;
				}
			},
			_ = tx.closed() => return,
		}
	}
}

fn valid_reference(source: &moq_mux::Source, broadcast: Option<&moq_net::path::Relative<'_>>) -> bool {
	source.resolve_reference(broadcast).is_some()
}

/// What a negotiated media line carries.
#[derive(Clone, Copy, Debug)]
enum Target {
	Opus,
	Video(VideoCodecKind),
}

impl Target {
	fn new(codec: Codec) -> Option<Self> {
		Some(match codec {
			Codec::Opus => Self::Opus,
			Codec::H264 => Self::Video(VideoCodecKind::H264),
			Codec::H265 => Self::Video(VideoCodecKind::H265),
			Codec::Vp8 => Self::Video(VideoCodecKind::VP8),
			Codec::Vp9 => Self::Video(VideoCodecKind::VP9),
			Codec::Av1 => Self::Video(VideoCodecKind::AV1),
			_ => return None,
		})
	}

	/// The best enabled rendition in this codec that `source` can reach.
	fn pick(self, source: &moq_mux::Source, catalog: &Catalog) -> Option<Rendition> {
		match self {
			Self::Opus => {
				pick_audio(source, catalog).map(|(name, config)| Rendition::Audio(name.clone(), config.clone()))
			}
			Self::Video(kind) => {
				pick_video(source, catalog, kind).map(|(name, config)| Rendition::Video(name.clone(), config.clone()))
			}
		}
	}
}

/// A catalog rendition a media line can serve.
enum Rendition {
	Audio(String, hang::catalog::AudioConfig),
	Video(String, hang::catalog::VideoConfig),
}

impl Rendition {
	fn name(&self) -> &str {
		match self {
			Self::Audio(name, _) | Self::Video(name, _) => name,
		}
	}

	/// Subscribe to the rendition, honoring its cross-broadcast reference (the catalog
	/// `broadcast` field).
	async fn subscribe(self, source: moq_mux::Source) -> Result<codec::Track> {
		match &self {
			Self::Audio(name, config) => {
				let track = source
					.subscribe_track(config.broadcast.as_ref(), hang::catalog::Reference::track(config, name))
					.await?;
				codec::Track::opus(track, config)
			}
			Self::Video(name, config) => {
				let track = source
					.subscribe_track(config.broadcast.as_ref(), hang::catalog::Reference::track(config, name))
					.await?;
				codec::Track::video(track, config)
			}
		}
	}
}

/// The best [enabled](hang::catalog::Audio::enabled) Opus rendition that `source` can reach.
fn pick_audio<'a>(
	source: &moq_mux::Source,
	catalog: &'a Catalog,
) -> Option<(&'a String, &'a hang::catalog::AudioConfig)> {
	catalog.audio.enabled().find(|(_, config)| {
		matches!(config.codec, AudioCodec::Opus) && valid_reference(source, config.broadcast.as_ref())
	})
}

/// The best [enabled](hang::catalog::Video::enabled) video rendition in `target`'s codec
/// that `source` can reach.
fn pick_video<'a>(
	source: &moq_mux::Source,
	catalog: &'a Catalog,
	target: VideoCodecKind,
) -> Option<(&'a String, &'a hang::catalog::VideoConfig)> {
	catalog
		.video
		.enabled()
		.find(|(_, c)| c.codec.kind() == target && valid_reference(source, c.broadcast.as_ref()))
}

/// A subscribed rendition.
struct Serving {
	name: String,
	track: codec::Track,
}

/// A rendition whose subscription is still resolving.
struct Subscribing {
	name: String,
	track: std::pin::Pin<Box<dyn Future<Output = Result<codec::Track>> + Send>>,
}

/// Serves one negotiated media line from the best enabled rendition in its codec.
///
/// The pump re-picks on every catalog change. A better or newly enabled rendition is
/// subscribed beside the current one, which keeps flowing until the new one reaches a
/// keyframe past the line's playhead, so a switch leaves no gap and never steps back in
/// time: a new subscription starts at the rendition's latest group, which for one re-enabled
/// is the stale group it stopped on. With nothing enabled it serves nothing until a
/// rendition re-enables. There is no ABR on the peer, so a re-enabled top rendition wins
/// the line back.
///
/// While a switch is pending, the current rendition runs one frame behind: each of its
/// frames is held until its next frame arrives or the new rendition reaches the same time.
/// Simulcast rungs share timestamps, so otherwise a current rung that delivers first would
/// claim every shared keyframe and the switch would never happen.
struct Pump {
	mid: Mid,
	pt: Pt,
	clock_rate: Frequency,
	target: Target,
	source: moq_mux::Source,
	catalog: watch::Receiver<Catalog>,
	tx: mpsc::Sender<WriteRequest>,
	/// The latest presentation time written, in microseconds.
	playhead: Option<u64>,
}

impl Pump {
	async fn run(mut self) {
		let mut serving: Option<Serving> = None;
		// The rendition to switch to, while its subscription resolves.
		let mut subscribing: Option<Subscribing> = None;
		// The rendition to switch to at its first keyframe.
		let mut next: Option<Serving> = None;
		// The current rendition's latest frame, withheld while a switch is pending.
		let mut held: Option<codec::PacketizedFrame> = None;
		let mut following = true;
		let mut repick = true;

		loop {
			if repick {
				repick = false;
				let pick = self.target.pick(&self.source, &self.catalog.borrow_and_update());
				match pick {
					None => {
						if serving.is_some() || next.is_some() || subscribing.is_some() {
							tracing::debug!(mid = ?self.mid, "no enabled rendition; egress track idle");
						}
						serving = None;
						next = None;
						subscribing = None;
						held = None;
					}
					Some(pick) if serving.as_ref().is_some_and(|s| s.name == pick.name()) => {
						next = None;
						subscribing = None;
					}
					Some(pick)
						if next.as_ref().is_some_and(|s| s.name == pick.name())
							|| subscribing.as_ref().is_some_and(|s| s.name == pick.name()) => {}
					Some(pick) => {
						next = None;
						subscribing = Some(Subscribing {
							name: pick.name().to_string(),
							track: Box::pin(pick.subscribe(self.source.clone())),
						});
					}
				}
			}

			if next.is_none()
				&& let Some(frame) = held.take()
				&& !self.write(frame).await
			{
				return;
			}

			// The catalog ended, so nothing new will be picked: serve what is left.
			if !following && serving.is_none() && next.is_none() && subscribing.is_none() {
				tracing::debug!(mid = ?self.mid, "egress track ended");
				return;
			}

			tokio::select! {
				// The session is gone, even if this line is idle.
				_ = self.tx.closed() => return,
				subscribed = subscribe(&mut subscribing) => {
					let name = subscribing.take().expect("a subscription resolved").name;
					match subscribed {
						Ok(track) => next = Some(Serving { name, track }),
						Err(err) => tracing::warn!(mid = ?self.mid, rendition = name, %err, "egress track subscribe failed"),
					}
				},
				changed = self.catalog.changed(), if following => match changed {
					Ok(()) => repick = true,
					Err(_) => following = false,
				},
				frame = read(self.mid, &mut serving) => match frame {
					Some(frame) => {
						let frame = match next {
							Some(_) => held.replace(frame),
							None => Some(frame),
						};
						if let Some(frame) = frame
							&& !self.write(frame).await
						{
							return;
						}
					}
					None => serving = None,
				},
				frame = read(self.mid, &mut next) => match frame {
					Some(frame) if self.joins(&frame) => {
						// The held frame plays out first unless the new rendition covers its time.
						if let Some(prev) = held.take_if(|prev| prev.timestamp_us < frame.timestamp_us)
							&& !self.write(prev).await
						{
							return;
						}
						held = None;
						serving = next.take();
						if !self.write(frame).await {
							return;
						}
					}
					Some(frame) => {
						// The new rendition reached the held frame's time without a keyframe.
						if let Some(prev) = held.take_if(|prev| prev.timestamp_us <= frame.timestamp_us)
							&& !self.write(prev).await
						{
							return;
						}
					}
					None => next = None,
				},
			}
		}
	}

	/// Whether the line can switch to a new rendition at `frame`: one the peer can start
	/// decoding at, past what was already written.
	fn joins(&self, frame: &codec::PacketizedFrame) -> bool {
		(frame.keyframe || matches!(self.target, Target::Opus))
			&& self.playhead.is_none_or(|playhead| frame.timestamp_us > playhead)
	}

	/// Forward one frame to the session loop, or return false once the session is gone.
	async fn write(&mut self, frame: codec::PacketizedFrame) -> bool {
		self.playhead = self.playhead.max(Some(frame.timestamp_us));
		let ticks = us_to_ticks(frame.timestamp_us, self.clock_rate);
		let request = WriteRequest {
			mid: self.mid,
			pt: self.pt,
			time: MediaTime::new(ticks, self.clock_rate),
			payload: frame.payload,
		};
		self.tx.send(request).await.is_ok()
	}
}

/// The resolved subscription of `subscribing`. Pending forever without one.
async fn subscribe(subscribing: &mut Option<Subscribing>) -> Result<codec::Track> {
	match subscribing {
		Some(subscribing) => subscribing.track.as_mut().await,
		None => std::future::pending().await,
	}
}

/// The next frame of `serving`, or `None` once it ends or fails. Pending forever without one.
async fn read(mid: Mid, serving: &mut Option<Serving>) -> Option<codec::PacketizedFrame> {
	let Some(serving) = serving else {
		return std::future::pending().await;
	};
	match serving.track.next().await {
		Ok(frame) => frame,
		Err(err) => {
			tracing::warn!(?mid, rendition = serving.name, %err, "egress track error");
			None
		}
	}
}

/// Convert a microsecond timestamp to a tick count at the given clock rate.
/// Uses u128 internally to avoid overflow at high tick rates.
fn us_to_ticks(timestamp_us: u64, clock_rate: Frequency) -> u64 {
	let rate = clock_rate.get() as u128;
	((timestamp_us as u128 * rate) / 1_000_000) as u64
}

/// Write one `WriteRequest` into str0m.
///
/// Lives here (not in session.rs) so the egress data shape is colocated
/// with the channel definition. Logs and swallows non-fatal errors; an
/// `UnknownPt` error after renegotiation isn't worth tearing down the
/// session over.
pub fn dispatch(rtc: &mut str0m::Rtc, request: WriteRequest, wallclock: Instant) {
	let Some(writer) = rtc.writer(request.mid) else {
		tracing::debug!(?request.mid, "egress write before media available");
		return;
	};
	let WriteRequest {
		pt,
		time,
		payload,
		mid: _,
	} = request;
	if let Err(err) = writer.write(pt, wallclock, time, payload.to_vec()) {
		tracing::warn!(%err, "egress write rejected by str0m");
	}
}

#[cfg(test)]
mod tests {
	/// Build an origin producer, spawning its driver on the ambient runtime.
	fn produce_origin() -> moq_net::origin::Producer {
		let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
		if tokio::runtime::Handle::try_current().is_ok() {
			tokio::spawn(moq_net::time::run(driver));
		} else {
			// A sync test: nothing polls the driver, and dropping it would tear
			// the origin down, so leak it and rely on the synchronous half.
			std::mem::forget(driver);
		}
		producer
	}

	use super::*;
	use hang::catalog::{AudioConfig, H264, VideoCodec, VideoConfig};
	use moq_net::path::Relative;

	#[test]
	fn catalog_codecs_ignores_codecs_available_only_via_escaping_references() {
		let origin = produce_origin();
		let source = moq_mux::Source::new(origin.consume(), origin.create_broadcast("a/pub").unwrap().consume());
		let mut catalog = Catalog::default();

		let mut escaped_audio = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
		escaped_audio.broadcast = Some(Relative::new("../../source").to_owned());
		catalog.audio.renditions.insert("opus".to_string(), escaped_audio);

		let mut escaped_video = VideoConfig::new(H264 {
			profile: 0x42,
			constraints: 0,
			level: 0x1e,
			inline: false,
		});
		escaped_video.broadcast = Some(Relative::new("../../source").to_owned());
		catalog.video.renditions.insert("h264".to_string(), escaped_video);

		let mut valid_video = VideoConfig::new(VideoCodec::VP8);
		valid_video.broadcast = Some(Relative::new("./source").to_owned());
		catalog.video.renditions.insert("vp8".to_string(), valid_video);

		let (writes_tx, writes_rx) = mpsc::channel(1);
		let egress = EgressSource {
			source,
			catalog: watch::channel(catalog).1,
			writes_tx,
			writes_rx: Some(writes_rx),
		};

		assert_eq!(egress.catalog_codecs(), vec![Codec::Vp8]);
	}

	#[test]
	fn picks_the_best_video_rendition_whatever_its_name() {
		let origin = produce_origin();
		let source = moq_mux::Source::new(origin.consume(), origin.create_broadcast("a/pub").unwrap().consume());
		let mut catalog = Catalog::default();

		let rendition = |height: u32| {
			let mut config = VideoConfig::new(H264 {
				profile: 0x42,
				constraints: 0,
				level: 0x1e,
				inline: false,
			});
			config.coded_width = Some(height * 16 / 9);
			config.coded_height = Some(height);
			config
		};
		// Name order would pick the lowest rendition.
		catalog.video.renditions.insert("a".to_string(), rendition(360));
		catalog.video.renditions.insert("b".to_string(), rendition(1080));
		catalog.video.renditions.insert("c".to_string(), rendition(720));
		catalog
			.video
			.renditions
			.insert("d".to_string(), VideoConfig::new(VideoCodec::VP8));

		let (name, _) = pick_video(&source, &catalog, VideoCodecKind::H264).unwrap();
		assert_eq!(name, "b");
		let (name, _) = pick_video(&source, &catalog, VideoCodecKind::VP8).unwrap();
		assert_eq!(name, "d");
		assert!(pick_video(&source, &catalog, VideoCodecKind::AV1).is_none());

		// A disabled rendition is never picked, so the next best serves instead, and a codec
		// with nothing enabled serves nothing.
		catalog.video.renditions.get_mut("b").unwrap().enabled = false;
		let (name, _) = pick_video(&source, &catalog, VideoCodecKind::H264).unwrap();
		assert_eq!(name, "c");
		catalog.video.renditions.get_mut("d").unwrap().enabled = false;
		assert!(pick_video(&source, &catalog, VideoCodecKind::VP8).is_none());
	}

	#[test]
	fn picks_the_best_opus_rendition_whatever_its_name() {
		let origin = produce_origin();
		let source = moq_mux::Source::new(origin.consume(), origin.create_broadcast("a/pub").unwrap().consume());
		let mut catalog = Catalog::default();

		let opus = |bitrate: Option<u64>| {
			let mut config = AudioConfig::new(AudioCodec::Opus, 48_000, 2);
			config.bitrate = bitrate;
			config
		};
		// Name order would pick the weakest Opus. The louder AAC is not Opus.
		catalog.audio.renditions.insert("a".to_string(), opus(Some(32_000)));
		let mut louder = AudioConfig::new(hang::catalog::AAC { profile: 2 }, 48_000, 2);
		louder.bitrate = Some(256_000);
		catalog.audio.renditions.insert("b".to_string(), louder);
		catalog.audio.renditions.insert("c".to_string(), opus(Some(128_000)));

		let (name, _) = pick_audio(&source, &catalog).unwrap();
		assert_eq!(name, "c");

		// A higher-bitrate Opus this source cannot reach loses to the best one it can.
		let mut escaped = opus(Some(512_000));
		escaped.broadcast = Some(Relative::new("../../source").to_owned());
		catalog.audio.renditions.insert("d".to_string(), escaped);
		let (name, _) = pick_audio(&source, &catalog).unwrap();
		assert_eq!(name, "c");

		// A disabled Opus rendition is never picked.
		catalog.audio.renditions.get_mut("c").unwrap().enabled = false;
		let (name, _) = pick_audio(&source, &catalog).unwrap();
		assert_eq!(name, "a");
	}

	/// A live VP8 rendition `name` of `height` lines, registered in `catalog`.
	fn vp8(
		broadcast: &mut moq_net::broadcast::Producer,
		catalog: &mut moq_mux::catalog::Producer,
		name: &str,
		height: u32,
	) -> moq_mux::container::Producer<moq_mux::catalog::hang::Container> {
		let mut config = VideoConfig::new(VideoCodec::VP8);
		config.container = hang::catalog::Container::Legacy;
		config.coded_width = Some(height * 16 / 9);
		config.coded_height = Some(height);
		catalog
			.modify()
			.unwrap()
			.video
			.renditions
			.insert(name.to_string(), config);
		let info = moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI);
		moq_mux::container::Producer::new(
			broadcast.create_track(name, info).unwrap(),
			moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
		)
	}

	fn frame(millis: u64, payload: &'static [u8], keyframe: bool) -> moq_mux::container::Frame {
		moq_mux::container::Frame {
			timestamp: moq_net::Timestamp::from_millis(millis).unwrap(),
			payload: Bytes::from_static(payload),
			keyframe,
			duration: None,
		}
	}

	/// The payload of the next write, required without another frame arriving.
	async fn written(writes: &mut mpsc::Receiver<WriteRequest>) -> Bytes {
		tokio::time::timeout(Duration::from_millis(10), writes.recv())
			.await
			.expect("a frame is written")
			.expect("the pump is alive")
			.payload
	}

	/// Nothing is written without another frame arriving.
	async fn idle(writes: &mut mpsc::Receiver<WriteRequest>) {
		let next = tokio::time::timeout(Duration::from_millis(10), writes.recv()).await;
		assert!(
			next.is_err(),
			"unexpected write: {:?}",
			next.map(|w| w.map(|w| w.payload))
		);
	}

	/// A media line follows the catalog for the session's life: a disabled rendition hands
	/// the line to its enabled sibling, nothing enabled serves nothing, and a re-enabled top
	/// rendition wins the line back at its first keyframe past the playhead, never replaying
	/// the stale group a new subscription starts on.
	#[tokio::test(start_paused = true)]
	async fn a_media_line_follows_the_catalog() {
		let origin = produce_origin();
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let mut high = vp8(&mut broadcast, &mut catalog, "high", 1080);
		let mut low = vp8(&mut broadcast, &mut catalog, "low", 360);
		let set_enabled = |catalog: &mut moq_mux::catalog::Producer, name: &str, enabled: bool| {
			catalog
				.modify()
				.unwrap()
				.video
				.renditions
				.get_mut(name)
				.unwrap()
				.enabled = enabled;
		};

		let mut egress = EgressSource::new(moq_mux::Source::new(origin.consume(), broadcast.consume()))
			.await
			.unwrap();
		let mut writes = egress.take_writes();
		egress
			.on_track(
				Mid::from("0"),
				Codec::Vp8,
				Pt::new_with_value(96),
				Frequency::NINETY_KHZ,
			)
			.unwrap();

		high.write(frame(0, b"high-0", true)).unwrap();
		low.write(frame(0, b"low-0", true)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"high-0");

		// The top rendition is disabled: the line moves to the low one at its next keyframe.
		set_enabled(&mut catalog, "high", false);
		idle(&mut writes).await;
		low.write(frame(100, b"low-1", true)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"low-1");
		low.write(frame(133, b"low-2", false)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"low-2");

		// Nothing enabled serves nothing, even when a disabled rendition still sends.
		set_enabled(&mut catalog, "low", false);
		idle(&mut writes).await;
		low.write(frame(166, b"low-3", false)).unwrap();
		idle(&mut writes).await;

		// The top rendition re-enables. Its stale group is skipped, and the line takes it
		// back at its first fresh keyframe.
		set_enabled(&mut catalog, "high", true);
		idle(&mut writes).await;
		high.write(frame(200, b"high-1", true)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"high-1");
	}

	/// A switch wins a keyframe time it shares with the current rendition, even when the
	/// current one keeps sending and delivers that time first.
	#[tokio::test(start_paused = true)]
	async fn a_switch_takes_a_shared_keyframe() {
		let origin = produce_origin();
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let mut high = vp8(&mut broadcast, &mut catalog, "high", 1080);
		let mut low = vp8(&mut broadcast, &mut catalog, "low", 360);
		let set_enabled = |catalog: &mut moq_mux::catalog::Producer, enabled: bool| {
			catalog
				.modify()
				.unwrap()
				.video
				.renditions
				.get_mut("high")
				.unwrap()
				.enabled = enabled;
		};
		set_enabled(&mut catalog, false);

		let mut egress = EgressSource::new(moq_mux::Source::new(origin.consume(), broadcast.consume()))
			.await
			.unwrap();
		let mut writes = egress.take_writes();
		egress
			.on_track(
				Mid::from("0"),
				Codec::Vp8,
				Pt::new_with_value(96),
				Frequency::NINETY_KHZ,
			)
			.unwrap();

		high.write(frame(0, b"high-0", true)).unwrap();
		low.write(frame(0, b"low-0", true)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"low-0");

		set_enabled(&mut catalog, true);
		idle(&mut writes).await;

		// The current rendition plays out one frame behind while the switch is pending.
		low.write(frame(33, b"low-1", false)).unwrap();
		idle(&mut writes).await;
		high.write(frame(33, b"high-1", false)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"low-1");

		// Both rungs open a group at 66 and the low one arrives first: the high one still wins.
		low.write(frame(66, b"low-2", true)).unwrap();
		idle(&mut writes).await;
		high.write(frame(66, b"high-2", true)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"high-2");
		low.write(frame(100, b"low-3", false)).unwrap();
		high.write(frame(100, b"high-3", false)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"high-3");
		idle(&mut writes).await;
	}

	/// A frame held for a pending switch is dropped once nothing is enabled, so it neither
	/// plays nor moves the playhead.
	#[tokio::test(start_paused = true)]
	async fn nothing_enabled_drops_a_held_frame() {
		let origin = produce_origin();
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let mut high = vp8(&mut broadcast, &mut catalog, "high", 1080);
		let mut low = vp8(&mut broadcast, &mut catalog, "low", 360);
		let set_enabled = |catalog: &mut moq_mux::catalog::Producer, name: &str, enabled: bool| {
			catalog
				.modify()
				.unwrap()
				.video
				.renditions
				.get_mut(name)
				.unwrap()
				.enabled = enabled;
		};
		set_enabled(&mut catalog, "high", false);

		let mut egress = EgressSource::new(moq_mux::Source::new(origin.consume(), broadcast.consume()))
			.await
			.unwrap();
		let mut writes = egress.take_writes();
		egress
			.on_track(
				Mid::from("0"),
				Codec::Vp8,
				Pt::new_with_value(96),
				Frequency::NINETY_KHZ,
			)
			.unwrap();

		high.write(frame(0, b"high-0", true)).unwrap();
		low.write(frame(0, b"low-0", true)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"low-0");

		// A switch is pending, so the low rendition's next frame is held.
		set_enabled(&mut catalog, "high", true);
		idle(&mut writes).await;
		low.write(frame(33, b"low-1", true)).unwrap();
		idle(&mut writes).await;

		{
			let mut modify = catalog.modify().unwrap();
			for config in modify.video.renditions.values_mut() {
				config.enabled = false;
			}
		}
		idle(&mut writes).await;

		// The held time was never played, so a rendition re-enabling can open there.
		set_enabled(&mut catalog, "high", true);
		idle(&mut writes).await;
		high.write(frame(33, b"high-1", true)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"high-1");
	}

	/// A better rendition whose subscription never resolves doesn't stall the line, nor
	/// stop it following the catalog.
	#[tokio::test(start_paused = true)]
	async fn a_pending_subscription_keeps_the_line_serving() {
		let origin = produce_origin();
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let mut low = vp8(&mut broadcast, &mut catalog, "low", 360);

		let mut egress = EgressSource::new(moq_mux::Source::new(origin.consume(), broadcast.consume()))
			.await
			.unwrap();
		let mut writes = egress.take_writes();
		egress
			.on_track(
				Mid::from("0"),
				Codec::Vp8,
				Pt::new_with_value(96),
				Frequency::NINETY_KHZ,
			)
			.unwrap();
		low.write(frame(0, b"low-0", true)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"low-0");

		// The better rendition lives in a broadcast whose request is never answered.
		let _handler = origin.dynamic("missing", Default::default()).unwrap();
		let mut config = VideoConfig::new(VideoCodec::VP8);
		config.container = hang::catalog::Container::Legacy;
		config.coded_width = Some(1920);
		config.coded_height = Some(1080);
		config.broadcast = Some(Relative::new("./missing").to_owned());
		catalog
			.modify()
			.unwrap()
			.video
			.renditions
			.insert("high".to_string(), config);
		idle(&mut writes).await;
		low.write(frame(33, b"low-1", false)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"low-1");

		catalog
			.modify()
			.unwrap()
			.video
			.renditions
			.get_mut("high")
			.unwrap()
			.enabled = false;
		idle(&mut writes).await;
		low.write(frame(66, b"low-2", false)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"low-2");
	}

	/// An idle line ends with its session rather than holding its subscription.
	#[tokio::test(start_paused = true)]
	async fn a_closed_session_ends_an_idle_line() {
		let origin = produce_origin();
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let mut low = vp8(&mut broadcast, &mut catalog, "low", 360);

		let mut egress = EgressSource::new(moq_mux::Source::new(origin.consume(), broadcast.consume()))
			.await
			.unwrap();
		let mut writes = egress.take_writes();
		egress
			.on_track(
				Mid::from("0"),
				Codec::Vp8,
				Pt::new_with_value(96),
				Frequency::NINETY_KHZ,
			)
			.unwrap();
		low.write(frame(0, b"low-0", true)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"low-0");

		drop(writes);
		tokio::time::timeout(Duration::from_secs(1), low.demand().unused())
			.await
			.expect("the line unsubscribes")
			.unwrap();
	}

	#[test]
	fn egress_clock_ignores_cross_track_dequeue_jitter() {
		let mut clock = EgressClock::default();
		let t0 = Instant::now();

		assert_eq!(clock.wallclock(MediaTime::from_millis(1_000), t0), t0);
		assert_eq!(
			clock.wallclock(MediaTime::from_millis(1_100), t0 + Duration::from_millis(100)),
			t0 + Duration::from_millis(100)
		);

		// Two tracks dequeue the same presentation time 50 ms apart. Their
		// sender-report wallclocks must still agree.
		let audio = clock.wallclock(MediaTime::from_millis(1_200), t0 + Duration::from_millis(250));
		let video = clock.wallclock(MediaTime::from_millis(1_200), t0 + Duration::from_millis(300));
		assert_eq!(audio, t0 + Duration::from_millis(200));
		assert_eq!(video, audio);
	}

	#[test]
	fn egress_clock_moves_epoch_earlier_for_catch_up_bursts() {
		let mut clock = EgressClock::default();
		let t0 = Instant::now();

		assert_eq!(clock.wallclock(MediaTime::from_millis(1_000), t0), t0);
		// The next 100 ms of media was already buffered and arrives immediately.
		// Re-anchor it at now instead of handing str0m a future wallclock.
		assert_eq!(clock.wallclock(MediaTime::from_millis(1_100), t0), t0);

		// Once the live edge is known, another track uses the same mapping even
		// when its frames dequeue later.
		assert_eq!(
			clock.wallclock(MediaTime::from_millis(1_100), t0 + Duration::from_millis(50)),
			t0
		);
	}
}
