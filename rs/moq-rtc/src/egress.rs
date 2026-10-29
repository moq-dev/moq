//! Per-broadcast egress source for the RTP-out paths.
//!
//! Counterpart to [`crate::ingest::IngestSink`]. Holds a
//! [`moq_net::broadcast::Consumer`] and follows its catalog; on each
//! `MediaAdded` event the session loop calls [`EgressSource::on_track`]
//! which spawns a pump task that serves the best enabled rendition in any of the
//! line's negotiated codecs, re-picks as the catalog changes, and feeds RTP-ready
//! frames back to the session loop via an mpsc channel.
//!
//! Used by `server subscribe` (WHEP server) and `client publish` (WHIP
//! client). SDP negotiation lives in the matching modules; this file is
//! transport-agnostic.

use std::collections::VecDeque;
use std::time::Duration;

use moq_clock::Instant;

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
	/// Why the catalog stopped, until the session takes it.
	catalog_end: Option<tokio::task::JoinHandle<Result<()>>>,
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
		let catalog_end = tokio::spawn(follow_catalog(consumer, catalog_tx));

		let (tx, rx) = mpsc::channel(64);
		Ok(Self {
			source,
			catalog,
			catalog_end: Some(catalog_end),
			writes_tx: tx,
			writes_rx: Some(rx),
		})
	}

	/// One-shot extractor for the write-request receiver. The session loop
	/// awaits on this to forward frames into str0m.
	pub fn take_writes(&mut self) -> mpsc::Receiver<WriteRequest> {
		self.writes_rx.take().expect("EgressSource writes_rx already taken")
	}

	/// One-shot extractor for why the catalog stopped. A failure, such as the source's route
	/// dying, ends the session, since nothing more will flow.
	pub(crate) fn take_catalog_end(&mut self) -> tokio::task::JoinHandle<Result<()>> {
		self.catalog_end.take().expect("EgressSource catalog_end already taken")
	}

	/// Spawn a pump task for a newly added (sendonly) media line.
	///
	/// `mid` and `formats` come from str0m's negotiated state, `formats` in the peer's order.
	/// The pump serves the best enabled catalog rendition in any of them and forwards every
	/// frame as a [`WriteRequest`] in that rendition's format.
	pub fn on_track(&mut self, mid: Mid, formats: Vec<Format>) -> Result<()> {
		if !formats
			.iter()
			.any(|format| format.codec == Codec::Opus || video_kind(format.codec).is_some())
		{
			tracing::warn!(?formats, "unsupported egress codecs; egress track ignored");
			return Ok(());
		}
		let pump = Pump {
			mid,
			formats,
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
			if let Some(c) = rtp_codec(rendition.codec.kind())
				&& !out.contains(&c)
			{
				out.push(c);
			}
		}
		out
	}
}

/// Publish each catalog snapshot until the catalog ends or every pump is gone, failing with the
/// catalog's failure.
async fn follow_catalog(mut consumer: moq_mux::catalog::Consumer<()>, tx: watch::Sender<Catalog>) -> Result<()> {
	loop {
		tokio::select! {
			next = consumer.next() => match next {
				Ok(Some(catalog)) => {
					tx.send_replace(catalog);
				}
				Ok(None) => return Ok(()),
				Err(err) => {
					tracing::warn!(%err, "egress catalog failed");
					return Err(err.into());
				}
			},
			_ = tx.closed() => return Ok(()),
		}
	}
}

fn valid_reference(source: &moq_mux::Source, broadcast: Option<&moq_net::path::Relative>) -> bool {
	source.resolve_reference(broadcast).is_some()
}

/// A payload format negotiated on a media line.
#[derive(Clone, Copy, Debug)]
pub struct Format {
	/// The negotiated codec.
	pub codec: Codec,
	/// Its RTP payload type.
	pub pt: Pt,
	/// Its RTP clock.
	pub clock_rate: Frequency,
}

/// The RTP codec carrying a catalog video codec, if egress supports it.
fn rtp_codec(kind: VideoCodecKind) -> Option<Codec> {
	Some(match kind {
		VideoCodecKind::H264 => Codec::H264,
		VideoCodecKind::H265 => Codec::H265,
		VideoCodecKind::VP8 => Codec::Vp8,
		VideoCodecKind::VP9 => Codec::Vp9,
		VideoCodecKind::AV1 => Codec::Av1,
		_ => return None,
	})
}

/// The catalog video codec an RTP codec carries, if egress supports it.
fn video_kind(codec: Codec) -> Option<VideoCodecKind> {
	Some(match codec {
		Codec::H264 => VideoCodecKind::H264,
		Codec::H265 => VideoCodecKind::H265,
		Codec::Vp8 => VideoCodecKind::VP8,
		Codec::Vp9 => VideoCodecKind::VP9,
		Codec::Av1 => VideoCodecKind::AV1,
		_ => return None,
	})
}

/// The best rendition a line negotiating `formats` can serve, with the format to send it in.
///
/// Video takes the first [enabled](hang::catalog::Video::enabled) rendition in any negotiated
/// codec, so the catalog's preference decides rather than the peer's codec order: a peer that
/// offers a fallback's codec first still gets the source when it also negotiated the source's.
fn pick(source: &moq_mux::Source, catalog: &Catalog, formats: &[Format]) -> Option<(Rendition, Format)> {
	let format = |codec: Codec| formats.iter().find(|format| format.codec == codec).copied();
	if let Some(format) = format(Codec::Opus) {
		let (name, config) = pick_audio(source, catalog)?;
		return Some((Rendition::Audio(name.clone(), config.clone()), format));
	}
	catalog.video.enabled().find_map(|(name, config)| {
		let format = format(rtp_codec(config.codec.kind())?)?;
		valid_reference(source, config.broadcast.as_ref())
			.then(|| (Rendition::Video(name.clone(), config.clone()), format))
	})
}

/// A catalog rendition a media line can serve.
#[allow(
	clippy::large_enum_variant,
	reason = "one per media line, so the variant sizes never add up to an allocation's worth; only large when extra features grow the video config"
)]
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

	/// The track the rendition reads through `source`.
	fn target(&self, source: &moq_mux::Source) -> Target {
		use hang::catalog::Reference;

		let (name, config): (_, &dyn Reference) = match self {
			Self::Audio(name, config) => (name, config),
			Self::Video(name, config) => (name, config),
		};
		Target {
			path: source.resolve_reference(config.broadcast()),
			epoch: config.epoch().cloned(),
			track: config.track(name).to_string(),
		}
	}

	/// Subscribe to the rendition, honoring its cross-broadcast reference (the catalog
	/// `broadcast` field).
	async fn subscribe(self, source: moq_mux::Source) -> Result<codec::Track> {
		use hang::catalog::Reference;

		match &self {
			Self::Audio(name, config) => {
				let track = source
					.subscribe_track(config.broadcast(), config.epoch(), config.track(name))
					.await?;
				codec::Track::opus(track, config)
			}
			Self::Video(name, config) => {
				let track = source
					.subscribe_track(config.broadcast(), config.epoch(), config.track(name))
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

/// The track a rendition reads: its broadcast's path, the publisher instance that broadcast
/// must be, and the track's name there. A catalog update that changes it moves the rendition.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Target {
	path: Option<moq_net::Path>,
	epoch: Option<moq_net::Epoch>,
	track: String,
}

/// A subscribed rendition, sent in `format`.
struct Serving {
	name: String,
	target: Target,
	format: Format,
	track: codec::Track,
}

/// A rendition whose subscription is still resolving.
struct Subscribing {
	name: String,
	target: Target,
	format: Format,
	track: std::pin::Pin<Box<dyn Future<Output = Result<codec::Track>> + Send>>,
}

/// Serves one negotiated media line from the best enabled rendition in its codecs.
///
/// The pump re-picks on every catalog change. A better or newly enabled rendition, or the
/// current one moved to another track, is subscribed beside the current one, which keeps
/// flowing until the new one reaches a keyframe past the line's playhead, so a switch leaves
/// no gap and never steps back in time: a new subscription starts at the rendition's latest
/// group, which for one re-enabled is the stale group it stopped on. A switch across codecs
/// changes the payload type at that keyframe. With nothing enabled it serves nothing until a
/// rendition re-enables. There is no ABR on the peer, so a re-enabled top rendition wins
/// the line back.
///
/// While a switch is pending, the current rendition is paced to the new one: each of its
/// frames is held until the new rendition reaches the same time. Simulcast rungs share
/// timestamps, so otherwise a current rung that delivers first would claim every shared
/// keyframe and the switch would never happen. See [`Held`] for how far the pacing goes.
struct Pump {
	mid: Mid,
	/// The negotiated formats, in the peer's order.
	formats: Vec<Format>,
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
		// The current rendition's frames, paced to `next` while a switch is pending.
		let mut held = Held::default();
		let mut following = true;
		let mut repick = true;

		loop {
			if repick {
				repick = false;
				let pick = pick(&self.source, &self.catalog.borrow_and_update(), &self.formats)
					.map(|(pick, format)| (pick.target(&self.source), pick, format));
				match pick {
					None => {
						if serving.is_some() || next.is_some() || subscribing.is_some() {
							tracing::debug!(mid = ?self.mid, "no enabled rendition; egress track idle");
						}
						serving = None;
						next = None;
						subscribing = None;
						held = Held::default();
					}
					// Compared by track rather than by ID: a rendition the catalog moves to
					// another track switches to it like any other pick.
					Some((target, ..)) if serving.as_ref().is_some_and(|s| s.target == target) => {
						next = None;
						subscribing = None;
					}
					Some((target, ..))
						if next.as_ref().is_some_and(|s| s.target == target)
							|| subscribing.as_ref().is_some_and(|s| s.target == target) => {}
					Some((target, pick, format)) => {
						next = None;
						subscribing = Some(Subscribing {
							name: pick.name().to_string(),
							target,
							format,
							track: Box::pin(pick.subscribe(self.source.clone())),
						});
					}
				}
			}

			// Without a pending switch, nothing paces the held frames.
			while let Some(frame) = held.pop(next.is_none()) {
				if !self.write(frame).await {
					return;
				}
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
					let Subscribing { name, target, format, .. } = subscribing.take().expect("a subscription resolved");
					match subscribed {
						Ok(track) => {
							next = Some(Serving { name, target, format, track });
							held.reached = None;
						}
						Err(err) => tracing::warn!(mid = ?self.mid, rendition = name, %err, "egress track subscribe failed"),
					}
				},
				changed = self.catalog.changed(), if following => match changed {
					Ok(()) => repick = true,
					Err(_) => following = false,
				},
				frame = read(self.mid, &mut serving) => match frame {
					Some(frame) if next.is_some() => held.frames.push_back(frame),
					Some(frame) => {
						if !self.write(frame).await {
							return;
						}
					}
					None => serving = None,
				},
				frame = read(self.mid, &mut next) => match frame {
					Some(frame) if self.joins(&frame) => {
						// Held frames play out first unless the new rendition covers their time.
						while let Some((_, prev)) = held.frames.front()
							&& prev.timestamp_us < frame.1.timestamp_us
						{
							let prev = held.frames.pop_front().expect("a held frame");
							if !self.write(prev).await {
								return;
							}
						}
						held = Held::default();
						serving = next.take();
						if !self.write(frame).await {
							return;
						}
					}
					// The new rendition progressed without a keyframe the line can join at.
					Some((_, frame)) => held.reached = held.reached.max(Some(frame.timestamp_us)),
					None => next = None,
				},
			}
		}
	}

	/// Whether the line can switch to a new rendition at `frame`: one the peer can start
	/// decoding at, past what was already written.
	fn joins(&self, (format, frame): &(Format, codec::PacketizedFrame)) -> bool {
		(frame.keyframe || format.codec.is_audio())
			&& self.playhead.is_none_or(|playhead| frame.timestamp_us > playhead)
	}

	/// Forward one frame to the session loop, or return false once the session is gone.
	async fn write(&mut self, (format, frame): (Format, codec::PacketizedFrame)) -> bool {
		self.playhead = self.playhead.max(Some(frame.timestamp_us));
		let ticks = us_to_ticks(frame.timestamp_us, format.clock_rate);
		let request = WriteRequest {
			mid: self.mid,
			pt: format.pt,
			time: MediaTime::new(ticks, format.clock_rate),
			payload: frame.payload,
		};
		self.tx.send(request).await.is_ok()
	}
}

/// How long a pending switch may hold a frame of the current rendition, in microseconds:
/// the frame plays once the current rendition delivers one more than this past it.
///
/// Frames are only released as frames arrive, so this bounds the latency the line takes on,
/// and how long it freezes if the new rendition stalls, to within one frame interval. A new
/// rendition lagging further than this can still be starved of a keyframe past the playhead.
const MAX_SWITCH_LAG_US: u64 = 500_000;

/// The current rendition's frames, withheld while a switch is pending.
#[derive(Default)]
struct Held {
	frames: VecDeque<(Format, codec::PacketizedFrame)>,
	/// The latest time the new rendition reached.
	reached: Option<u64>,
}

impl Held {
	/// The oldest held frame if it may play: every frame once `released`, otherwise one the
	/// new rendition has reached or that is more than [`MAX_SWITCH_LAG_US`] behind.
	fn pop(&mut self, released: bool) -> Option<(Format, codec::PacketizedFrame)> {
		let oldest = self.frames.front()?.1.timestamp_us;
		let latest = self.frames.back()?.1.timestamp_us;
		let ready = released
			|| self.reached.is_some_and(|reached| oldest <= reached)
			|| latest.saturating_sub(oldest) > MAX_SWITCH_LAG_US;
		ready.then(|| self.frames.pop_front()).flatten()
	}
}

/// The resolved subscription of `subscribing`. Pending forever without one.
async fn subscribe(subscribing: &mut Option<Subscribing>) -> Result<codec::Track> {
	match subscribing {
		Some(subscribing) => subscribing.track.as_mut().await,
		None => std::future::pending().await,
	}
}

/// The next frame of `serving` in its format, or `None` once it ends or fails. Pending forever
/// without one.
async fn read(mid: Mid, serving: &mut Option<Serving>) -> Option<(Format, codec::PacketizedFrame)> {
	let Some(serving) = serving else {
		return std::future::pending().await;
	};
	match serving.track.next().await {
		Ok(frame) => Some((serving.format, frame?)),
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
	if let Err(err) = writer.write(pt, wallclock.into_std(), time, payload.to_vec()) {
		tracing::warn!(%err, "egress write rejected by str0m");
	}
}

#[cfg(test)]
#[expect(clippy::disallowed_methods, reason = "tests read the host clock directly")]
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
		escaped_audio.broadcast = Some(Relative::new("../../source").unwrap());
		catalog.audio.renditions.insert("opus".to_string(), escaped_audio);

		let mut escaped_video = VideoConfig::new(H264 {
			profile: 0x42,
			constraints: 0,
			level: 0x1e,
			inline: false,
		});
		escaped_video.broadcast = Some(Relative::new("../../source").unwrap());
		catalog.video.renditions.insert("h264".to_string(), escaped_video);

		let mut valid_video = VideoConfig::new(VideoCodec::VP8);
		valid_video.broadcast = Some(Relative::new("./source").unwrap());
		catalog.video.renditions.insert("vp8".to_string(), valid_video);

		let (writes_tx, writes_rx) = mpsc::channel(1);
		let egress = EgressSource {
			source,
			catalog: watch::channel(catalog).1,
			catalog_end: None,
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

		let h264 = [format(Codec::H264, 102)];
		let vp8 = [format(Codec::Vp8, 96)];
		assert_eq!(picked(&source, &catalog, &h264), Some(("b", 102)));
		assert_eq!(picked(&source, &catalog, &vp8), Some(("d", 96)));
		assert_eq!(picked(&source, &catalog, &[format(Codec::Av1, 45)]), None);

		// A disabled rendition is never picked, so the next best serves instead, and a codec
		// with nothing enabled serves nothing.
		catalog.video.renditions.get_mut("b").unwrap().enabled = false;
		assert_eq!(picked(&source, &catalog, &h264), Some(("c", 102)));
		catalog.video.renditions.get_mut("d").unwrap().enabled = false;
		assert_eq!(picked(&source, &catalog, &vp8), None);
	}

	#[test]
	fn picks_the_preferred_codec_whatever_the_peer_offers_first() {
		let origin = produce_origin();
		let source = moq_mux::Source::new(origin.consume(), origin.create_broadcast("a/pub").unwrap().consume());
		let mut catalog = Catalog::default();

		// An H.265 source beside its own H.264 compatibility transcode, which has the higher bitrate.
		let mut h265 = VideoConfig::new("hvc1.1.6.L93.B0".parse::<VideoCodec>().unwrap());
		h265.coded_width = Some(1280);
		h265.coded_height = Some(720);
		h265.bitrate = Some(2_000_000);
		catalog.video.renditions.insert("source".to_string(), h265);
		let mut h264 = VideoConfig::new(H264 {
			profile: 0x42,
			constraints: 0,
			level: 0x1f,
			inline: false,
		});
		h264.coded_width = Some(1280);
		h264.coded_height = Some(720);
		h264.bitrate = Some(4_000_000);
		h264.preference = -1;
		catalog.video.renditions.insert("fallback".to_string(), h264);

		// The peer offers the fallback's codec first, yet negotiated the source's too.
		let both = [format(Codec::H264, 102), format(Codec::H265, 49)];
		assert_eq!(picked(&source, &catalog, &both), Some(("source", 49)));

		// A peer without the source's codec gets the fallback.
		assert_eq!(
			picked(&source, &catalog, &[format(Codec::H264, 102)]),
			Some(("fallback", 102))
		);
	}

	fn format(codec: Codec, pt: u8) -> Format {
		Format {
			codec,
			pt: Pt::new_with_value(pt),
			clock_rate: Frequency::NINETY_KHZ,
		}
	}

	/// The video rendition and payload type a line negotiating `formats` serves.
	fn picked<'a>(source: &moq_mux::Source, catalog: &'a Catalog, formats: &[Format]) -> Option<(&'a str, u8)> {
		let (rendition, format) = pick(source, catalog, formats)?;
		let (name, _) = catalog.video.renditions.get_key_value(rendition.name())?;
		Some((name.as_str(), *format.pt))
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
		escaped.broadcast = Some(Relative::new("../../source").unwrap());
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
		config.coded_width = Some(height * 16 / 9);
		config.coded_height = Some(height);
		video(broadcast, catalog, name, config)
	}

	/// A live video rendition `name` with `config`, registered in `catalog`.
	fn video(
		broadcast: &mut moq_net::broadcast::Producer,
		catalog: &mut moq_mux::catalog::Producer,
		name: &str,
		mut config: VideoConfig,
	) -> moq_mux::container::Producer<moq_mux::catalog::hang::Container> {
		config.container = hang::catalog::Container::Legacy;
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
		egress.on_track(Mid::from("0"), vec![format(Codec::Vp8, 96)]).unwrap();

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

	/// A rendition the catalog moves to another track, in its broadcast or a sibling one, keeps
	/// its media line: the line switches to the new track at its first keyframe past the
	/// playhead and serves nothing more of the track it left, even while that one still sends.
	#[tokio::test(start_paused = true)]
	async fn a_moved_rendition_follows_its_new_track() {
		let origin = produce_origin();
		let live = origin.publish("live", Default::default()).unwrap();
		let other = origin.publish("other", Default::default()).unwrap();
		let mut catalog =
			moq_mux::catalog::Producer::new(&mut live.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();
		let track = |broadcast: &mut moq_net::broadcast::Producer, name: &str| {
			let info = moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI);
			moq_mux::container::Producer::new(
				broadcast.create_track(name, info).unwrap(),
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
			)
		};
		let mut a = track(&mut live.broadcast().clone(), "a");
		let mut b = track(&mut live.broadcast().clone(), "b");
		let mut sibling = track(&mut other.broadcast().clone(), "a");
		let point = |catalog: &mut moq_mux::catalog::Producer, track: &str, broadcast: Option<&str>| {
			let mut config = VideoConfig::new(VideoCodec::VP8);
			config.container = hang::catalog::Container::Legacy;
			config.track = Some(track.to_string());
			config.broadcast = broadcast.map(|rel| Relative::new(rel).unwrap());
			catalog
				.modify()
				.unwrap()
				.video
				.renditions
				.insert("main".to_string(), config);
		};
		point(&mut catalog, "a", None);

		let broadcast = origin.consume().request("live", None).await.unwrap();
		let mut egress = EgressSource::new(moq_mux::Source::new(origin.consume(), broadcast))
			.await
			.unwrap();
		let mut writes = egress.take_writes();
		egress.on_track(Mid::from("0"), vec![format(Codec::Vp8, 96)]).unwrap();

		a.write(frame(0, b"a-0", true)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"a-0");

		// Moved within the broadcast: the old track is paced to the new one until its keyframe.
		point(&mut catalog, "b", None);
		idle(&mut writes).await;
		a.write(frame(33, b"a-1", false)).unwrap();
		idle(&mut writes).await;
		b.write(frame(66, b"b-2", true)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"a-1");
		assert_eq!(written(&mut writes).await.as_ref(), b"b-2");
		a.write(frame(100, b"a-3", false)).unwrap();
		idle(&mut writes).await;

		// Moved to a sibling broadcast.
		point(&mut catalog, "a", Some("other"));
		idle(&mut writes).await;
		sibling.write(frame(100, b"sibling-3", true)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"sibling-3");
		a.write(frame(133, b"a-4", false)).unwrap();
		b.write(frame(133, b"b-4", false)).unwrap();
		idle(&mut writes).await;
		sibling.write(frame(133, b"sibling-4", false)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"sibling-4");
	}

	/// A line that negotiated both codecs serves the preferred rendition in its payload type,
	/// and switches payload type when it falls back.
	#[tokio::test(start_paused = true)]
	async fn a_fallback_switches_the_payload_type() {
		let origin = produce_origin();
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let mut catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let mut preferred = video(
			&mut broadcast,
			&mut catalog,
			"source",
			VideoConfig::new("vp09.00.10.08".parse::<VideoCodec>().unwrap()),
		);
		let mut config = VideoConfig::new(VideoCodec::VP8);
		config.preference = -1;
		let mut fallback = video(&mut broadcast, &mut catalog, "fallback", config);

		let mut egress = EgressSource::new(moq_mux::Source::new(origin.consume(), broadcast.consume()))
			.await
			.unwrap();
		let mut writes = egress.take_writes();
		egress
			.on_track(Mid::from("0"), vec![format(Codec::Vp8, 96), format(Codec::Vp9, 98)])
			.unwrap();

		let next = async |writes: &mut mpsc::Receiver<WriteRequest>| {
			let write = tokio::time::timeout(Duration::from_millis(10), writes.recv())
				.await
				.expect("a frame is written")
				.expect("the pump is alive");
			(write.payload, *write.pt)
		};

		preferred.write(frame(0, b"vp9-0", true)).unwrap();
		fallback.write(frame(0, b"vp8-0", true)).unwrap();
		assert_eq!(next(&mut writes).await, (Bytes::from_static(b"vp9-0"), 98));

		catalog
			.modify()
			.unwrap()
			.video
			.renditions
			.get_mut("source")
			.unwrap()
			.enabled = false;
		idle(&mut writes).await;
		fallback.write(frame(100, b"vp8-1", true)).unwrap();
		assert_eq!(next(&mut writes).await, (Bytes::from_static(b"vp8-1"), 96));
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
		egress.on_track(Mid::from("0"), vec![format(Codec::Vp8, 96)]).unwrap();

		high.write(frame(0, b"high-0", true)).unwrap();
		low.write(frame(0, b"low-0", true)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"low-0");

		set_enabled(&mut catalog, true);
		idle(&mut writes).await;

		// The current rendition is paced to the new one while the switch is pending.
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

	/// A switch to a rendition whose frames arrive several frames behind the current one's
	/// still takes over at its first keyframe.
	#[tokio::test(start_paused = true)]
	async fn a_switch_waits_for_a_lagging_rendition() {
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
		egress.on_track(Mid::from("0"), vec![format(Codec::Vp8, 96)]).unwrap();

		high.write(frame(0, b"high-0", true)).unwrap();
		low.write(frame(0, b"low-0", true)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"low-0");

		set_enabled(&mut catalog, true);
		idle(&mut writes).await;

		// The low rendition runs three frames ahead, through the next keyframe.
		low.write(frame(33, b"low-1", false)).unwrap();
		low.write(frame(66, b"low-2", false)).unwrap();
		low.write(frame(100, b"low-3", true)).unwrap();
		low.write(frame(133, b"low-4", false)).unwrap();
		idle(&mut writes).await;

		// Each low frame plays once the high rendition reaches its time.
		high.write(frame(33, b"high-1", false)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"low-1");
		idle(&mut writes).await;
		high.write(frame(66, b"high-2", false)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"low-2");
		idle(&mut writes).await;

		// The high rendition's keyframe is still past the playhead, so it takes over.
		high.write(frame(100, b"high-3", true)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"high-3");
		high.write(frame(133, b"high-4", false)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"high-4");
		low.write(frame(166, b"low-5", false)).unwrap();
		idle(&mut writes).await;
	}

	/// A switch to a rendition that stalls holds the line back by at most
	/// [`MAX_SWITCH_LAG_US`] rather than freezing it.
	#[tokio::test(start_paused = true)]
	async fn a_stalled_switch_bounds_the_lag() {
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
		egress.on_track(Mid::from("0"), vec![format(Codec::Vp8, 96)]).unwrap();

		high.write(frame(0, b"high-0", true)).unwrap();
		low.write(frame(0, b"low-0", true)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"low-0");

		// The high rendition never sends again.
		set_enabled(&mut catalog, true);
		idle(&mut writes).await;
		low.write(frame(100, b"low-1", false)).unwrap();
		low.write(frame(600, b"low-2", false)).unwrap();
		idle(&mut writes).await;

		// Past the bound, the oldest held frame plays, one per frame the low rendition sends.
		low.write(frame(700, b"low-3", false)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"low-1");
		idle(&mut writes).await;
		low.write(frame(1200, b"low-4", false)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"low-2");
		idle(&mut writes).await;

		// Abandoning the switch plays everything held.
		set_enabled(&mut catalog, false);
		assert_eq!(written(&mut writes).await.as_ref(), b"low-3");
		assert_eq!(written(&mut writes).await.as_ref(), b"low-4");
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
		egress.on_track(Mid::from("0"), vec![format(Codec::Vp8, 96)]).unwrap();

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
		egress.on_track(Mid::from("0"), vec![format(Codec::Vp8, 96)]).unwrap();
		low.write(frame(0, b"low-0", true)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"low-0");

		// The better rendition lives in a broadcast whose request is never answered.
		let _handler = origin.requests("missing").unwrap();
		let mut config = VideoConfig::new(VideoCodec::VP8);
		config.container = hang::catalog::Container::Legacy;
		config.coded_width = Some(1920);
		config.coded_height = Some(1080);
		config.broadcast = Some(Relative::new("./missing").unwrap());
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
		egress.on_track(Mid::from("0"), vec![format(Codec::Vp8, 96)]).unwrap();
		low.write(frame(0, b"low-0", true)).unwrap();
		assert_eq!(written(&mut writes).await.as_ref(), b"low-0");

		drop(writes);
		let mut demand = low.demand();
		let unwanted = async {
			while demand.wanted_changed().await?.is_some() {}
			moq_net::Result::Ok(())
		};
		tokio::time::timeout(Duration::from_secs(1), unwanted)
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
