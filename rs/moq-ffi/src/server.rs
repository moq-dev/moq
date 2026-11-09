//! Accept incoming MoQ sessions.

use std::path::PathBuf;
use std::sync::Arc;

use crate::error::MoqError;
use crate::ffi::{OneShot, Task};
use crate::origin::MoqOriginProducer;
use crate::session::{MoqQuicConfig, MoqSession};

/// Configuration for [`MoqServer::new`], mirroring moq-tokio's server config.
///
/// Every field has a default, so set only what you need. The TLS identity needs one of
/// `tls.generate` or a `tls.cert`/`tls.key` pair.
#[derive(Clone, Default, uniffi::Record)]
pub struct MoqServerConfig {
	/// Address to bind, e.g. `127.0.0.1:4443`, `[::]:443`, or `localhost:0`. Null binds `[::]:443`.
	///
	/// DNS hostnames are resolved when [`MoqServer::listen`] binds.
	#[uniffi(default = None)]
	pub bind: Option<String>,
	/// Protocol versions to accept, spelled like `moq-lite-03`. Empty accepts every supported version.
	#[uniffi(default = [])]
	pub versions: Vec<String>,
	/// The served TLS identity.
	#[uniffi(default)]
	pub tls: MoqServerTls,
	/// QUIC transport tuning.
	#[uniffi(default)]
	pub quic: MoqQuicConfig,
	/// Your own `name/version` product tokens, space-separated, advertised in SETUP ahead of
	/// moq-ffi's, as [`MoqClientConfig::implementation`](crate::session::MoqClientConfig::implementation) does.
	#[uniffi(default = None)]
	pub implementation: Option<String>,
	/// The origin whose broadcasts are served to incoming sessions.
	///
	/// With neither `publish` nor `consume` set, each session's two sides share one fresh
	/// origin. A [`MoqRequest`] can override either side in [`accept`](MoqRequest::accept).
	#[uniffi(default = None)]
	pub publish: Option<Arc<MoqOriginProducer>>,
	/// The origin that receives broadcasts published by incoming sessions. See `publish`.
	#[uniffi(default = None)]
	pub consume: Option<Arc<MoqOriginProducer>>,
}

/// The served TLS identity for a [`MoqServerConfig`].
#[derive(Clone, Debug, Default, uniffi::Record)]
pub struct MoqServerTls {
	/// PEM certificate chain files, one per identity.
	#[uniffi(default = [])]
	pub cert: Vec<String>,
	/// PEM private key files, paired with `cert` in order.
	#[uniffi(default = [])]
	pub key: Vec<String>,
	/// Hostnames to generate a self-signed certificate for.
	///
	/// Clients must either pin the certificate fingerprint or disable verification.
	#[uniffi(default = [])]
	pub generate: Vec<String>,
}

/// What a server's configuration resolves to, fixed at construction.
struct Settings {
	config: moq_tokio::listen::Config,
	quic: moq_tokio::quic::Config,
}

/// The origins served to incoming sessions, held until the server is cancelled.
#[derive(Clone)]
struct Origins {
	publish: Option<Arc<MoqOriginProducer>>,
	consume: Option<Arc<MoqOriginProducer>>,
}

/// Where a server is in its lifecycle.
enum Phase {
	/// Nothing bound yet, or the last bind failed.
	Idle(Origins),
	/// `listen` started the accept loop.
	Listening(Listening),
	/// Terminal. Keeps the loop's completion, so a later call still reports its panic.
	Cancelled(crate::ffi::Background),
}

/// A running accept loop, which `cancel` stops without waiting for a parked `accept`.
struct Listening {
	driver: crate::ffi::Background,
	/// Dropped to stop the loop.
	stop: tokio::sync::oneshot::Sender<()>,
	/// Disconnects once the loop has closed the listener.
	closed: std::sync::mpsc::Receiver<()>,
	/// The requests the loop accepted, read by one `accept` at a time.
	queue: Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<moq_tokio::server::Request>>>,
	certificates: moq_tokio::tls::Certificates,
	origins: Origins,
}

impl Listening {
	/// Stop the accept loop and block until the listener's sockets are released, returning
	/// the loop's completion.
	///
	/// Waits on a std channel so it works from inside another runtime, but must not run on
	/// an FFI runtime worker, which the accept loop may need.
	fn close(self) -> crate::ffi::Background {
		drop(self.stop);
		// Disconnects when the loop finishes, or when the runtime is gone and dropped it.
		let _ = self.closed.recv();
		self.driver
	}
}

/// Start `server`, report its address to `bound`, and move requests into `queue`, then close
/// the listener once `stop` fires or nothing more can arrive.
///
/// Runs on the FFI runtime so `MoqServer::cancel` can close the listener while an `accept`
/// is parked, and so a cancelled `listen` never drops a half-started server instead of
/// closing it. A request waits in `queue` until an `accept` is polled for it, so one cancelled
/// but not yet freed never takes it. A slot is reserved before accepting, so at most one
/// request waits unclaimed.
async fn serve(
	server: moq_tokio::Server,
	bound: tokio::sync::oneshot::Sender<Result<String, MoqError>>,
	queue: tokio::sync::mpsc::Sender<moq_tokio::server::Request>,
	mut stop: tokio::sync::oneshot::Receiver<()>,
	closed: std::sync::mpsc::Sender<()>,
) {
	// Not raced against `stop`: `Listener::close` is the only synchronous release, so a
	// cancel waits out the bind rather than dropping it.
	let mut listener = match server.listen().await {
		Ok(listener) => listener,
		Err(err) => {
			let _ = bound.send(Err(MoqError::Bind {
				reason: format!("{err}"),
			}));
			return;
		}
	};
	match listener.local_addr() {
		Ok(addr) => {
			let _ = bound.send(Ok(addr.to_string()));
		}
		Err(err) => {
			let _ = bound.send(Err(MoqError::Bind {
				reason: format!("{err}"),
			}));
			listener.close().await;
			return;
		}
	}

	loop {
		let slot = tokio::select! {
			_ = &mut stop => break,
			slot = queue.reserve() => match slot {
				Ok(slot) => slot,
				Err(_) => break,
			},
		};
		let request = tokio::select! {
			_ = &mut stop => break,
			request = listener.accept() => request,
		};
		match request {
			Ok(request) => slot.send(request),
			Err(err) => {
				tracing::error!(%err, "server stopped accepting");
				break;
			}
		}
	}
	// `Listener::close` releases the sockets before it returns; a drop only schedules that.
	listener.close().await;
	drop(closed);
}

impl Settings {
	fn new(config: MoqServerConfig) -> Result<(Self, Origins), MoqError> {
		let mut listen = moq_tokio::listen::Config::default();
		listen.implementation = Some(crate::session::implementation(config.implementation.as_deref())?);
		if let Some(bind) = config.bind {
			let parsed = bind.parse().map_err(|_| MoqError::Config {
				reason: format!("invalid bind address: {bind}"),
			})?;
			listen.bind = Some(parsed);
		}
		listen.version = crate::session::parse_versions(&config.versions)?;
		listen.tls.cert = config.tls.cert.into_iter().map(PathBuf::from).collect();
		listen.tls.key = config.tls.key.into_iter().map(PathBuf::from).collect();
		listen.tls.generate = config.tls.generate;

		// moq-tokio only checks these when `listen` binds, which would report them as `Bind`.
		let quic: moq_tokio::quic::Config = config.quic.into();
		quic.validate().map_err(|err| MoqError::Config {
			reason: err.to_string(),
		})?;

		let origins = Origins {
			publish: config.publish,
			consume: config.consume,
		};
		Ok((Self { config: listen, quic }, origins))
	}
}

/// A terminal call's answer: the accept loop's panic, or `Cancelled`.
async fn cancelled<T>(driver: crate::ffi::Background) -> Result<T, MoqError> {
	driver.finished().await?;
	Err(MoqError::Cancelled)
}

fn not_listening() -> MoqError {
	MoqError::Bind {
		reason: "not listening; call listen() first".into(),
	}
}

/// A MoQ server that accepts incoming QUIC/WebTransport sessions.
#[derive(uniffi::Object)]
pub struct MoqServer {
	settings: Settings,
	/// Never held across an await, so `cancel` closes the listener while an `accept` is parked.
	phase: std::sync::Mutex<Phase>,
}

impl MoqServer {
	/// Bind, and start the accept loop, moving `phase` from `Idle` to `Listening`.
	///
	/// `init` binds the QUIC socket, so it runs under the lock that `cancel` takes to close it.
	fn start(
		&self,
		phase: &mut Phase,
		origins: Origins,
	) -> Result<
		(
			tokio::sync::oneshot::Receiver<Result<String, MoqError>>,
			crate::ffi::Background,
		),
		MoqError,
	> {
		let _guard = crate::ffi::enter();
		let server = self
			.settings
			.config
			.clone()
			.init(self.settings.quic.clone())
			.map_err(|err| MoqError::Bind {
				reason: format!("{err}"),
			})?;
		let certificates = server.certificates();
		let (bound, addr) = tokio::sync::oneshot::channel();
		let (queue, requests) = tokio::sync::mpsc::channel(1);
		let (stop, stopped) = tokio::sync::oneshot::channel();
		let (closed, wait) = std::sync::mpsc::channel();
		let driver = crate::ffi::spawn(serve(server, bound, queue, stopped, closed));
		*phase = Phase::Listening(Listening {
			driver: driver.clone(),
			stop,
			closed: wait,
			queue: Arc::new(tokio::sync::Mutex::new(requests)),
			certificates,
			origins,
		});
		Ok((addr, driver))
	}

	/// Whether an `accept` is waiting on the queue.
	#[cfg(test)]
	pub(crate) fn accepting(&self) -> bool {
		match &*self.phase.lock().unwrap() {
			Phase::Listening(listening) => listening.queue.try_lock().is_err(),
			Phase::Idle(_) | Phase::Cancelled(_) => false,
		}
	}
}

#[uniffi::export]
impl MoqServer {
	/// Create a server from `config`, failing on any value it cannot use.
	///
	/// Nothing is bound until [`listen`](Self::listen).
	#[uniffi::constructor]
	pub fn new(config: MoqServerConfig) -> Result<Arc<Self>, MoqError> {
		let _guard = crate::ffi::runtime().enter();
		let (settings, origins) = Settings::new(config)?;
		Ok(Arc::new(Self {
			settings,
			phase: std::sync::Mutex::new(Phase::Idle(origins)),
		}))
	}

	/// Bind the listening socket. Returns the bound local address as a string,
	/// which is useful when binding to an ephemeral port (`:0`).
	pub async fn listen(&self) -> Result<String, MoqError> {
		let started = {
			let mut phase = self.phase.lock().unwrap();
			match &*phase {
				Phase::Idle(origins) => {
					let origins = origins.clone();
					Ok(self.start(&mut phase, origins)?)
				}
				Phase::Listening(_) => {
					return Err(MoqError::Bind {
						reason: "already listening".into(),
					});
				}
				Phase::Cancelled(driver) => Err(driver.clone()),
			}
		};
		let (addr, driver) = match started {
			Ok(started) => started,
			Err(driver) => return cancelled(driver).await,
		};

		let result = crate::ffi::run(async {
			// Dropped only when the runtime is gone.
			addr.await.map_err(|_| MoqError::Cancelled)?
		})
		.await;
		let after = {
			let mut phase = self.phase.lock().unwrap();
			match &*phase {
				Phase::Cancelled(driver) => Some(driver.clone()),
				// Only this call leaves `Idle`, so the loop that failed to bind is the one listening.
				Phase::Listening(listening) if result.is_err() => {
					*phase = Phase::Idle(listening.origins.clone());
					None
				}
				Phase::Idle(_) | Phase::Listening(_) => None,
			}
		};
		if let Some(driver) = after {
			return cancelled(driver).await;
		}
		driver.check()?;
		result
	}

	/// Accept the next incoming session. Returns `None` when the server has closed.
	///
	/// `listen()` must be called first. Dropping the returned future aborts this
	/// call alone and leaves the server listening.
	pub async fn accept(&self) -> Result<Option<Arc<MoqRequest>>, MoqError> {
		let listening = match &*self.phase.lock().unwrap() {
			Phase::Idle(_) => return Err(not_listening()),
			Phase::Listening(listening) => Ok((listening.driver.clone(), listening.queue.clone())),
			Phase::Cancelled(driver) => Err(driver.clone()),
		};
		let (driver, queue) = match listening {
			Ok(listening) => listening,
			Err(driver) => return cancelled(driver).await,
		};
		// A cancel closes the loop, which ends the queue and wakes this.
		let request = driver
			.watch(crate::ffi::run(async move {
				Ok::<_, MoqError>(queue.lock().await.recv().await)
			}))
			.await;
		// Read after the wait, so a parked accept holds no origin past a cancel.
		let origins = match &*self.phase.lock().unwrap() {
			Phase::Cancelled(driver) => Err(driver.clone()),
			Phase::Idle(origins) => Ok(origins.clone()),
			Phase::Listening(listening) => Ok(listening.origins.clone()),
		};
		let origins = match origins {
			Ok(origins) => origins,
			Err(driver) => return cancelled(driver).await,
		};
		match request? {
			Some(request) => Ok(Some(MoqRequest::new(request, origins.publish, origins.consume)?)),
			None => {
				driver.finished().await?;
				Ok(None)
			}
		}
	}

	/// SHA-256 fingerprints of the configured TLS certificates, hex-encoded.
	///
	/// Useful for pinning a generated self-signed certificate in a browser via
	/// WebTransport's `serverCertificateHashes`. Returns an error if called
	/// before `listen()`.
	pub fn cert_fingerprints(&self) -> Result<Vec<String>, MoqError> {
		match &*self.phase.lock().unwrap() {
			Phase::Idle(_) => Err(not_listening()),
			Phase::Listening(listening) => {
				listening.driver.check()?;
				Ok(listening.certificates.fingerprints())
			}
			Phase::Cancelled(driver) => {
				driver.check()?;
				Err(MoqError::Cancelled)
			}
		}
	}

	/// Cancel any in-flight `listen()` or `accept()` call.
	///
	/// Terminal, and synchronous: it returns once the listening socket is closed,
	/// not when the handle is, so the address can be bound again immediately.
	/// `cert_fingerprints()` returns `Cancelled` afterwards.
	pub fn cancel(&self) {
		// Closed under the lock, so a concurrent `cancel` returns only once the socket is released.
		let mut phase = self.phase.lock().unwrap();
		let driver = match std::mem::replace(&mut *phase, Phase::Cancelled(Default::default())) {
			Phase::Idle(_) => Default::default(),
			Phase::Listening(listening) => listening.close(),
			Phase::Cancelled(driver) => driver,
		};
		*phase = Phase::Cancelled(driver);
	}
}

struct RequestState {
	request: OneShot<moq_tokio::server::Request>,
	publish: Option<Arc<MoqOriginProducer>>,
	consume: Option<Arc<MoqOriginProducer>>,
}

/// The network transport carrying an incoming session.
#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum MoqTransport {
	/// Raw QUIC, negotiating a MoQ ALPN directly.
	Quic,
	/// A WebSocket connection using qmux framing.
	WebSocket,
	/// A plaintext TCP connection using qmux framing.
	Tcp,
	/// A Unix domain socket using qmux framing.
	Unix,
	/// WebTransport over HTTP/3 on QUIC.
	WebTransport,
}

impl TryFrom<moq_tokio::Transport> for MoqTransport {
	type Error = MoqError;

	fn try_from(value: moq_tokio::Transport) -> Result<Self, Self::Error> {
		Ok(match value {
			moq_tokio::Transport::Quic => Self::Quic,
			moq_tokio::Transport::WebSocket => Self::WebSocket,
			moq_tokio::Transport::Tcp => Self::Tcp,
			moq_tokio::Transport::Unix => Self::Unix,
			moq_tokio::Transport::WebTransport => Self::WebTransport,
			_ => return Err(MoqError::Unsupported),
		})
	}
}

#[cfg(test)]
mod transport_tests {
	use super::MoqTransport;
	use moq_tokio::Transport;

	#[test]
	fn converts_supported_transports() {
		assert_eq!(MoqTransport::try_from(Transport::Quic).unwrap(), MoqTransport::Quic);
		assert_eq!(
			MoqTransport::try_from(Transport::WebSocket).unwrap(),
			MoqTransport::WebSocket
		);
		assert_eq!(MoqTransport::try_from(Transport::Tcp).unwrap(), MoqTransport::Tcp);
		assert_eq!(MoqTransport::try_from(Transport::Unix).unwrap(), MoqTransport::Unix);
		assert_eq!(
			MoqTransport::try_from(Transport::WebTransport).unwrap(),
			MoqTransport::WebTransport
		);
	}
}

/// An incoming MoQ session that can be accepted or rejected.
///
/// Origin arguments are captured when [`accept`](Self::accept) starts. A second
/// response fails with [`MoqError::AlreadyResponded`], or with the first one's error if it
/// failed, and calls after [`cancel`](Self::cancel) fail with [`MoqError::Cancelled`].
#[derive(uniffi::Object)]
pub struct MoqRequest {
	task: Task<RequestState>,
	transport: MoqTransport,
	url: Option<String>,
	path: String,
	query: Option<String>,
	peer_implementation: Option<String>,
}

impl MoqRequest {
	fn new(
		request: moq_tokio::server::Request,
		publish: Option<Arc<MoqOriginProducer>>,
		consume: Option<Arc<MoqOriginProducer>>,
	) -> Result<Arc<Self>, MoqError> {
		let transport = request.transport().try_into()?;
		let url = request.url().map(|u| u.to_string());
		let path = request.path().to_string();
		let query = request.query().map(str::to_string);
		let peer_implementation = request.peer_implementation().map(str::to_string);
		Ok(Arc::new(Self {
			task: Task::new(RequestState {
				request: OneShot::new(request),
				publish,
				consume,
			}),
			transport,
			url,
			path,
			query,
			peer_implementation,
		}))
	}
}

#[cfg(test)]
impl MoqRequest {
	/// Hold the request lock until `held` finishes.
	///
	/// `accept`/`reject` use the same `Task::run` path; a live handshake can
	/// finish before a queued accept samples the locked state.
	pub(crate) async fn hold_lock<F, Fut>(&self, held: F) -> Result<(), MoqError>
	where
		F: FnOnce() -> Fut + Send + 'static,
		Fut: std::future::Future<Output = ()> + Send + 'static,
	{
		self.task
			.run(move |state| async move {
				let _state = state;
				held().await;
				Ok(())
			})
			.await
	}
}

#[uniffi::export]
impl MoqRequest {
	/// The URL provided by the client, if any.
	pub fn url(&self) -> Option<String> {
		self.url.clone()
	}

	/// The query-free request path, or empty for the root/missing path.
	pub fn path(&self) -> String {
		self.path.clone()
	}

	/// The encoded request query without the leading `?`, if present.
	pub fn query(&self) -> Option<String> {
		self.query.clone()
	}

	/// The implementation the client advertised in its SETUP, such as
	/// `moq-py/0.6.0 moq-ffi/0.4.12 moq-net-rs/0.3.10`, or null when it advertised none.
	///
	/// Peer-controlled: use it for logs and rollout counts, never to authorize.
	pub fn peer_implementation(&self) -> Option<String> {
		self.peer_implementation.clone()
	}

	/// The network transport carrying this session.
	pub fn transport(&self) -> MoqTransport {
		self.transport
	}

	/// Complete the MoQ handshake and return the established session.
	///
	/// A null origin inherits the server's configured origin; a supplied origin replaces it.
	/// Pass a fresh origin for isolation, or the same fresh origin on both sides to share it.
	/// Returns `AlreadyResponded` after a response, the response's error if it failed, and
	/// `Cancelled` after cancellation.
	#[uniffi::method(default(publish = None, consume = None))]
	pub async fn accept(
		&self,
		publish: Option<Arc<MoqOriginProducer>>,
		consume: Option<Arc<MoqOriginProducer>>,
	) -> Result<Arc<MoqSession>, MoqError> {
		// Foreign runtime threads can have a 512 KiB stack. Keep the transport
		// handshake out of the nested Task::run futures polled on that stack.
		self.task
			.run(move |state| {
				Box::pin(async move {
					let (request, answering) = state.request.take()?;
					// Materialize both origin sides so the session can publish/subscribe and the
					// FFI can hand back a publisher/consumer.
					let (publish, subscribe) = crate::origin::resolve_pair(
						publish.as_ref().or(state.publish.as_ref()),
						consume.as_ref().or(state.consume.as_ref()),
					);
					let session = request
						.with_publisher(&publish.inner)
						.with_subscriber(subscribe.inner.clone())
						.ok()
						.await
						.map_err(|err| MoqError::Connect {
							reason: format!("{err}"),
						});
					answering.settle(session.map(|session| Arc::new(MoqSession::accepted(session, publish, subscribe))))
				})
			})
			.await
	}

	/// Reject the established MoQ session with an application error code.
	///
	/// Codes 401 and 403 map to the protocol's unauthorized error; every other
	/// code is sent as an application error.
	///
	/// Returns `AlreadyResponded` if `accept()` or `reject()` has already been called, or that
	/// call's error if it failed.
	pub async fn reject(&self, code: u16) -> Result<(), MoqError> {
		self.task
			.run(move |state| async move {
				let (request, answering) = state.request.take()?;
				let reject = match code {
					401 | 403 => moq_net::SessionError::Unauthorized,
					code => moq_net::SessionError::App(code),
				};
				answering.settle(request.reject(reject).await.map_err(|err| MoqError::Reject {
					reason: format!("{err}"),
				}))
			})
			.await
	}

	/// Cancel any in-flight `accept()` or `reject()` call.
	///
	/// Terminal: an unanswered request is dropped here rather than when the handle is, which
	/// rejects the session.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}

#[cfg(test)]
mod lifecycle_tests {
	use super::*;

	/// A server can complete a real handshake on an Apple-sized foreign thread.
	#[test]
	fn accept_on_a_foreign_thread() {
		std::thread::Builder::new()
			.name("foreign-session".into())
			.stack_size(512 * 1024)
			.spawn(|| {
				let server = MoqServer::new(MoqServerConfig {
					bind: Some("127.0.0.1:0".into()),
					versions: vec!["moq-lite-07-wip".into()],
					tls: MoqServerTls {
						generate: vec!["localhost".into()],
						..Default::default()
					},
					..Default::default()
				})
				.unwrap();
				let addr = pollster::block_on(server.listen()).unwrap();
				let client = crate::session::MoqClient::new(crate::session::MoqClientConfig {
					bind: Some("127.0.0.1:0".into()),
					versions: vec!["moq-lite-07-wip".into()],
					tls: crate::session::MoqClientTls {
						insecure: true,
						..Default::default()
					},
					..Default::default()
				})
				.unwrap();
				let connect =
					crate::ffi::runtime().spawn(async move { client.connect(format!("https://{addr}")).await });
				let request = pollster::block_on(server.accept()).unwrap().unwrap();
				let peer = pollster::block_on(request.accept(None, None)).unwrap();
				let session = pollster::block_on(connect).unwrap().unwrap();
				let everything = crate::auth::MoqGrant {
					publish: vec!["**".into()],
					subscribe: vec!["**".into()],
					expires: None,
				};
				for grants in [peer.grants(), session.grants()] {
					let mut grant = grants.grant();
					while grant.as_ref() != Some(&everything) {
						grant = pollster::block_on(grants.changed()).unwrap();
					}
				}
				peer.cancel(0);
				session.cancel(0);
				server.cancel();
			})
			.unwrap()
			.join()
			.unwrap();
	}
}
