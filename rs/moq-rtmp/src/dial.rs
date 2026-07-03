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

use bytes::Bytes;
use moq_mux::container::flv::{Export as FlvExport, Import as FlvImport};
use moq_net::{BroadcastConsumer, BroadcastInfo, OriginProducer, OriginPublish};
use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
use rml_rtmp::sessions::{
	ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult, PublishRequestType,
};
use rml_rtmp::time::RtmpTimestamp;
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
	/// How long [`publish`](Self::publish)'s FLV muxer waits for a stalled group
	/// before skipping. Zero (the default) drops stale groups aggressively.
	latency: Duration,
}

impl Client<TcpStream> {
	/// Dial `addr` over plaintext TCP and complete the RTMP handshake + `connect`
	/// for `app` (the `<app>` in `rtmp://host/<app>/<key>`).
	///
	/// For `rtmps://` or any other transport, establish the stream yourself and use
	/// [`with_stream`](Self::with_stream) instead.
	pub async fn connect(addr: SocketAddr, app: &str) -> Result<Self> {
		let stream = TcpStream::connect(addr).await?;
		stream.set_nodelay(true).ok();
		// Advertise a tcUrl derived from the dial target: several ingest servers
		// (YouTube, Twitch, some nginx-rtmp configs) reject a connect without one.
		Self::with_stream_config(stream, app, Some(format!("rtmp://{addr}/{app}"))).await
	}
}

impl<S: Stream> Client<S> {
	/// Complete the RTMP handshake and `connect` for `app` over an
	/// already-established byte stream.
	///
	/// The bring-your-own-transport entry point: establish the connection (and, for
	/// `rtmps://`, the TLS handshake) yourself, then hand the stream here.
	pub async fn with_stream(stream: S, app: &str) -> Result<Self> {
		Self::with_stream_config(stream, app, None).await
	}

	async fn with_stream_config(mut stream: S, app: &str, tc_url: Option<String>) -> Result<Self> {
		let remaining = client_handshake(&mut stream).await?;

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
					ClientSessionResult::RaisedEvent(ClientSessionEvent::ConnectionRequestAccepted) => break 'connect,
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
			latency: Duration::ZERO,
		})
	}

	/// Set how long [`publish`](Self::publish)'s FLV muxer waits for a stalled group
	/// before skipping to a newer one (the moq-level frame-drop latency). Defaults
	/// to zero, which drops stale groups aggressively.
	pub fn with_latency(mut self, latency: Duration) -> Self {
		self.latency = latency;
		self
	}

	/// Push a MoQ broadcast out to the remote: request `publish` on `stream_key`,
	/// mux the broadcast to FLV, and send each tag as an RTMP audio/video message
	/// until the broadcast ends or the connection drops.
	///
	/// `broadcast` is the read side of whatever you want restreamed (e.g. from
	/// `origin.consume().announced_broadcast(path)`). This future resolves when the
	/// broadcast ends, so callers usually run it on its own task.
	pub async fn publish(mut self, stream_key: &str, broadcast: BroadcastConsumer) -> Result<()> {
		let request = self
			.session
			.request_publishing(stream_key.to_string(), PublishRequestType::Live)
			.map_err(|e| anyhow::anyhow!("rtmp request publish: {e:?}"))?;
		self.work.push_back(request);
		self.await_event(Direction::Publish).await?;

		tracing::info!(%stream_key, "rtmp publish accepted by remote");

		// Flush anything queued alongside the publish-accepted event before streaming.
		let queued = std::mem::take(&mut self.work);
		self.drain(queued).await?;

		let mut export = FlvExport::new(broadcast)
			.await
			.map_err(|e| anyhow::anyhow!("init FLV export: {e}"))?
			.with_latency(self.latency);
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
					// Broadcast ended.
					None => break,
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
	pub async fn pull(mut self, stream_key: &str, origin: &OriginProducer, path: &str) -> Result<()> {
		let request = self
			.session
			.request_playback(stream_key.to_string())
			.map_err(|e| anyhow::anyhow!("rtmp request play: {e:?}"))?;
		self.work.push_back(request);
		self.await_event(Direction::Play).await?;

		tracing::info!(%stream_key, %path, "rtmp play accepted by remote");

		let mut publisher = Publisher::new(origin, path)?;
		let mut buffer = [0u8; READ_BUFFER];

		// Drain anything queued alongside the play-accepted event (the server can bundle
		// the first media messages in the same read), then keep reading.
		let queued = std::mem::take(&mut self.work);
		self.pull_results(queued, &mut publisher).await?;
		loop {
			let n = self.stream.read(&mut buffer).await?;
			if n == 0 {
				break;
			}
			let results = self
				.session
				.handle_input(&buffer[..n])
				.map_err(|e| anyhow::anyhow!("rtmp handle_input: {e:?}"))?;
			self.pull_results(results, &mut publisher).await?;
		}

		if let Err(err) = publisher.finish() {
			tracing::debug!(%err, "error finishing RTMP pull");
		}
		Ok(())
	}

	/// Process one batch of session results on the pull path: write outbound packets,
	/// republish received media, and ignore the rest.
	async fn pull_results(
		&mut self,
		results: impl IntoIterator<Item = ClientSessionResult>,
		publisher: &mut Publisher,
	) -> Result<()> {
		for result in results {
			match result {
				ClientSessionResult::OutboundResponse(packet) => self.stream.write_all(&packet.bytes).await?,
				ClientSessionResult::RaisedEvent(event) => match event {
					// A frame that fails to demux is dropped, not fatal: the importer
					// consumes whole tags atomically, so one bad frame doesn't desync.
					ClientSessionEvent::VideoDataReceived { data, timestamp } => {
						if let Err(err) = publisher.push(flv::TAG_VIDEO, timestamp.value, &data) {
							tracing::warn!(%err, "dropping RTMP video frame that failed to demux");
						}
					}
					ClientSessionEvent::AudioDataReceived { data, timestamp } => {
						if let Err(err) = publisher.push(flv::TAG_AUDIO, timestamp.value, &data) {
							tracing::warn!(%err, "dropping RTMP audio frame that failed to demux");
						}
					}
					// Codec config arrives in the sequence-header tags, so metadata
					// isn't forwarded.
					ClientSessionEvent::StreamMetadataReceived { .. } => {}
					other => tracing::trace!(?other, "ignoring RTMP event during pull"),
				},
				ClientSessionResult::UnhandleableMessageReceived(_) => {}
			}
		}
		Ok(())
	}

	/// Pump the session until the publish/play request is accepted, writing any
	/// outbound packets and failing on an explicit rejection.
	async fn await_event(&mut self, direction: Direction) -> Result<()> {
		let mut buffer = [0u8; READ_BUFFER];
		loop {
			while let Some(result) = self.work.pop_front() {
				match result {
					ClientSessionResult::OutboundResponse(packet) => self.stream.write_all(&packet.bytes).await?,
					ClientSessionResult::RaisedEvent(event) => match (direction, event) {
						(Direction::Publish, ClientSessionEvent::PublishRequestAccepted)
						| (Direction::Play, ClientSessionEvent::PlaybackRequestAccepted) => return Ok(()),
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
/// server's publisher; dropping it unannounces the broadcast.
struct Publisher {
	/// Held to keep the broadcast announced for the publisher's lifetime.
	_publish: OriginPublish,
	importer: FlvImport,
}

impl Publisher {
	fn new(origin: &OriginProducer, path: &str) -> anyhow::Result<Self> {
		let mut broadcast = BroadcastInfo::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast)?;
		let mut importer = FlvImport::new(broadcast.clone(), catalog);

		let publish = origin
			.publish_broadcast(path, broadcast.consume())
			.map_err(|err| anyhow::anyhow!("broadcast '{path}' could not be published: {err}"))?;

		// Feed the FLV file header once up front; media tags follow per message.
		importer.decode(&flv::file_header())?;
		Ok(Self {
			_publish: publish,
			importer,
		})
	}

	fn push(&mut self, tag_type: u8, timestamp: u32, body: &Bytes) -> anyhow::Result<()> {
		// FLV's tag DataSize is 24-bit; a larger body would desync the demuxer.
		anyhow::ensure!(
			body.len() <= 0xFF_FFFF,
			"RTMP message body {} exceeds FLV's 24-bit tag size limit",
			body.len()
		);
		self.importer.decode(&flv::tag(tag_type, timestamp, body))
	}

	fn finish(&mut self) -> anyhow::Result<()> {
		self.importer.finish()
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::*;
	use crate::server::{Request, Server};

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

		let server_origin = moq_net::Origin::random().produce();
		let mut broadcast = BroadcastInfo::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();
		let mut importer = FlvImport::new(broadcast.clone(), catalog);
		let _publish = server_origin
			.publish_broadcast("live/cam0", broadcast.consume())
			.unwrap();
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
			play.accept(&consumer, "live/cam0").await.unwrap();
		});

		// Client: dial, connect(`live`), play(`cam0`), republish into our own origin.
		let client_origin = moq_net::Origin::random().produce();
		let announced = client_origin.consume();
		let pull_origin = client_origin.clone();
		let pull = tokio::spawn(async move {
			let client = Client::connect(addr, "live").await.unwrap();
			client.pull("cam0", &pull_origin, "pulled/cam0").await.unwrap();
		});

		// The republished broadcast should show up in the client's origin.
		let broadcast = tokio::time::timeout(Duration::from_secs(5), announced.announced_broadcast("pulled/cam0"))
			.await
			.expect("client republish timed out")
			.expect("broadcast announced in client origin");

		// It should carry a hang catalog track (proof the FLV demux produced real
		// media on the far side): subscribe to it and read one catalog frame.
		let mut catalog_track = broadcast
			.track("catalog.json")
			.expect("catalog track")
			.subscribe(None)
			.await
			.expect("subscribe catalog");
		let frame = tokio::time::timeout(Duration::from_secs(5), catalog_track.read_frame())
			.await
			.expect("catalog read timed out")
			.expect("catalog read")
			.expect("a catalog frame");
		assert!(!frame.is_empty(), "pulled broadcast should carry a catalog");

		pull.abort();
		server_task.abort();
	}
}
