//! str0m session driver shared by every HTTP role / media direction.
//!
//! [`moq_ice::Agent`] drives the sans-IO [`str0m::Rtc`] over UDP;
//! [`Session::run`] feeds it egress writes and handles the media events it surfaces.
//!
//! The session itself doesn't care whether the [`Rtc`] was populated by
//! accepting an SDP offer (server side) or by minting one and posting it
//! to a remote URL (client side), or whether the media flow is RTP-in
//! ([`MediaSink`]) or RTP-out ([`crate::egress::EgressSource`]).

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use kio::clock::Instant;

use str0m::{Candidate, Event, Rtc};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::egress::{EgressClock, EgressSource, Format, WriteRequest};
use crate::rate::{self, Cap, Meter};
use crate::{Error, Result, codec};

/// str0m's outbound video buffer depth (packets), which also backs NACK resends.
/// Raised above the str0m default (1000) so a late-joining peer can recover a
/// large keyframe and the rest of the current group via NACK; see
/// [`rtc_config_with_codecs`].
const EGRESS_SEND_BUFFER_VIDEO: usize = 3000;

/// How often a capped ingest repeats its REMB, as libwebrtc does, so a lost one costs
/// at most this long, and an encoder that drifted up is pulled back.
const REMB_INTERVAL: Duration = Duration::from_secs(1);

/// Receives `MediaData` events from str0m and dispatches to the right codec
/// [`Bridge`](codec::Bridge). Used as the per-session sink in [`Session::run`]
/// for any flow where RTP arrives from the peer (`server publish` / WHIP
/// server, `client subscribe` / WHEP client).
pub trait MediaSink: Send {
	/// Called once str0m has confirmed which codec is on which `mid`.
	fn on_track(
		&mut self,
		mid: str0m::media::Mid,
		kind: str0m::media::MediaKind,
		codec: str0m::format::Codec,
		audio_params: Option<(u32, u32)>,
	) -> Result<()>;

	/// Called on each [`MediaData`](str0m::media::MediaData) event. The session
	/// loop has already converted the timestamp to microseconds.
	fn on_frame(&mut self, mid: str0m::media::Mid, frame: codec::Frame) -> Result<()>;

	/// Called once when the session ends other than by the peer's clean end, so the sink can
	/// abort its tracks, the catalog included, with the real cause instead of a bare
	/// `Error::Dropped`.
	fn abort(&mut self, err: moq_net::Error);

	/// Called once when the peer ends the session cleanly after its final media: finish
	/// every track and return the broadcast. The finished tracks stay servable while the sink
	/// is held.
	fn finish(&mut self) -> Result<moq_net::broadcast::Producer>;
}

/// What the session does with the negotiated media stream.
#[non_exhaustive]
pub enum MediaRole {
	/// RTP-in: dispatch peer frames into a [`MediaSink`].
	Ingest(Box<dyn MediaSink>),
	/// RTP-out: pull frames from a [`crate::egress::EgressSource`] and forward to the peer.
	Egress(Box<EgressSource>),
}

#[derive(Clone, Copy)]
enum MediaFlow {
	Ingest,
	Egress,
}

impl MediaFlow {
	fn accepts(self, direction: str0m::media::Direction) -> bool {
		match self {
			Self::Ingest => direction.is_receiving(),
			Self::Egress => direction.is_sending(),
		}
	}
}

/// Drives a [`Rtc`] instance until it ends.
///
/// The caller pre-populates the `Rtc` with whatever SDP exchange they need.
/// Sends go out the (possibly shared) `socket`; inbound datagrams arrive on
/// `inbound` rather than being read off the socket directly, so several
/// sessions can share one socket behind the `crate::server::mux`.
pub struct Session {
	agent: moq_ice::Agent,
	role: MediaRole,
	opened_media: HashSet<str0m::media::Mid>,
	/// Egress write requests. `Some` only for [`MediaRole::Egress`]
	/// sessions; pumps send frames here, the main loop forwards them into
	/// str0m's [`Writer`](str0m::media::Writer).
	writes_rx: Option<mpsc::Receiver<WriteRequest>>,
	/// Why the egress source's catalog stopped. `Some` only for [`MediaRole::Egress`] sessions,
	/// until it does.
	catalog_end: Option<tokio::task::JoinHandle<Result<()>>>,
	/// Rebases each ingested track's raw RTP timestamps onto one session
	/// timeline so audio and video stay in sync. Unused by egress sessions.
	ingest_clock: IngestClock,
	/// Maps the shared MoQ presentation timeline to str0m's sender-report
	/// wallclock. Unused by ingest sessions.
	egress_clock: EgressClock,
	/// The ingest's cap, metered and steered through REMB. `None` until
	/// [`Session::with_rate`], so always for egress sessions.
	rate: Option<IngestRate>,
}

impl Session {
	/// Convenience for the ingest case (WHIP server, WHEP client). The socket's
	/// `inbound` comes from the server mux, or [`moq_ice::reader`] for a 1:1 client socket.
	pub fn ingest(rtc: Rtc, socket: moq_ice::Socket, sink: Box<dyn MediaSink>) -> Result<Self> {
		Ok(Self {
			agent: moq_ice::Agent::new(rtc, socket)?,
			role: MediaRole::Ingest(sink),
			opened_media: HashSet::new(),
			writes_rx: None,
			catalog_end: None,
			ingest_clock: IngestClock::default(),
			egress_clock: EgressClock::default(),
			rate: None,
		})
	}

	/// Convenience for the egress case (WHEP server, WHIP client). The socket's
	/// `inbound` comes from the server mux, or [`moq_ice::reader`] for a 1:1 client socket.
	pub fn egress(rtc: Rtc, socket: moq_ice::Socket, mut source: EgressSource) -> Result<Self> {
		let writes_rx = source.take_writes();
		let catalog_end = source.take_catalog_end();
		Ok(Self {
			agent: moq_ice::Agent::new(rtc, socket)?,
			role: MediaRole::Egress(Box::new(source)),
			opened_media: HashSet::new(),
			writes_rx: Some(writes_rx),
			catalog_end: Some(catalog_end),
			ingest_clock: IngestClock::default(),
			egress_clock: EgressClock::default(),
			rate: None,
		})
	}

	/// Meter an ingest against `limit`'s cap, following its updates, and steer the
	/// publisher under it with REMB on every video line that negotiated it.
	pub(crate) fn with_rate(mut self, limit: &rate::Limit) -> Self {
		let limit = limit.subscribe();
		let cap = *limit.borrow();
		self.rate = Some(IngestRate {
			meter: Meter::new(cap, Instant::now()),
			limit: Some(limit),
			remb: Vec::new(),
			next_remb: Instant::now(),
			steered: cap.is_some(),
		});
		self
	}

	/// Open a locally offered media line after its answer has been applied.
	pub(crate) fn open_media(&mut self, mid: str0m::media::Mid) -> Result<()> {
		self.handle_media(mid)
	}

	/// End an ingest cleanly after its final media: finish the sink's tracks and keep the
	/// finished broadcast up for `linger`. `None` for an egress session, which publishes nothing.
	pub fn finish(self, linger: std::time::Duration) -> Result<Option<moq_mux::Linger<Box<dyn MediaSink>>>> {
		match self.role {
			MediaRole::Ingest(mut sink) => {
				let broadcast = sink.finish()?;
				Ok(Some(moq_mux::Linger::new([broadcast], sink, linger)))
			}
			MediaRole::Egress(_) => Ok(None),
		}
	}

	pub async fn run(&mut self) -> Result<()> {
		let result = self.run_loop().await;
		// Any end but the peer's DELETE, a disconnect or an offer that never connected
		// included, ends the sink's tracks with its cause, so subscribers see the publisher
		// go rather than its tracks dropped unfinished.
		if let Err(err) = &result
			&& let MediaRole::Ingest(sink) = &mut self.role
		{
			sink.abort(moq_net::Error::Transport(err.to_string()));
		}
		result
	}

	async fn run_loop(&mut self) -> Result<()> {
		loop {
			let remb_at = self.rate.as_ref().and_then(IngestRate::remb_at);
			tokio::select! {
				// Egress writes get drained promptly; `Agent::next` is cancel safe.
				biased;

				// A failed catalog, as every subscription fails once the source's route dies,
				// means nothing more will flow, so fail rather than idle. A clean end keeps
				// serving what the pumps still hold. Polled before the writes, which could
				// otherwise stay ready and starve it; it completes once.
				ended = async {
					match self.catalog_end.as_mut() {
						Some(end) => end.await,
						None => std::future::pending().await,
					}
				} => {
					self.catalog_end = None;
					if let Ok(Err(err)) = ended {
						return Err(err);
					}
				}

				Some(req) = async {
					match self.writes_rx.as_mut() {
						Some(rx) => rx.recv().await,
						None => std::future::pending::<Option<WriteRequest>>().await,
					}
				} => {
					let wallclock = self.egress_clock.wallclock(req.time, Instant::now());
					crate::egress::dispatch(&mut self.agent.rtc_mut(), req, wallclock);
				}

				changed = async {
					match self.rate.as_mut().and_then(|rate| rate.limit.as_mut()) {
						Some(limit) => limit.changed().await.map(|()| *limit.borrow_and_update()),
						None => std::future::pending().await,
					}
				} => {
					let rate = self.rate.as_mut().expect("only a rated session watches its cap");
					match changed {
						Ok(cap) => {
							tracing::debug!(?cap, "ingest cap updated");
							rate.meter.set(cap, Instant::now());
							rate.steered |= cap.is_some();
							// REMB follows an update at once.
							rate.next_remb = Instant::now();
						}
						// Every handle is gone, so the cap can no longer change: keep the last one.
						Err(_) => rate.limit = None,
					}
				}

				_ = async {
					match remb_at {
						Some(at) => kio::clock::sleep_until(at).await,
						None => std::future::pending().await,
					}
				} => self.send_remb(),

				event = self.agent.next() => match event {
					Ok(moq_ice::Event::Rtc(event)) => self.handle_event(event)?,
					// No STUN servers are configured, so there is nothing to trickle.
					Ok(_) => {}
					Err(moq_ice::Error::Closed) => return Err(Error::SessionClosed),
					Err(moq_ice::Error::Timeout) => return Err(Error::IceTimeout),
					Err(err) => return Err(Error::rtc(err)),
				},
			}
		}
	}

	fn handle_event(&mut self, event: Event) -> Result<()> {
		match event {
			Event::IceConnectionStateChange(state) => {
				tracing::debug!(?state, "ice state");
			}
			Event::MediaAdded(added) => self.handle_media(added.mid)?,
			Event::MediaData(data) => {
				// `ingest_clock` and `role` are disjoint fields, so the borrow checker lets
				// us rebase the (random, per-track) RTP base and feed the sink in one
				// block; egress sessions never get here so the clock stays untouched.
				if let MediaRole::Ingest(sink) = &mut self.role {
					let arrival = Instant::from_std(data.network_time);
					if let Some(rate) = &mut self.rate
						&& let Err(cap) = rate.meter.charge(data.data.len(), arrival, Instant::now())
					{
						tracing::warn!(cap = cap.rate.as_bps(), "ingest exceeded its cap plus burst; closing");
						return Err(Error::RateLimited { cap: cap.rate });
					}
					let media_us = media_time_to_micros(&data.time);
					let timestamp_us = self.ingest_clock.normalize(data.mid, arrival, media_us);
					sink.on_frame(
						data.mid,
						codec::Frame {
							timestamp_us,
							payload: bytes::Bytes::from_owner(data.data),
						},
					)?;
				}
			}
			Event::SenderFeedback(feedback) => {
				if matches!(&self.role, MediaRole::Ingest(_)) {
					self.ingest_clock.observe(feedback.mid, feedback.sender_info);
				}
			}
			Event::EgressBitrateEstimate(str0m::bwe::BweKind::Remb { estimate, mid }) => {
				// The receiver's ceiling for what we send. Egress forwards the catalog's renditions
				// rather than encoding, so there is no encoder to steer: only log it.
				tracing::debug!(?mid, bps = estimate.as_u64(), "peer sent REMB");
			}
			Event::KeyframeRequest(req) => {
				// PLI / FIR from the egress peer. For v1 we just log and
				// rely on the next natural keyframe from the MoQ source.
				tracing::debug!(?req, "keyframe request from peer");
			}
			_ => {}
		}
		Ok(())
	}

	fn handle_media(&mut self, mid: str0m::media::Mid) -> Result<()> {
		if self.opened_media.contains(&mid) {
			return Ok(());
		}

		// str0m's CodecConfig is the negotiated set; `remote_pts` lists this
		// `mid`'s share of it in the peer's order.
		let Some(media) = self.agent.rtc().media(mid) else {
			tracing::warn!(?mid, "negotiated media is missing; ignoring");
			return Ok(());
		};
		let kind = media.kind();
		let flow = match &self.role {
			MediaRole::Ingest(_) => MediaFlow::Ingest,
			MediaRole::Egress(_) => MediaFlow::Egress,
		};
		if !flow.accepts(media.direction()) {
			return Ok(());
		}
		let negotiated: Vec<_> = media
			.remote_pts()
			.iter()
			.filter_map(|pt| {
				self.agent
					.rtc()
					.codec_config()
					.params()
					.iter()
					.find(|p| p.pt() == *pt)
					.copied()
			})
			.collect();
		let Some(first) = negotiated.first() else {
			tracing::warn!(?mid, "no codec params for media; ignoring");
			return Ok(());
		};
		let spec = first.spec();
		self.opened_media.insert(mid);

		match &mut self.role {
			MediaRole::Ingest(sink) => {
				let codec = spec.codec;
				let audio_params = if codec.is_audio() {
					Some((spec.clock_rate.get(), spec.channels.unwrap_or(1) as u32))
				} else {
					None
				};
				sink.on_track(mid, kind, codec, audio_params)?;
				// REMB only where the publisher negotiated it; str0m offers it on video alone.
				if let Some(rate) = &mut self.rate
					&& negotiated.iter().any(|params| params.fb_remb())
				{
					rate.remb.push(mid);
					rate.next_remb = Instant::now();
				}
			}
			// Any negotiated codec may be sent, so the catalog picks among all of them.
			MediaRole::Egress(source) => {
				let formats = negotiated
					.iter()
					.map(|params| Format {
						codec: params.spec().codec,
						pt: params.pt(),
						clock_rate: params.spec().clock_rate,
					})
					.collect();
				source.on_track(mid, formats)?;
			}
		}
		Ok(())
	}

	/// Ask the publisher for at most the steered rate on every video line that negotiated
	/// REMB, and schedule the next repeat.
	fn send_remb(&mut self) {
		let Some(rate) = &mut self.rate else {
			return;
		};
		rate.next_remb = Instant::now() + REMB_INTERVAL;
		// A REMB ceiling holds until the next one replaces it, so a lifted cap keeps sending
		// the largest REMB can carry rather than going quiet and leaving the old ceiling.
		let bitrate = match rate.meter.cap() {
			Some(cap) => str0m::bwe::Bitrate::bps(cap.steer().as_bps()),
			None => str0m::bwe::Bitrate::MAX,
		};
		let mut rtc = self.agent.rtc_mut();
		for &mid in &rate.remb {
			// One REMB per receive stream, simulcast layers included; a stream that has yet to
			// see its first packet gets the next one.
			let rids = match rtc.media(mid).map(|media| media.rids_rx()) {
				Some(str0m::media::Rids::Specific(rids)) => rids.iter().copied().map(Some).collect(),
				_ => vec![None],
			};
			let mut api = rtc.direct_api();
			for rid in rids {
				if let Some(stream) = api.stream_rx_by_mid(mid, rid) {
					stream.request_remb(bitrate);
				}
			}
		}
	}
}

/// An ingest's cap: the meter that closes it past the cap plus burst, and the REMB that
/// steers the publisher under it.
struct IngestRate {
	meter: Meter,
	/// Live cap updates; `None` once every handle is dropped.
	limit: Option<tokio::sync::watch::Receiver<Option<Cap>>>,
	/// Video lines that negotiated REMB.
	remb: Vec<str0m::media::Mid>,
	next_remb: Instant,
	/// Whether a cap was ever set, so REMB has a ceiling to keep current.
	steered: bool,
}

impl IngestRate {
	/// When the next REMB is due, once a cap was set and there is a line to send it on.
	fn remb_at(&self) -> Option<Instant> {
		(self.steered && !self.remb.is_empty()).then_some(self.next_remb)
	}
}

/// Per-session clock that rebases each ingested track's raw RTP timestamps onto
/// one timeline so audio and video stay in sync.
///
/// str0m hands us the RTP header timestamp verbatim
/// ([`MediaData::time`](str0m::media::MediaData::time)). Per RFC 3550 that base
/// is random and independent for each track, and str0m applies no RTCP
/// sender-report correlation, so publishing the values as-is would desync audio
/// from video (their bases differ by hours) and start the broadcast at an
/// arbitrary offset. Until every track has an RTCP sender report, we anchor each
/// track on its first frame's arrival. Once reports are available, their common
/// NTP clock replaces arrival time as the cross-track reference. The NTP epoch
/// is chosen so the transition can only move timestamps forward, never rewind a
/// track that has already been published.
#[derive(Default)]
pub(crate) struct IngestClock {
	/// Arrival time of the first frame seen in the session; the timeline origin.
	arrival_epoch: Option<Instant>,
	/// Remote NTP time corresponding to timestamp zero on the published timeline.
	ntp_epoch_us: Option<i128>,
	tracks: HashMap<str0m::media::Mid, IngestTrackClock>,
}

impl IngestClock {
	/// Record the newest RTP-to-NTP correlation for a track.
	///
	/// A report outside the wall range is ignored, keeping the track on arrival
	/// time: RFC 3550 lets a sender without a wall clock report NTP time zero.
	fn observe(&mut self, mid: str0m::media::Mid, sender: str0m::rtp::rtcp::SenderInfo) {
		let Ok(anchor) = SenderAnchor::new(sender) else {
			tracing::debug!(?mid, "ignoring a sender report outside the wall range");
			return;
		};
		self.tracks.entry(mid).or_default().sender = Some(anchor);
		self.establish_ntp_epoch();
	}

	/// Map a raw RTP-derived microsecond timestamp onto the session timeline.
	/// `arrival` is the packet's network time
	/// ([`MediaData::network_time`](str0m::media::MediaData::network_time)).
	fn normalize(&mut self, mid: str0m::media::Mid, arrival: Instant, media_us: u64) -> u64 {
		let epoch = *self.arrival_epoch.get_or_insert(arrival);
		let track = self.tracks.entry(mid).or_default();
		let offset = *track.arrival_offset_us.get_or_insert_with(|| {
			// Signed wall delta from the epoch: a track whose first frame we dequeue
			// after the epoch frame may have actually arrived *before* it, and that
			// lead must pull its timeline earlier (not clamp to the epoch via an
			// unsigned subtraction) so it stays in sync.
			let wall_us = if arrival >= epoch {
				arrival.duration_since(epoch).as_micros() as i64
			} else {
				-(epoch.duration_since(arrival).as_micros() as i64)
			};
			wall_us as i128 - media_us as i128
		});
		let fallback = to_u64(media_us as i128 + offset);
		let previous = track.last_output_us;
		track.last_media_us = Some(media_us);
		track.last_output_us = Some(fallback);

		self.establish_ntp_epoch();
		let mapped = self
			.ntp_epoch_us
			.zip(self.tracks.get(&mid).and_then(|track| track.sender))
			.map(|(epoch, sender)| to_u64(sender.capture_time_us(media_us) - epoch));
		let output = match mapped {
			// The epoch chosen during the transition makes `mapped >= fallback`.
			// Later sender reports can make tiny clock corrections, so retain strict
			// monotonicity if one would otherwise move this track backwards.
			Some(mapped) => mapped.max(previous.map_or(fallback, |last| last.saturating_add(1))),
			None => fallback,
		};
		self.tracks.get_mut(&mid).expect("track was inserted").last_output_us = Some(output);
		output
	}

	/// Switch to the sender's common clock once every negotiated track has both
	/// a media sample and an RTCP sender report.
	fn establish_ntp_epoch(&mut self) {
		// A single RTP clock needs rebasing but no cross-track synchronization.
		// Waiting for two observed tracks also avoids a dormant negotiated m-line
		// preventing active audio and video from ever switching to sender reports.
		if self.ntp_epoch_us.is_some() || self.tracks.len() < 2 {
			return;
		}

		let mut epoch = i128::MAX;
		for track in self.tracks.values() {
			let (Some(sender), Some(media_us), Some(output_us)) =
				(track.sender, track.last_media_us, track.last_output_us)
			else {
				return;
			};
			// Choosing the minimum candidate makes every track's NTP-derived
			// timestamp at least its last published timestamp. The common timeline
			// may jump forward, but no individual track can rewind.
			epoch = epoch.min(sender.capture_time_us(media_us) - output_us as i128);
		}
		self.ntp_epoch_us = Some(epoch);
	}
}

#[derive(Default)]
struct IngestTrackClock {
	arrival_offset_us: Option<i128>,
	sender: Option<SenderAnchor>,
	last_media_us: Option<u64>,
	last_output_us: Option<u64>,
}

#[derive(Clone, Copy)]
struct SenderAnchor {
	ntp_us: i128,
	rtp_us: i128,
}

impl SenderAnchor {
	fn new(sender: str0m::rtp::rtcp::SenderInfo) -> std::result::Result<Self, kio::clock::OutOfRange> {
		let wall = kio::clock::Wall::try_from(sender.ntp_time)?;
		Ok(Self {
			ntp_us: wall.saturating_duration_since(kio::clock::Wall::UNIX_EPOCH).as_micros() as i128,
			rtp_us: media_time_to_micros(&sender.rtp_time) as i128,
		})
	}

	fn capture_time_us(self, media_us: u64) -> i128 {
		self.ntp_us + media_us as i128 - self.rtp_us
	}
}

fn to_u64(value: i128) -> u64 {
	value.clamp(0, u64::MAX as i128) as u64
}

/// Log a finished session at the right level, then fold an ordinary peer disconnect
/// ([`Error::SessionClosed`]) into `Ok` so only genuine failures error. Keeps normal
/// WebRTC churn out of the warning stream. `role` labels the path (e.g. `"whip server"`).
pub(crate) fn end(role: &str, result: Result<()>) -> Result<()> {
	match &result {
		Ok(()) | Err(Error::SessionClosed) => tracing::debug!(role, "session ended"),
		// An abandoned offer (peer answered but never connected) is normal churn, not
		// a failure: keep it out of the warning stream.
		Err(Error::IceTimeout) => tracing::debug!(role, "session ended: ICE never connected"),
		Err(err) => tracing::warn!(%err, role, "session ended"),
	}
	match result {
		Ok(()) | Err(Error::SessionClosed) => Ok(()),
		Err(err) => Err(err),
	}
}

/// Convert a str0m [`MediaTime`](str0m::media::MediaTime) to microseconds.
fn media_time_to_micros(time: &str0m::media::MediaTime) -> u64 {
	// MediaTime stores `numer / denom` seconds; cast through i128 so the
	// product doesn't overflow at 90 kHz video timestamps.
	let numer = time.numer() as i128;
	let denom = time.denom() as i128;
	if denom == 0 {
		return 0;
	}
	let micros = (numer.saturating_mul(1_000_000)) / denom;
	micros.max(0) as u64
}

/// Type-erased map of `Mid` -> codec bridge, populated as `MediaAdded`
/// events arrive on the ingest side.
pub(crate) struct Bridges {
	inner: HashMap<str0m::media::Mid, Box<dyn codec::Bridge>>,
}

impl Bridges {
	pub fn new() -> Self {
		Self { inner: HashMap::new() }
	}

	pub fn insert(&mut self, mid: str0m::media::Mid, bridge: Box<dyn codec::Bridge>) {
		self.inner.insert(mid, bridge);
	}

	pub fn push(&mut self, mid: str0m::media::Mid, frame: codec::Frame) -> Result<()> {
		if let Some(bridge) = self.inner.get_mut(&mid) {
			bridge.push(frame)?;
		}
		Ok(())
	}

	/// Finish every bridge's track after the final frame.
	pub fn finish(&mut self) -> Result<()> {
		for bridge in self.inner.values_mut() {
			bridge.finish()?;
		}
		Ok(())
	}

	/// Abort every bridge's track with `err` so subscribers see the real cause
	/// rather than a bare `Error::Dropped`.
	///
	/// Aborting consumes each bridge, so the map is emptied: the session is over.
	pub fn abort(&mut self, err: moq_net::Error) {
		for bridge in std::mem::take(&mut self.inner).into_values() {
			bridge.abort(err.clone());
		}
	}
}

/// Build a [`Rtc`] with `CodecConfig` restricted to the supplied codecs.
///
/// Used by the two egress paths so we don't advertise codecs we have no
/// source for in the catalog (WHIP client) or accept incoming codecs we
/// can't fulfil (WHEP server). For both, the negotiated SDP intersects with
/// what we can actually deliver, so `MediaAdded` only fires for codecs that
/// [`crate::egress::EgressSource`] can match to a rendition.
pub fn rtc_config_with_codecs(codecs: &[str0m::format::Codec]) -> str0m::RtcConfig {
	use str0m::format::Codec;
	// str0m fulfils NACK resends from the video send buffer (default 1000
	// packets). MoQ has no PLI path back to the publisher, so a late joiner's
	// recovery is whatever the peer can NACK out of this buffer while the current
	// group is still in flight. Widen it so a large keyframe plus the rest of the
	// group stays recoverable instead of aging out after ~1000 packets.
	let mut config = str0m::RtcConfig::new()
		.clear_codecs()
		.set_send_buffer_video(EGRESS_SEND_BUFFER_VIDEO);
	for c in codecs {
		config = match c {
			Codec::Opus => config.enable_opus(true, false),
			Codec::H264 => config.enable_h264(true),
			Codec::H265 => config.enable_h265(true),
			Codec::Vp8 => config.enable_vp8(true),
			Codec::Vp9 => config.enable_vp9(true),
			Codec::Av1 => config.enable_av1(true),
			// Any other codec str0m grows is one we have no egress source for.
			_ => config,
		};
	}
	config
}

/// Build a codec-restricted [`Rtc`] for the client egress path (which lets
/// str0m mint its own ICE credentials). The server egress path uses
/// [`rtc_config_with_codecs`] directly so it can inject the mux's known
/// credentials before building.
pub fn rtc_with_codecs(codecs: &[str0m::format::Codec]) -> Rtc {
	rtc_config_with_codecs(codecs).build(kio::clock::now().into_std())
}

/// Bind an ephemeral UDP socket for a single client session, read by its own
/// [`moq_ice::reader`], with the ICE candidates to advertise.
///
/// The client paths are 1:1 (one socket per dialed session, no demux); the
/// server paths share one socket via `crate::server::mux` instead. `advertise`
/// IPs are used verbatim (reusing the bound port); empty falls back to the
/// bound address, substituting loopback when that address is unspecified.
pub async fn bind_udp(advertise: &[SocketAddr]) -> Result<moq_ice::Socket> {
	let udp = Arc::new(UdpSocket::bind(("0.0.0.0", 0)).await?);
	let locals = advertised_candidates(advertise, udp.local_addr()?)?;
	Ok(moq_ice::Socket::new(udp.clone(), locals, moq_ice::reader(udp))?)
}

/// Pair configured ICE candidates with the bound UDP port and validate them.
pub(crate) fn advertised_candidates(advertise: &[SocketAddr], local: SocketAddr) -> Result<Vec<SocketAddr>> {
	let port = local.port();
	let candidates = if advertise.is_empty() {
		let ip = match local.ip() {
			IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
			IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
			ip => ip,
		};

		let candidate = SocketAddr::new(ip, port);
		if candidate != local {
			tracing::info!(bound = %local, advertised = %candidate, "webrtc udp bind is unspecified, advertising loopback ICE candidate");
		}
		vec![candidate]
	} else {
		// Reuse the bound port across each advertised IP, since str0m's ICE agent
		// picks the destination port from the candidate it's pairing against.
		advertise.iter().map(|addr| SocketAddr::new(addr.ip(), port)).collect()
	};

	for addr in &candidates {
		Candidate::host(*addr, "udp").map_err(Error::rtc)?;
	}
	Ok(candidates)
}

#[cfg(test)]
mod tests {
	use std::time::{Duration, SystemTime, UNIX_EPOCH};

	use str0m::media::Mid;
	use str0m::rtp::Ssrc;
	use str0m::rtp::rtcp::SenderInfo;

	use super::*;

	#[test]
	fn sender_reports_outside_the_wall_range_are_ignored() {
		let mut clock = IngestClock::default();
		let audio = Mid::from("0");
		let video = Mid::from("1");
		let t0 = Instant::now();
		assert_eq!(clock.normalize(audio, t0, 0), 0);
		assert_eq!(clock.normalize(video, t0 + Duration::from_millis(50), 0), 50_000);

		// NTP time zero (1900), which a sender without a wall clock may report, and past 2554.
		let ntp_zero = UNIX_EPOCH - Duration::from_secs(2_208_988_800);
		clock.observe(audio, sender_info(1, ntp_zero, 0));
		clock.observe(
			video,
			sender_info(2, UNIX_EPOCH + Duration::from_secs(18_446_744_074), 0),
		);
		assert!(clock.tracks.values().all(|track| track.sender.is_none()));

		// Both tracks stay on arrival time.
		assert_eq!(
			clock.normalize(audio, t0 + Duration::from_secs(1), 1_000_000),
			1_000_000
		);
		assert_eq!(
			clock.normalize(video, t0 + Duration::from_millis(1_050), 1_000_000),
			1_050_000
		);
		assert!(SenderAnchor::new(sender_info(1, UNIX_EPOCH, 0)).is_ok());
	}

	#[test]
	fn media_flow_respects_negotiated_direction() {
		use str0m::media::Direction::{Inactive, RecvOnly, SendOnly, SendRecv};

		assert!(MediaFlow::Ingest.accepts(RecvOnly));
		assert!(MediaFlow::Ingest.accepts(SendRecv));
		assert!(!MediaFlow::Ingest.accepts(SendOnly));
		assert!(!MediaFlow::Ingest.accepts(Inactive));

		assert!(MediaFlow::Egress.accepts(SendOnly));
		assert!(MediaFlow::Egress.accepts(SendRecv));
		assert!(!MediaFlow::Egress.accepts(RecvOnly));
		assert!(!MediaFlow::Egress.accepts(Inactive));
	}

	#[test]
	fn advertised_candidates_use_loopback_for_unspecified_ipv4() {
		let local: SocketAddr = "0.0.0.0:4444".parse().unwrap();
		let candidates = advertised_candidates(&[], local).unwrap();
		assert_eq!(candidates, vec!["127.0.0.1:4444".parse().unwrap()]);
	}

	#[test]
	fn advertised_candidates_use_loopback_for_unspecified_ipv6() {
		let local: SocketAddr = "[::]:4444".parse().unwrap();
		let candidates = advertised_candidates(&[], local).unwrap();
		assert_eq!(candidates, vec!["[::1]:4444".parse().unwrap()]);
	}

	#[test]
	fn advertised_candidates_keep_bound_address_when_specific() {
		let local: SocketAddr = "127.0.0.1:4444".parse().unwrap();
		assert_eq!(advertised_candidates(&[], local).unwrap(), vec![local]);
	}

	#[test]
	fn advertised_candidates_reuse_bound_port_for_configured_addresses() {
		let local: SocketAddr = "0.0.0.0:4444".parse().unwrap();
		let advertised = vec!["127.0.0.1:1000".parse().unwrap(), "[::1]:2000".parse().unwrap()];

		assert_eq!(
			advertised_candidates(&advertised, local).unwrap(),
			vec!["127.0.0.1:4444".parse().unwrap(), "[::1]:4444".parse().unwrap()]
		);
	}

	#[test]
	fn advertised_candidates_reject_configured_unspecified_addresses() {
		let local: SocketAddr = "127.0.0.1:4444".parse().unwrap();
		let advertised = vec!["0.0.0.0:1000".parse().unwrap()];
		assert!(advertised_candidates(&advertised, local).is_err());
	}

	#[test]
	fn ingest_clock_tracks_rtp_delta_within_track() {
		let mut clock = IngestClock::default();
		let mid = Mid::from("0");
		let t0 = Instant::now();
		// Raw RTP base is a large random value; the first frame must map to 0.
		assert_eq!(clock.normalize(mid, t0, 5_000_000_000), 0);
		// A later frame advances by the RTP delta, not by arrival jitter.
		let arrival = t0 + Duration::from_millis(17); // jittered arrival, ignored after anchor
		assert_eq!(clock.normalize(mid, arrival, 5_000_020_000), 20_000);
	}

	#[test]
	fn ingest_clock_keeps_tracks_in_sync_via_arrival() {
		let mut clock = IngestClock::default();
		let audio = Mid::from("0");
		let video = Mid::from("1");
		let t0 = Instant::now();
		// Audio anchors the session at 0 with its own random RTP base.
		assert_eq!(clock.normalize(audio, t0, 1_000_000_000), 0);
		// Video's first frame arrives 5 ms later with an unrelated RTP base; it
		// must land at +5 ms on the shared timeline, not at video's raw base.
		let video_arrival = t0 + Duration::from_millis(5);
		assert_eq!(clock.normalize(video, video_arrival, 8_000_000_000), 5_000);
		// And then track its own RTP delta.
		assert_eq!(
			clock.normalize(video, video_arrival + Duration::from_millis(33), 8_000_033_000),
			38_000
		);
	}

	#[test]
	fn ingest_clock_handles_track_arriving_before_epoch() {
		let mut clock = IngestClock::default();
		let audio = Mid::from("0");
		let video = Mid::from("1");
		let t0 = Instant::now();
		// Audio's MediaData is dequeued first and sets the epoch at t0.
		assert_eq!(clock.normalize(audio, t0, 1_000_000), 0);
		// Video's first frame actually arrived 5 ms *before* the epoch. Its lead
		// pulls the start below zero (clamped to 0), and a frame 33 ms into video
		// lands 28 ms onto the shared timeline (33 ms - the 5 ms head start).
		let video_arrival = t0 - Duration::from_millis(5);
		assert_eq!(clock.normalize(video, video_arrival, 8_000_000), 0);
		assert_eq!(
			clock.normalize(video, video_arrival + Duration::from_millis(33), 8_033_000),
			28_000
		);
	}

	#[test]
	fn ingest_clock_replaces_arrival_jitter_with_sender_report_sync() {
		let mut clock = IngestClock::default();
		let audio = Mid::from("0");
		let video = Mid::from("1");
		let t0 = Instant::now();
		let audio_base = 1_000_000_000;
		let video_base = 8_000_000_000;

		assert_eq!(clock.normalize(audio, t0, audio_base), 0);
		assert_eq!(
			clock.normalize(video, t0 + Duration::from_millis(50), video_base),
			50_000
		);
		assert_eq!(
			clock.normalize(audio, t0 + Duration::from_secs(1), audio_base + 1_000_000),
			1_000_000
		);
		assert_eq!(
			clock.normalize(video, t0 + Duration::from_millis(1_050), video_base + 1_000_000,),
			1_050_000
		);

		// Both reports identify the same capture instant despite unrelated RTP
		// bases. The 50 ms first-packet arrival skew must disappear permanently.
		let report_time = UNIX_EPOCH + Duration::from_secs(1_700_000_001);
		clock.observe(audio, sender_info(1, report_time, audio_base + 1_000_000));
		clock.observe(video, sender_info(2, report_time, video_base + 1_000_000));

		let audio_time = clock.normalize(audio, t0 + Duration::from_millis(1_020), audio_base + 1_020_000);
		let video_time = clock.normalize(video, t0 + Duration::from_millis(1_070), video_base + 1_020_000);
		assert_eq!(audio_time, video_time);
		assert_eq!(audio_time, 1_070_000);
	}

	fn sender_info(ssrc: u32, ntp_time: SystemTime, rtp_us: u64) -> SenderInfo {
		SenderInfo {
			ssrc: Ssrc::from(ssrc),
			ntp_time,
			rtp_time: str0m::media::MediaTime::from_micros(rtp_us),
			sender_packet_count: 0,
			sender_octet_count: 0,
		}
	}
}
