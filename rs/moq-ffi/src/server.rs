use std::path::PathBuf;
use std::sync::Arc;

use crate::error::MoqError;
use crate::ffi::Task;
use crate::origin::MoqOriginProducer;
use crate::session::MoqSession;

struct ServerState {
	config: moq_native::ServerConfig,
	publish: Option<Arc<MoqOriginProducer>>,
	consume: Option<Arc<MoqOriginProducer>>,
	server: Option<moq_native::Server>,
}

impl ServerState {
	async fn listen(&mut self) -> Result<String, MoqError> {
		if self.server.is_some() {
			return Err(MoqError::Bind("already listening".into()));
		}
		let server = self
			.config
			.clone()
			.init()
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
#[derive(uniffi::Object)]
pub struct MoqServer {
	task: Task<ServerState>,
}

#[uniffi::export]
impl MoqServer {
	/// Create a new MoQ server with default configuration.
	#[uniffi::constructor]
	pub fn new() -> Arc<Self> {
		let _guard = crate::ffi::RUNTIME.enter();
		Arc::new(Self {
			task: Task::new(ServerState {
				config: moq_native::ServerConfig::default(),
				publish: None,
				consume: None,
				server: None,
			}),
		})
	}

	/// Set the address to bind, e.g. `127.0.0.1:4443`, `[::]:443`, or `localhost:0`.
	///
	/// Validated syntactically up-front. DNS hostnames are accepted and resolved
	/// at `listen()` time.
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
		if let Some(mut state) = self.task.lock() {
			state.config.bind = Some(addr);
		}
		Ok(())
	}

	/// Load TLS certificate chains from PEM files on disk.
	pub fn set_tls_cert(&self, paths: Vec<String>) {
		if let Some(mut state) = self.task.lock() {
			state.config.tls.cert = paths.into_iter().map(PathBuf::from).collect();
		}
	}

	/// Load TLS private keys from PEM files on disk.
	pub fn set_tls_key(&self, paths: Vec<String>) {
		if let Some(mut state) = self.task.lock() {
			state.config.tls.key = paths.into_iter().map(PathBuf::from).collect();
		}
	}

	/// Generate self-signed TLS certificates for the given hostnames.
	///
	/// Clients must either pin the certificate fingerprint or disable verification.
	pub fn set_tls_generate(&self, hostnames: Vec<String>) {
		if let Some(mut state) = self.task.lock() {
			state.config.tls.generate = hostnames;
		}
	}

	/// Set the origin to publish broadcasts to incoming sessions.
	pub fn set_publish(&self, origin: Option<Arc<MoqOriginProducer>>) {
		if let Some(mut state) = self.task.lock() {
			state.publish = origin;
		}
	}

	/// Set the origin to consume broadcasts from incoming sessions.
	pub fn set_consume(&self, origin: Option<Arc<MoqOriginProducer>>) {
		if let Some(mut state) = self.task.lock() {
			state.consume = origin;
		}
	}

	/// Bind the listening socket. Returns the bound local address as a string,
	/// which is useful when binding to an ephemeral port (`:0`).
	pub async fn listen(&self) -> Result<String, MoqError> {
		self.task.run(|mut state| async move { state.listen().await }).await
	}

	/// Accept the next incoming session. Returns `None` when the server has closed.
	///
	/// `listen()` must be called first.
	pub async fn accept(&self) -> Result<Option<Arc<MoqRequest>>, MoqError> {
		self.task.run(|mut state| async move { state.accept().await }).await
	}

	/// SHA-256 fingerprints of the configured TLS certificates, hex-encoded.
	///
	/// Useful for pinning a generated self-signed certificate in a browser via
	/// WebTransport's `serverCertificateHashes`. Returns an error if called
	/// before `listen()`.
	pub fn cert_fingerprints(&self) -> Result<Vec<String>, MoqError> {
		let state = self
			.task
			.lock()
			.ok_or_else(|| MoqError::Bind("server is busy".into()))?;
		let server = state
			.server
			.as_ref()
			.ok_or_else(|| MoqError::Bind("not listening; call listen() first".into()))?;
		Ok(server.certificates().fingerprints())
	}

	/// Cancel any in-flight `listen()` or `accept()` call.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}

struct RequestState {
	request: Option<moq_native::Request>,
	publish: Option<Arc<MoqOriginProducer>>,
	consume: Option<Arc<MoqOriginProducer>>,
}

/// An incoming MoQ session that can be accepted or rejected.
#[derive(uniffi::Object)]
pub struct MoqRequest {
	task: Task<RequestState>,
	transport: String,
	url: Option<String>,
}

impl MoqRequest {
	fn new(
		request: moq_native::Request,
		publish: Option<Arc<MoqOriginProducer>>,
		consume: Option<Arc<MoqOriginProducer>>,
	) -> Arc<Self> {
		let transport = request.transport().to_string();
		let url = request.url().map(|u| u.to_string());
		Arc::new(Self {
			task: Task::new(RequestState {
				request: Some(request),
				publish,
				consume,
			}),
			transport,
			url,
		})
	}
}

#[uniffi::export]
impl MoqRequest {
	/// The URL provided by the client, if any.
	pub fn url(&self) -> Option<String> {
		self.url.clone()
	}

	/// The transport type, e.g. `"quic"`, `"iroh"`, or `"websocket"`.
	pub fn transport(&self) -> String {
		self.transport.clone()
	}

	/// Override the publish origin for this session. Falls back to the server's
	/// configured publish origin if unset.
	pub fn set_publish(&self, origin: Option<Arc<MoqOriginProducer>>) {
		if let Some(mut state) = self.task.lock() {
			state.publish = origin;
		}
	}

	/// Override the consume origin for this session. Falls back to the server's
	/// configured consume origin if unset.
	pub fn set_consume(&self, origin: Option<Arc<MoqOriginProducer>>) {
		if let Some(mut state) = self.task.lock() {
			state.consume = origin;
		}
	}

	/// Complete the MoQ handshake and return the established session.
	///
	/// Returns `AlreadyResponded` if `ok()` or `close()` has already been called.
	pub async fn ok(&self) -> Result<Arc<MoqSession>, MoqError> {
		self.task
			.run(|mut state| async move {
				let request = state.request.take().ok_or(MoqError::AlreadyResponded)?;
				// Materialize both origin sides (reusing the caller's if wired) so the
				// session can publish/subscribe and the FFI can hand back a publisher/consumer.
				let publish = state
					.publish
					.as_ref()
					.map(|p| p.inner().clone())
					.unwrap_or_else(|| moq_net::Origin::random().produce());
				let subscribe = state
					.consume
					.as_ref()
					.map(|p| p.inner().clone())
					.unwrap_or_else(|| moq_net::Origin::random().produce());
				let session = request
					.with_publisher(&publish)
					.with_subscriber(subscribe.clone())
					.ok()
					.await
					.map_err(|err| MoqError::Connect(format!("{err}")))?;
				Ok(Arc::new(MoqSession::new(session, publish, subscribe)))
			})
			.await
	}

	/// Reject the session with the given HTTP status code.
	///
	/// Returns `AlreadyResponded` if `ok()` or `close()` has already been called.
	pub async fn close(&self, code: u16) -> Result<(), MoqError> {
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

	/// Cancel any in-flight `ok()` or `close()` call.
	pub fn cancel(&self) {
		self.task.cancel();
	}
}
