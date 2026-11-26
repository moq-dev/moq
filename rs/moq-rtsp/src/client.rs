//! Pull one RTSP session into a broadcast.

use futures::StreamExt;
use moq_mux::codec::{aac, h264, h265};
use moq_tokio::RedactedUrl;
use retina::client::{Credentials, PlayOptions, Session, SessionOptions, SetupOptions, Transport};
use retina::codec::{
	CodecItem, FrameFormat, ParameterSetInsertion, ParametersRef, VideoParameters, VideoParametersCodec,
};
use url::Url;

use crate::{Error, Result};

/// Annex-B video and raw AAC with its AudioSpecificConfig out of band.
///
/// retina strips in-band parameter sets and would re-insert them only ahead of an IDR, missing
/// an H.265 CRA and an H.264 recovery point, so the ingest hands them to the splitter instead.
const FRAME_FORMAT: FrameFormat = FrameFormat {
	h26x_framing: retina::codec::h26x::Framing::AnnexB,
	parameter_set_insertion: ParameterSetInsertion::Never,
	aac_framing: retina::codec::aac::Framing::Raw,
};

/// How far a frame may land behind its stream's previous one before the session counts as
/// restarted. Generous enough for B-frame reordering.
const MAX_BACKWARD: std::time::Duration = std::time::Duration::from_secs(1);

/// How far a frame may land ahead of its stream's previous one, beyond the time between their
/// arrivals, before the session counts as restarted. A stream that pauses (motion-triggered
/// video beside continuous audio) returns that far ahead, while a camera resetting its RTP clock
/// lands anywhere in a 32-bit range, hours away.
const MAX_FORWARD: std::time::Duration = std::time::Duration::from_secs(10);

/// How long a request may go unanswered, or the session without a frame, before it counts as
/// stuck. retina's keepalives only prove the server answers, not that the camera sends media.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How long every video stream may go without a keyframe after PLAY. One that never sends any
/// would withhold the catalog while the session's other streams keep [`TIMEOUT`] from firing.
/// Generous, since a camera's "smart" codec can stretch the keyframe interval well past 10s.
const KEYFRAME_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// How long after its first frame a session waits for every stream's RTCP sender report: RFC 3550's
/// minimum report interval, which a sender keeping it reports within.
const REPORT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// The most frames a session holds while it waits for its sender reports: past it, the session is
/// placed at once, so a camera's frame rate never sizes what the wait keeps.
const MAX_HELD: usize = 1024;

/// The most payload and parameter set bytes a session holds while it waits for its sender
/// reports, as many as one group's live tail keeps: past it, the session is placed at once, so a camera's bitrate never
/// sizes what the wait keeps either.
const MAX_HELD_BYTES: usize = 32 * 1024 * 1024;

/// How far after the timebase's zero a session's earliest stream starts, as a fresh catalog clock
/// leads its own: room for a session sharing the catalog that started a little earlier, and for
/// frames a server replays from before retina's first timestamp.
const LEAD: std::time::Duration = std::time::Duration::from_secs(10);

/// How long before its arrival a sender report may say a frame was captured: longer than any
/// encoder and network take. Further back is a camera clock set wrong, or never set.
const MAX_CAPTURE_LATENCY: std::time::Duration = std::time::Duration::from_secs(5);

/// How far after its arrival a sender report may say a frame was captured, a camera clock
/// slightly ahead of this host's: timed at its arrival instead. Further ahead is a clock set wrong.
const MAX_CAPTURE_LEAD: std::time::Duration = std::time::Duration::from_secs(1);

/// Why [`Client::pull`] returned without an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum End {
	/// The server ended the session.
	Closed,
	/// The session's RTP timeline jumped, so its tracks finished before the jump.
	///
	/// Its timestamps no longer continue the broadcast's, so the caller finishes the
	/// broadcast and restarts every session sharing it under a fresh instance.
	Jumped,
}

/// An RTSP source to pull, such as an IP camera's stream URL.
///
/// Credentials ride the URL's userinfo, as every RTSP tool takes them
/// (`rtsp://user:pass@camera/stream`).
#[derive(Clone)]
pub struct Client {
	/// The URL without its userinfo.
	url: Url,
	creds: Option<Credentials>,
	/// [`TIMEOUT`], lengthened by tests so only [`KEYFRAME_TIMEOUT`] fires.
	timeout: std::time::Duration,
	/// [`MAX_FORWARD`], shortened by tests.
	max_forward: std::time::Duration,
	/// [`REPORT_INTERVAL`], shortened by tests.
	report_wait: std::time::Duration,
}

impl Client {
	/// Pull from `url`, taking any credentials out of its userinfo.
	pub fn new(mut url: Url) -> Result<Self> {
		if url.scheme() != "rtsp" {
			return Err(Error::InvalidUrl(format!(
				"expected the rtsp scheme, got {}",
				url.scheme()
			)));
		}
		if url.host_str().is_none() {
			return Err(Error::InvalidUrl("missing host".to_string()));
		}

		let creds = if url.username().is_empty() && url.password().is_none() {
			None
		} else {
			let decode = |part: &str| {
				percent_encoding::percent_decode_str(part)
					.decode_utf8()
					.map(|part| part.into_owned())
					.map_err(|_| Error::InvalidUrl("credentials are not UTF-8".to_string()))
			};
			let creds = Credentials {
				username: decode(url.username())?,
				password: decode(url.password().unwrap_or_default())?,
			};
			// retina refuses a URL that still carries userinfo.
			let _ = url.set_username("");
			let _ = url.set_password(None);
			Some(creds)
		};

		Ok(Self {
			url,
			creds,
			timeout: TIMEOUT,
			max_forward: MAX_FORWARD,
			report_wait: REPORT_INTERVAL,
		})
	}

	/// Ingest one session into `broadcast`, publishing its renditions on `catalog`.
	///
	/// Publishes the session's H.264 and H.265 video and AAC audio, starting on a keyframe.
	/// Audio the stack cannot play is skipped with a warning.
	///
	/// Each frame lands on the catalog's clock at its capture: where the camera's RTCP sender report
	/// for its stream says it was taken, or, for a stream without a usable report, no later than
	/// any of its frames arrived. The session waits up to 5 s after its first frame (RFC 3550's
	/// minimum report interval: a camera reporting less often is placed on its arrivals) for every
	/// stream's report to agree with its arrivals, holding the catalog and each stream's frames
	/// since its latest keyframe meanwhile. It stops waiting sooner once it holds 1024 frames or
	/// 32 MiB, or when the session ends or its timeline jumps. Streams are placed apart, since a camera's `rtptime` need not
	/// align them, so a stream without a report trails its capture by the camera's encoding and
	/// the network. A clock fixed before the session (a shared catalog, or
	/// [`Config::with_clock`](moq_mux::catalog::Config::with_clock)) is shifted onto, so several
	/// sessions sharing a catalog line up by wall time.
	///
	/// Never reconnects: returns once the session ends or its timeline jumps, finishing its
	/// tracks, or fails, aborting them, including when a request or the media stalls. The
	/// caller owns the broadcast and decides whether to retry; after [`End::Jumped`], on a fresh
	/// broadcast and catalog, since the new timeline is new content. Dropping the future stops the
	/// session and finishes its tracks.
	pub async fn pull(
		&self,
		broadcast: &moq_net::broadcast::Producer,
		catalog: &moq_mux::catalog::Producer,
	) -> Result<End> {
		let options = SessionOptions::default()
			.creds(self.creds.clone())
			.user_agent(concat!("moq-rtsp/", env!("CARGO_PKG_VERSION")).to_string());
		let mut session = self.within(Session::describe(self.url.clone(), options)).await??;

		// Every stream of one session shares one timebase, so its audio and video stay in sync,
		// while another session sharing the catalog gets its own.
		let timebase = catalog.timebase();
		let mut tracks = Vec::new();
		let mut skipped = Vec::new();
		for (index, stream) in session.streams().iter().enumerate() {
			let kind = match (stream.media(), stream.encoding_name()) {
				("video", "h264") => Kind::H264,
				("video", "h265") => Kind::H265,
				("audio", "mpeg4-generic") => Kind::Aac,
				(media, encoding) => {
					skipped.push(format!("{media}/{encoding}"));
					continue;
				}
			};
			tracks.push((index, kind));
		}

		let url = RedactedUrl::new(&self.url);
		if tracks.is_empty() {
			return Err(Error::NoSupportedStream(skipped.join(", ")));
		}
		if !skipped.is_empty() {
			tracing::warn!(%url, skipped = %skipped.join(", "), "skipping RTSP streams with a codec the stack cannot play");
		}

		for (index, _) in &tracks {
			let setup = SetupOptions::default()
				.transport(Transport::Tcp(Default::default()))
				.frame_format(FRAME_FORMAT);
			self.within(session.setup(*index, setup)).await??;
		}

		let reserved = catalog.reserve();
		let mut ingest = Ingest {
			streams: Vec::new(),
			video: tracks.iter().any(|(_, kind)| *kind != Kind::Aac),
			started: false,
			max_forward: self.max_forward,
			timebase,
			// Its own reservation holds the catalog, and so the clock, until the session is placed.
			timing: Timing::Waiting(Box::new(Waiting {
				gate: catalog.reserve(),
				wait: self.report_wait,
				since: None,
				held: Vec::new(),
				bytes: 0,
			})),
		};
		ingest.streams.resize_with(session.streams().len(), || None);
		// A failure here aborts the tracks already made rather than leaving `Drop` to finish them.
		let built = (|| -> Result<()> {
			for (index, kind) in &tracks {
				let track = match kind {
					Kind::H264 => {
						let track = broadcast.unique_track(".avc3", catalog.track_info(hang::catalog::PRIORITY.video));
						Track::Video(Box::new(Video::H264 {
							split: h264::Split::new(),
							import: h264::Import::new(track, reserved.clone(), Default::default())?,
						}))
					}
					Kind::H265 => {
						let track = broadcast.unique_track(".hev1", catalog.track_info(hang::catalog::PRIORITY.video));
						Track::Video(Box::new(Video::H265 {
							split: h265::Split::new(),
							import: h265::Import::new(track, reserved.clone(), Default::default())?,
						}))
					}
					Kind::Aac => {
						let Some(ParametersRef::Audio(params)) = session.streams()[*index].parameters() else {
							return Err(Error::Rtsp("AAC stream without an AudioSpecificConfig".to_string()));
						};
						let config = aac::config(params.extra_data())?;
						let track = broadcast.unique_track(".aac", catalog.track_info(hang::catalog::PRIORITY.audio));
						Track::Aac(Box::new(aac::Import::new(track, reserved.clone(), config)?))
					}
				};
				ingest.streams[*index] = Some(Stream {
					track,
					last: None,
					started: false,
					held: None,
					opened: None,
					fresh: true,
					clock: StreamClock::Unplaced {
						report: None,
						bound: None,
					},
				});
			}
			Ok(())
		})();
		if let Err(err) = built {
			ingest.abort(moq_net::Error::Transport(err.to_string()));
			return Err(err);
		}
		// The catalog waits for each video rendition's first keyframe, not for this.
		drop(reserved);

		// Each stream counts from its own first packet: its sender report or its arrivals place it,
		// as `rtptime` may be missing on some streams, or name a time after the frames a server
		// replays from before PLAY, and retina does not say which streams it aligned.
		let play = PlayOptions::default().initial_timestamp(retina::client::InitialTimestampPolicy::Ignore);
		let result: Result<End> = async {
			let mut session = self.within(session.play(play)).await??.demuxed()?;
			tracing::info!(%url, streams = tracks.len(), "RTSP session playing");
			let playing = kio::clock::now();
			// Measured from the last frame, so RTCP reports alone cannot keep a dead camera alive.
			let mut deadline = playing.saturating_add(self.timeout);
			let end = loop {
				// A report wait that ends first releases the frames it held, with or without another.
				let due = ingest.timing.due().filter(|due| *due < deadline);
				let item = match kio::clock::timeout_at(due.unwrap_or(deadline), session.next()).await {
					Ok(item) => item,
					Err(_) if due.is_some() => {
						ingest.expire(false)?;
						continue;
					}
					Err(_) => return Err(Error::Stalled(self.timeout)),
				};
				let Some(item) = item else {
					// A session ending during a report wait still publishes what it held.
					ingest.expire(true)?;
					break End::Closed;
				};
				let item = item?;
				if matches!(item, CodecItem::VideoFrame(_) | CodecItem::AudioFrame(_)) {
					deadline = kio::clock::now().saturating_add(self.timeout);
				}
				if ingest.push(item, session.streams())?.is_some() {
					break End::Jumped;
				}
				if ingest.awaiting_keyframe(playing, KEYFRAME_TIMEOUT) {
					return Err(Error::Stalled(KEYFRAME_TIMEOUT));
				}
			};
			if end == End::Jumped {
				tracing::warn!(%url, "RTSP timeline jumped, ending the session");
			}
			Ok(end)
		}
		.await;

		match result {
			Ok(end) => {
				ingest.finish()?;
				Ok(end)
			}
			Err(err) => {
				ingest.abort(moq_net::Error::Transport(err.to_string()));
				Err(err)
			}
		}
	}
}

impl Client {
	/// Fail `step` once it takes longer than the timeout.
	async fn within<T>(&self, step: impl Future<Output = T>) -> Result<T> {
		kio::clock::timeout(self.timeout, step)
			.await
			.map_err(|_| Error::Stalled(self.timeout))
	}
}

impl std::fmt::Debug for Client {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		// Never the credentials.
		f.debug_struct("Client")
			.field("url", &RedactedUrl::new(&self.url))
			.finish_non_exhaustive()
	}
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
	H264,
	H265,
	Aac,
}

enum Track {
	Video(Box<Video>),
	Aac(Box<aac::Import>),
}

impl Track {
	/// End the track cleanly. It leaves the broadcast with this handle.
	fn finish(self) -> moq_mux::Result<()> {
		match self {
			Self::Video(video) => match *video {
				Video::H264 { import, .. } => import.finish().map(drop),
				Video::H265 { import, .. } => import.finish().map(drop),
			},
			Self::Aac(import) => import.finish().map(drop),
		}
	}

	fn abort(self, err: moq_net::Error) {
		match self {
			Self::Video(video) => match *video {
				Video::H264 { import, .. } => import.abort(err),
				Video::H265 { import, .. } => import.abort(err),
			},
			Self::Aac(import) => import.abort(err),
		}
	}
}

enum Video {
	H264 { split: h264::Split, import: h264::Import },
	H265 { split: h265::Split, import: h265::Import },
}

impl Video {
	/// Split one whole access unit into frames, inserting `params` ahead of it.
	///
	/// Flushes, since retina hands over no partial access units.
	fn split(
		&mut self,
		data: &[u8],
		params: Option<&VideoParameters>,
		pts: moq_net::Timestamp,
	) -> moq_mux::Result<Vec<moq_mux::container::Frame>> {
		let mut prefix = Vec::new();
		if let Some(params) = params {
			let sets: &[&[u8]] = match params.codec_params() {
				VideoParametersCodec::H264 { sps, pps } => &[sps, pps],
				VideoParametersCodec::H265 { vps, sps, pps } => &[vps, sps, pps],
				_ => &[],
			};
			for set in sets {
				prefix.extend_from_slice(&[0, 0, 0, 1]);
				prefix.extend_from_slice(set);
			}
		}

		// The splitter parses a stream, so the parameter sets and the access unit can arrive apart.
		let (mut frames, more, flushed) = match self {
			Self::H264 { split, .. } => (split.decode(&prefix, pts)?, split.decode(data, pts)?, split.flush(pts)?),
			Self::H265 { split, .. } => (split.decode(&prefix, pts)?, split.decode(data, pts)?, split.flush(pts)?),
		};
		frames.extend(more);
		frames.extend(flushed);
		Ok(frames)
	}

	/// Drop any access unit the splitter holds, keeping its parameter sets.
	fn reset(&mut self) {
		match self {
			Self::H264 { split, .. } => split.reset(),
			Self::H265 { split, .. } => split.reset(),
		}
	}

	fn decode(&mut self, frames: Vec<moq_mux::container::Frame>) -> moq_mux::Result<()> {
		match self {
			Self::H264 { import, .. } => import.decode(frames),
			Self::H265 { import, .. } => import.decode(frames),
		}
	}
}

struct Stream {
	track: Track,
	/// The previous frame received's timestamp, in the stream's clock units since the session
	/// start, and when it arrived.
	last: Option<(i64, kio::clock::Instant)>,
	/// Whether a keyframe has been published (video) or the session started (audio).
	started: bool,
	/// When a lost packet broke the video's reference chain, so it waits for a keyframe.
	held: Option<kio::clock::Instant>,
	/// The timestamp of the keyframe the video last started on. A picture displayed before it, such
	/// as an open-GOP CRA's leading picture, references what came before, so it never publishes.
	opened: Option<i64>,
	/// Whether retina's parameter sets still need inserting: until the splitter emits a keyframe
	/// carrying them, after which it re-inserts them ahead of every keyframe itself.
	fresh: bool,
	/// Where the stream's RTP time zero is on the wall clock.
	clock: StreamClock,
}

/// Where one stream's RTP time zero is on the wall clock, in Unix microseconds.
///
/// Each stream counts from its own first packet, so each is placed on its own.
#[derive(Clone, Copy)]
enum StreamClock {
	/// Not placed yet.
	Unplaced {
		/// Where the camera's latest sender report puts it.
		report: Option<i128>,
		/// The latest it can be: no frame was captured after it arrived. `None` before a frame.
		bound: Option<i128>,
	},
	/// Placed, for good: moving it would step the stream's timestamps.
	Placed(i128),
}

impl Stream {
	/// Whether a frame `elapsed` units of `rate` Hz in, which arrived at `received`, landed too far
	/// from the stream's previous frame: a camera resetting its RTP clock.
	fn jumped(&self, elapsed: i64, rate: u32, received: kio::clock::Instant, max_forward: std::time::Duration) -> bool {
		let Some((last, arrived)) = self.last else {
			return false;
		};
		let delta = i128::from(elapsed) - i128::from(last);
		let slack = max_forward + received.saturating_duration_since(arrived);
		let forward = slack.as_micros() as i128 * i128::from(rate) / 1_000_000;
		delta < -backward(rate) || delta > forward
	}

	/// Keep a sender report putting the stream's zero at `zero`, while it is not placed.
	fn report(&mut self, zero: i128) {
		if let StreamClock::Unplaced { report, .. } = &mut self.clock {
			*report = Some(zero);
		}
	}

	/// Note a frame `elapsed` units of `rate` Hz in, which arrived at `arrival`.
	fn arrived(&mut self, elapsed: i64, rate: u32, arrival: i128) {
		if let StreamClock::Unplaced { bound, .. } = &mut self.clock {
			let zero = arrival - micros(elapsed, rate);
			*bound = Some(bound.map_or(zero, |bound| bound.min(zero)));
		}
	}

	/// Whether the stream's sender report settles its placement, or it no longer needs one.
	///
	/// A later frame only lowers the bound, so a report capturing frames too long before they
	/// arrived may still agree with the arrivals once live frames follow a replayed burst. A report
	/// that never agrees, from a camera clock set wrong, settles nothing either: the stream waits out
	/// the report interval as without one, since a replay drains for a while and holds the bound
	/// late until it is over.
	fn reported(&self) -> bool {
		match self.clock {
			StreamClock::Placed(_) => true,
			StreamClock::Unplaced { report: None, .. } => false,
			StreamClock::Unplaced {
				report: Some(report),
				bound,
			} => bound.is_none_or(|bound| usable(report, bound)),
		}
	}

	/// Place the stream at its sender report, or at its arrivals when it has none or the report is
	/// far from them, returning its zero; `None` before its first frame.
	fn place(&mut self, stream_id: usize) -> Option<i128> {
		let (report, bound) = match self.clock {
			StreamClock::Placed(zero) => return Some(zero),
			StreamClock::Unplaced { report, bound } => (report, bound?),
		};
		let zero = match report {
			Some(report) => {
				// The shortest any frame took from its capture to its arrival.
				let latency = bound - report;
				if usable(report, bound) {
					tracing::info!(
						stream_id,
						capture_latency_ms = latency as f64 / 1000.0,
						"stream placed at its sender report"
					);
					// A camera clock a little ahead puts capture after arrival: no frame is from the future.
					report.min(bound)
				} else {
					tracing::warn!(
						stream_id,
						latency_ms = latency as f64 / 1000.0,
						"sender report far from the frame's arrival, placing the stream at its arrival"
					);
					bound
				}
			}
			None => {
				tracing::info!(stream_id, "stream placed at its arrival, without a sender report");
				bound
			}
		};
		self.clock = StreamClock::Placed(zero);
		Some(zero)
	}
}

/// Whether a sender report putting a stream's zero at `report` agrees with its arrivals, which put
/// it no later than `bound`: within an encoder and network's latency of them.
fn usable(report: i128, bound: i128) -> bool {
	(-(MAX_CAPTURE_LEAD.as_micros() as i128)..=MAX_CAPTURE_LATENCY.as_micros() as i128).contains(&(bound - report))
}

/// The session's tracks, indexed by retina stream id.
struct Ingest {
	streams: Vec<Option<Stream>>,
	/// Whether the session carries video, so audio waits for its first keyframe.
	video: bool,
	/// Whether any video stream published its first keyframe.
	started: bool,
	/// [`MAX_FORWARD`], or a test's override.
	max_forward: std::time::Duration,
	/// Every stream's timestamp base, so the session shifts onto the catalog's clock by one offset.
	timebase: moq_mux::catalog::Timebase,
	/// Whether the session is placed on the catalog's clock yet.
	timing: Timing,
}

/// Where the session stands on the catalog's clock.
enum Timing {
	/// Not placed yet: holding the catalog, and the frames received meanwhile.
	Waiting(Box<Waiting>),
	/// Placed: the timebase's zero is `reference`, in Unix microseconds, [`LEAD`] before the
	/// earliest stream zero, so each stream's timestamps are its own moved by its zero's distance
	/// from it.
	Placed { reference: i128 },
}

struct Waiting {
	/// Withholds the catalog, so nothing fixes its clock before [`Timebase::place`].
	///
	/// [`Timebase::place`]: moq_mux::catalog::Timebase::place
	gate: moq_mux::catalog::Reserved,
	/// How long after the first held frame to wait for the sender reports: [`REPORT_INTERVAL`].
	wait: std::time::Duration,
	/// When the first frame was held, which the wait counts from.
	since: Option<kio::clock::Instant>,
	/// The frames to publish once placed, in order: each stream's from its latest keyframe, each
	/// with the video parameters its stream had then, since retina strips in-band parameter sets
	/// and a later frame may change the stream's.
	held: Vec<(CodecItem, Option<VideoParameters>)>,
	/// The payload and parameter set bytes `held` keeps.
	bytes: usize,
}

impl Timing {
	/// When a report wait holding frames ends.
	fn due(&self) -> Option<kio::clock::Instant> {
		match self {
			Self::Waiting(waiting) => waiting.since.map(|since| since.saturating_add(waiting.wait)),
			Self::Placed { .. } => None,
		}
	}
}

impl Waiting {
	/// Hold a frame of `stream_id`, keeping only what a decoder needs: a video stream from its
	/// latest keyframe on, and audio from the first video held on in a session with video, since
	/// audio waits for the video's first keyframe.
	fn hold(&mut self, item: CodecItem, params: Option<VideoParameters>, video: bool) {
		if let Some((stream_id, ..)) = media(&item) {
			self.since.get_or_insert_with(kio::clock::now);
			// A damaged keyframe publishes nothing, so it never evicts a clean group held before it.
			if matches!(&item, CodecItem::VideoFrame(frame) if frame.is_random_access_point() && frame.loss() == 0) {
				self.held
					.retain(|(held, _)| media(held).is_none_or(|(id, ..)| id != stream_id));
			}
		}
		self.held.push((item, params));
		if video {
			let first = self
				.held
				.iter()
				.position(|(held, _)| matches!(held, CodecItem::VideoFrame(_)))
				.unwrap_or(self.held.len());
			self.held.drain(..first);
		}
		self.bytes = self
			.held
			.iter()
			.map(|(held, params)| payload(held).len() + params.as_ref().map_or(0, |params| params.extra_data().len()))
			.sum();
	}
}

/// Seconds from the NTP epoch (1900) to the Unix one (1970).
const NTP_UNIX: i128 = 2_208_988_800;

/// An NTP timestamp (Q32.32 seconds since 1900) in Unix microseconds.
///
/// NTP seconds wrap in 2036; a value below 2^31 is read as the next era (RFC 4330 §3), so a camera
/// clock never set (zero) reads as 2036, far from any arrival.
fn ntp_unix_micros(ntp: u64) -> i128 {
	let mut secs = i128::from(ntp >> 32);
	if secs < 1 << 31 {
		secs += 1 << 32;
	}
	(secs - NTP_UNIX) * 1_000_000 + ((i128::from(ntp & 0xFFFF_FFFF) * 1_000_000) >> 32)
}

/// Microseconds from the Unix epoch to `wall`, negative before it.
fn unix_micros(wall: kio::clock::Wall) -> i128 {
	let epoch = kio::clock::Wall::UNIX_EPOCH;
	wall.saturating_duration_since(epoch).as_micros() as i128
		- epoch.saturating_duration_since(wall).as_micros() as i128
}

/// `elapsed` units of a `rate` Hz clock in microseconds, rounded down.
fn micros(elapsed: i64, rate: u32) -> i128 {
	(i128::from(elapsed) * 1_000_000).div_euclid(i128::from(rate))
}

/// A sender report's stream, and where it puts the stream's zero, in Unix microseconds.
fn sender_report(rtcp: &retina::rtcp::ReceivedCompoundPacket) -> Option<(usize, i128)> {
	let at = rtcp.rtp_timestamp()?;
	let Some(Ok(Some(report))) = rtcp.pkts().next().map(|pkt| pkt.as_sender_report()) else {
		return None;
	};
	let zero = ntp_unix_micros(report.ntp_timestamp().0) - micros(at.elapsed(), at.clock_rate().get());
	Some((rtcp.stream_id(), zero))
}

/// The video parameters `item`'s stream has now.
fn parameters<'a>(streams: &'a [retina::client::Stream], item: &CodecItem) -> Option<&'a VideoParameters> {
	let CodecItem::VideoFrame(frame) = item else {
		return None;
	};
	match streams.get(frame.stream_id())?.parameters()? {
		ParametersRef::Video(params) => Some(params),
		_ => None,
	}
}

/// A media item's payload, empty for anything else.
fn payload(item: &CodecItem) -> &[u8] {
	match item {
		CodecItem::VideoFrame(frame) => frame.data(),
		CodecItem::AudioFrame(frame) => frame.data(),
		_ => &[],
	}
}

/// A media item's stream, RTP timestamp, and arrival.
fn media(item: &CodecItem) -> Option<(usize, retina::Timestamp, kio::clock::Instant)> {
	match item {
		CodecItem::VideoFrame(frame) => Some((
			frame.stream_id(),
			frame.timestamp(),
			kio::clock::Instant::from_std(frame.end_ctx().received()),
		)),
		CodecItem::AudioFrame(frame) => Some((
			frame.stream_id(),
			frame.timestamp(),
			kio::clock::Instant::from_std(frame.ctx().received()),
		)),
		_ => None,
	}
}

/// Dropping the [`Client::pull`] future stops the session: its tracks end cleanly, so a caller
/// restarting a broadcast never leaves subscribers with a dropped track.
impl Drop for Ingest {
	fn drop(&mut self) {
		if let Err(err) = self.finish() {
			tracing::warn!(%err, "failed to finish an RTSP track");
		}
	}
}

/// [`MAX_BACKWARD`] in units of a `rate` Hz clock.
fn backward(rate: u32) -> i128 {
	MAX_BACKWARD.as_micros() as i128 * i128::from(rate) / 1_000_000
}

/// A frame's timestamp landed too far from its stream's previous one.
struct Jump;

impl Ingest {
	/// Publish one depacketized item, or report that the timeline jumped. Until the session is
	/// placed on the catalog's clock, items are held, then published in order.
	fn push(&mut self, item: CodecItem, streams: &[retina::client::Stream]) -> Result<Option<Jump>> {
		// A stream whose first frame comes after the session is placed is placed then, on these.
		match &item {
			CodecItem::Rtcp(rtcp) => {
				if let Some((stream_id, zero)) = sender_report(rtcp)
					&& let Some(stream) = self.streams.get_mut(stream_id).and_then(Option::as_mut)
				{
					stream.report(zero);
				}
			}
			item => {
				if let Some((stream_id, timestamp, received)) = media(item)
					&& let Some(stream) = self.streams.get_mut(stream_id).and_then(Option::as_mut)
				{
					let (elapsed, rate) = (timestamp.elapsed(), timestamp.clock_rate().get());
					// Checked as each frame arrives, held or not, so a reset during a report wait
					// neither evicts what was held nor moves the stream's placement.
					if stream.jumped(elapsed, rate, received, self.max_forward) {
						// What came before the jump still publishes.
						if self.ready(true) {
							self.release()?;
						}
						return Ok(Some(Jump));
					}
					stream.last = Some((elapsed, received));
					if matches!(stream.clock, StreamClock::Unplaced { .. }) {
						let now = kio::clock::now();
						let arrival = unix_micros(kio::clock::wall())
							- now.saturating_duration_since(received).as_micros() as i128;
						stream.arrived(elapsed, rate, arrival);
					}
				}
			}
		}

		let Timing::Waiting(waiting) = &mut self.timing else {
			let params = parameters(streams, &item);
			self.publish(item, params)?;
			return Ok(None);
		};
		if media(&item).is_some_and(|(stream_id, ..)| self.streams.get(stream_id).is_some_and(Option::is_some)) {
			let params = parameters(streams, &item).cloned();
			waiting.hold(item, params, self.video);
		}
		// Checked here too, not only when the pull loop wakes for it: a timeout polls the session
		// first, so frames that keep arriving ready would hold that wake off.
		if self.ready(false) {
			self.release()?;
		}
		Ok(None)
	}

	/// Whether a session still waiting can be placed: once it holds a frame, and every stream
	/// reported, the wait is over, the hold is full, or the session is `ending`.
	fn ready(&self, ending: bool) -> bool {
		let Timing::Waiting(waiting) = &self.timing else {
			return false;
		};
		let Some(since) = waiting.since else {
			return false;
		};
		ending
			|| waiting.held.len() >= MAX_HELD
			|| waiting.bytes >= MAX_HELD_BYTES
			|| kio::clock::now() >= since.saturating_add(waiting.wait)
			|| self.streams.iter().flatten().all(Stream::reported)
	}

	/// Place a session still waiting once its report wait is over, or once it ends, and publish
	/// what it held. The pull loop wakes for the first.
	fn expire(&mut self, ending: bool) -> Result<()> {
		if self.ready(ending) {
			self.release()?;
		}
		Ok(())
	}

	/// Place the session, and publish the frames it held.
	///
	/// Every stream holding a frame is placed; the earliest of their zeros is the timebase's.
	fn release(&mut self) -> Result<()> {
		let reference = self
			.streams
			.iter_mut()
			.enumerate()
			.filter_map(|(stream_id, stream)| stream.as_mut()?.place(stream_id))
			.min()
			.expect("a held frame bounds its stream")
			- LEAD.as_micros() as i128;

		// A host whose wall clock is unset has no wall to place on: the timebase then anchors on the
		// first frame's arrival, as any importer's does.
		if kio::clock::wall() >= kio::clock::Wall::MOQ_EPOCH {
			let wall = u64::try_from(reference)
				.ok()
				.and_then(|micros| kio::clock::Wall::UNIX_EPOCH.checked_add(std::time::Duration::from_micros(micros)))
				.ok_or_else(|| Error::Rtsp(format!("the session starts before 1970, at {reference} µs")))?;
			self.timebase.place(moq_net::Timestamp::ZERO, wall)?;
		}

		// The session is placed: the catalog may go out.
		let Timing::Waiting(waiting) = std::mem::replace(&mut self.timing, Timing::Placed { reference }) else {
			unreachable!("only a waiting session is released");
		};
		let Waiting { gate, held, .. } = *waiting;
		drop(gate);
		for (item, params) in held {
			self.publish(item, params.as_ref())?;
		}
		Ok(())
	}

	/// Publish one depacketized item under its stream's video `params`.
	fn publish(&mut self, item: CodecItem, params: Option<&VideoParameters>) -> Result<()> {
		let (stream_id, timestamp, data, changed, loss) = match &item {
			CodecItem::VideoFrame(frame) => (
				frame.stream_id(),
				frame.timestamp(),
				frame.data(),
				frame.has_new_parameters(),
				frame.loss(),
			),
			CodecItem::AudioFrame(frame) => (
				frame.stream_id(),
				frame.timestamp(),
				frame.data(),
				false,
				// AAC packets decode alone, and the decoder conceals a lost one.
				0,
			),
			_ => return Ok(()),
		};
		let Timing::Placed { reference } = self.timing else {
			unreachable!("frames publish once the session is placed");
		};
		let Some(stream) = self.streams.get_mut(stream_id).and_then(Option::as_mut) else {
			return Ok(());
		};

		let elapsed = timestamp.elapsed();
		let rate = timestamp.clock_rate().get();
		// A stream whose first frame came after the session was placed is placed on that frame.
		let zero = stream.place(stream_id).expect("an arrived frame bounds its stream");
		let offset = ((zero - reference) * i128::from(rate)).div_euclid(1_000_000);
		let Ok(units) = u64::try_from(i128::from(elapsed) + offset) else {
			// Captured more than [`LEAD`] before the session's start, such as a replay from long
			// before PLAY: the timebase has no time for it, so drop it.
			return Ok(());
		};
		let pts = moq_net::Timestamp::from_scale(units, u64::from(rate)).map_err(moq_mux::Error::from)?;
		let pts = self.timebase.shift(pts)?;

		match &mut stream.track {
			Track::Video(video) => {
				// Before any drop, so the next clean keyframe still gets retina's new parameters.
				stream.fresh |= changed;
				if stream.opened.is_some_and(|opened| elapsed < opened) {
					return Ok(());
				}
				if loss > 0 {
					// The pictures after a lost packet reference what it carried, so hold the
					// video until the next keyframe rather than publish a broken chain. This
					// access unit is damaged too, even when it is a keyframe, so drop it.
					if stream.started && stream.held.is_none() {
						tracing::warn!(stream_id, loss, "RTP packets lost, waiting for the next keyframe");
						stream.held = Some(kio::clock::now());
					}
					video.reset();
					return Ok(());
				}
				let params = params.filter(|_| stream.fresh);
				let frames = video.split(data, params, pts)?;
				let keyframe = frames.iter().any(|frame| frame.keyframe);
				stream.fresh &= !keyframe;
				if !stream.started || stream.held.is_some() {
					// Publish nothing before a keyframe, by the splitter's reckoning (an IDR, an
					// H.264 recovery point, or any H.265 IRAP picture).
					if !frames.first().is_some_and(|frame| frame.keyframe) {
						return Ok(());
					}
					stream.started = true;
					stream.held = None;
					stream.opened = Some(elapsed);
					self.started = true;
				}
				video.decode(frames)?;
			}
			Track::Aac(import) => {
				if !stream.started {
					// Audio waits for the first keyframe too.
					if self.video && !self.started {
						return Ok(());
					}
					stream.started = true;
				}
				import.decode(data, Some(pts))?;
				// Each packet its own group (one QUIC stream), so relays forward it at once.
				import.cut(None)?;
			}
		}
		Ok(())
	}

	/// Whether a video stream has waited longer than `timeout` for a keyframe: its first since
	/// `playing`, or its next since a loss held it. A session waiting for a sender report holds
	/// its keyframes, so that wait counts too.
	fn awaiting_keyframe(&self, playing: kio::clock::Instant, timeout: std::time::Duration) -> bool {
		self.streams.iter().flatten().any(|stream| {
			let since = match stream.held {
				Some(held) => held,
				None if !stream.started => playing,
				None => return false,
			};
			matches!(stream.track, Track::Video(_)) && since.elapsed() > timeout
		})
	}

	/// Flush every track and end it cleanly, reporting the first failure.
	fn finish(&mut self) -> Result<()> {
		let mut result = Ok(());
		for stream in self.streams.drain(..).flatten() {
			result = result.and(stream.track.finish().map_err(Error::from));
		}
		result
	}

	/// End every track with `err`, so subscribers see the cause.
	fn abort(&mut self, err: moq_net::Error) {
		for stream in self.streams.drain(..).flatten() {
			stream.track.abort(err.clone());
		}
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::*;
	use crate::fake::{self, Media, Server};

	/// Real sockets, so a wall-clock bound on every wait instead of paused time.
	const WAIT: Duration = Duration::from_secs(10);
	/// 30 fps at the RTP video clock.
	const FRAME: u32 = 3000;

	#[expect(
		clippy::disallowed_methods,
		reason = "the watchdog runs on tokio's host timer: tests that install a manual clock advance the ambient one past it"
	)]
	async fn within<T>(what: &str, step: impl Future<Output = T>) -> T {
		tokio::time::timeout(WAIT, step)
			.await
			.unwrap_or_else(|_| panic!("{what} timed out"))
	}

	/// Make the test runtime's clock one only the test moves, so a setup step on a loaded runner
	/// never reaches a deadline. Call it before anything reads the clock.
	fn manual() -> kio::clock::Installed {
		kio::clock::Manual::new().install(&tokio::runtime::Handle::current())
	}

	/// A client whose session is placed on its first frame, unless a test waits for sender reports.
	fn client(url: Url) -> Client {
		Client::new(url).unwrap().waiting(Duration::ZERO)
	}

	impl Client {
		/// Wait up to `wait` after the first frame for every stream's sender report.
		fn waiting(mut self, wait: Duration) -> Self {
			self.report_wait = wait;
			self
		}
	}

	fn broadcast() -> (moq_net::broadcast::Producer, moq_mux::catalog::Producer) {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, Default::default()).unwrap();
		(broadcast, catalog)
	}

	/// The first catalog snapshot with `video` video and `audio` audio renditions.
	async fn catalog_with(
		catalog: &moq_mux::catalog::Producer,
		video: usize,
		audio: usize,
	) -> moq_mux::catalog::hang::Catalog {
		let mut updates = catalog.consume().unwrap();
		within("the catalog", async {
			loop {
				let snapshot = updates.next().await.expect("catalog update").expect("catalog open");
				if snapshot.video.renditions.len() == video && snapshot.audio.renditions.len() == audio {
					return snapshot;
				}
			}
		})
		.await
	}

	/// Subscribe to `name` from its first group, before it finishes and leaves the broadcast.
	async fn subscribe(
		broadcast: &moq_net::broadcast::Producer,
		(name, container): (String, moq_mux::catalog::hang::Container),
	) -> moq_mux::container::Consumer<moq_mux::catalog::hang::Container> {
		let subscription = moq_net::track::Subscription::default().with_ranges([moq_net::track::Range::from(
			moq_net::track::Start::Live {
				min: Some(Duration::from_secs(60)),
				max: Some(Duration::from_secs(60)),
			},
		)]);
		let track = broadcast
			.consume()
			.track(&name)
			.unwrap()
			.subscribe(subscription)
			.await
			.unwrap();
		moq_mux::container::Consumer::new(track, container)
	}

	/// Every frame left on a track, failing unless it finishes cleanly.
	async fn frames(
		mut reader: moq_mux::container::Consumer<moq_mux::catalog::hang::Container>,
	) -> Vec<moq_mux::container::Frame> {
		let mut frames = Vec::new();
		within("the track to finish", async {
			while let Some(read) = reader.read().await.expect("track finishes cleanly") {
				if let moq_mux::container::Read::Frame { frame, .. } = read {
					frames.push(frame);
				}
			}
		})
		.await;
		frames
	}

	fn video_track(snapshot: &moq_mux::catalog::hang::Catalog) -> (String, moq_mux::catalog::hang::Container) {
		let (name, config) = snapshot.video.renditions.iter().next().unwrap();
		(name.clone(), config.try_into().unwrap())
	}

	fn audio_track(snapshot: &moq_mux::catalog::hang::Catalog) -> (String, moq_mux::catalog::hang::Container) {
		let (name, config) = snapshot.audio.renditions.iter().next().unwrap();
		(name.clone(), config.try_into().unwrap())
	}

	/// H.264 with the parameter sets only in the SDP, plus AAC: nothing publishes before the
	/// first keyframe, which carries the SDP's SPS and PPS in band.
	#[tokio::test]
	async fn h264_and_aac_start_on_a_keyframe() {
		let server = Server::bind(vec![Media::H264 { sprop: true }, Media::Aac]).await;
		let (broadcast, catalog) = broadcast();
		let client = client(server.url());
		let pull = tokio::spawn({
			let (broadcast, catalog) = (broadcast.clone(), catalog.clone());
			async move { client.pull(&broadcast, &catalog).await }
		});

		let mut session = within("PLAY", server.accept(1000)).await;
		// Both streams' first RTP time captured at one instant, so each lands on its own.
		let captured = ntp(wall_now() - 100_000);
		session.report(0, 1000, captured).await;
		session.report(1, 1000, captured).await;
		// Before the first keyframe: dropped, audio included.
		session.aac(1, 1000, b"early").await;
		session.video(0, 1000, &[fake::H264_SLICE]).await;
		// Only the slice in band; the ingest inserts the SDP's parameter sets.
		session.video(0, 1000 + FRAME, &[fake::H264_IDR]).await;
		session.aac(1, 1000 + 2048, b"audio").await;
		session.video(0, 1000 + 2 * FRAME, &[fake::H264_SLICE]).await;

		let snapshot = catalog_with(&catalog, 1, 1).await;
		let video = snapshot.video.renditions.values().next().unwrap();
		let hang::catalog::VideoCodec::H264(h264) = &video.codec else {
			panic!("expected H.264, got {:?}", video.codec);
		};
		assert!(h264.inline, "parameter sets ride in band");
		assert_eq!((h264.profile, h264.level), (0x42, 0x0d));
		assert_eq!((video.coded_width, video.coded_height), (Some(320), Some(240)));
		let audio = snapshot.audio.renditions.values().next().unwrap();
		assert!(matches!(audio.codec, hang::catalog::AudioCodec::AAC(_)));
		assert_eq!((audio.sample_rate, audio.channel_count), (48_000, 2));

		let video = subscribe(&broadcast, video_track(&snapshot)).await;
		let audio = subscribe(&broadcast, audio_track(&snapshot)).await;
		session.close().await;
		assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Closed);

		let video = frames(video).await;
		assert_eq!(video.len(), 2, "the slice before the keyframe never publishes");
		assert!(video[0].keyframe);
		assert_eq!(
			video[0].timestamp.as_micros().unwrap(),
			LEAD.as_micros() + 33_333,
			"one frame past the session start, after the lead"
		);
		let sps = [&[0, 0, 0, 1], fake::H264_SPS].concat();
		assert!(
			video[0].payload.windows(sps.len()).any(|window| window == sps),
			"the keyframe carries the SDP's SPS"
		);
		assert!(!video[1].keyframe);

		let audio = frames(audio).await;
		assert_eq!(audio.len(), 1, "audio before the keyframe never publishes");
		assert_eq!(audio[0].payload.as_ref(), b"audio");
		assert_eq!(
			audio[0].timestamp.as_micros().unwrap(),
			LEAD.as_micros() + 42_666,
			"two AAC frames past the session start, after the lead"
		);
	}

	/// An H.264 recovery point starts the broadcast, though retina flags only an IDR as a
	/// keyframe, and it carries the SDP's parameter sets.
	#[tokio::test]
	async fn h264_starts_on_a_recovery_point() {
		let server = Server::bind(vec![Media::H264 { sprop: true }]).await;
		let (broadcast, catalog) = broadcast();
		let client = client(server.url());
		let pull = tokio::spawn({
			let (broadcast, catalog) = (broadcast.clone(), catalog.clone());
			async move { client.pull(&broadcast, &catalog).await }
		});

		let mut session = within("PLAY", server.accept(0)).await;
		session.video(0, 0, &[fake::H264_SLICE]).await;
		session.video(0, FRAME, &[fake::H264_RECOVERY, fake::H264_SLICE]).await;

		let snapshot = catalog_with(&catalog, 1, 0).await;
		let reader = subscribe(&broadcast, video_track(&snapshot)).await;
		session.video(0, 2 * FRAME, &[fake::H264_SLICE]).await;
		session.close().await;
		assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Closed);

		let frames = frames(reader).await;
		assert_eq!(frames.len(), 2, "the slice before the recovery point never publishes");
		assert!(frames[0].keyframe && !frames[1].keyframe);
		let sps = [&[0, 0, 0, 1], fake::H264_SPS].concat();
		assert!(
			frames[0].payload.windows(sps.len()).any(|window| window == sps),
			"the recovery point carries the SDP's SPS"
		);
		assert!(
			!frames[1].payload.windows(sps.len()).any(|window| window == sps),
			"a delta frame carries no parameter sets"
		);
	}

	/// An open-GOP CRA starts the broadcast, though retina flags only an IDR as a keyframe and
	/// strips the parameter sets it carries. Its leading pictures, displayed before the session
	/// started, are dropped rather than taken for a jump.
	#[tokio::test]
	async fn h265_starts_on_a_cra() {
		let server = Server::bind(vec![Media::H265]).await;
		let (broadcast, catalog) = broadcast();
		let client = client(server.url());
		let pull = tokio::spawn({
			let (broadcast, catalog) = (broadcast.clone(), catalog.clone());
			async move { client.pull(&broadcast, &catalog).await }
		});

		let mut session = within("PLAY", server.accept(FRAME)).await;
		session
			.video(
				0,
				FRAME,
				&[fake::H265_VPS, fake::H265_SPS, fake::H265_PPS, fake::H265_CRA],
			)
			.await;

		let snapshot = catalog_with(&catalog, 1, 0).await;
		let video = snapshot.video.renditions.values().next().unwrap();
		assert!(
			matches!(video.codec, hang::catalog::VideoCodec::H265(_)),
			"{:?}",
			video.codec
		);
		assert_eq!((video.coded_width, video.coded_height), (Some(320), Some(240)));

		let reader = subscribe(&broadcast, video_track(&snapshot)).await;
		session.video(0, 0, &[fake::H265_RASL]).await;
		session.video(0, 2 * FRAME, &[fake::H265_TRAIL]).await;
		session.close().await;
		assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Closed);

		let frames = frames(reader).await;
		assert_eq!(frames.len(), 2, "the leading picture never publishes");
		assert!(frames[0].keyframe && !frames[1].keyframe);
		let sps = [&[0, 0, 0, 1], fake::H265_SPS].concat();
		assert!(
			frames[0].payload.windows(sps.len()).any(|window| window == sps),
			"the CRA carries its parameter sets"
		);
	}

	/// G.711, common on cameras, is skipped with one warning and the broadcast goes on
	/// video-only.
	#[tokio::test]
	#[tracing_test::traced_test]
	async fn g711_is_skipped() {
		let server = Server::bind(vec![Media::H264 { sprop: true }, Media::Pcmu]).await;
		let (broadcast, catalog) = broadcast();
		let client = client(server.url());
		let pull = tokio::spawn({
			let (broadcast, catalog) = (broadcast.clone(), catalog.clone());
			async move { client.pull(&broadcast, &catalog).await }
		});

		let mut session = within("PLAY", server.accept(0)).await;
		session.video(0, 0, &[fake::H264_IDR]).await;
		catalog_with(&catalog, 1, 0).await;
		session.close().await;
		assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Closed);

		assert!(logs_contain("audio/pcmu"));
	}

	#[tokio::test]
	async fn no_supported_stream_is_refused() {
		let server = Server::bind(vec![Media::Pcmu]).await;
		let (broadcast, catalog) = broadcast();
		let client = client(server.url());
		let pull = tokio::spawn(async move { client.pull(&broadcast, &catalog).await });
		// DESCRIBE only: the client gives up before SETUP, so `accept` never sees a PLAY.
		let _accept = tokio::spawn(async move { server.accept(0).await });
		let err = within("the pull", pull).await.unwrap().unwrap_err();
		assert!(matches!(err, Error::NoSupportedStream(_)), "{err}");
	}

	/// A camera resetting its RTP clock mid-session ends the pull with `Jumped`, finishing
	/// the tracks cleanly with only the frames before the jump.
	#[tokio::test]
	async fn a_timestamp_jump_ends_the_session() {
		// Parameter sets in band only, as many cameras send them.
		let server = Server::bind(vec![Media::H264 { sprop: false }]).await;
		let (broadcast, catalog) = broadcast();
		let client = client(server.url());
		let pull = tokio::spawn({
			let (broadcast, catalog) = (broadcast.clone(), catalog.clone());
			async move { client.pull(&broadcast, &catalog).await }
		});

		let mut session = within("PLAY", server.accept(1000)).await;
		session
			.video(0, 1000, &[fake::H264_SPS, fake::H264_PPS, fake::H264_IDR])
			.await;
		let snapshot = catalog_with(&catalog, 1, 0).await;
		let video = subscribe(&broadcast, video_track(&snapshot)).await;
		session.video(0, 1000 + FRAME, &[fake::H264_SLICE]).await;
		session.video(0, 0x8000_0000, &[fake::H264_IDR]).await;

		assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Jumped);
		assert_eq!(frames(video).await.len(), 2);
	}

	/// Two sessions (a camera's SD and HD URLs) publish renditions of one broadcast. When one
	/// session's timeline jumps, the caller ends the broadcast and restarts both under a fresh
	/// instance, each new broadcast with its own catalog.
	#[tokio::test]
	async fn two_sessions_restart_together_on_a_jump() {
		let (origin, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
		tokio::spawn(moq_net::time::run(driver));
		let sd = Server::bind(vec![Media::H264 { sprop: true }]).await;
		let hd = Server::bind(vec![Media::H265]).await;
		let sd_client = client(sd.url());
		let hd_client = client(hd.url());

		let (published_tx, mut published) = tokio::sync::mpsc::unbounded_channel();
		let caller = tokio::spawn({
			let origin = origin.clone();
			async move {
				let mut ends = Vec::new();
				for _ in 0..2 {
					let route = origin
						.route("camera", moq_net::origin::RouteOptions::default())
						.unwrap();
					let broadcast = route.broadcast();
					let mut catalog =
						moq_mux::catalog::Producer::new(&mut broadcast.clone(), Default::default()).unwrap();
					published_tx
						.send((route.instance(), broadcast.clone(), catalog.clone()))
						.unwrap();
					// The first session to return ends the run; dropping the other's pull
					// finishes its tracks.
					let end = tokio::select! {
						end = sd_client.pull(broadcast, &catalog) => end,
						end = hd_client.pull(broadcast, &catalog) => end,
					};
					ends.push(end.unwrap());
					catalog.finish().unwrap();
					broadcast.close();
				}
				ends
			}
		});

		let mut instances = Vec::new();
		for run in 0..2 {
			let (instance, broadcast, catalog) = within("the broadcast", published.recv()).await.unwrap();
			instances.push(instance);
			let mut sd_session = within("SD PLAY", sd.accept(500)).await;
			let mut hd_session = within("HD PLAY", hd.accept(9000)).await;
			sd_session.video(0, 500, &[fake::H264_IDR]).await;
			hd_session.video(0, 9000, &[fake::H265_IDR]).await;

			let snapshot = catalog_with(&catalog, 2, 0).await;
			if run == 0 {
				let mut readers = Vec::new();
				for (name, config) in &snapshot.video.renditions {
					readers.push(subscribe(&broadcast, (name.clone(), config.try_into().unwrap())).await);
				}
				sd_session.video(0, 500 + FRAME, &[fake::H264_SLICE]).await;
				hd_session.video(0, 0x8000_0000, &[fake::H265_IDR]).await;
				// Both sessions' tracks finish cleanly: the jumped one's and the dropped one's.
				for reader in readers {
					assert!(frames(reader).await[0].keyframe);
				}
			} else {
				sd_session.close().await;
				hd_session.close().await;
			}
		}

		let ends = within("the caller", caller).await.unwrap();
		assert_eq!(ends[0], End::Jumped);
		assert_eq!(ends[1], End::Closed);
		assert_ne!(instances[1], instances[0], "the restart mints a fresh instance");
	}

	/// Credentials come out of the URL, never reach a log, and still authenticate.
	#[tokio::test]
	#[tracing_test::traced_test]
	async fn credentials_ride_the_userinfo() {
		let server = Server::bind(vec![Media::H264 { sprop: true }])
			.await
			.with_basic_auth("admin", "p@ss word");
		let mut url = server.url();
		url.set_username("admin").unwrap();
		url.set_password(Some("p%40ss%20word")).unwrap();

		let client = client(url);
		let debug = format!("{client:?}");
		assert!(!debug.contains("admin") && !debug.contains("ss"), "{debug}");
		assert!(client.url.username().is_empty() && client.url.password().is_none());

		let (broadcast, catalog) = broadcast();
		let pull = tokio::spawn(async move { client.pull(&broadcast, &catalog).await });
		let session = within("PLAY", server.accept(0)).await;
		session.close().await;
		assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Closed);
		assert!(logs_contain("RTSP session playing"));
		assert!(!logs_contain("p@ss") && !logs_contain("p%40ss"));
	}

	/// A camera that keeps the session up but stops sending media ends the pull, so a caller
	/// redials it rather than waiting forever.
	#[tokio::test]
	async fn a_silent_session_stalls() {
		let clock = manual();
		let server = Server::bind(vec![Media::H264 { sprop: true }]).await;
		let (broadcast, catalog) = broadcast();
		let client = client(server.url());
		let pull = tokio::spawn({
			let (broadcast, catalog) = (broadcast.clone(), catalog.clone());
			async move { client.pull(&broadcast, &catalog).await }
		});

		let mut session = within("PLAY", server.accept(0)).await;
		session.video(0, 0, &[fake::H264_IDR]).await;
		let snapshot = catalog_with(&catalog, 1, 0).await;
		let mut reader = subscribe(&broadcast, video_track(&snapshot)).await;

		// Just short of the deadline the session is still up, so a frame still publishes.
		clock.advance(super::TIMEOUT - Duration::from_millis(1));
		session.video(0, FRAME, &[fake::H264_SLICE]).await;
		within("both frames", async {
			let mut frames = 0;
			while frames < 2 {
				let read = reader.read().await.unwrap().expect("track open");
				frames += usize::from(matches!(read, moq_mux::container::Read::Frame { .. }));
			}
		})
		.await;

		clock.advance(super::TIMEOUT);
		let err = within("the pull", pull).await.unwrap().unwrap_err();
		assert!(
			matches!(err, Error::Stalled(timeout) if timeout == super::TIMEOUT),
			"{err}"
		);
	}

	/// A camera that stops sending media but keeps sending RTCP reports still stalls.
	#[tokio::test]
	async fn rtcp_alone_stalls() {
		let clock = manual();
		let server = Server::bind(vec![Media::H264 { sprop: true }]).await;
		let (broadcast, catalog) = broadcast();
		let client = client(server.url());
		let pull = tokio::spawn({
			let catalog = catalog.clone();
			async move { client.pull(&broadcast, &catalog).await }
		});

		let mut session = within("PLAY", server.accept(0)).await;
		session.video(0, 0, &[fake::H264_IDR]).await;
		// Published once the frame armed the deadline.
		catalog_with(&catalog, 1, 0).await;
		for _ in 0..4 {
			session.rtcp(0, 0).await;
			// On this current-thread runtime a yield resumes only after the I/O driver polls, so
			// the pull sees the report as readable before the clock moves. Its timeout polls the
			// read before the deadline, so it takes the report even when the deadline is due.
			tokio::task::yield_now().await;
			clock.advance(super::TIMEOUT / 4);
		}
		let err = within("the pull", pull).await.unwrap().unwrap_err();
		assert!(
			matches!(err, Error::Stalled(timeout) if timeout == super::TIMEOUT),
			"{err}"
		);
	}

	/// A video stream that never sends a keyframe would withhold the catalog while its audio
	/// keeps the session alive, so the pull fails rather than idling.
	#[tokio::test]
	async fn a_stream_without_keyframes_stalls() {
		let clock = manual();
		let server = Server::bind(vec![Media::H264 { sprop: true }, Media::Aac]).await;
		let (broadcast, catalog) = broadcast();
		let mut client = client(server.url());
		// Lengthened so the clock can run well ahead of the frames read and only the keyframe
		// deadline fires.
		client.timeout = 10 * KEYFRAME_TIMEOUT;
		let mut pull = tokio::spawn(async move { client.pull(&broadcast, &catalog).await });

		let mut session = within("PLAY", server.accept(0)).await;
		let err = within("the pull", async {
			for frame in 0.. {
				session.video(0, frame * FRAME, &[fake::H264_SLICE]).await;
				session.aac(1, frame * 1600, b"audio").await;
				clock.advance(Duration::from_secs(1));
				tokio::select! {
					result = &mut pull => return result.unwrap().unwrap_err(),
					// A driver turn, so the pull can read what was sent.
					_ = kio::clock::sleep(Duration::ZERO) => {}
				}
			}
			unreachable!()
		})
		.await;
		assert!(
			matches!(err, Error::Stalled(timeout) if timeout == KEYFRAME_TIMEOUT),
			"{err}"
		);
	}

	/// A stream that pauses, such as motion-triggered video beside continuous audio, returns as
	/// far ahead as it paused, which continues the timeline rather than jumping it.
	#[tokio::test]
	async fn a_paused_stream_is_not_a_jump() {
		kio::clock::host_time(); // real RTSP sockets, and the pause is real time
		let server = Server::bind(vec![Media::H264 { sprop: true }]).await;
		let (broadcast, catalog) = broadcast();
		let mut client = client(server.url());
		client.max_forward = Duration::from_millis(100);
		let pull = tokio::spawn({
			let (broadcast, catalog) = (broadcast.clone(), catalog.clone());
			async move { client.pull(&broadcast, &catalog).await }
		});

		let mut session = within("PLAY", server.accept(0)).await;
		session.video(0, 0, &[fake::H264_IDR]).await;
		let snapshot = catalog_with(&catalog, 1, 0).await;
		let reader = subscribe(&broadcast, video_track(&snapshot)).await;
		// Half a second of silence, well past the 100ms allowance, then the matching timestamp.
		kio::clock::sleep(Duration::from_millis(500)).await;
		session.video(0, 45_000, &[fake::H264_SLICE]).await;
		session.close().await;
		assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Closed);
		assert_eq!(frames(reader).await.len(), 2);
	}

	/// A lost packet holds the video until the next keyframe rather than publishing pictures
	/// whose references never arrived.
	#[tokio::test]
	async fn packet_loss_waits_for_a_keyframe() {
		let server = Server::bind(vec![Media::H264 { sprop: true }]).await;
		let (broadcast, catalog) = broadcast();
		let client = client(server.url());
		let pull = tokio::spawn({
			let (broadcast, catalog) = (broadcast.clone(), catalog.clone());
			async move { client.pull(&broadcast, &catalog).await }
		});

		let mut session = within("PLAY", server.accept(0)).await;
		session.video(0, 0, &[fake::H264_IDR]).await;
		let snapshot = catalog_with(&catalog, 1, 0).await;
		let reader = subscribe(&broadcast, video_track(&snapshot)).await;
		session.video(0, FRAME, &[fake::H264_SLICE]).await;
		session.lose(0, 1);
		session.video(0, 3 * FRAME, &[fake::H264_SLICE]).await;
		session.video(0, 4 * FRAME, &[fake::H264_SLICE]).await;
		// A keyframe that itself lost a packet is damaged too.
		session.lose(0, 1);
		session.video(0, 5 * FRAME, &[fake::H264_IDR]).await;
		session.video(0, 6 * FRAME, &[fake::H264_IDR]).await;
		session.close().await;
		assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Closed);

		let frames = frames(reader).await;
		let keyframes: Vec<bool> = frames.iter().map(|frame| frame.keyframe).collect();
		assert_eq!(
			keyframes,
			[true, false, true],
			"the slices after the loss never publish"
		);
	}

	/// A first keyframe that lost a packet is damaged, so the track opens on the next one.
	#[tokio::test]
	async fn a_lossy_first_keyframe_is_dropped() {
		let server = Server::bind(vec![Media::H264 { sprop: true }]).await;
		let (broadcast, catalog) = broadcast();
		let client = client(server.url());
		let pull = tokio::spawn({
			let (broadcast, catalog) = (broadcast.clone(), catalog.clone());
			async move { client.pull(&broadcast, &catalog).await }
		});

		let mut session = within("PLAY", server.accept(0)).await;
		session.video(0, 0, &[fake::H264_SLICE]).await;
		session.lose(0, 1);
		session.video(0, 2 * FRAME, &[fake::H264_IDR]).await;
		session.video(0, 3 * FRAME, &[fake::H264_IDR]).await;

		let snapshot = catalog_with(&catalog, 1, 0).await;
		let reader = subscribe(&broadcast, video_track(&snapshot)).await;
		session.close().await;
		assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Closed);
		let frames = frames(reader).await;
		assert_eq!(frames.len(), 1, "the lossy keyframe never publishes");
		assert_eq!(
			frames[0].timestamp.as_micros().unwrap(),
			LEAD.as_micros() + 100_000,
			"the track opens on the clean keyframe"
		);
	}

	/// A keyframe that lost a packet during a report wait publishes nothing, so it leaves the clean
	/// group held before it to publish.
	#[tokio::test]
	async fn a_lossy_keyframe_keeps_the_held_group() {
		kio::clock::host_time(); // real RTSP sockets, and the report wait is real time
		let server = Server::bind(vec![Media::H264 { sprop: true }]).await;
		let (broadcast, catalog, pull) = pull(client(server.url()).waiting(10 * WAIT));

		let mut session = within("PLAY", server.accept(0)).await;
		let captured = wall_now();
		session.video(0, 0, &[fake::H264_IDR]).await;
		session.video(0, FRAME, &[fake::H264_SLICE]).await;
		session.lose(0, 1);
		session.video(0, 3 * FRAME, &[fake::H264_IDR]).await;
		session.report(0, 0, ntp(captured)).await;
		let snapshot = catalog_with(&catalog, 1, 0).await;
		let reader = subscribe(&broadcast, video_track(&snapshot)).await;
		session.close().await;
		assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Closed);

		let frames = frames(reader).await;
		assert_eq!(
			frames.len(),
			2,
			"the clean group publishes, the lossy keyframe does not"
		);
		assert!(frames[0].keyframe && !frames[1].keyframe);
	}

	/// A camera resetting its RTP clock during a report wait ends the pull with `Jumped`, rather
	/// than its new keyframe replacing the group held before it and the wait going on.
	#[tokio::test]
	async fn a_timestamp_jump_during_the_wait_ends_the_session() {
		kio::clock::host_time(); // real RTSP sockets, and the report wait is real time
		let server = Server::bind(vec![Media::H264 { sprop: true }]).await;
		let (broadcast, catalog, pull) = pull(client(server.url()).waiting(10 * WAIT));

		let mut session = within("PLAY", server.accept(0)).await;
		session.video(0, 0, &[fake::H264_IDR]).await;
		session.video(0, FRAME, &[fake::H264_SLICE]).await;
		session.video(0, 0x8000_0000, &[fake::H264_IDR]).await;
		assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Jumped);
		drop((broadcast, catalog));
	}

	/// The wall time `frame` advertises under the catalog's `sync.clock`, in Unix microseconds.
	fn wall_of(snapshot: &moq_mux::catalog::hang::Catalog, frame: &moq_mux::container::Frame) -> i128 {
		let clock = snapshot.sync.clock.expect("the catalog advertises its clock");
		unix_micros(clock.wall_clock(frame.timestamp).unwrap())
	}

	/// The wall clock now, in Unix microseconds.
	fn wall_now() -> i128 {
		unix_micros(kio::clock::wall())
	}

	/// `unix` microseconds as an NTP timestamp.
	fn ntp(unix: i128) -> u64 {
		let secs = (unix.div_euclid(1_000_000) + NTP_UNIX) as u64;
		let frac = ((unix.rem_euclid(1_000_000) as u64) << 32).div_ceil(1_000_000);
		(secs << 32) | frac
	}

	const H265_KEYFRAME: &[&[u8]] = &[fake::H265_VPS, fake::H265_SPS, fake::H265_PPS, fake::H265_IDR];

	/// Pull `server` into a fresh catalog with `client`, returning the broadcast, its catalog, and
	/// the pull.
	fn pull(
		client: Client,
	) -> (
		moq_net::broadcast::Producer,
		moq_mux::catalog::Producer,
		tokio::task::JoinHandle<Result<End>>,
	) {
		let (broadcast, catalog) = broadcast();
		let pull = tokio::spawn({
			let (broadcast, catalog) = (broadcast.clone(), catalog.clone());
			async move { client.pull(&broadcast, &catalog).await }
		});
		(broadcast, catalog, pull)
	}

	/// Without a sender report, `sync.clock` places the session's first frame at its arrival,
	/// while its timestamps stay the session's own, moved by the lead.
	#[tokio::test]
	async fn the_catalog_clock_times_a_frame_at_its_arrival() {
		let server = Server::bind(vec![Media::H265]).await;
		let (broadcast, catalog, pull) = pull(client(server.url()));

		let mut session = within("PLAY", server.accept(0)).await;
		let sent = wall_now();
		session.video(0, 0, H265_KEYFRAME).await;
		let snapshot = catalog_with(&catalog, 1, 0).await;
		let listed = wall_now();
		let reader = subscribe(&broadcast, video_track(&snapshot)).await;
		session.video(0, FRAME, &[fake::H265_TRAIL]).await;
		session.close().await;
		assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Closed);

		let frames = frames(reader).await;
		assert_eq!(
			frames[0].timestamp.as_micros().unwrap(),
			LEAD.as_micros(),
			"the session's own timestamps, after the lead"
		);
		let wall = wall_of(&snapshot, &frames[0]);
		assert!(
			(sent..=listed).contains(&wall),
			"the first frame lands {} µs after it was sent, {} µs before the catalog listed it",
			wall - sent,
			listed - wall
		);
		assert_eq!(wall_of(&snapshot, &frames[1]) - wall, 33_333, "one frame later");
	}

	/// A camera's sender report times the session at capture: `sync.clock` places each frame where
	/// the camera's NTP clock says it was taken, not when it arrived. Waiting for the report holds
	/// the opening keyframe rather than dropping it.
	#[tokio::test]
	async fn a_sender_report_times_the_session_at_capture() {
		kio::clock::host_time(); // real RTSP sockets, and the report wait is real time
		let server = Server::bind(vec![Media::H265]).await;
		let client = client(server.url()).waiting(WAIT);
		let (broadcast, catalog, pull) = pull(client);

		let mut session = within("PLAY", server.accept(1000)).await;
		session.video(0, 1000, H265_KEYFRAME).await;
		// The camera took the opening keyframe 300 ms before it arrived, on its NTP clock; its
		// report names the RTP time a second earlier.
		let captured = wall_now() - 300_000;
		session
			.report(0, 1000u32.wrapping_sub(90_000), ntp(captured - 1_000_000))
			.await;
		let snapshot = catalog_with(&catalog, 1, 0).await;
		let reader = subscribe(&broadcast, video_track(&snapshot)).await;
		session.video(0, 1000 + FRAME, &[fake::H265_TRAIL]).await;
		session.close().await;
		assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Closed);

		let frames = frames(reader).await;
		assert_eq!(frames.len(), 2, "the keyframe held for the report publishes");
		assert!(frames[0].keyframe);
		assert_eq!(
			frames[0].timestamp.as_micros().unwrap(),
			LEAD.as_micros(),
			"the session's own timestamps, after the lead"
		);
		assert_eq!(
			wall_of(&snapshot, &frames[0]),
			captured,
			"timed at the camera's capture"
		);
		assert_eq!(wall_of(&snapshot, &frames[1]), captured + 33_333);
	}

	/// A report far from the frame's arrival is a camera clock set wrong: once the wait ends
	/// without the arrivals coming to agree with it, the session is timed on arrival.
	#[tokio::test]
	#[tracing_test::traced_test]
	async fn a_sender_report_far_from_arrival_is_ignored() {
		kio::clock::host_time(); // real RTSP sockets, and the report wait is real time
		let server = Server::bind(vec![Media::H265]).await;
		let client = client(server.url()).waiting(Duration::from_millis(200));
		let (broadcast, catalog, pull) = pull(client);

		let mut session = within("PLAY", server.accept(0)).await;
		let sent = wall_now();
		session.video(0, 0, H265_KEYFRAME).await;
		// Captured 6 s before its arrival: longer than any camera takes.
		session.report(0, 0, ntp(sent - 6_000_000)).await;
		let snapshot = catalog_with(&catalog, 1, 0).await;
		let listed = wall_now();
		let reader = subscribe(&broadcast, video_track(&snapshot)).await;
		session.close().await;
		assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Closed);

		let wall = wall_of(&snapshot, &frames(reader).await[0]);
		assert!(
			(sent..=listed).contains(&wall),
			"timed {} µs after it was sent",
			wall - sent
		);
		assert!(logs_contain("sender report far from the frame's arrival"));
	}

	/// A camera whose clock was never set reports NTP time zero, which no later frame brings
	/// closer: the stream is timed at its arrivals once the wait is over, as without a report.
	#[tokio::test]
	async fn a_camera_clock_never_set_is_timed_at_arrival() {
		kio::clock::host_time(); // real RTSP sockets, and the report wait is real time
		let server = Server::bind(vec![Media::H265]).await;
		let client = client(server.url()).waiting(Duration::from_millis(200));
		let (broadcast, catalog, pull) = pull(client);

		let mut session = within("PLAY", server.accept(0)).await;
		let sent = wall_now();
		session.video(0, 0, H265_KEYFRAME).await;
		session.rtcp(0, 0).await;
		let snapshot = catalog_with(&catalog, 1, 0).await;
		let listed = wall_now();
		let reader = subscribe(&broadcast, video_track(&snapshot)).await;
		session.close().await;
		assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Closed);

		let wall = wall_of(&snapshot, &frames(reader).await[0]);
		assert!(
			(sent..=listed).contains(&wall),
			"timed {} µs after it was sent",
			wall - sent
		);
	}

	/// A camera clock that booted at a past date reports captures years before their arrival,
	/// further than any replay explains: the stream is timed at its arrivals once the wait is over.
	#[tokio::test]
	async fn a_camera_clock_in_the_past_is_timed_at_arrival() {
		kio::clock::host_time(); // real RTSP sockets, and the report wait is real time
		let server = Server::bind(vec![Media::H265]).await;
		let client = client(server.url()).waiting(Duration::from_millis(200));
		let (broadcast, catalog, pull) = pull(client);

		let mut session = within("PLAY", server.accept(0)).await;
		let sent = wall_now();
		session.video(0, 0, H265_KEYFRAME).await;
		// 2000-01-01, a common default for a camera without NTP.
		session.report(0, 0, ntp(946_684_800_000_000)).await;
		let snapshot = catalog_with(&catalog, 1, 0).await;
		let listed = wall_now();
		let reader = subscribe(&broadcast, video_track(&snapshot)).await;
		session.close().await;
		assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Closed);

		let wall = wall_of(&snapshot, &frames(reader).await[0]);
		assert!(
			(sent..=listed).contains(&wall),
			"timed {} µs after it was sent",
			wall - sent
		);
	}

	/// Without a report by the end of the wait, the session is timed on its first frame's arrival,
	/// every frame held meanwhile included.
	#[tokio::test]
	async fn the_wait_for_a_report_ends_on_arrival() {
		kio::clock::host_time();
		let server = Server::bind(vec![Media::H265]).await;
		let wait = Duration::from_millis(200);
		let client = client(server.url()).waiting(wait);
		let (broadcast, catalog, pull) = pull(client);

		let mut session = within("PLAY", server.accept(0)).await;
		let sent = wall_now();
		session.video(0, 0, H265_KEYFRAME).await;
		kio::clock::sleep(wait).await;
		session.video(0, FRAME, &[fake::H265_TRAIL]).await;
		let snapshot = catalog_with(&catalog, 1, 0).await;
		let reader = subscribe(&broadcast, video_track(&snapshot)).await;
		session.close().await;
		assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Closed);

		let frames = frames(reader).await;
		assert_eq!(frames.len(), 2, "the frames held during the wait publish");
		let wall = wall_of(&snapshot, &frames[0]);
		assert!(wall >= sent, "timed {} µs before the first frame was sent", sent - wall);
	}

	/// A camera without RTCP that falls silent during the report wait is still placed when the wait
	/// ends, rather than holding its catalog until a frame that never comes.
	#[tokio::test]
	async fn a_silent_camera_is_placed_when_the_report_wait_ends() {
		// Real sockets stamp each frame's arrival on the host clock, so the wait runs on it too.
		kio::clock::host_time();
		const REPORT_WAIT: Duration = Duration::from_millis(200);
		let server = Server::bind(vec![Media::H265]).await;
		let (broadcast, catalog, pull) = pull(client(server.url()).waiting(REPORT_WAIT));

		let mut session = within("PLAY", server.accept(0)).await;
		session.video(0, 0, H265_KEYFRAME).await;
		session.video(0, FRAME, &[fake::H265_TRAIL]).await;
		// Nothing follows, so only the end of the wait can publish the catalog.
		let snapshot = catalog_with(&catalog, 1, 0).await;
		let published = wall_now();
		let reader = subscribe(&broadcast, video_track(&snapshot)).await;
		session.close().await;
		assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Closed);

		let frames = frames(reader).await;
		assert_eq!(frames.len(), 2, "both held frames publish");
		let arrived = wall_of(&snapshot, &frames[0]);
		assert!(
			published - arrived >= REPORT_WAIT.as_micros() as i128,
			"published {} µs after the first frame arrived, before the wait was over",
			published - arrived
		);
		assert_eq!(wall_of(&snapshot, &frames[1]) - arrived, 33_333, "one frame later");
	}

	/// A session ending during its report wait still publishes the frames it held.
	#[tokio::test]
	async fn a_session_ending_during_its_report_wait_publishes_what_it_held() {
		kio::clock::host_time(); // real RTSP sockets, and the report wait is real time
		let server = Server::bind(vec![Media::H265]).await;
		let client = client(server.url()).waiting(10 * WAIT);
		let (broadcast, _catalog, pull) = pull(client);

		let mut session = within("PLAY", server.accept(0)).await;
		// Read from before the end, as no catalog lists the track until the session is placed:
		// `0.hev1` is the first name `unique_track` mints for H.265 on a fresh broadcast.
		let reader = subscribe(
			&broadcast,
			(
				"0.hev1".to_string(),
				moq_mux::catalog::hang::Container::Legacy(moq_mux::container::Kind::Video),
			),
		)
		.await;
		session.video(0, 0, H265_KEYFRAME).await;
		session.close().await;
		assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Closed);
		assert_eq!(frames(reader).await.len(), 1, "the held keyframe publishes");
	}

	/// A held keyframe publishes under the parameter sets it arrived with, even when a frame after
	/// it changes them before the session is placed.
	#[tokio::test]
	async fn a_held_keyframe_keeps_its_parameter_sets() {
		kio::clock::host_time(); // real RTSP sockets, and the report wait is real time
		let server = Server::bind(vec![Media::H264 { sprop: true }]).await;
		let client = client(server.url()).waiting(10 * WAIT);
		let (broadcast, catalog, pull) = pull(client);

		let mut session = within("PLAY", server.accept(0)).await;
		session.video(0, 0, &[fake::H264_IDR]).await;
		// The next frame brings a new SPS, at another level, while the keyframe is still held.
		let mut changed = fake::H264_SPS.to_vec();
		changed[3] = 0x1e;
		session
			.video(0, FRAME, &[&changed, fake::H264_PPS, fake::H264_SLICE])
			.await;
		session.report(0, 0, ntp(wall_now() - 300_000)).await;
		let snapshot = catalog_with(&catalog, 1, 0).await;
		let reader = subscribe(&broadcast, video_track(&snapshot)).await;
		session.close().await;
		assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Closed);

		let frames = frames(reader).await;
		let contains = |payload: &[u8], sps: &[u8]| {
			let sps = [&[0, 0, 0, 1], sps].concat();
			payload.windows(sps.len()).any(|window| window == sps)
		};
		assert!(
			contains(&frames[0].payload, fake::H264_SPS),
			"the keyframe keeps the SDP's SPS"
		);
		assert!(
			!contains(&frames[0].payload, &changed),
			"not the SPS a later frame brought"
		);
	}

	/// AAC alone lists its rendition before any frame, yet the catalog still waits for the
	/// session to be placed.
	#[tokio::test]
	async fn an_audio_only_session_is_timed_on_its_first_frame() {
		let server = Server::bind(vec![Media::Aac]).await;
		let (broadcast, catalog, pull) = pull(client(server.url()));

		let mut session = within("PLAY", server.accept(0)).await;
		let sent = wall_now();
		session.aac(0, 0, b"audio").await;
		let snapshot = catalog_with(&catalog, 0, 1).await;
		let listed = wall_now();
		let reader = subscribe(&broadcast, audio_track(&snapshot)).await;
		session.close().await;
		assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Closed);

		let wall = wall_of(&snapshot, &frames(reader).await[0]);
		assert!(
			(sent..=listed).contains(&wall),
			"timed {} µs after it was sent",
			wall - sent
		);
	}

	/// A camera clock slightly ahead of this host's says a frame was captured after it arrived:
	/// the session is timed at its arrival rather than in the future.
	#[tokio::test]
	async fn a_camera_clock_slightly_ahead_is_timed_at_arrival() {
		kio::clock::host_time(); // real RTSP sockets, and the report wait is real time
		let server = Server::bind(vec![Media::H265]).await;
		let client = client(server.url()).waiting(10 * WAIT);
		let (broadcast, catalog, pull) = pull(client);

		let mut session = within("PLAY", server.accept(0)).await;
		let sent = wall_now();
		session.video(0, 0, H265_KEYFRAME).await;
		session.report(0, 0, ntp(wall_now() + 200_000)).await;
		let snapshot = catalog_with(&catalog, 1, 0).await;
		let listed = wall_now();
		let reader = subscribe(&broadcast, video_track(&snapshot)).await;
		session.close().await;
		assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Closed);

		let wall = wall_of(&snapshot, &frames(reader).await[0]);
		assert!(
			(sent..=listed).contains(&wall),
			"timed {} µs after it was sent",
			wall - sent
		);
	}

	/// A clock fixed before the session, by a catalog published or read before the pull or by
	/// [`Config::with_clock`](moq_mux::catalog::Config::with_clock), is shifted onto: the frames
	/// land at their arrival on it, not ten seconds or more before it.
	#[tokio::test]
	async fn a_clock_fixed_before_the_session_is_shifted_onto() {
		kio::clock::host_time(); // real RTSP sockets, and the delay is real time
		for fixed in ["read before a delayed pull", "configured"] {
			let server = Server::bind(vec![Media::H265]).await;
			let mut broadcast = moq_net::broadcast::Info::new().produce();
			let config = match fixed {
				"configured" => moq_mux::catalog::Config::default().with_clock(moq_mux::Clock::new()),
				_ => moq_mux::catalog::Config::default(),
			};
			let catalog = moq_mux::catalog::Producer::new(&mut broadcast, config).unwrap();
			let _fixed = catalog.clock();
			kio::clock::sleep(Duration::from_millis(200)).await;
			let pull = tokio::spawn({
				let (broadcast, catalog, client) = (broadcast.clone(), catalog.clone(), client(server.url()));
				async move { client.pull(&broadcast, &catalog).await }
			});

			let mut session = within("PLAY", server.accept(0)).await;
			let sent = wall_now();
			session.video(0, 0, H265_KEYFRAME).await;
			let snapshot = catalog_with(&catalog, 1, 0).await;
			let listed = wall_now();
			let reader = subscribe(&broadcast, video_track(&snapshot)).await;
			session.video(0, FRAME, &[fake::H265_TRAIL]).await;
			session.close().await;
			assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Closed);

			let frames = frames(reader).await;
			let wall = wall_of(&snapshot, &frames[0]);
			assert!(
				(sent..=listed).contains(&wall),
				"{fixed}: the first frame lands {} µs after it was sent, {} µs before the catalog listed it",
				wall - sent,
				listed - wall
			);
			// The shift rounds down to a whole 90 kHz tick.
			let step = wall_of(&snapshot, &frames[1]) - wall;
			assert!((33_322..=33_345).contains(&step), "{fixed}: one frame later, {step} µs");
		}
	}

	/// Two sessions sharing one catalog each land where their camera captured them, so their
	/// renditions line up by wall time though each counts from its own PLAY, even when the second
	/// opens on a capture before the one the first placed the clock on.
	#[tokio::test]
	async fn two_sessions_sharing_a_catalog_each_land_at_capture() {
		kio::clock::host_time(); // real RTSP sockets, and the report wait is real time
		let first = Server::bind(vec![Media::H265]).await;
		let second = Server::bind(vec![Media::H264 { sprop: true }]).await;
		let (broadcast, catalog) = broadcast();
		let spawn = |client: Client| {
			let (broadcast, catalog) = (broadcast.clone(), catalog.clone());
			tokio::spawn(async move { client.pull(&broadcast, &catalog).await })
		};
		let pulls = [
			spawn(client(first.url()).waiting(10 * WAIT)),
			spawn(client(second.url()).waiting(10 * WAIT)),
		];

		let mut a = within("PLAY", first.accept(0)).await;
		let captured = wall_now() - 300_000;
		a.video(0, 0, H265_KEYFRAME).await;
		a.report(0, 0, ntp(captured)).await;
		catalog_with(&catalog, 1, 0).await;
		kio::clock::sleep(Duration::from_millis(200)).await;

		// Captured 20 ms before the first camera's keyframe, at its own RTP time zero.
		let mut b = within("PLAY", second.accept(0)).await;
		b.video(0, 0, &[fake::H264_IDR]).await;
		b.report(0, 0, ntp(captured - 20_000)).await;
		let snapshot = catalog_with(&catalog, 2, 0).await;
		let mut readers = Vec::new();
		for (name, config) in &snapshot.video.renditions {
			readers.push(subscribe(&broadcast, (name.clone(), config.try_into().unwrap())).await);
		}
		a.close().await;
		b.close().await;
		for pull in pulls {
			assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Closed);
		}

		let mut walls = Vec::new();
		for reader in readers {
			walls.push(wall_of(&snapshot, &frames(reader).await[0]));
		}
		walls.sort();
		assert_eq!(walls[1], captured, "the first session places the clock at its capture");
		// The second shifts onto it, rounded down to a whole 90 kHz tick.
		let apart = walls[1] - walls[0];
		assert!((20_000..=20_011).contains(&apart), "{apart} µs apart");
	}

	/// Video held after a loss still has to see a keyframe in time, or the pull fails rather
	/// than leaving the broadcast frozen.
	#[tokio::test]
	async fn held_video_without_a_keyframe_stalls() {
		let clock = manual();
		let server = Server::bind(vec![Media::H264 { sprop: true }]).await;
		let (broadcast, catalog) = broadcast();
		let mut client = client(server.url());
		// Lengthened so the clock can run well ahead of the frames read and only the keyframe
		// deadline fires.
		client.timeout = 10 * KEYFRAME_TIMEOUT;
		let mut pull = tokio::spawn(async move { client.pull(&broadcast, &catalog).await });

		let mut session = within("PLAY", server.accept(0)).await;
		session.video(0, 0, &[fake::H264_IDR]).await;
		session.lose(0, 1);
		let err = within("the pull", async {
			for frame in 2.. {
				session.video(0, frame * FRAME, &[fake::H264_SLICE]).await;
				clock.advance(Duration::from_secs(1));
				tokio::select! {
					result = &mut pull => return result.unwrap().unwrap_err(),
					// A driver turn, so the pull can read what was sent.
					_ = kio::clock::sleep(Duration::ZERO) => {}
				}
			}
			unreachable!()
		})
		.await;
		assert!(
			matches!(err, Error::Stalled(timeout) if timeout == KEYFRAME_TIMEOUT),
			"{err}"
		);
	}

	/// A server that opens on the group it buffered before PLAY replays frames from before its
	/// `rtptime`: the session starts on that replayed keyframe, timed at its capture, rather than
	/// failing.
	#[tokio::test]
	async fn frames_replayed_from_before_rtptime_publish() {
		kio::clock::host_time(); // real RTSP sockets, and the report wait is real time
		// retina reads `rtptime` only when playing several streams.
		let server = Server::bind(vec![Media::H264 { sprop: true }, Media::Aac]).await;
		let (broadcast, catalog, pull) = pull(client(server.url()).waiting(10 * WAIT));

		// The live start, 1.05 s after the keyframe the server replays first.
		let live = 10 * 90_000;
		let replayed = live - 94_500;
		let mut session = within("PLAY", server.accept(live)).await;
		// The replayed keyframe was captured 1.05 s before the live start, which is now.
		let captured = wall_now() - 1_050_000;
		session.video(0, replayed, &[fake::H264_IDR]).await;
		session.video(0, replayed + FRAME, &[fake::H264_SLICE]).await;
		session.video(0, live, &[fake::H264_SLICE]).await;
		session.aac(1, live, b"audio").await;
		session.report(0, replayed, ntp(captured)).await;
		session.report(1, live, ntp(captured + 1_050_000)).await;
		let snapshot = catalog_with(&catalog, 1, 1).await;
		let video = subscribe(&broadcast, video_track(&snapshot)).await;
		let audio = subscribe(&broadcast, audio_track(&snapshot)).await;
		session.close().await;
		assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Closed);

		let video = frames(video).await;
		assert_eq!(video.len(), 3, "the replayed group publishes, then the live frames");
		assert!(video[0].keyframe);
		assert_eq!(wall_of(&snapshot, &video[0]), captured, "timed at its capture");
		let audio = frames(audio).await;
		assert_eq!(wall_of(&snapshot, &audio[0]), captured + 1_050_000);
	}

	/// A report arriving partway through a replayed group, before any live frame, looks far from
	/// the replayed frames' arrivals: the session waits for the live frames to agree with it
	/// instead of timing the stream on the replay's arrival, seconds late.
	#[tokio::test]
	async fn a_report_during_a_replay_waits_for_the_live_frames() {
		kio::clock::host_time(); // real RTSP sockets, and the report wait is real time
		let server = Server::bind(vec![Media::H265]).await;
		let (broadcast, catalog, pull) = pull(client(server.url()).waiting(10 * WAIT));

		let mut session = within("PLAY", server.accept(0)).await;
		// The replayed keyframe was captured 8 s ago, and the live frame is now.
		let captured = wall_now() - 8_000_000;
		session.video(0, 0, H265_KEYFRAME).await;
		session.report(0, 0, ntp(captured)).await;
		session.video(0, 8 * 90_000, &[fake::H265_TRAIL]).await;
		let snapshot = catalog_with(&catalog, 1, 0).await;
		let video = subscribe(&broadcast, video_track(&snapshot)).await;
		session.close().await;
		assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Closed);

		let video = frames(video).await;
		assert_eq!(wall_of(&snapshot, &video[0]), captured, "timed at its capture");
		assert_eq!(wall_of(&snapshot, &video[1]), captured + 8_000_000);
	}

	/// A replayed group arrives faster than the live pace, so its first frames look captured as they
	/// arrived: with a report too far off to time it, the stream waits out the report interval and
	/// is timed on its live frames, or every frame would land after it arrived.
	#[tokio::test]
	async fn a_replay_with_a_wrong_camera_clock_is_timed_on_its_live_frames() {
		kio::clock::host_time(); // real RTSP sockets, and the report wait is real time
		let server = Server::bind(vec![Media::H265]).await;
		let (broadcast, catalog, pull) = pull(client(server.url()).waiting(Duration::from_millis(500)));

		let mut session = within("PLAY", server.accept(0)).await;
		// The replayed keyframe was captured 300 ms before the live start, which is now.
		let live = 27_000;
		let sent = wall_now();
		session.video(0, 0, H265_KEYFRAME).await;
		session.video(0, FRAME, &[fake::H265_TRAIL]).await;
		// A camera clock 15 min ahead.
		session.report(0, 0, ntp(sent + 900_000_000)).await;
		session.video(0, live, &[fake::H265_TRAIL]).await;
		let snapshot = catalog_with(&catalog, 1, 0).await;
		let listed = wall_now();
		let video = subscribe(&broadcast, video_track(&snapshot)).await;
		session.close().await;
		assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Closed);

		let video = frames(video).await;
		assert_eq!(video.len(), 3, "the replayed group publishes, then the live frame");
		for frame in &video {
			let wall = wall_of(&snapshot, frame);
			assert!(
				wall <= listed,
				"timed {} µs after the catalog was listed",
				wall - listed
			);
		}
		let captured = wall_of(&snapshot, &video[0]);
		assert!(
			captured <= sent - 250_000,
			"the replayed keyframe timed {} µs before it was sent",
			sent - captured
		);
	}

	/// Two streams whose first packets came a second apart each count from their own: each lands
	/// where its own sender report says it was captured.
	#[tokio::test]
	async fn streams_starting_apart_are_placed_apart() {
		kio::clock::host_time(); // real RTSP sockets, and the report wait is real time
		let server = Server::bind(vec![Media::H265, Media::Aac]).await;
		let (broadcast, catalog, pull) = pull(client(server.url()).waiting(10 * WAIT));

		let mut session = within("PLAY", server.accept(0)).await;
		let captured = wall_now() - 1_200_000;
		session.video(0, 7_000, H265_KEYFRAME).await;
		session.video(0, 7_000 + 90_000, &[fake::H265_TRAIL]).await;
		// The audio's first packet, a second into the video, reads zero like the video's.
		session.aac(1, 3_000, b"audio").await;
		session.report(0, 7_000, ntp(captured)).await;
		session.report(1, 3_000, ntp(captured + 1_000_000)).await;
		let snapshot = catalog_with(&catalog, 1, 1).await;
		let video = subscribe(&broadcast, video_track(&snapshot)).await;
		let audio = subscribe(&broadcast, audio_track(&snapshot)).await;
		session.close().await;
		assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Closed);

		let video = frames(video).await;
		let audio = frames(audio).await;
		assert_eq!(wall_of(&snapshot, &video[0]), captured);
		assert_eq!(wall_of(&snapshot, &video[1]), captured + 1_000_000);
		assert_eq!(
			wall_of(&snapshot, &audio[0]),
			captured + 1_000_000,
			"the audio lands with the video captured beside it"
		);
	}

	/// A stream without a sender report of its own is placed at its own arrivals, not at another
	/// stream's report or at the session's start.
	#[tokio::test]
	async fn a_stream_without_a_report_is_placed_at_its_arrival() {
		kio::clock::host_time(); // real RTSP sockets, and the report wait is real time
		let server = Server::bind(vec![Media::H265, Media::Aac]).await;
		let wait = Duration::from_millis(200);
		let (broadcast, catalog, pull) = pull(client(server.url()).waiting(wait));

		let mut session = within("PLAY", server.accept(0)).await;
		let captured = wall_now() - 300_000;
		session.video(0, 0, H265_KEYFRAME).await;
		session.report(0, 0, ntp(captured)).await;
		kio::clock::sleep(wait).await;
		// Well after the video's, though both streams' first packets read zero.
		let sent = wall_now();
		session.aac(1, 0, b"audio").await;
		let snapshot = catalog_with(&catalog, 1, 1).await;
		let video = subscribe(&broadcast, video_track(&snapshot)).await;
		let audio = subscribe(&broadcast, audio_track(&snapshot)).await;
		session.close().await;
		assert_eq!(within("the pull", pull).await.unwrap().unwrap(), End::Closed);
		let ended = wall_now();

		assert_eq!(wall_of(&snapshot, &frames(video).await[0]), captured);
		let wall = wall_of(&snapshot, &frames(audio).await[0]);
		assert!(
			(sent..=ended).contains(&wall),
			"the audio lands {} µs after it was sent, {} µs before the session ended",
			wall - sent,
			ended - wall
		);
	}

	/// A session whose report wait is over is placed at its deadline: the pull wakes for it, with
	/// no frame arriving to end the wait.
	#[tokio::test]
	async fn a_wait_expires_at_its_deadline() {
		let clock = manual();
		let (broadcast, catalog) = broadcast();
		let track = broadcast.unique_track(".aac", catalog.track_info(hang::catalog::PRIORITY.audio));
		let config = aac::config(fake::AAC_CONFIG).unwrap();
		let mut ingest = Ingest {
			streams: vec![Some(Stream {
				track: Track::Aac(Box::new(aac::Import::new(track, catalog.reserve(), config).unwrap())),
				last: None,
				started: false,
				held: None,
				opened: None,
				fresh: true,
				clock: StreamClock::Unplaced {
					report: None,
					bound: None,
				},
			})],
			video: false,
			started: false,
			max_forward: MAX_FORWARD,
			timebase: catalog.timebase(),
			timing: Timing::Waiting(Box::new(Waiting {
				gate: catalog.reserve(),
				wait: REPORT_INTERVAL,
				since: None,
				held: Vec::new(),
				bytes: 0,
			})),
		};
		assert_eq!(ingest.timing.due(), None, "nothing held, nothing to wake for");
		assert!(!ingest.ready(true), "nothing held, nothing to place");

		let first = kio::clock::now();
		let Timing::Waiting(waiting) = &mut ingest.timing else {
			unreachable!()
		};
		waiting.since = Some(first);
		assert_eq!(ingest.timing.due(), Some(first.saturating_add(REPORT_INTERVAL)));
		assert!(!ingest.ready(false), "waiting for the stream's report");

		clock.advance(REPORT_INTERVAL);
		assert!(ingest.ready(false), "the wait is over");
	}

	#[tokio::test]
	async fn wrong_credentials_are_unauthorized() {
		let server = Server::bind(vec![Media::H264 { sprop: true }])
			.await
			.with_basic_auth("admin", "secret");
		let mut url = server.url();
		url.set_username("admin").unwrap();
		url.set_password(Some("wrong")).unwrap();

		let client = client(url);
		let (broadcast, catalog) = broadcast();
		let pull = tokio::spawn(async move { client.pull(&broadcast, &catalog).await });
		let _accept = tokio::spawn(async move { server.accept(0).await });
		let err = within("the pull", pull).await.unwrap().unwrap_err();
		assert!(matches!(err, Error::Unauthorized(_)), "{err}");
	}

	#[tokio::test]
	async fn forbidden_is_unauthorized() {
		let server = Server::bind(vec![Media::H264 { sprop: true }]).await.forbidden();
		let client = client(server.url());
		let (broadcast, catalog) = broadcast();
		let pull = tokio::spawn(async move { client.pull(&broadcast, &catalog).await });
		let _accept = tokio::spawn(async move { server.accept(0).await });
		let err = within("the pull", pull).await.unwrap().unwrap_err();
		assert!(matches!(err, Error::Unauthorized(_)), "{err}");
	}

	#[test]
	fn only_rtsp_urls_are_accepted() {
		assert!(matches!(
			Client::new("http://camera/stream".parse().unwrap()),
			Err(Error::InvalidUrl(_))
		));
	}
}
