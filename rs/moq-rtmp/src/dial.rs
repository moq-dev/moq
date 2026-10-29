//! RTMP client (dial-out): connect to a remote RTMP server and bridge it to MoQ.
//!
//! The mirror of [`crate::server`]: where that runs a listener and accepts
//! publishers/players, this *dials* a remote `rtmp://host[:1935]/<app>/<key>` and
//! drives an [`rml_rtmp`] `ClientSession` in one of two directions:
//!
//! - **[`Client::publish`] (push / restream)**: read a MoQ broadcast from an
//!   origin, mux it to FLV with [`moq_mux`], split it back into tags, and publish
//!   each as an RTMP audio/video message. This restreams MoQ out to a remote
//!   ingest (e.g. `rtmp://a.rtmp.youtube.com/live2/<key>`, Twitch, an SRS/nginx
//!   relay). The egress counterpart of the listener's play path.
//! - **[`Client::pull`] (ingest)**: play a stream from the remote, receive its FLV
//!   tags, demux them with [`moq_mux`], and publish the result into an origin as an
//!   ordinary MoQ broadcast. This ingests a remote RTMP source (e.g. pull from
//!   another relay). The ingest counterpart of the listener's publish path.
//!
//! Both legacy RTMP (H.264 + AAC, MP3) and enhanced RTMP (E-RTMP) work in each
//! direction: the codec handling lives in the [`moq_mux`] FLV demuxer/muxer, and
//! this module only drives the RTMP client transport. It reuses the same
//! [`crate::flv`] tag framing and moq-mux import/export plumbing as the server.
//!
//! Transport: [`Client::connect`] dials plaintext TCP (`rtmp://`). To reach an
//! `rtmps://` endpoint, or any other transport, establish the stream yourself
//! (e.g. a `tokio_rustls` client stream) and hand it to [`Client::with_stream`];
//! everything here is generic over the [`crate::Stream`] trait.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::Duration;

use crate::rml::handshake::{Handshake, HandshakeProcessResult, PeerType};
use crate::rml::sessions::{
	ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult, PublishRequestType,
};
use crate::rml::time::RtmpTimestamp;
use bytes::Bytes;
use moq_mux::container::flv::{Export as FlvExport, Import as FlvImport};
use moq_net::origin;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::Result;
use crate::flv;
use crate::server::Stream;

/// Read buffer size for pulling RTMP chunk-stream bytes off the socket.
const READ_BUFFER: usize = 16 * 1024;

/// A connected RTMP client, ready to publish a MoQ broadcast to the remote or pull
/// a remote stream into an origin.
///
/// Build it with [`connect`](Self::connect) (plaintext `rtmp://`) or
/// [`with_stream`](Self::with_stream) (bring your own transport, e.g. `rtmps://`).
/// Both run the RTMP handshake and the `connect` command for the given app, so the
/// returned client is connected and idle. Then pick a direction:
///
/// - [`publish`](Self::publish): MoQ broadcast -> remote (push / restream).
/// - [`pull`](Self::pull): remote stream -> MoQ origin (ingest).
///
/// `S` is the underlying stream: a [`tokio::net::TcpStream`] from
/// [`connect`](Self::connect), or your own transport via
/// [`with_stream`](Self::with_stream).
pub struct Client<S = TcpStream> {
	stream: S,
	session: ClientSession,
	/// Session results queued during connect, drained by the first publish/pull.
	work: VecDeque<ClientSessionResult>,
	/// The `<app>` this client connected to, logged in place of the stream key.
	app: String,
	/// How long [`publish`](Self::publish)'s FLV muxer waits for a stalled group
	/// before skipping.
	max_delay: Duration,
	/// Retention declared on the media tracks [`pull`](Self::pull) publishes, or `None`
	/// for hang's own default.
	max_age: Option<Duration>,
	/// Connection allocator each track [`pull`](Self::pull) ingests claims its peak-hold bitrate on.
	bandwidth: moq_net::bandwidth::Allocator,
	/// How long a broadcast [`pull`](Self::pull) finished stays up.
	linger: Duration,
	/// Script data [`pull`](Self::pull) discards, including any sent before Play.Start.
	metadata: crate::metadata::Dropped,
}

impl Client<TcpStream> {
	/// Dial `addr` over plaintext TCP and complete the RTMP handshake + `connect`
	/// for `app` (the `<app>` in `rtmp://host/<app>/<key>`).
	///
	/// For `rtmps://` or any other transport, establish the stream yourself and use
	/// [`with_stream`](Self::with_stream) instead.
	pub async fn connect(addr: SocketAddr, app: &str) -> Result<Self> {
		moq_clock::timeout(crate::server::REQUEST_TIMEOUT, async {
			let stream = TcpStream::connect(addr).await?;
			crate::server::configure_socket(&stream, addr);
			// Advertise a tcUrl derived from the dial target: several ingest servers
			// (YouTube, Twitch, some nginx-rtmp configs) reject a connect without one.
			Self::with_stream_config(stream, app, Some(format!("rtmp://{addr}/{app}"))).await
		})
		.await
		.map_err(|_| anyhow::anyhow!("RTMP connection timed out"))?
	}
}

impl<S: Stream> Client<S> {
	/// Complete the RTMP handshake and `connect` for `app` over an
	/// already-established byte stream.
	///
	/// The bring-your-own-transport entry point: establish the connection (and, for
	/// `rtmps://`, the TLS handshake) yourself, then hand the stream here.
	pub async fn with_stream(stream: S, app: &str) -> Result<Self> {
		moq_clock::timeout(
			crate::server::REQUEST_TIMEOUT,
			Self::with_stream_config(stream, app, None),
		)
		.await
		.map_err(|_| anyhow::anyhow!("RTMP connect request timed out"))?
	}

	async fn with_stream_config(mut stream: S, app: &str, tc_url: Option<String>) -> Result<Self> {
		let remaining = moq_clock::timeout(crate::server::HANDSHAKE_TIMEOUT, client_handshake(&mut stream))
			.await
			.map_err(|_| anyhow::anyhow!("RTMP handshake timed out"))??;

		let mut config = ClientSessionConfig::new();
		config.tc_url = tc_url;
		let (mut session, initial) =
			ClientSession::new(config).map_err(|e| anyhow::anyhow!("rtmp client init: {e:?}"))?;
		let mut work: VecDeque<ClientSessionResult> = VecDeque::from(initial);
		// Bytes the handshake read past its end are the first RTMP chunks; feed them
		// so the session stays byte-aligned.
		if !remaining.is_empty() {
			let results = session
				.handle_input(&remaining)
				.map_err(|e| anyhow::anyhow!("rtmp handle_input: {e:?}"))?;
			work.extend(results);
		}
		work.push_back(
			session
				.request_connection(app.to_string())
				.map_err(|e| anyhow::anyhow!("rtmp request connect: {e:?}"))?,
		);

		// Pump until the server accepts the connect; queue anything else for the
		// caller's direction to drain.
		let mut buffer = [0u8; READ_BUFFER];
		'connect: loop {
			while let Some(result) = work.pop_front() {
				match result {
					ClientSessionResult::OutboundResponse(packet) => stream.write_all(&packet.bytes).await?,
					ClientSessionResult::RaisedEvent(ClientSessionEvent::ConnectionRequestAccepted) => {
						break 'connect;
					}
					ClientSessionResult::RaisedEvent(ClientSessionEvent::ConnectionRequestRejected { description }) => {
						return Err(anyhow::anyhow!("rtmp connect rejected: {description}").into());
					}
					ClientSessionResult::RaisedEvent(_) | ClientSessionResult::UnhandleableMessageReceived(_) => {}
				}
			}
			let n = stream.read(&mut buffer).await?;
			if n == 0 {
				return Err(anyhow::anyhow!("rtmp server closed during connect").into());
			}
			let results = session
				.handle_input(&buffer[..n])
				.map_err(|e| anyhow::anyhow!("rtmp handle_input: {e:?}"))?;
			work.extend(results);
		}

		Ok(Self {
			stream,
			session,
			work,
			app: app.to_string(),
			max_delay: crate::DEFAULT_MAX_DELAY,
			max_age: None,
			bandwidth: moq_net::bandwidth::Allocator::unlimited(),
			linger: moq_mux::Linger::DEFAULT,
			metadata: Default::default(),
		})
	}

	/// Set the FLV release delay and stalled-group budget for [`publish`](Self::publish).
	/// Defaults to 500 ms; zero releases available frames immediately without deadline drops.
	///
	/// The publish (egress) direction only; [`pull`](Self::pull) takes
	/// [`with_max_age`](Self::with_max_age) instead.
	pub fn with_max_delay(mut self, max_delay: Duration) -> Self {
		self.max_delay = max_delay;
		self
	}

	/// Set how long relays keep a non-latest group of the media tracks
	/// [`pull`](Self::pull) publishes fetchable. `None` keeps hang's own default.
	///
	/// A retention budget, not a delivery one: it never makes a subscriber play further
	/// behind live, it caps how far back a FETCH can still reach. The default suits a
	/// segmented egress (HLS/DASH) reading the broadcast downstream, which may only
	/// advertise segments that are still fetchable. Lower it when nothing reads history
	/// and the memory matters.
	///
	/// The pull (ingest) direction only; [`publish`](Self::publish) reads a broadcast
	/// someone else declared, and takes [`with_max_delay`](Self::with_max_delay) instead.
	pub fn with_max_age(mut self, max_age: impl Into<Option<Duration>>) -> Self {
		self.max_age = max_age.into();
		self
	}

	/// Claim each ingested track's peak-hold catalog bitrate on `bandwidth`.
	///
	/// A passthrough import has no configured ceiling, so it reserves the measured
	/// maximum instead. A co-resident encoder then targets what is left of the
	/// uplink. Unlimited (the default) claims nothing a sender can follow.
	///
	/// The pull (ingest) direction only; [`publish`](Self::publish) reads a
	/// broadcast someone else declared.
	pub fn with_bandwidth(mut self, bandwidth: moq_net::bandwidth::Allocator) -> Self {
		self.bandwidth = bandwidth;
		self
	}

	/// Keep the broadcast [`pull`](Self::pull) publishes up for `linger` after the remote
	/// completes the play, so a subscriber that arrives after the end still gets the tail.
	/// Defaults to [`moq_mux::Linger::DEFAULT`]; zero closes it at once.
	///
	/// The pull (ingest) direction only.
	pub fn with_linger(mut self, linger: Duration) -> Self {
		self.linger = linger;
		self
	}

	/// Push a MoQ broadcast out to the remote: request `publish` on `stream_key`,
	/// mux the broadcast to FLV, and send each tag as an RTMP audio/video message
	/// until the broadcast ends or the connection drops.
	///
	/// `path` names the broadcast to restream on `origin`; the FLV export resolves it
	/// (and any sibling broadcast a rendition's catalog `broadcast` field references)
	/// through the origin. This future resolves when the broadcast ends, so callers
	/// usually run it on its own task.
	pub async fn publish(
		mut self,
		stream_key: &str,
		origin: origin::Consumer,
		path: impl Into<moq_net::Path>,
	) -> Result<()> {
		let path: moq_net::Path = path.into();
		let request = self
			.session
			.request_publishing(stream_key.to_string(), PublishRequestType::Live)
			.map_err(|e| anyhow::anyhow!("rtmp request publish: {e:?}"))?;
		self.work.push_back(request);
		self.await_event(Direction::Publish).await?;

		// The stream key is the ingest credential (`rtmp://host/<app>/<key>`), so the
		// app and broadcast path stand in for it.
		tracing::info!(app = %self.app, %path, "rtmp publish accepted by remote");

		// Flush anything queued alongside the publish-accepted event before streaming.
		let queued = std::mem::take(&mut self.work);
		self.drain(queued).await?;

		let broadcast = origin.request(path, None).await?;
		let source = moq_mux::Source::new(origin, broadcast);
		let catalog = source
			.catalog::<()>(moq_mux::catalog::CatalogFormat::default())
			.await
			.map_err(|e| anyhow::anyhow!("init FLV export: {e}"))?;
		let mut export = FlvExport::new(source, catalog).with_delay(self.max_delay);
		let mut tags = flv::TagReader::new();
		let mut buffer = [0u8; READ_BUFFER];

		loop {
			tokio::select! {
				// Media from the broadcast: split into tags and publish each one.
				chunk = export.next() => match chunk? {
					Some(bytes) => {
						tags.push(&bytes);
						while let Some(tag) = tags.next()? {
							let ts = RtmpTimestamp::new(tag.timestamp);
							let result = match tag.tag_type {
								flv::TAG_VIDEO => self.session.publish_video_data(tag.body, ts, false),
								flv::TAG_AUDIO => self.session.publish_audio_data(tag.body, ts, false),
								_ => continue,
							}
							.map_err(|e| anyhow::anyhow!("rtmp publish media: {e:?}"))?;
							if let ClientSessionResult::OutboundResponse(packet) = result {
								self.stream.write_all(&packet.bytes).await?;
							}
						}
					}
					// The broadcast finished: unpublish, so the remote's ingest ends cleanly too.
					None => {
						let results = self
							.session
							.stop_publishing()
							.map_err(|e| anyhow::anyhow!("rtmp stop publish: {e:?}"))?;
						self.drain(results).await?;
						break;
					}
				},
				// Server input (acks, pings): feed the session so it stays healthy.
				res = self.stream.read(&mut buffer) => {
					let n = res?;
					if n == 0 {
						break;
					}
					let results = self
						.session
						.handle_input(&buffer[..n])
						.map_err(|e| anyhow::anyhow!("rtmp handle_input: {e:?}"))?;
					self.drain(results).await?;
				}
			}
		}

		self.stream.shutdown().await.ok();
		Ok(())
	}

	/// Pull a remote stream into `origin`: request `play` on `stream_key`, receive
	/// its FLV tags, demux them, and publish the result at `path` until the remote
	/// stops or the connection drops.
	///
	/// This future resolves when the remote stream ends, so callers usually run it
	/// on its own task.
	///
	/// A remote that completes the play (`NetStream.Play.Complete` or `NetStream.Play.Stop`)
	/// ends cleanly: the finished broadcast stays up for the [linger](Self::with_linger), and
	/// the future resolves after that. Dropping the future ends the linger early. A
	/// connection that drops first closes the broadcast at once.
	pub async fn pull(
		mut self,
		stream_key: &str,
		origin: &origin::Producer,
		path: impl Into<moq_net::Path>,
	) -> Result<()> {
		let path: moq_net::Path = path.into();
		let request = self
			.session
			.request_playback(stream_key.to_string())
			.map_err(|e| anyhow::anyhow!("rtmp request play: {e:?}"))?;
		self.work.push_back(request);
		self.metadata = crate::metadata::Dropped::new(path.as_str());
		self.await_event(Direction::Play).await?;

		let config = moq_mux::catalog::Config::default()
			.with_max_age(self.max_age)
			.with_bandwidth(self.bandwidth.clone());
		let mut publisher = Publisher::new(origin, path.as_str(), config)?;

		// The stream key is the ingest credential (`rtmp://host/<app>/<key>`), so the
		// app and broadcast path stand in for it.
		tracing::info!(app = %self.app, %path, epoch = %publisher.epoch, "rtmp play accepted by remote");

		let result = self.pull_media(&mut publisher).await;
		match result {
			// A clean end: serve the finished broadcast for the linger, so a subscriber that
			// arrives after the end still gets the tail. The remote is done, so hang up first.
			Ok(true) => {
				let linger = self.linger;
				drop(self);
				match publisher.finish(linger) {
					Ok(publisher) => {
						tracing::info!(%path, ?linger, "rtmp play completed, serving the finished broadcast");
						publisher.wait().await;
					}
					Err(err) => tracing::debug!(%err, "error finishing RTMP pull"),
				}
			}
			// The connection dropped without completing the play: flush, then close at once.
			Ok(false) => {
				if let Err(err) = publisher.close() {
					tracing::debug!(%err, "error closing RTMP pull");
				}
			}
			// The read loop failed (a protocol error): abort with the real cause so
			// subscribers see it instead of a bare drop's Error::Dropped.
			Err(err) => {
				publisher.abort(moq_net::Error::Transport(err.to_string()));
				return Err(err.into());
			}
		}
		Ok(())
	}

	/// Read FLV tags from the remote and feed them to `publisher` until the remote
	/// completes the play (`true`) or the connection closes (`false`).
	///
	/// Split out from [`Self::pull`] so a read/demux error propagates to the caller,
	/// which aborts the publisher with the cause rather than dropping it silently.
	async fn pull_media(&mut self, publisher: &mut Publisher) -> anyhow::Result<bool> {
		let mut buffer = [0u8; READ_BUFFER];

		// Drain anything queued alongside the play-accepted event (the server can bundle
		// the first media messages in the same read), then keep reading.
		let queued = std::mem::take(&mut self.work);
		if self.pull_results(queued, publisher).await? {
			return Ok(true);
		}
		loop {
			let n = tokio::select! {
				result = self.stream.read(&mut buffer) => result?,
				_ = moq_clock::sleep_until(publisher.media_deadline) => anyhow::bail!("remote sent no RTMP media before timeout"),
			};
			if n == 0 {
				return Ok(false);
			}
			let results = self
				.session
				.handle_input(&buffer[..n])
				.map_err(|e| anyhow::anyhow!("rtmp handle_input: {e:?}"))?;
			if self.pull_results(results, publisher).await? {
				return Ok(true);
			}
		}
	}

	/// Process one batch of session results on the pull path: write outbound packets,
	/// republish received media, and ignore the rest. Returns whether the remote
	/// completed the play, after which nothing else in the batch is media.
	async fn pull_results(
		&mut self,
		results: impl IntoIterator<Item = ClientSessionResult>,
		publisher: &mut Publisher,
	) -> Result<bool> {
		for result in results {
			match result {
				ClientSessionResult::OutboundResponse(packet) => {
					moq_clock::timeout_at(publisher.media_deadline, self.stream.write_all(&packet.bytes))
						.await
						.map_err(|_| {
							anyhow::anyhow!("remote blocked RTMP control output before the media deadline")
						})??;
				}
				ClientSessionResult::RaisedEvent(event) => match event {
					// A timestamp restart ends this publisher; isolated malformed tags are skipped.
					ClientSessionEvent::VideoDataReceived { data, timestamp } => {
						publisher.push(flv::TAG_VIDEO, timestamp.value, &data)?;
					}
					ClientSessionEvent::AudioDataReceived { data, timestamp } => {
						publisher.push(flv::TAG_AUDIO, timestamp.value, &data)?;
					}
					// Codec config arrives in the sequence-header tags, so script
					// data isn't forwarded.
					ClientSessionEvent::StreamMetadataReceived { .. }
					| ClientSessionEvent::UnhandleableAmf0Data { .. } => self.metadata.record(),
					// The remote's clean end of the stream it plays out, after its final media.
					ClientSessionEvent::UnhandleableOnStatusCode { code }
						if code == "NetStream.Play.Complete" || code == "NetStream.Play.Stop" =>
					{
						tracing::debug!(%code, "rtmp play completed by remote");
						return Ok(true);
					}
					other => tracing::trace!(?other, "ignoring RTMP event during pull"),
				},
				ClientSessionResult::UnhandleableMessageReceived(_) => {}
			}
		}
		Ok(false)
	}

	/// Pump the session until the publish/play request is accepted, writing any
	/// outbound packets and failing on an explicit rejection.
	async fn await_event(&mut self, direction: Direction) -> Result<()> {
		moq_clock::timeout(crate::server::REQUEST_TIMEOUT, async {
			let mut buffer = [0u8; READ_BUFFER];
			loop {
				while let Some(result) = self.work.pop_front() {
					match result {
						ClientSessionResult::OutboundResponse(packet) => self.stream.write_all(&packet.bytes).await?,
						ClientSessionResult::RaisedEvent(event) => match (direction, event) {
							(Direction::Publish, ClientSessionEvent::PublishRequestAccepted)
							| (Direction::Play, ClientSessionEvent::PlaybackRequestAccepted) => return Ok(()),
							// Script data can precede Play.Start; count it without keeping the payload.
							(
								Direction::Play,
								ClientSessionEvent::StreamMetadataReceived { .. }
								| ClientSessionEvent::UnhandleableAmf0Data { .. },
							) => self.metadata.record(),
							// A refused publish/play arrives as an onStatus *failure* code, not a
							// Rejected event; surface it instead of hanging until the peer closes.
							// Benign progress codes (e.g. NetStream.Play.Reset, which precedes
							// .Start) come through here too, so only fail on error codes.
							(_, ClientSessionEvent::UnhandleableOnStatusCode { code }) if is_status_failure(&code) => {
								return Err(anyhow::anyhow!("rtmp {direction} rejected by remote: {code}").into());
							}
							_ => {}
						},
						ClientSessionResult::UnhandleableMessageReceived(_) => {}
					}
				}
				let n = self.stream.read(&mut buffer).await?;
				if n == 0 {
					return Err(anyhow::anyhow!("rtmp server closed before {direction} was accepted").into());
				}
				let results = self
					.session
					.handle_input(&buffer[..n])
					.map_err(|e| anyhow::anyhow!("rtmp handle_input: {e:?}"))?;
				self.work.extend(results);
			}
		})
		.await
		.map_err(|_| anyhow::anyhow!("RTMP {direction} request timed out"))?
	}

	/// Write any outbound packets in `results` and discard the rest.
	async fn drain(&mut self, results: impl IntoIterator<Item = ClientSessionResult>) -> Result<()> {
		for result in results {
			if let ClientSessionResult::OutboundResponse(packet) = result {
				self.stream.write_all(&packet.bytes).await?;
			}
		}
		Ok(())
	}
}

/// Which command we're waiting on the remote to accept.
#[derive(Clone, Copy)]
enum Direction {
	Publish,
	Play,
}

impl std::fmt::Display for Direction {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(match self {
			Direction::Publish => "publish",
			Direction::Play => "play",
		})
	}
}

/// Perform the RTMP client handshake, returning once it completes. Any bytes that
/// trail the final handshake packet are fed back into the session by the caller.
/// Whether an RTMP `onStatus` code denotes a failure (a refused publish/play),
/// as opposed to a benign progress status like `NetStream.Play.Reset`. The RTMP
/// error codes reliably carry one of these words; the info codes (`.Start`,
/// `.Reset`, `.Notify`, `.Stop`, ...) do not.
fn is_status_failure(code: &str) -> bool {
	["Failed", "NotFound", "BadName", "Denied", "Rejected", "Unauthorized"]
		.iter()
		.any(|needle| code.contains(needle))
}

/// Run the client handshake and return any bytes read past its end. The final
/// handshake read can also carry the first RTMP chunks, so those `remaining_bytes`
/// must be fed to the session or chunk parsing desyncs.
async fn client_handshake<S: Stream>(stream: &mut S) -> anyhow::Result<Vec<u8>> {
	let mut handshake = Handshake::new(PeerType::Client);
	let p0_p1 = handshake
		.generate_outbound_p0_and_p1()
		.map_err(|e| anyhow::anyhow!("rtmp handshake p0/p1: {e:?}"))?;
	stream.write_all(&p0_p1).await?;

	let mut buffer = [0u8; 4096];
	loop {
		let n = stream.read(&mut buffer).await?;
		anyhow::ensure!(n != 0, "rtmp server closed during handshake");
		match handshake
			.process_bytes(&buffer[..n])
			.map_err(|e| anyhow::anyhow!("rtmp handshake: {e:?}"))?
		{
			HandshakeProcessResult::InProgress { response_bytes } => {
				if !response_bytes.is_empty() {
					stream.write_all(&response_bytes).await?;
				}
			}
			HandshakeProcessResult::Completed {
				response_bytes,
				remaining_bytes,
			} => {
				if !response_bytes.is_empty() {
					stream.write_all(&response_bytes).await?;
				}
				return Ok(remaining_bytes);
			}
		}
	}
}

/// An active pull: the moq-mux FLV importer publishing into the origin. Mirrors the
/// server's publisher; [`Self::close`] or dropping it unannounces the path, and
/// [`Self::finish`] hands back the finished broadcast to linger on.
struct Publisher {
	importer: FlvImport,
	// Finished at a clean end, so the finished broadcast keeps serving the catalog.
	catalog: moq_mux::catalog::Producer,
	// Controls publication so an end can retract before the importer's fill clone drops.
	broadcast: moq_net::origin::Publication,
	// This pull's publisher instance, so pulling again replaces it rather than resuming into it.
	epoch: moq_net::Epoch,
	media_deadline: moq_clock::Instant,
}

impl Publisher {
	fn new(origin: &origin::Producer, path: &str, config: moq_mux::catalog::Config) -> anyhow::Result<Self> {
		let epoch = moq_net::Epoch::mint();
		let broadcast = origin
			.publish(path, moq_net::origin::Route::default().with_epoch(epoch.clone()))
			.map_err(|err| anyhow::anyhow!("broadcast '{path}' could not be published: {err}"))?;
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), config)?;
		let handle = broadcast.clone();
		let mut importer = FlvImport::new(broadcast.broadcast().clone(), catalog.reserve());

		// Feed the FLV file header once up front; media tags follow per message.
		importer.decode(&flv::file_header())?;
		Ok(Self {
			importer,
			catalog,
			epoch,
			media_deadline: moq_clock::Instant::now() + crate::server::PUBLISH_IDLE_TIMEOUT,
			broadcast: handle,
		})
	}

	fn push(&mut self, tag_type: u8, timestamp: u32, body: &Bytes) -> anyhow::Result<()> {
		self.media_deadline = moq_clock::Instant::now() + crate::server::PUBLISH_IDLE_TIMEOUT;
		flv::import(&mut self.importer, tag_type, timestamp, body)
	}

	/// Flush any buffered media and finish every track, the catalog included, keeping the
	/// finished broadcast up for `linger`.
	fn finish(mut self, linger: Duration) -> anyhow::Result<moq_mux::Linger<Self>> {
		self.importer.finish()?;
		self.catalog.finish()?;
		Ok(moq_mux::Linger::new([self.broadcast.broadcast().clone()], self, linger))
	}

	/// Flush any buffered media and finish the catalog, then end the broadcast so the origin unannounces it at once.
	fn close(mut self) -> anyhow::Result<()> {
		self.importer.finish()?;
		self.catalog.finish()?;
		self.broadcast.close();
		Ok(())
	}

	/// Abort the published tracks with `err` so subscribers see the real cause
	/// (the remote dropped, a protocol error) rather than a generic `Error::Dropped`.
	///
	/// Consumes the publisher and closes the broadcast.
	fn abort(self, err: moq_net::Error) {
		self.catalog.abort(err.clone());
		self.importer.abort(err);
		self.broadcast.close();
	}
}

#[cfg(test)]
#[expect(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "tests read the host clock directly"
)]
mod tests {
	use std::time::Duration;

	use super::*;

	#[tokio::test(start_paused = true)]
	async fn client_handshakes_and_requests_have_finite_budgets() {
		let (stream, _peer) = tokio::io::duplex(4096);
		let start = moq_clock::Instant::now();
		assert!(Client::with_stream(stream, "live").await.is_err());
		assert_eq!(start.elapsed(), crate::server::HANDSHAKE_TIMEOUT);

		for direction in [Direction::Publish, Direction::Play] {
			let (stream, _peer) = tokio::io::duplex(1);
			let (session, _) = ClientSession::new(ClientSessionConfig::new()).unwrap();
			let mut client = Client {
				stream,
				session,
				work: VecDeque::new(),
				app: "live".into(),
				max_delay: crate::DEFAULT_MAX_DELAY,
				max_age: None,
				bandwidth: Default::default(),
				linger: Duration::ZERO,
				metadata: Default::default(),
			};
			let start = moq_clock::Instant::now();
			assert!(client.await_event(direction).await.is_err());
			assert_eq!(start.elapsed(), crate::server::REQUEST_TIMEOUT);
			let origin = moq_tokio::origin::spawn();
			let mut publisher = Publisher::new(&origin, "idle", Default::default()).unwrap();
			if matches!(direction, Direction::Publish) {
				let (packet, _) = client.session.send_ping_request().unwrap();
				client.work.push_back(ClientSessionResult::OutboundResponse(packet));
			}
			let start = moq_clock::Instant::now();
			let mut pull = Box::pin(client.pull_media(&mut publisher));
			assert!(futures::poll!(pull.as_mut()).is_pending());
			tokio::time::advance(crate::server::PUBLISH_IDLE_TIMEOUT).await;
			assert!(matches!(futures::poll!(pull.as_mut()), std::task::Poll::Ready(Err(_))));
			drop(pull);
			assert_eq!(start.elapsed(), crate::server::PUBLISH_IDLE_TIMEOUT);
			publisher.abort(moq_net::Error::Cancel);
		}
	}
	#[tokio::test(start_paused = true)]
	#[tracing_test::traced_test]
	async fn pull_metadata_drops_warn_once() {
		let (stream, _peer) = tokio::io::duplex(1024);
		let (mut session, _) = ClientSession::new(crate::rml::sessions::ClientSessionConfig::new()).unwrap();
		// A custom script message goes through the session's own parser, ahead of
		// Play.Start, then onMetaData follows the acceptance.
		let mut work = VecDeque::from(session.handle_input(&crate::metadata::cue_point_packet()).unwrap());
		work.push_back(ClientSessionResult::RaisedEvent(
			ClientSessionEvent::PlaybackRequestAccepted,
		));
		work.push_back(ClientSessionResult::RaisedEvent(
			ClientSessionEvent::StreamMetadataReceived {
				metadata: crate::rml::sessions::StreamMetadata::new(),
			},
		));
		let mut client = Client {
			stream,
			session,
			work,
			app: "live".into(),
			max_delay: crate::DEFAULT_MAX_DELAY,
			max_age: None,
			bandwidth: Default::default(),
			linger: Duration::ZERO,
			metadata: crate::metadata::Dropped::new("metadata"),
		};
		let origin = moq_tokio::origin::spawn();
		let mut publisher = Publisher::new(&origin, "metadata", Default::default()).unwrap();
		client.await_event(Direction::Play).await.unwrap();
		let queued = std::mem::take(&mut client.work);
		client.pull_results(queued, &mut publisher).await.unwrap();
		assert_eq!(client.metadata.count, 2);
		drop(publisher.finish(Duration::ZERO).unwrap());
		drop(client);
		logs_assert(|lines| {
			let warnings = lines
				.iter()
				.filter(|line| line.contains("discarding RTMP stream metadata"))
				.count();
			let summary = lines.iter().any(|line| {
				line.contains("RTMP metadata discard summary")
					&& line.contains("path=metadata")
					&& line.contains("count=2")
			});
			if warnings == 1 && summary {
				Ok(())
			} else {
				Err(format!("warnings={warnings}, summary={summary}"))
			}
		});
	}

	use crate::server::{Request, Server, accept_stream};

	/// A broadcast with one AVC keyframe at `live/cam0` and its tracks finished, with the
	/// importer that keeps them listed and the catalog to finish it with.
	fn source(origin: &origin::Producer) -> (moq_net::origin::Publication, moq_mux::catalog::Producer, FlvImport) {
		let mut vseq = vec![0x17, 0x00, 0x00, 0x00, 0x00];
		vseq.extend_from_slice(&[0x01, 0x42, 0xc0, 0x1f, 0xff, 0xe1, 0x00, 0x04, 0x67, 0x42, 0xc0, 0x1f]);
		vseq.extend_from_slice(&[0x01, 0x00, 0x04, 0x68, 0xce, 0x3c, 0x80]);
		let mut vframe = vec![0x17, 0x01, 0x00, 0x00, 0x00];
		vframe.extend_from_slice(&[0, 0, 0, 5, 0x65, 0x88, 0x84, 0x21, 0x00]);

		let broadcast = origin.create_broadcast("live/cam0").unwrap();
		broadcast.announce(Default::default()).unwrap();
		let catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();
		let mut importer = FlvImport::new(broadcast.broadcast().clone(), catalog.reserve());
		importer.decode(&flv::file_header()).unwrap();
		importer.decode(&flv::tag(flv::TAG_VIDEO, 0, &vseq)).unwrap();
		importer.decode(&flv::tag(flv::TAG_VIDEO, 0, &vframe)).unwrap();
		importer.finish().unwrap();
		(broadcast, catalog, importer)
	}

	/// A clean end crosses an RTMP hop in both directions: a finished export unpublishes,
	/// so the server it pushes to lingers, and a server whose play completes lets a pull
	/// linger. Each lingering side serves its finished copy for its linger.
	#[tokio::test(start_paused = true)]
	async fn a_finished_broadcast_lingers_across_rtmp() {
		let peer = "127.0.0.1:1935".parse().unwrap();
		let linger = Duration::from_secs(10);
		for push in [true, false] {
			let upstream = moq_tokio::origin::spawn();
			let downstream = moq_tokio::origin::spawn();
			let (broadcast, mut catalog, importer) = source(&upstream);
			let (client_io, server_io) = tokio::io::duplex(64 * 1024);

			// The server ingests a push into `downstream`, or plays `upstream` out to a pull.
			let (served, pulled) = (upstream.consume(), downstream.clone());
			let ingest = downstream.clone();
			let server = tokio::spawn(async move {
				match accept_stream(server_io, peer).await.unwrap().unwrap() {
					Request::Publish(publish) => publish
						.with_linger(linger)
						.accept(&ingest, moq_net::Path::from_static("live/cam0"))
						.await
						.unwrap(),
					Request::Play(play) => play
						.accept(&served, moq_net::Path::from_static("live/cam0"))
						.await
						.unwrap(),
				}
			});
			let exported = upstream.consume();
			let client = tokio::spawn(async move {
				let client = Client::with_stream(client_io, "live").await.unwrap();
				match push {
					true => client
						.publish("cam0", exported, moq_net::Path::from_static("live/cam0"))
						.await
						.unwrap(),
					false => client
						.with_linger(linger)
						.pull("cam0", &pulled, moq_net::Path::from_static("live/cam0"))
						.await
						.unwrap(),
				}
			});

			let consumer = downstream.consume();
			let copy = consumer.follow("live/cam0", None).unwrap().available().await.unwrap();
			// The catalog goes out once the keyframe landed, so the copy holds media to linger with.
			let mut listed = copy.track("catalog.json").unwrap().subscribe(None).await.unwrap();
			assert!(listed.recv_group().await.unwrap().is_some());

			let start = tokio::time::Instant::now();
			catalog.finish().unwrap();
			broadcast.close();
			drop(importer);
			let (lingering, done) = match push {
				true => (server, client),
				false => (client, server),
			};
			done.await.unwrap();
			assert!(!copy.is_closed(), "push={push}: the copy lingers");
			lingering.await.unwrap();
			assert_eq!(start.elapsed(), linger, "push={push}");
			assert!(copy.is_closed());
		}
	}

	/// Contains `needle` somewhere in `haystack`.
	fn contains(haystack: &[u8], needle: &[u8]) -> bool {
		haystack.windows(needle.len()).any(|window| window == needle)
	}

	/// A push stays on the instance it resolved: a publisher taking over the path never reaches
	/// the remote, so the push never splices in another timeline or codec state, and it fails
	/// once its instance's route dies.
	#[tokio::test(start_paused = true)]
	async fn a_push_stays_on_its_instance_until_the_route_dies() {
		const OLD_IDR: &[u8] = &[0x65, 0x88, 0x84, 0x21, 0x00];
		const OLD_DELTA: &[u8] = &[0x41, 0x9a, 0x02, 0x03];
		const NEW_IDR: &[u8] = &[0x65, 0x99, 0x99, 0x99, 0x99];
		let publish = |origin: &origin::Producer, epoch: &moq_net::Epoch, idr: &[u8]| {
			let mut vseq = vec![0x17, 0x00, 0x00, 0x00, 0x00];
			vseq.extend_from_slice(&[0x01, 0x42, 0xc0, 0x1f, 0xff, 0xe1, 0x00, 0x04, 0x67, 0x42, 0xc0, 0x1f]);
			vseq.extend_from_slice(&[0x01, 0x00, 0x04, 0x68, 0xce, 0x3c, 0x80]);
			let mut vframe = vec![0x17, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, idr.len() as u8];
			vframe.extend_from_slice(idr);
			let broadcast = origin.create_broadcast("live/cam0").unwrap();
			broadcast
				.announce(origin::Route::default().with_epoch(epoch.clone()))
				.unwrap();
			let catalog = moq_mux::catalog::Producer::new(
				&mut broadcast.broadcast().clone(),
				moq_mux::catalog::Config::default(),
			)
			.unwrap();
			let mut importer = FlvImport::new(broadcast.broadcast().clone(), catalog.reserve());
			importer.decode(&flv::file_header()).unwrap();
			importer.decode(&flv::tag(flv::TAG_VIDEO, 0, &vseq)).unwrap();
			importer.decode(&flv::tag(flv::TAG_VIDEO, 0, &vframe)).unwrap();
			(importer, catalog)
		};

		let upstream = moq_tokio::origin::spawn();
		let downstream = moq_tokio::origin::spawn();
		let (mut old, old_catalog) = publish(&upstream, &moq_net::Epoch::mint(), OLD_IDR);
		let (client_io, server_io) = tokio::io::duplex(64 * 1024);
		let ingest = downstream.clone();
		let server = tokio::spawn(async move {
			let Some(Request::Publish(publish)) = accept_stream(server_io, "127.0.0.1:1935".parse().unwrap())
				.await
				.unwrap()
			else {
				panic!("expected a publish request");
			};
			let _ = publish.accept(&ingest, moq_net::Path::from_static("live/cam0")).await;
		});
		let exported = upstream.consume();
		let pushed = tokio::spawn(async move {
			let client = Client::with_stream(client_io, "live").await.unwrap();
			client
				.publish("cam0", exported, moq_net::Path::from_static("live/cam0"))
				.await
		});

		// Read what reaches the remote back out as FLV.
		let consumer = downstream.consume();
		let copy = consumer.follow("live/cam0", None).unwrap().available().await.unwrap();
		let source = moq_mux::Source::new(consumer, copy);
		let catalog = source
			.catalog::<()>(moq_mux::catalog::CatalogFormat::default())
			.await
			.unwrap();
		let mut remote = FlvExport::new(source, catalog);
		let mut received = Vec::new();
		let mut until = async |needle: &[u8]| {
			while !contains(&received, needle) {
				let chunk = remote.next().await.unwrap().expect("the remote copy ended");
				received.extend_from_slice(&chunk);
			}
		};
		until(OLD_IDR).await;

		let epoch = moq_net::Epoch::mint();
		let mut instances = moq_mux::follow::Instances::new(&upstream.consume(), "live/cam0", Some(&epoch)).unwrap();
		let (_new, _new_catalog) = publish(&upstream, &epoch, NEW_IDR);
		tokio::time::timeout(Duration::from_secs(5), instances.next())
			.await
			.expect("the new publisher takes over")
			.unwrap();

		let mut delta = vec![0x27, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, OLD_DELTA.len() as u8];
		delta.extend_from_slice(OLD_DELTA);
		old.decode(&flv::tag(flv::TAG_VIDEO, 40, &delta)).unwrap();
		until(OLD_DELTA).await;
		assert!(!contains(&received, NEW_IDR), "the push left its instance");

		old.abort(moq_net::Error::Transport("connection lost".into()));
		drop(old_catalog);
		assert!(pushed.await.unwrap().is_err(), "the push ended cleanly");
		server.abort();
	}

	/// Loopback: dial the crate's own server and pull a broadcast the server plays
	/// out of a populated origin. Exercises the client handshake, connect, play
	/// request, and the receive -> FLV-demux -> republish path end to end.
	#[tokio::test]
	async fn pull_round_trips_a_broadcast() {
		// A minimal AVC sequence header + keyframe, published into the server's origin
		// via the FLV importer so the played-back stream carries a catalog + frames.
		let avcc = {
			let sps = [0x67u8, 0x42, 0xc0, 0x1f];
			let mut out = vec![0x01, 0x42, 0xc0, 0x1f, 0xff, 0xe1, 0x00, sps.len() as u8];
			out.extend_from_slice(&sps);
			out.extend_from_slice(&[0x01, 0x00, 0x04, 0x68, 0xce, 0x3c, 0x80]);
			out
		};
		let mut vseq = vec![0x17, 0x00, 0x00, 0x00, 0x00];
		vseq.extend_from_slice(&avcc);
		let mut vframe = vec![0x17, 0x01, 0x00, 0x00, 0x00];
		vframe.extend_from_slice(&[0, 0, 0, 5, 0x65, 0x88, 0x84, 0x21, 0x00]);

		let server_origin = moq_tokio::origin::spawn();
		let broadcast = server_origin.create_broadcast("live/cam0").unwrap();
		broadcast.announce(Default::default()).unwrap();
		let catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();
		let mut importer = FlvImport::new(broadcast.broadcast().clone(), catalog.reserve());
		importer.decode(&flv::file_header()).unwrap();
		importer.decode(&flv::tag(flv::TAG_VIDEO, 0, &vseq)).unwrap();
		importer.decode(&flv::tag(flv::TAG_VIDEO, 0, &vframe)).unwrap();
		importer.finish().unwrap();

		// Run the crate's own server, serving the one play request from that origin.
		let mut server = Server::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
		let addr = server.local_addr().unwrap();
		let consumer = server_origin.consume();
		let server_task = tokio::spawn(async move {
			let request = server.accept().await.expect("a request");
			let Request::Play(play) = request else {
				panic!("expected a play request");
			};
			play.accept(&consumer, moq_net::Path::from_static("live/cam0"))
				.await
				.unwrap();
		});

		// Client: dial, connect(`live`), play(`cam0`), republish into our own origin.
		let client_origin = moq_tokio::origin::spawn();
		let announced = client_origin.consume();
		let pull_origin = client_origin.clone();
		let pull = tokio::spawn(async move {
			let client = Client::connect(addr, "live").await.unwrap();
			client
				.pull("cam0", &pull_origin, moq_net::Path::from_static("pulled/cam0"))
				.await
				.unwrap();
		});

		// The republished broadcast should show up in the client's origin.
		let broadcast = announced
			.follow("pulled/cam0", None)
			.unwrap()
			.available()
			.await
			.unwrap();

		// It should carry a hang catalog track (proof the FLV demux produced real
		// media on the far side): subscribe to it and read one catalog frame.
		let mut catalog_track = broadcast
			.track("catalog.json")
			.expect("catalog track")
			.subscribe(None)
			.await
			.expect("subscribe catalog")
			.ordered();
		let mut group = tokio::time::timeout(Duration::from_secs(5), catalog_track.next_group())
			.await
			.expect("catalog read timed out")
			.expect("catalog read")
			.expect("a catalog group");
		let frame = group
			.read_frame()
			.await
			.expect("catalog read")
			.expect("a catalog frame");
		assert!(!frame.payload.is_empty(), "pulled broadcast should carry a catalog");

		pull.abort();
		server_task.abort();
	}
}
