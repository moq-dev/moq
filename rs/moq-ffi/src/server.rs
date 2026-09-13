use std::path::PathBuf;
use std::sync::Arc;

use crate::cancel::{self, MoqCancel};
use crate::error::MoqError;
use crate::ffi::Task;
use crate::origin::MoqOriginProducer;
use crate::session::MoqSession;

struct ServerState {
	config: moq_tokio::listen::Config,
	publish: Option<Arc<MoqOriginProducer>>,
	consume: Option<Arc<MoqOriginProducer>>,
	server: Option<moq_tokio::Listener>,
}

impl ServerState {
	async fn listen(&mut self) -> Result<String, MoqError> {
		if self.server.is_some() {
			return Err(MoqError::Bind("already listening".into()));
		}
		let server = self
			.config
			.clone()
			.init(Default::default())
			.map_err(|err| MoqError::Bind(format!("{err}")))?
			.listen()
			.await
			.map_err(|err| MoqError::Bind(format!("{err}")))?;
		let addr = server
			.local_addr()
			.map_err(|err| MoqError::Bind(format!("{err}")))?
			.to_string();
		self.server = Some(server);
		Ok(addr)
	}

	async fn accept(&mut self) -> Result<Option<Arc<MoqRequest>>, MoqError> {
		let server = self
			.server
			.as_mut()
			.ok_or_else(|| MoqError::Bind("not listening; call listen() first".into()))?;
		let publish = self.publish.clone();
		let consume = self.consume.clone();
		match server.accept().await {
			Some(request) => Ok(Some(MoqRequest::new(request, publish, consume))),
			None => Ok(None),
		}
	}
}

/// A MoQ server that accepts incoming QUIC/WebTransport sessions.
///
/// Bind and TLS are captured at [`listen`](Self::listen); those setters fail
/// afterwards. Origins are captured at each [`accept`](Self::accept). Every setter
/// fails with [`MoqError::Busy`] while listen/accept is in flight and
/// [`MoqError::Cancelled`] after [`cancel`](Self::cancel).
#[derive(uniffi::Object)]
pub struct MoqServer {
	task: Task<ServerState>,
}

impl MoqServer {
	fn configure<R>(&self, f: impl FnOnce(&mut ServerState) -> R) -> Result<R, MoqError> {
		Ok(f(&mut *self.task.configure()?))
	}

	fn configure_listen<R>(&self, f: impl FnOnce(&mut ServerState) -> R) -> Result<R, MoqError> {
		let mut state = self.task.configure()?;
		if state.server.is_some() {
			return Err(MoqError::Bind("already listening".into()));
		}
		Ok(f(&mut state))
	}
}

#[uniffi::export]
impl MoqServer {
	/// Create a new MoQ server with default configuration.
	#[uniffi::constructor]
	pub fn new() -> Arc<Self> {
		let _guard = crate::ffi::RUNTIME.enter();
		Arc::new(Self {
			task: Task::new(ServerState {
				config: moq_tokio::listen::Config::default(),
				publish: None,
				consume: None,
				server: None,
			}),
		})
	}

	/// Set the address to bind, e.g. `127.0.0.1:4443`, `[::]:443`, or `localhost:0`.
	///
	/// Validated syntactically up-front. DNS hostnames are accepted and resolved
	/// at `listen()` time. Captured at [`listen`](Self::listen); fails afterwards.
	pub fn set_bind(&self, addr: String) -> Result<(), MoqError> {
		// Mirrors `MoqClient::set_bind` by surfacing parse errors here rather
		// than at listen() time. The server takes a String (not SocketAddr) so
		// DNS hostnames are allowed; we only check syntactic structure here.
		if addr.parse::<std::net::SocketAddr>().is_err() {
			let port_ok = addr
				.rsplit_once(':')
				.is_some_and(|(_, port)| port.parse::<u16>().is_ok());
			if !port_ok {
				return Err(MoqError::Bind(format!("invalid bind address: {addr}")));
			}
		}
		self.configure_listen(|state| {
			state.config.bind = Some(addr);
		})
	}

	/// Load TLS certificate chains from PEM files on disk.
	///
	/// Captured at [`listen`](Self::listen); fails afterwards.
	pub fn set_tls_cert(&self, paths: Vec<String>) -> Result<(), MoqError> {
		self.configure_listen(|state| {
			state.config.tls.cert = paths.into_iter().map(PathBuf::from).collect();
		})
	}

	/// Load TLS private keys from PEM files on disk.
	///
	/// Captured at [`listen`](Self::listen); fails afterwards.
	pub fn set_tls_key(&self, paths: Vec<String>) -> Result<(), MoqError> {
		self.configure_listen(|state| {
			state.config.tls.key = paths.into_iter().map(PathBuf::from).collect();
		})
	}

	/// Generate self-signed TLS certificates for the given hostnames.
	///
	/// Clients must either pin the certificate fingerprint or disable verification.
	/// Captured at [`listen`](Self::listen); fails afterwards.
	pub fn set_tls_generate(&self, hostnames: Vec<String>) -> Result<(), MoqError> {
		self.configure_listen(|state| {
			state.config.tls.generate = hostnames;
		})
	}

	/// Set the origin to publish broadcasts to incoming sessions.
	///
	/// Captured at each [`accept`](Self::accept).
	pub fn set_publish(&self, origin: Option<Arc<MoqOriginProducer>>) -> Result<(), MoqError> {
		self.configure(|state| {
			state.publish = origin;
		})
	}

	/// Set the origin to consume broadcasts from incoming sessions.
	///
	/// Captured at each [`accept`](Self::accept).
	pub fn set_consume(&self, origin: Option<Arc<MoqOriginProducer>>) -> Result<(), MoqError> {
		self.configure(|state| {
			state.consume = origin;
		})
	}

	/// Bind the listening socket. Returns the bound local address as a string,
	/// which is useful when binding to an ephemeral port (`:0`).
	pub async fn listen(&self) -> Result<String, MoqError> {
		self.task.run(|mut state| async move { state.listen().await }).await
	}

	/// Accept the next incoming session. Returns `None` when the server has closed.
	///
	/// `listen()` must be called first. `cancel` aborts this call alone, leaving the
	/// server listening; see [`MoqCancel`].
	#[uniffi::method(default(cancel = None))]
	pub async fn accept(&self, cancel: Option<Arc<MoqCancel>>) -> Result<Option<Arc<MoqRequest>>, MoqError> {
		cancel::guard(cancel, self.task.run(|mut state| async move { state.accept().await })).await
	}

	/// SHA-256 fingerprints of the configured TLS certificates, hex-encoded.
	///
	/// Useful for pinning a generated self-signed certificate in a browser via
	/// WebTransport's `serverCertificateHashes`. Returns an error if called
	/// before `listen()`.
	pub fn cert_fingerprints(&self) -> Result<Vec<String>, MoqError> {
		let state = self.task.configure()?;
		let server = state
			.server
			.as_ref()
			.ok_or_else(|| MoqError::Bind("not listening; call listen() first".into()))?;
		Ok(server.certificates().fingerprints())
	}

	/// Cancel any in-flight `listen()` or `accept()` call.
	///
	/// Terminal: the listening socket is closed here, not when the handle is, and
	/// `cert_fingerprints()` returns `Cancelled` afterwards.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}

struct RequestState {
	request: Option<moq_tokio::Request>,
	publish: Option<Arc<MoqOriginProducer>>,
	consume: Option<Arc<MoqOriginProducer>>,
}

/// An incoming MoQ session that can be accepted or rejected.
///
/// Origin overrides are captured at [`accept`](Self::accept). Setters fail with
/// [`MoqError::Busy`] while accept/reject is in flight, [`MoqError::AlreadyResponded`]
/// after a response, and [`MoqError::Cancelled`] after [`cancel`](Self::cancel).
#[derive(uniffi::Object)]
pub struct MoqRequest {
	task: Task<RequestState>,
	transport: String,
	url: Option<String>,
	path: String,
	query: Option<String>,
}

impl MoqRequest {
	fn new(
		request: moq_tokio::Request,
		publish: Option<Arc<MoqOriginProducer>>,
		consume: Option<Arc<MoqOriginProducer>>,
	) -> Arc<Self> {
		let transport = request.transport().to_string();
		let url = request.url().map(|u| u.to_string());
		let path = request.path().to_string();
		let query = request.query().map(str::to_string);
		Arc::new(Self {
			task: Task::new(RequestState {
				request: Some(request),
				publish,
				consume,
			}),
			transport,
			url,
			path,
			query,
		})
	}

	fn configure_origin(&self, f: impl FnOnce(&mut RequestState)) -> Result<(), MoqError> {
		let mut state = self.task.configure()?;
		if state.request.is_none() {
			return Err(MoqError::AlreadyResponded);
		}
		f(&mut state);
		Ok(())
	}
}

#[cfg(test)]
impl MoqRequest {
	/// Hold the request lock until `held` finishes.
	///
	/// `accept`/`reject` use the same `Task::run` path; a live handshake can
	/// finish before a waiter samples `Busy`.
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

	/// The transport type, e.g. `"quic"`, `"iroh"`, or `"websocket"`.
	pub fn transport(&self) -> String {
		self.transport.clone()
	}

	/// Override the publish origin for this session. Falls back to the server's
	/// configured publish origin if unset. Captured at [`accept`](Self::accept).
	pub fn set_publish(&self, origin: Option<Arc<MoqOriginProducer>>) -> Result<(), MoqError> {
		self.configure_origin(|state| {
			state.publish = origin;
		})
	}

	/// Override the consume origin for this session. Falls back to the server's
	/// configured consume origin if unset. Captured at [`accept`](Self::accept).
	pub fn set_consume(&self, origin: Option<Arc<MoqOriginProducer>>) -> Result<(), MoqError> {
		self.configure_origin(|state| {
			state.consume = origin;
		})
	}

	/// Complete the MoQ handshake and return the established session.
	///
	/// Returns `AlreadyResponded` if `accept()` or `reject()` has already been called.
	pub async fn accept(&self) -> Result<Arc<MoqSession>, MoqError> {
		self.task
			.run(|mut state| async move {
				let request = state.request.take().ok_or(MoqError::AlreadyResponded)?;
				// Materialize both origin sides so the session can publish/subscribe and the
				// FFI can hand back a publisher/consumer.
				let (publish, subscribe) = crate::origin::resolve_pair(state.publish.as_ref(), state.consume.as_ref());
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

	/// Reject the session with the given HTTP status code.
	///
	/// Returns `AlreadyResponded` if `accept()` or `reject()` has already been called.
	pub async fn reject(&self, code: u16) -> Result<(), MoqError> {
		self.task
			.run(move |mut state| async move {
				let request = state.request.take().ok_or(MoqError::AlreadyResponded)?;
				request
					.close(code)
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
