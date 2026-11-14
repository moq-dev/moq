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
//! Transport: [`Client::connect`] dials an `rtmp://` or `rtmps://` URL itself.
//! For any other transport, establish the stream yourself and hand it to
//! [`Client::with_stream`] with the URL it reaches; everything here is generic
//! over the [`crate::Stream`] trait.

use std::collections::VecDeque;
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
use crate::server::{Conn, Stream};
use url::Url;

/// Read buffer size for pulling RTMP chunk-stream bytes off the socket.
const READ_BUFFER: usize = 16 * 1024;

/// A connected RTMP client, ready to publish a MoQ broadcast to the remote or pull
/// a remote stream into an origin.
///
/// Build it with [`connect`](Self::connect) (dial `rtmp://` or `rtmps://`) or
/// [`with_stream`](Self::with_stream) (bring your own transport).
/// Both run the RTMP handshake and the `connect` command for the given app, so the
/// returned client is connected and idle. Then pick a direction:
///
/// - [`publish`](Self::publish): MoQ broadcast -> remote (push / restream).
/// - [`pull`](Self::pull): remote stream -> MoQ origin (ingest).
///
/// `S` is the underlying stream: a [`Conn`] from [`connect`](Self::connect), or
/// your own transport via [`with_stream`](Self::with_stream).
pub struct Client<S = Conn> {
	stream: S,
	session: ClientSession,
	/// Session results queued during connect, drained by the first publish/pull.
	work: VecDeque<ClientSessionResult>,
	/// The `<app>` this client connected to, logged in place of the stream key.
	app: String,
	/// How long [`publish`](Self::publish)'s FLV muxer waits for a stalled group
	/// before skipping.
	delay: Duration,
	/// Script data [`pull`](Self::pull) discards, including any sent before Play.Start.
	metadata: crate::metadata::Dropped,
}

/// Where [`Client::connect`] dials, and the TLS it uses for `rtmps://`.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Connect {
	/// `rtmp://host[:1935]/<app>` or `rtmps://host[:443]/<app>`, without the stream key.
	pub url: Url,

	/// The TLS client configuration for `rtmps://`, which requires one.
	#[cfg(feature = "tls")]
	pub tls: Option<std::sync::Arc<rustls::ClientConfig>>,
}

impl Connect {
	/// Dial `url` (`rtmp://host[:port]/<app>` or `rtmps://...`), without TLS until
	/// [`with_tls`](Self::with_tls) supplies it.
	pub fn new(url: Url) -> Self {
		Self {
			url,
			#[cfg(feature = "tls")]
			tls: None,
		}
	}

	/// Use `tls` to dial `rtmps://`; it decides which server certificates to trust.
	#[cfg(feature = "tls")]
	pub fn with_tls(mut self, tls: impl Into<Option<std::sync::Arc<rustls::ClientConfig>>>) -> Self {
		self.tls = tls.into();
		self
	}
}

/// What an RTMP URL names: whether it is RTMPS, the host and port to dial, the app,
/// and the `tcUrl` to advertise.
struct Target {
	secure: bool,
	host: url::Host<String>,
	port: u16,
	app: String,
	tc_url: String,
}

impl Target {
	/// Parse `rtmp[s]://host[:port]/<app>`. The path holds the app alone, so a stream
	/// key passed by mistake is refused instead of leaking into the `tcUrl`.
	fn parse(url: &Url) -> Result<Self> {
		let secure = match url.scheme() {
			"rtmp" => false,
			"rtmps" => true,
			other => return Err(anyhow::anyhow!("rtmp url must use rtmp:// or rtmps://, not {other}://").into()),
		};
		let host = url
			.host()
			.ok_or_else(|| anyhow::anyhow!("rtmp url is missing a host"))?
			.to_owned();
		if !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
			return Err(anyhow::anyhow!(
				"rtmp url must be scheme://host[:port]/<app>, with no credentials, query, or fragment"
			)
			.into());
		}
		let app = url.path().trim_start_matches('/');
		if app.is_empty() || app.contains('/') {
			return Err(anyhow::anyhow!(
				"rtmp url path must be the app alone, without the stream key: scheme://host[:port]/<app>"
			)
			.into());
		}
		let port = url.port().unwrap_or(if secure { 443 } else { 1935 });
		let tc_url = match url.port() {
			Some(port) => format!("{}://{host}:{port}/{app}", url.scheme()),
			None => format!("{}://{host}/{app}", url.scheme()),
		};
		Ok(Self {
			secure,
			host,
			port,
			app: app.to_string(),
			tc_url,
		})
	}
}

impl Client<Conn> {
	/// Dial `connect.url`, completing the TLS handshake against its host for
	/// `rtmps://`, then the RTMP handshake and `connect` for its app, advertising a
	/// `tcUrl` of its scheme, host, port if it names one, and app.
	pub async fn connect(connect: Connect) -> Result<Self> {
		moq_crypto::cpu::check()?;
		let target = Target::parse(&connect.url)?;
		#[cfg(feature = "tls")]
		let tls = match (target.secure, connect.tls) {
			(true, None) => return Err(anyhow::anyhow!("rtmps:// needs a TLS client configuration").into()),
			(true, Some(tls)) => Some(tokio_rustls::TlsConnector::from(tls)),
			(false, _) => None,
		};
		#[cfg(not(feature = "tls"))]
		if target.secure {
			return Err(anyhow::anyhow!("rtmps:// needs moq-rtmp's `tls` feature").into());
		}

		kio::clock::timeout(crate::server::REQUEST_TIMEOUT, async {
			let stream = match &target.host {
				url::Host::Domain(name) => TcpStream::connect((name.as_str(), target.port)).await?,
				url::Host::Ipv4(ip) => TcpStream::connect((*ip, target.port)).await?,
				url::Host::Ipv6(ip) => TcpStream::connect((*ip, target.port)).await?,
			};
			crate::server::configure_socket(&stream, stream.peer_addr()?);
			#[cfg(feature = "tls")]
			let conn = match tls {
				Some(connector) => {
					let name = match &target.host {
						url::Host::Domain(name) => rustls::pki_types::ServerName::try_from(name.clone())
							.map_err(|e| anyhow::anyhow!("rtmps host is not a valid TLS name: {e}"))?,
						url::Host::Ipv4(ip) => rustls::pki_types::ServerName::from(std::net::IpAddr::from(*ip)),
						url::Host::Ipv6(ip) => rustls::pki_types::ServerName::from(std::net::IpAddr::from(*ip)),
					};
					let tls = connector
						.connect(name, stream)
						.await
						.map_err(|e| anyhow::anyhow!("rtmps tls handshake: {e}"))?;
					Conn::Tls(Box::new(tls.into()))
				}
				None => Conn::Plain(stream),
			};
			#[cfg(not(feature = "tls"))]
			let conn = Conn::Plain(stream);
			Self::with_stream_config(conn, &target.app, target.tc_url).await
		})
		.await
		.map_err(|_| anyhow::anyhow!("RTMP connection timed out"))?
	}
}

impl<S: Stream> Client<S> {
	/// Complete the RTMP handshake and `connect` over an already-established byte
	/// stream that reaches `url` (`rtmp[s]://host[:port]/<app>`), which names the app
	/// and the `tcUrl` to advertise.
	///
	/// The bring-your-own-transport entry point: establish the connection (and any
	/// TLS) yourself, then hand the stream here.
	pub async fn with_stream(stream: S, url: &Url) -> Result<Self> {
		moq_crypto::cpu::check()?;
		let target = Target::parse(url)?;
		kio::clock::timeout(
			crate::server::REQUEST_TIMEOUT,
			Self::with_stream_config(stream, &target.app, target.tc_url),
		)
		.await
		.map_err(|_| anyhow::anyhow!("RTMP connect request timed out"))?
	}

	async fn with_stream_config(mut stream: S, app: &str, tc_url: String) -> Result<Self> {
		let remaining = kio::clock::timeout(crate::server::HANDSHAKE_TIMEOUT, client_handshake(&mut stream))
			.await
			.map_err(|_| anyhow::anyhow!("RTMP handshake timed out"))??;

		let mut config = ClientSessionConfig::new();
		config.tc_url = Some(tc_url);
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
					ClientSessionResult::RaisedEvent(ClientSessionEvent::ConnectionRequestRejected {
						code,
						description,
					}) => {
						return Err(anyhow::anyhow!("rtmp connect rejected: {code}: {description}").into());
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
			delay: crate::DEFAULT_DELAY,
			metadata: Default::default(),
		})
	}

	/// Set the FLV release delay and stalled-group budget for [`publish`](Self::publish).
	/// Defaults to 500 ms; zero releases available frames immediately without deadline drops.
	///
	/// The publish (egress) direction only; [`pull`](Self::pull) takes a
	/// [`moq_mux::Ingest`] instead.
	pub fn with_delay(mut self, delay: Duration) -> Self {
		self.delay = delay;
		self
	}

	/// Push a MoQ broadcast out to the remote: request `publish` on `stream_key`,
	/// mux the broadcast to FLV, and send each tag as an RTMP audio/video message
	/// until the broadcast ends or the connection drops.
	///
	/// `source` is the resolved broadcast to restream; the FLV export resolves any sibling
	/// broadcast a rendition's catalog `broadcast` field references through its origin.
	/// This future resolves when the broadcast ends, so callers usually run it on its own
	/// task.
	pub async fn publish(mut self, stream_key: &str, source: moq_mux::Source) -> Result<()> {
		let path = source.broadcast().info().path.clone();
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

		let catalog = source
			.catalog::<()>(moq_mux::catalog::CatalogFormat::default())
			.await
			.map_err(|e| anyhow::anyhow!("init FLV export: {e}"))?;
		let mut export = FlvExport::new(source, catalog).with_delay(
			moq_net::track::Delay::default()
				.with_min(self.delay)
				.with_max(self.delay),
		);
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
	/// its FLV tags, demux them, and publish the result at `path` as `ingest` says until the remote
	/// stops or the connection drops.
	///
	/// This future resolves when the remote stream ends, so callers usually run it
	/// on its own task.
	///
	/// A remote that completes the play (`NetStream.Play.Complete` or `NetStream.Play.Stop`)
	/// ends cleanly: the finished broadcast stays up for the ingest's
	/// [linger](moq_mux::Ingest::linger), and the future resolves after that. Dropping the
	/// future ends the linger early. A connection that drops first closes the broadcast at once.
	pub async fn pull(
		mut self,
		stream_key: &str,
		origin: &origin::Producer,
		path: impl Into<moq_net::Path>,
		ingest: &moq_mux::Ingest,
	) -> Result<()> {
		let path: moq_net::Path = path.into();
		let request = self
			.session
			.request_playback(stream_key.to_string())
			.map_err(|e| anyhow::anyhow!("rtmp request play: {e:?}"))?;
		self.work.push_back(request);
		self.metadata = crate::metadata::Dropped::new(path.as_str());
		self.await_event(Direction::Play).await?;

		let mut publisher = Publisher::new(origin, path.as_str(), ingest.catalog())?;

		// The stream key is the ingest credential (`rtmp://host/<app>/<key>`), so the
		// app and broadcast path stand in for it.
		tracing::info!(app = %self.app, %path, instance = %publisher.route.instance(), "rtmp play accepted by remote");

		let result = self.pull_media(&mut publisher).await;
		match result {
			// A clean end: serve the finished broadcast for the linger, so a subscriber that
			// arrives after the end still gets the tail. The remote is done, so hang up first.
			Ok(true) => {
				let linger = ingest.linger;
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
				_ = kio::clock::sleep_until(publisher.media_deadline) => anyhow::bail!("remote sent no RTMP media before timeout"),
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
					kio::clock::timeout_at(publisher.media_deadline, self.stream.write_all(&packet.bytes))
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
		kio::clock::timeout(crate::server::REQUEST_TIMEOUT, async {
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
							// A refused publish/play arrives as an `_error`, or as an onStatus
							// *failure* code; surface it instead of hanging until the peer closes.
							(Direction::Publish, ClientSessionEvent::PublishRequestRejected { code, description })
							| (Direction::Play, ClientSessionEvent::PlaybackRequestRejected { code, description }) => {
								return Err(anyhow::anyhow!(
									"rtmp {direction} rejected by remote: {code}: {description}"
								)
								.into());
							}
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
	// Announces the path under this pull's fresh instance, so pulling again replaces it rather
	// than resuming into it. Closing its broadcast retracts it before the importer's fill clone drops.
	route: moq_net::origin::Route,
	media_deadline: kio::clock::Instant,
}

impl Publisher {
	fn new(origin: &origin::Producer, path: &str, config: moq_mux::catalog::Config) -> anyhow::Result<Self> {
		let route = origin
			.route(path, moq_net::origin::RouteOptions::default())
			.map_err(|err| anyhow::anyhow!("broadcast '{path}' could not be published: {err}"))?;
		let catalog = moq_mux::catalog::Producer::new(&mut route.broadcast().clone(), config)?;
		let mut importer = FlvImport::new(route.broadcast().clone(), catalog.reserve());

		// Feed the FLV file header once up front; media tags follow per message.
		importer.decode(&flv::file_header())?;
		Ok(Self {
			importer,
			catalog,
			media_deadline: kio::clock::Instant::now().saturating_add(crate::server::PUBLISH_IDLE_TIMEOUT),
			route,
		})
	}

	fn push(&mut self, tag_type: u8, timestamp: u32, body: &Bytes) -> anyhow::Result<()> {
		self.media_deadline = kio::clock::Instant::now().saturating_add(crate::server::PUBLISH_IDLE_TIMEOUT);
		flv::import(&mut self.importer, tag_type, timestamp, body)
	}

	/// Flush any buffered media and finish every track, the catalog included, keeping the
	/// finished broadcast up for `linger`.
	fn finish(self, linger: Duration) -> anyhow::Result<moq_mux::Linger<impl Sized>> {
		let Self {
			importer,
			mut catalog,
			route,
			..
		} = self;
		let importer = importer.finish()?;
		catalog.finish()?;
		let broadcast = route.broadcast().clone();
		Ok(moq_mux::Linger::new([broadcast], (importer, catalog, route), linger))
	}

	/// Flush any buffered media and finish the catalog, then end the broadcast so the origin unannounces it at once.
	fn close(mut self) -> anyhow::Result<()> {
		// Held until the broadcast closes, so the route retracts before the tracks drop.
		let _importer = self.importer.finish()?;
		self.catalog.finish()?;
		self.route.broadcast().close();
		Ok(())
	}

	/// Abort the published tracks with `err` so subscribers see the real cause
	/// (the remote dropped, a protocol error) rather than a generic `Error::Dropped`.
	///
	/// Consumes the publisher and closes the broadcast.
	fn abort(self, err: moq_net::Error) {
		self.catalog.abort(err.clone());
		self.importer.abort(err);
		self.route.broadcast().close();
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::*;

	fn live_url() -> Url {
		"rtmp://127.0.0.1/live".parse().unwrap()
	}

	#[test]
	fn target_derives_tc_url_without_the_key() {
		let parse = |url: &str| Target::parse(&url.parse().unwrap());
		let t = parse("rtmps://ingest.example.com/live2").unwrap();
		assert!(t.secure);
		assert_eq!((t.port, t.app.as_str()), (443, "live2"));
		assert_eq!(t.tc_url, "rtmps://ingest.example.com/live2");
		let t = parse("rtmp://[::1]:1936/live").unwrap();
		assert_eq!(t.port, 1936);
		assert_eq!(t.tc_url, "rtmp://[::1]:1936/live");
		assert_eq!(parse("rtmp://h/live").unwrap().port, 1935);
		for bad in [
			"rtmp://h/live/key",
			"rtmp://h/",
			"http://h/live",
			"rtmp://u:p@h/live",
			"rtmp://h/live?k=v",
		] {
			assert!(parse(bad).is_err(), "{bad}");
		}
	}

	/// Serve RTMPS on `localhost` with a fresh identity, returning the listener and
	/// that identity's fingerprint.
	#[cfg(feature = "tls")]
	async fn rtmps_server() -> (Server, String) {
		let identity = moq_tokio::tls::Identity::generate(["localhost"]).unwrap();
		let fingerprint = identity.fingerprint().to_string();
		let mut listen = moq_tokio::tls::Listen::default();
		listen.identity = Some(identity);
		let config = listen.server_config(vec![]).unwrap();
		let server = Server::bind("127.0.0.1:0".parse().unwrap())
			.await
			.unwrap()
			.with_tls(config);
		(server, fingerprint)
	}

	#[cfg(feature = "tls")]
	fn pinned(fingerprint: String) -> std::sync::Arc<rustls::ClientConfig> {
		let mut tls = moq_tokio::tls::Connect::default();
		tls.fingerprint = vec![fingerprint];
		std::sync::Arc::new(tls.build().unwrap())
	}

	#[cfg(feature = "tls")]
	#[tokio::test]
	async fn connect_dials_rtmps_and_advertises_tc_url() {
		let (mut server, fingerprint) = rtmps_server().await;
		let port = server.local_addr().unwrap().port();
		let url: Url = format!("rtmps://localhost:{port}/live").parse().unwrap();
		// The server yields a request at play, so the dial goes on to pull.
		let dial = tokio::spawn(async move {
			let client = Client::connect(Connect::new(url).with_tls(pinned(fingerprint))).await?;
			let origin = moq_tokio::origin::spawn();
			client
				.pull("cam0", &origin, moq_net::Path::from_static("cam0"), &Default::default())
				.await
		});

		let request = tokio::select! {
			request = server.accept() => request.unwrap(),
			result = dial => panic!("dial ended first: {:?}", result.unwrap()),
		};
		assert_eq!(
			request.tc_url(),
			Some(format!("rtmps://localhost:{port}/live").as_str())
		);
		drop(request);
	}

	#[cfg(feature = "tls")]
	#[tokio::test]
	async fn connect_refuses_an_untrusted_certificate() {
		let (mut server, _) = rtmps_server().await;
		let port = server.local_addr().unwrap().port();
		let url: Url = format!("rtmps://localhost:{port}/live").parse().unwrap();
		let other = moq_tokio::tls::Identity::generate(["localhost"]).unwrap();
		let dial = Client::connect(Connect::new(url.clone()).with_tls(pinned(other.fingerprint().to_string())));
		let accept = async { kio::clock::timeout(Duration::from_secs(5), server.accept()).await };
		tokio::select! {
			result = dial => assert!(result.is_err()),
			_ = accept => panic!("an untrusted dial reached the RTMP connect"),
		}
		assert!(
			Client::connect(Connect::new(url)).await.is_err(),
			"rtmps:// without TLS config"
		);
	}

	#[tokio::test(start_paused = true)]
	async fn client_handshakes_and_requests_have_finite_budgets() {
		let (stream, _peer) = tokio::io::duplex(4096);
		let start = kio::clock::Instant::now();
		assert!(Client::with_stream(stream, &live_url()).await.is_err());
		assert_eq!(start.elapsed(), crate::server::HANDSHAKE_TIMEOUT);

		for direction in [Direction::Publish, Direction::Play] {
			let (stream, _peer) = tokio::io::duplex(1);
			let (session, _) = ClientSession::new(ClientSessionConfig::new()).unwrap();
			let mut client = Client {
				stream,
				session,
				work: VecDeque::new(),
				app: "live".into(),
				delay: crate::DEFAULT_DELAY,
				metadata: Default::default(),
			};
			let start = kio::clock::Instant::now();
			assert!(client.await_event(direction).await.is_err());
			assert_eq!(start.elapsed(), crate::server::REQUEST_TIMEOUT);
			let origin = moq_tokio::origin::spawn();
			let mut publisher = Publisher::new(&origin, "idle", Default::default()).unwrap();
			if matches!(direction, Direction::Publish) {
				let (packet, _) = client.session.send_ping_request().unwrap();
				client.work.push_back(ClientSessionResult::OutboundResponse(packet));
			}
			let start = kio::clock::Instant::now();
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
			delay: crate::DEFAULT_DELAY,
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
	fn source(
		origin: &origin::Producer,
	) -> (
		moq_net::origin::Route,
		moq_mux::catalog::Producer,
		moq_mux::container::flv::Finished,
	) {
		let mut vseq = vec![0x17, 0x00, 0x00, 0x00, 0x00];
		vseq.extend_from_slice(&[0x01, 0x42, 0xc0, 0x1f, 0xff, 0xe1, 0x00, 0x04, 0x67, 0x42, 0xc0, 0x1f]);
		vseq.extend_from_slice(&[0x01, 0x00, 0x04, 0x68, 0xce, 0x3c, 0x80]);
		let mut vframe = vec![0x17, 0x01, 0x00, 0x00, 0x00];
		vframe.extend_from_slice(&[0, 0, 0, 5, 0x65, 0x88, 0x84, 0x21, 0x00]);

		let broadcast = origin
			.route("live/cam0", moq_net::origin::RouteOptions::default())
			.unwrap();
		let catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();
		let mut importer = FlvImport::new(broadcast.broadcast().clone(), catalog.reserve());
		importer.decode(&flv::file_header()).unwrap();
		importer.decode(&flv::tag(flv::TAG_VIDEO, 0, &vseq)).unwrap();
		importer.decode(&flv::tag(flv::TAG_VIDEO, 0, &vframe)).unwrap();
		(broadcast, catalog, importer.finish().unwrap())
	}

	/// Resolve `live/cam0` on `origin` to push it.
	async fn exported(origin: origin::Consumer) -> moq_mux::Source {
		let broadcast = origin.request("live/cam0", None).await.unwrap();
		moq_mux::Source::new(origin, broadcast)
	}

	/// A clean end crosses an RTMP hop in both directions: a finished export unpublishes,
	/// so the server it pushes to lingers, and a server whose play completes lets a pull
	/// linger. Each lingering side serves its finished copy for its linger.
	#[tokio::test(start_paused = true)]
	async fn a_finished_broadcast_lingers_across_rtmp() {
		let peer = "127.0.0.1:1935".parse().unwrap();
		let linger = Duration::from_secs(10);
		let lingering = |linger| {
			let mut ingest = moq_mux::Ingest::default();
			ingest.linger = linger;
			ingest
		};
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
						.accept(&ingest, moq_net::Path::from_static("live/cam0"), &lingering(linger))
						.await
						.unwrap(),
					Request::Play(play) => play
						.accept(&served, moq_net::Path::from_static("live/cam0"))
						.await
						.unwrap(),
				}
			});
			let origin = upstream.consume();
			let client = tokio::spawn(async move {
				let client = Client::with_stream(client_io, &live_url()).await.unwrap();
				match push {
					true => client.publish("cam0", exported(origin).await).await.unwrap(),
					false => client
						.pull(
							"cam0",
							&pulled,
							moq_net::Path::from_static("live/cam0"),
							&lingering(linger),
						)
						.await
						.unwrap(),
				}
			});

			let consumer = downstream.consume();
			let copy = consumer
				.follow("live/cam0", None)
				.unwrap()
				.available()
				.await
				.unwrap()
				.unwrap();
			// The catalog goes out once the keyframe landed, so the copy holds media to linger with.
			let mut listed = copy.track("catalog.json").unwrap().subscribe(None).await.unwrap();
			assert!(listed.recv_group().await.unwrap().is_some());

			let start = kio::clock::now();
			catalog.finish().unwrap();
			broadcast.broadcast().close();
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

	/// A server that answers the publish or play with `_error` fails the client with
	/// that error's code and description, not with the server closing afterwards.
	#[tokio::test(start_paused = true)]
	async fn a_rejected_request_reports_its_code_and_description() {
		let peer = "127.0.0.1:1935".parse().unwrap();
		for push in [true, false] {
			let (client_io, server_io) = tokio::io::duplex(64 * 1024);
			let server = tokio::spawn(async move {
				match accept_stream(server_io, peer).await.unwrap().unwrap() {
					Request::Publish(publish) => publish.reject("no ingest here").await.unwrap(),
					Request::Play(play) => play.reject("no ingest here").await.unwrap(),
				}
			});

			let upstream = moq_tokio::origin::spawn();
			let client = Client::with_stream(client_io, &live_url()).await.unwrap();
			let err = match push {
				true => {
					let (_broadcast, _catalog, _importer) = source(&upstream);
					client.publish("cam0", exported(upstream.consume()).await).await
				}
				false => {
					client
						.pull(
							"cam0",
							&upstream,
							moq_net::Path::from_static("live/cam0"),
							&Default::default(),
						)
						.await
				}
			}
			.unwrap_err()
			.to_string();
			let expected = match push {
				true => "rtmp publish rejected by remote: NetStream.Publish.Denied: no ingest here",
				false => "rtmp play rejected by remote: NetStream.Play.Failed: no ingest here",
			};
			assert!(err.ends_with(expected), "push={push}: {err}");
			server.await.unwrap();
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
		let publish = |origin: &origin::Producer, instance: &moq_net::Instance, idr: &[u8]| {
			let mut vseq = vec![0x17, 0x00, 0x00, 0x00, 0x00];
			vseq.extend_from_slice(&[0x01, 0x42, 0xc0, 0x1f, 0xff, 0xe1, 0x00, 0x04, 0x67, 0x42, 0xc0, 0x1f]);
			vseq.extend_from_slice(&[0x01, 0x00, 0x04, 0x68, 0xce, 0x3c, 0x80]);
			let mut vframe = vec![0x17, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, idr.len() as u8];
			vframe.extend_from_slice(idr);
			let route = origin
				.route(
					"live/cam0",
					moq_net::origin::RouteOptions::default().with_instance(instance.clone(), 0),
				)
				.unwrap();
			let catalog =
				moq_mux::catalog::Producer::new(&mut route.broadcast().clone(), moq_mux::catalog::Config::default())
					.unwrap();
			let mut importer = FlvImport::new(route.broadcast().clone(), catalog.reserve());
			importer.decode(&flv::file_header()).unwrap();
			importer.decode(&flv::tag(flv::TAG_VIDEO, 0, &vseq)).unwrap();
			importer.decode(&flv::tag(flv::TAG_VIDEO, 0, &vframe)).unwrap();
			(route, importer, catalog)
		};

		let upstream = moq_tokio::origin::spawn();
		let downstream = moq_tokio::origin::spawn();
		let (old_route, mut old, old_catalog) = publish(&upstream, &moq_net::Instance::random(), OLD_IDR);
		let (client_io, server_io) = tokio::io::duplex(64 * 1024);
		let ingest = downstream.clone();
		let server = tokio::spawn(async move {
			let Some(Request::Publish(publish)) = accept_stream(server_io, "127.0.0.1:1935".parse().unwrap())
				.await
				.unwrap()
			else {
				panic!("expected a publish request");
			};
			let _ = publish
				.accept(&ingest, moq_net::Path::from_static("live/cam0"), &Default::default())
				.await;
		});
		let origin = upstream.consume();
		let pushed = tokio::spawn(async move {
			let client = Client::with_stream(client_io, &live_url()).await.unwrap();
			client.publish("cam0", exported(origin).await).await
		});

		// Read what reaches the remote back out as FLV.
		let consumer = downstream.consume();
		let copy = consumer
			.follow("live/cam0", None)
			.unwrap()
			.available()
			.await
			.unwrap()
			.unwrap();
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

		let instance = moq_net::Instance::random();
		let mut instances = upstream.consume().follow("live/cam0", instance.clone()).unwrap();
		let (_new_route, _new, _new_catalog) = publish(&upstream, &instance, NEW_IDR);
		kio::clock::timeout(Duration::from_secs(5), instances.available())
			.await
			.expect("the new publisher takes over")
			.unwrap()
			.expect("the new publisher serves");

		let mut delta = vec![0x27, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, OLD_DELTA.len() as u8];
		delta.extend_from_slice(OLD_DELTA);
		old.decode(&flv::tag(flv::TAG_VIDEO, 40, &delta)).unwrap();
		until(OLD_DELTA).await;
		assert!(!contains(&received, NEW_IDR), "the push left its instance");

		old.abort(moq_net::Error::Transport("connection lost".into()));
		drop(old_catalog);
		drop(old_route);
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
		let broadcast = server_origin
			.route("live/cam0", moq_net::origin::RouteOptions::default())
			.unwrap();
		let catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();
		let mut importer = FlvImport::new(broadcast.broadcast().clone(), catalog.reserve());
		importer.decode(&flv::file_header()).unwrap();
		importer.decode(&flv::tag(flv::TAG_VIDEO, 0, &vseq)).unwrap();
		importer.decode(&flv::tag(flv::TAG_VIDEO, 0, &vframe)).unwrap();
		let _finished = importer.finish().unwrap();

		// Run the crate's own server, serving the one play request from that origin.
		let mut server = Server::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
		let addr = server.local_addr().unwrap();
		let consumer = server_origin.consume();
		let server_task = tokio::spawn(async move {
			let request = server.accept().await.expect("a request");
			assert_eq!(request.tc_url(), Some(format!("rtmp://{addr}/live").as_str()));
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
			let url = format!("rtmp://{addr}/live").parse().unwrap();
			let client = Client::connect(Connect::new(url)).await.unwrap();
			client
				.pull(
					"cam0",
					&pull_origin,
					moq_net::Path::from_static("pulled/cam0"),
					&Default::default(),
				)
				.await
				.unwrap();
		});

		// The republished broadcast should show up in the client's origin.
		let broadcast = announced
			.follow("pulled/cam0", None)
			.unwrap()
			.available()
			.await
			.unwrap()
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
		let mut group = kio::clock::timeout(Duration::from_secs(5), catalog_track.next_group())
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
