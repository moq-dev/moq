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

/// Why [`Client::pull`] returned without an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum End {
	/// The server ended the session.
	Closed,
	/// The session's RTP timeline jumped, so its tracks finished before the jump.
	///
	/// Its timestamps no longer continue the broadcast's, so the caller finishes the
	/// broadcast and restarts every session sharing it under a fresh epoch.
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
		})
	}

	/// Ingest one session into `broadcast`, publishing its renditions on `catalog`.
	///
	/// Publishes the session's H.264 and H.265 video and AAC audio, starting on a keyframe.
	/// Audio the stack cannot play is skipped with a warning. The session's timestamps shift
	/// onto the catalog's clock, so several sessions can publish renditions of one broadcast.
	///
	/// Never reconnects: returns once the session ends or its timeline jumps, finishing its
	/// tracks, or fails, aborting them, including when a request or the media stalls. The
	/// caller owns the broadcast and decides whether to retry. Dropping the future stops the
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

		let reserved = timebase.reserve();
		let mut ingest = Ingest {
			streams: Vec::new(),
			video: tracks.iter().any(|(_, kind)| *kind != Kind::Aac),
			started: false,
			max_forward: self.max_forward,
		};
		ingest.streams.resize_with(session.streams().len(), || None);
		// A failure here aborts the tracks already made rather than leaving `Drop` to finish them.
		let built = (|| -> Result<()> {
			for (index, kind) in &tracks {
				let track = match kind {
					Kind::H264 => {
						let track =
							broadcast.unique_track(".avc3", catalog.track_info(hang::catalog::PRIORITY.video))?;
						Track::Video(Box::new(Video::H264 {
							split: h264::Split::new(),
							import: h264::Import::new(track, reserved.clone(), Default::default())?,
						}))
					}
					Kind::H265 => {
						let track =
							broadcast.unique_track(".hev1", catalog.track_info(hang::catalog::PRIORITY.video))?;
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
						let track =
							broadcast.unique_track(".aac", catalog.track_info(hang::catalog::PRIORITY.audio))?;
						Track::Aac(Box::new(aac::Import::new(track, reserved.clone(), config)?))
					}
				};
				ingest.streams[*index] = Some(Stream {
					track,
					last: None,
					started: false,
					held: None,
					fresh: true,
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

		// Not every camera sends `rtptime` for every stream; fall back to the first packet
		// rather than refusing it, leaving those streams only roughly in sync.
		let play = PlayOptions::default().initial_timestamp(retina::client::InitialTimestampPolicy::Permissive);
		let result: Result<End> = async {
			let mut session = self.within(session.play(play)).await??.demuxed()?;
			tracing::info!(%url, streams = tracks.len(), "RTSP session playing");
			let playing = kio::clock::now();
			// Measured from the last frame, so RTCP reports alone cannot keep a dead camera alive.
			let mut deadline = playing + self.timeout;
			while let Some(item) = kio::clock::timeout(deadline - kio::clock::now(), session.next())
				.await
				.map_err(|_| Error::Stalled(self.timeout))?
			{
				let item = item?;
				if matches!(item, CodecItem::VideoFrame(_) | CodecItem::AudioFrame(_)) {
					deadline = kio::clock::now() + self.timeout;
				}
				if ingest.push(item, session.streams())?.is_some() {
					tracing::warn!(%url, "RTSP timeline jumped, ending the session");
					return Ok(End::Jumped);
				}
				if ingest.awaiting_keyframe(playing, KEYFRAME_TIMEOUT) {
					return Err(Error::Stalled(KEYFRAME_TIMEOUT));
				}
			}
			Ok(End::Closed)
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
	fn finish(&mut self) -> moq_mux::Result<()> {
		match self {
			Self::Video(video) => match &mut **video {
				Video::H264 { import, .. } => import.finish(),
				Video::H265 { import, .. } => import.finish(),
			},
			Self::Aac(import) => import.finish(),
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
	/// The previous frame's timestamp, in the stream's clock units since the session start, and
	/// when it arrived.
	last: Option<(i64, kio::clock::Instant)>,
	/// Whether a keyframe has been published (video) or the session started (audio).
	started: bool,
	/// When a lost packet broke the video's reference chain, so it waits for a keyframe.
	held: Option<kio::clock::Instant>,
	/// Whether retina's parameter sets still need inserting: until the splitter emits a keyframe
	/// carrying them, after which it re-inserts them ahead of every keyframe itself.
	fresh: bool,
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
	/// Publish one depacketized item, or report that the timeline jumped.
	fn push(&mut self, item: CodecItem, streams: &[retina::client::Stream]) -> Result<Option<Jump>> {
		let (stream_id, timestamp, data, changed, received, loss) = match &item {
			CodecItem::VideoFrame(frame) => (
				frame.stream_id(),
				frame.timestamp(),
				frame.data(),
				frame.has_new_parameters(),
				kio::clock::Instant::from_std(frame.end_ctx().received()),
				frame.loss(),
			),
			CodecItem::AudioFrame(frame) => (
				frame.stream_id(),
				frame.timestamp(),
				frame.data(),
				false,
				kio::clock::Instant::from_std(frame.ctx().received()),
				// AAC packets decode alone, and the decoder conceals a lost one.
				0,
			),
			_ => return Ok(None),
		};
		let Some(stream) = self.streams.get_mut(stream_id).and_then(Option::as_mut) else {
			return Ok(None);
		};

		let elapsed = timestamp.elapsed();
		let rate = timestamp.clock_rate().get();
		if let Some((last, arrived)) = stream.last {
			let delta = i128::from(elapsed) - i128::from(last);
			let slack = self.max_forward + received.saturating_duration_since(arrived);
			let forward = slack.as_micros() as i128 * i128::from(rate) / 1_000_000;
			if delta < -backward(rate) || delta > forward {
				return Ok(Some(Jump));
			}
		}
		let Ok(units) = u64::try_from(elapsed) else {
			// A reordered picture can land just before the session's start, such as a leading
			// picture of the keyframe it opened on. Undecodable from here, so drop it. Further
			// back, the server's `rtptime` is wrong and nothing would ever publish.
			if i128::from(elapsed) < -backward(rate) {
				return Err(Error::Rtsp(format!(
					"stream {stream_id} starts {elapsed} units before the server's rtptime"
				)));
			}
			return Ok(None);
		};
		let pts = moq_net::Timestamp::from_scale(units, u64::from(rate)).map_err(moq_mux::Error::from)?;

		match &mut stream.track {
			Track::Video(video) => {
				// Before any drop, so the next clean keyframe still gets retina's new parameters.
				stream.fresh |= changed;
				if loss > 0 {
					// The pictures after a lost packet reference what it carried, so hold the
					// video until the next keyframe rather than publish a broken chain. This
					// access unit is damaged too, even when it is a keyframe, so drop it.
					if stream.started && stream.held.is_none() {
						tracing::warn!(stream_id, loss, "RTP packets lost, waiting for the next keyframe");
						stream.held = Some(kio::clock::now());
					}
					video.reset();
					return Ok(None);
				}
				let params = match streams.get(stream_id).and_then(|stream| stream.parameters()) {
					Some(ParametersRef::Video(params)) if stream.fresh => Some(params),
					_ => None,
				};
				let frames = video.split(data, params, pts)?;
				let keyframe = frames.iter().any(|frame| frame.keyframe);
				stream.fresh &= !keyframe;
				if !stream.started || stream.held.is_some() {
					// Publish nothing before a keyframe, by the splitter's reckoning (an IDR, an
					// H.264 recovery point, or any H.265 IRAP picture).
					if !frames.first().is_some_and(|frame| frame.keyframe) {
						return Ok(None);
					}
					stream.started = true;
					stream.held = None;
					self.started = true;
				}
				stream.last = Some((elapsed, received));
				video.decode(frames)?;
			}
			Track::Aac(import) => {
				if !stream.started {
					// Audio waits for the first keyframe too.
					if self.video && !self.started {
						return Ok(None);
					}
					stream.started = true;
				}
				stream.last = Some((elapsed, received));
				import.decode(data, Some(pts))?;
				// Each packet its own group (one QUIC stream), so relays forward it at once.
				import.cut(None)?;
			}
		}
		Ok(None)
	}

	/// Whether a video stream has waited longer than `timeout` for a keyframe: its first since
	/// `playing`, or its next since a loss held it.
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
		for mut stream in self.streams.drain(..).flatten() {
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
		let subscription = moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(60));
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
		let client = Client::new(server.url()).unwrap();
		let pull = tokio::spawn({
			let (broadcast, catalog) = (broadcast.clone(), catalog.clone());
			async move { client.pull(&broadcast, &catalog).await }
		});

		let mut session = within("PLAY", server.accept(1000)).await;
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
			video[0].timestamp.as_micros(),
			33_333,
			"one frame past the session start"
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
			audio[0].timestamp.as_micros(),
			42_666,
			"two AAC frames past the session start"
		);
	}

	/// An H.264 recovery point starts the broadcast, though retina flags only an IDR as a
	/// keyframe, and it carries the SDP's parameter sets.
	#[tokio::test]
	async fn h264_starts_on_a_recovery_point() {
		let server = Server::bind(vec![Media::H264 { sprop: true }]).await;
		let (broadcast, catalog) = broadcast();
		let client = Client::new(server.url()).unwrap();
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
		let client = Client::new(server.url()).unwrap();
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
		let client = Client::new(server.url()).unwrap();
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
		let client = Client::new(server.url()).unwrap();
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
		let client = Client::new(server.url()).unwrap();
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
	/// epoch, each new broadcast with its own catalog.
	#[tokio::test]
	async fn two_sessions_restart_together_on_a_jump() {
		let (origin, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
		tokio::spawn(moq_net::time::run(driver));
		let sd = Server::bind(vec![Media::H264 { sprop: true }]).await;
		let hd = Server::bind(vec![Media::H265]).await;
		let sd_client = Client::new(sd.url()).unwrap();
		let hd_client = Client::new(hd.url()).unwrap();

		let (published_tx, mut published) = tokio::sync::mpsc::unbounded_channel();
		let caller = tokio::spawn({
			let origin = origin.clone();
			async move {
				let mut ends = Vec::new();
				for _ in 0..2 {
					let epoch = moq_net::Epoch::mint();
					let route = moq_net::origin::Route::default().with_epoch(epoch.clone());
					let broadcast = origin.publish("camera", route).unwrap();
					let mut catalog =
						moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), Default::default())
							.unwrap();
					published_tx.send((epoch, broadcast.clone(), catalog.clone())).unwrap();
					// The first session to return ends the run; dropping the other's pull
					// finishes its tracks.
					let end = tokio::select! {
						end = sd_client.pull(&broadcast, &catalog) => end,
						end = hd_client.pull(&broadcast, &catalog) => end,
					};
					ends.push(end.unwrap());
					catalog.finish().unwrap();
					broadcast.close();
				}
				ends
			}
		});

		let mut epochs = Vec::new();
		for run in 0..2 {
			let (epoch, broadcast, catalog) = within("the broadcast", published.recv()).await.unwrap();
			epochs.push(epoch);
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
		assert!(epochs[1] > epochs[0], "the restart mints a fresh epoch");
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

		let client = Client::new(url).unwrap();
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
		let client = Client::new(server.url()).unwrap();
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
		let client = Client::new(server.url()).unwrap();
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
		let mut client = Client::new(server.url()).unwrap();
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
		let server = Server::bind(vec![Media::H264 { sprop: true }]).await;
		let (broadcast, catalog) = broadcast();
		let mut client = Client::new(server.url()).unwrap();
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
		let client = Client::new(server.url()).unwrap();
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
		let client = Client::new(server.url()).unwrap();
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
			frames[0].timestamp.as_micros(),
			100_000,
			"the track opens on the clean keyframe"
		);
	}

	/// Video held after a loss still has to see a keyframe in time, or the pull fails rather
	/// than leaving the broadcast frozen.
	#[tokio::test]
	async fn held_video_without_a_keyframe_stalls() {
		let clock = manual();
		let server = Server::bind(vec![Media::H264 { sprop: true }]).await;
		let (broadcast, catalog) = broadcast();
		let mut client = Client::new(server.url()).unwrap();
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

	/// A server whose `rtptime` lands well after its packets would never publish anything, so
	/// the pull fails rather than idling.
	#[tokio::test]
	async fn a_wrong_rtptime_fails() {
		// retina reads `rtptime` only when playing several streams.
		let server = Server::bind(vec![Media::H264 { sprop: true }, Media::Aac]).await;
		let (broadcast, catalog) = broadcast();
		let client = Client::new(server.url()).unwrap();
		let pull = tokio::spawn(async move { client.pull(&broadcast, &catalog).await });

		let mut session = within("PLAY", server.accept(10 * 90_000)).await;
		session.video(0, 0, &[fake::H264_IDR]).await;
		let err = within("the pull", pull).await.unwrap().unwrap_err();
		assert!(matches!(err, Error::Rtsp(_)), "{err}");
	}

	#[tokio::test]
	async fn wrong_credentials_are_unauthorized() {
		let server = Server::bind(vec![Media::H264 { sprop: true }])
			.await
			.with_basic_auth("admin", "secret");
		let mut url = server.url();
		url.set_username("admin").unwrap();
		url.set_password(Some("wrong")).unwrap();

		let client = Client::new(url).unwrap();
		let (broadcast, catalog) = broadcast();
		let pull = tokio::spawn(async move { client.pull(&broadcast, &catalog).await });
		let _accept = tokio::spawn(async move { server.accept(0).await });
		let err = within("the pull", pull).await.unwrap().unwrap_err();
		assert!(matches!(err, Error::Unauthorized(_)), "{err}");
	}

	#[tokio::test]
	async fn forbidden_is_unauthorized() {
		let server = Server::bind(vec![Media::H264 { sprop: true }]).await.forbidden();
		let client = Client::new(server.url()).unwrap();
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
