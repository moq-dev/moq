use std::path::PathBuf;
use std::sync::Arc;

use crate::error::MoqError;
use crate::ffi::Task;
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

struct ServerState {
	config: moq_tokio::listen::Config,
	quic: moq_tokio::quic::Config,
	publish: Option<Arc<MoqOriginProducer>>,
	consume: Option<Arc<MoqOriginProducer>>,
	/// Set by `listen`, to the requests the accept loop has taken from the listener.
	requests: Option<Requests>,
}

struct Requests {
	queue: tokio::sync::mpsc::Receiver<moq_tokio::server::Request>,
	certificates: moq_tokio::tls::Certificates,
}

/// Stops a listening server's accept loop without the state lock, which a parked
/// `accept` holds until the host frees it.
struct Listening {
	/// Dropped to stop the loop.
	stop: tokio::sync::oneshot::Sender<()>,
	/// Disconnects once the loop has closed the listener.
	closed: std::sync::mpsc::Receiver<()>,
}

impl Listening {
	/// Stop the accept loop and block until the listener's sockets are released.
	///
	/// Waits on a std channel so it works from inside another runtime, but must not run on
	/// the FFI runtime thread, which the loop needs.
	fn close(self) {
		drop(self.stop);
		// Disconnects when the loop finishes, or when the runtime is gone and dropped it.
		let _ = self.closed.recv();
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
			let _ = bound.send(Err(MoqError::Bind(format!("{err}"))));
			return;
		}
	};
	match listener.local_addr() {
		Ok(addr) => {
			let _ = bound.send(Ok(addr.to_string()));
		}
		Err(err) => {
			let _ = bound.send(Err(MoqError::Bind(format!("{err}"))));
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
			Some(request) => slot.send(request),
			None => break,
		}
	}
	// `Listener::close` releases the sockets before it returns; a drop only schedules that.
	listener.close().await;
	drop(closed);
}

impl ServerState {
	fn new(config: MoqServerConfig) -> Result<Self, MoqError> {
		let mut listen = moq_tokio::listen::Config::default();
		if let Some(bind) = config.bind {
			let parsed = bind
				.parse()
				.map_err(|_| MoqError::Config(format!("invalid bind address: {bind}")))?;
			listen.bind = Some(parsed);
		}
		listen.version = crate::session::parse_versions(&config.versions)?;
		listen.tls.cert = config.tls.cert.into_iter().map(PathBuf::from).collect();
		listen.tls.key = config.tls.key.into_iter().map(PathBuf::from).collect();
		listen.tls.generate = config.tls.generate;

		let mut quic = moq_tokio::quic::Config::default();
		quic.max_streams = config.quic.max_streams;

		Ok(Self {
			config: listen,
			quic,
			publish: config.publish,
			consume: config.consume,
			requests: None,
		})
	}

	/// Bind, and start the accept loop that `listening` stops.
	async fn listen(
		&mut self,
		task: &Task<ServerState>,
		listening: &std::sync::Mutex<Option<Listening>>,
	) -> Result<String, MoqError> {
		if self.requests.is_some() {
			return Err(MoqError::Bind("already listening".into()));
		}
		let (bound, addr) = tokio::sync::oneshot::channel();
		let (queue, requests) = tokio::sync::mpsc::channel(1);
		let certificates = {
			// `cancel` flags the task before it takes `listening`, so checking the flag under
			// that lock means either `cancel` finds the loop or this finds the cancel. `init`
			// binds the QUIC socket, so it goes after the check and straight to the loop.
			let mut slot = listening.lock().unwrap();
			if task.is_cancelled() {
				return Err(MoqError::Cancelled);
			}
			let server = self
				.config
				.clone()
				.init(self.quic.clone())
				.map_err(|err| MoqError::Bind(format!("{err}")))?;
			let certificates = server.certificates();
			let (stop, stopped) = tokio::sync::oneshot::channel();
			let (closed, wait) = std::sync::mpsc::channel();
			crate::ffi::spawn(serve(server, bound, queue, stopped, closed));
			*slot = Some(Listening { stop, closed: wait });
			certificates
		};

		// Dropped only when the runtime is gone.
		let addr = addr.await.map_err(|_| MoqError::Cancelled)??;
		self.requests = Some(Requests {
			queue: requests,
			certificates,
		});
		Ok(addr)
	}

	async fn accept(&mut self) -> Result<Option<Arc<MoqRequest>>, MoqError> {
		let requests = self
			.requests
			.as_mut()
			.ok_or_else(|| MoqError::Bind("not listening; call listen() first".into()))?;
		match requests.queue.recv().await {
			Some(request) => Ok(Some(MoqRequest::new(
				request,
				self.publish.clone(),
				self.consume.clone(),
			)?)),
			None => Ok(None),
		}
	}
}

/// A MoQ server that accepts incoming QUIC/WebTransport sessions.
#[derive(uniffi::Object)]
pub struct MoqServer {
	task: Task<ServerState>,
	/// Outside `task`, so `cancel` closes the listener without waiting for a parked `accept`.
	listening: std::sync::Mutex<Option<Listening>>,
}

#[uniffi::export]
impl MoqServer {
	/// Create a server from `config`, failing on any value it cannot use.
	///
	/// Nothing is bound until [`listen`](Self::listen).
	#[uniffi::constructor]
	pub fn new(config: MoqServerConfig) -> Result<Arc<Self>, MoqError> {
		let _guard = crate::ffi::runtime().enter();
		Ok(Arc::new(Self {
			task: Task::new(ServerState::new(config)?),
			listening: Default::default(),
		}))
	}

	/// Bind the listening socket. Returns the bound local address as a string,
	/// which is useful when binding to an ephemeral port (`:0`).
	pub async fn listen(&self) -> Result<String, MoqError> {
		self.task
			.run(|mut state| async move { state.listen(&self.task, &self.listening).await })
			.await
	}

	/// Accept the next incoming session. Returns `None` when the server has closed.
	///
	/// `listen()` must be called first. Dropping the returned future aborts this
	/// call alone and leaves the server listening.
	pub async fn accept(&self) -> Result<Option<Arc<MoqRequest>>, MoqError> {
		self.task.run(|mut state| async move { state.accept().await }).await
	}

	/// SHA-256 fingerprints of the configured TLS certificates, hex-encoded.
	///
	/// Useful for pinning a generated self-signed certificate in a browser via
	/// WebTransport's `serverCertificateHashes`. Returns an error if called
	/// before `listen()`, and [`MoqError::Busy`] while `listen()` or `accept()` is in flight.
	pub fn cert_fingerprints(&self) -> Result<Vec<String>, MoqError> {
		let state = self.task.configure()?;
		let requests = state
			.requests
			.as_ref()
			.ok_or_else(|| MoqError::Bind("not listening; call listen() first".into()))?;
		Ok(requests.certificates.fingerprints())
	}

	/// Cancel any in-flight `listen()` or `accept()` call.
	///
	/// Terminal, and synchronous: it returns once the listening socket is closed,
	/// not when the handle is, so the address can be bound again immediately.
	/// `cert_fingerprints()` returns `Cancelled` afterwards.
	pub fn cancel(&self) {
		self.task.cancel();
		// Closed under the lock, so a concurrent `cancel` returns only once the socket is released.
		let mut listening = self.listening.lock().unwrap();
		if let Some(listening) = listening.take() {
			listening.close();
		}
	}
}

struct RequestState {
	request: Option<moq_tokio::server::Request>,
	publish: Option<Arc<MoqOriginProducer>>,
	consume: Option<Arc<MoqOriginProducer>>,
}

/// The network transport carrying an incoming session.
#[derive(Clone, Copy, Debug, Eq, PartialEq, uniffi::Enum)]
pub enum MoqTransport {
	/// Raw QUIC, negotiating a MoQ ALPN directly.
	Quic,
	/// An Iroh QUIC connection.
	Iroh,
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
			moq_tokio::Transport::Iroh => Self::Iroh,
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
		assert_eq!(MoqTransport::try_from(Transport::Iroh).unwrap(), MoqTransport::Iroh);
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
/// response fails with [`MoqError::AlreadyResponded`], and calls after
/// [`cancel`](Self::cancel) fail with [`MoqError::Cancelled`].
#[derive(uniffi::Object)]
pub struct MoqRequest {
	task: Task<RequestState>,
	transport: MoqTransport,
	url: Option<String>,
	path: String,
	query: Option<String>,
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
		Ok(Arc::new(Self {
			task: Task::new(RequestState {
				request: Some(request),
				publish,
				consume,
			}),
			transport,
			url,
			path,
			query,
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

	/// The network transport carrying this session.
	pub fn transport(&self) -> MoqTransport {
		self.transport
	}

	/// Complete the MoQ handshake and return the established session.
	///
	/// A null origin inherits the server's configured origin; a supplied origin replaces it.
	/// Pass a fresh origin for isolation, or the same fresh origin on both sides to share it.
	/// Returns `AlreadyResponded` after a response and `Cancelled` after cancellation.
	#[uniffi::method(default(publish = None, consume = None))]
	pub async fn accept(
		&self,
		publish: Option<Arc<MoqOriginProducer>>,
		consume: Option<Arc<MoqOriginProducer>>,
	) -> Result<Arc<MoqSession>, MoqError> {
		self.task
			.run(move |mut state| async move {
				let request = state.request.take().ok_or(MoqError::AlreadyResponded)?;
				// Materialize both origin sides so the session can publish/subscribe and the
				// FFI can hand back a publisher/consumer.
				let (publish, subscribe) = crate::origin::resolve_pair(
					publish.as_ref().or(state.publish.as_ref()),
					consume.as_ref().or(state.consume.as_ref()),
				);
				let session = request
					.with_publisher(&publish)
					.with_subscriber(subscribe.clone())
					.ok()
					.await
					.map_err(|err| MoqError::Connect(format!("{err}")))?;
				Ok(Arc::new(MoqSession::accepted(session, publish, subscribe)))
			})
			.await
	}

	/// Reject the established MoQ session with an application error code.
	///
	/// Codes 401 and 403 map to the protocol's unauthorized error; every other
	/// code is sent as an application error.
	///
	/// Returns `AlreadyResponded` if `accept()` or `reject()` has already been called.
	pub async fn reject(&self, code: u16) -> Result<(), MoqError> {
		self.task
			.run(move |mut state| async move {
				let request = state.request.take().ok_or(MoqError::AlreadyResponded)?;
				let reject = match code {
					401 => moq_tokio::server::Reject::Unauthorized,
					403 => moq_tokio::server::Reject::Forbidden,
					code => moq_tokio::server::Reject::App(code),
				};
				request
					.reject(reject)
					.await
					.map_err(|err| MoqError::Reject(format!("{err}")))?;
				Ok(())
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
