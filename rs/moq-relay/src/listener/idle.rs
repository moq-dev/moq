//! A connection is idle until complete request headers arrive, regardless of PINGs.

use axum::{
	body::Body,
	http::{Method, Request, Response, StatusCode},
};
use axum_server::accept::Accept;
use futures::{future::BoxFuture, task::AtomicWaker};
use http_body::Body as _;
use moq_clock::{Instant, Sleep};
use std::{
	io,
	pin::Pin,
	sync::{Arc, Mutex, PoisonError},
	task::{Context, Poll},
	time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tower_service::Service;

/// A connection's accept guard, handed off once to a WebSocket upgrade.
#[derive(Clone)]
pub(crate) struct Admission(Arc<Mutex<Option<moq_net::server::Active>>>);

impl Admission {
	pub(crate) fn new(active: moq_net::server::Active) -> Self {
		Self(Arc::new(Mutex::new(Some(active))))
	}

	pub(crate) fn take(&self) -> Option<moq_net::server::Active> {
		self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
	}
}

#[derive(Clone)]
pub(crate) struct Acceptor<A> {
	pub inner: A,
	pub timeout: Option<Duration>,
}

impl<I, S, A> Accept<I, S> for Acceptor<A>
where
	A: Accept<I, S>,
	A::Future: Send + 'static,
	A::Stream: Send + 'static,
	A::Service: Send + 'static,
{
	type Stream = Io<A::Stream>;
	type Service = Requests<A::Service>;
	type Future = BoxFuture<'static, io::Result<(Self::Stream, Self::Service)>>;
	fn accept(&self, stream: I, service: S) -> Self::Future {
		let accepted = self.inner.accept(stream, service);
		let timeout = self.timeout;
		// The clock starts at accept, so a peer that stalls the inner (TLS) handshake
		// is bounded by the same deadline as one that stalls its first request.
		let since = Instant::now();
		Box::pin(async move {
			let (inner, service) = match timeout.and_then(|timeout| since.checked_add(timeout)) {
				Some(deadline) => moq_clock::timeout_at(deadline, accepted)
					.await
					.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "HTTP connection idle deadline"))??,
				None => accepted.await?,
			};
			let state = Arc::new(State {
				timeout,
				activity: Mutex::new(Activity {
					active: 0,
					since,
					upgraded: false,
				}),
				wake: AtomicWaker::new(),
			});
			let timer = Box::pin(moq_clock::sleep(Duration::ZERO));
			Ok((
				Io {
					inner,
					state: state.clone(),
					timer,
				},
				Requests { inner: service, state },
			))
		})
	}
}

struct Activity {
	active: usize,
	since: Instant,
	upgraded: bool,
}
struct State {
	timeout: Option<Duration>,
	activity: Mutex<Activity>,
	wake: AtomicWaker,
}

struct Active(Arc<State>);
impl Active {
	fn new(state: Arc<State>) -> Self {
		state.activity.lock().unwrap_or_else(PoisonError::into_inner).active += 1;
		state.wake.wake();
		Self(state)
	}
}
impl Drop for Active {
	fn drop(&mut self) {
		let mut activity = self.0.activity.lock().unwrap_or_else(PoisonError::into_inner);
		activity.active -= 1;
		if activity.active == 0 {
			activity.since = Instant::now();
		}
		drop(activity);
		self.0.wake.wake();
	}
}

#[derive(Clone)]
pub(crate) struct Requests<S> {
	inner: S,
	state: Arc<State>,
}
impl<S, B> Service<Request<B>> for Requests<S>
where
	S: Service<Request<B>, Response = Response<Body>>,
	S::Future: Send + 'static,
	S::Error: 'static,
{
	type Response = Response<Body>;
	type Error = S::Error;
	type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;
	fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		self.inner.poll_ready(cx)
	}
	fn call(&mut self, request: Request<B>) -> Self::Future {
		let connect = request.method() == Method::CONNECT && request.version() != axum::http::Version::HTTP_2;
		let active = Active::new(self.state.clone());
		let response = self.inner.call(request);
		Box::pin(async move {
			let response = response.await?;
			// Once HTTP hands its stream to another protocol, that protocol owns its lifetime.
			if response.status() == StatusCode::SWITCHING_PROTOCOLS || (connect && response.status().is_success()) {
				active
					.0
					.activity
					.lock()
					.unwrap_or_else(PoisonError::into_inner)
					.upgraded = true;
			}
			Ok(response.map(|body| {
				let active = (!body.is_end_stream()).then_some(active);
				Body::new(ResponseBody { body, active })
			}))
		})
	}
}

struct ResponseBody {
	body: Body,
	active: Option<Active>,
}
impl http_body::Body for ResponseBody {
	type Data = bytes::Bytes;
	type Error = axum::Error;
	fn poll_frame(
		mut self: Pin<&mut Self>,
		cx: &mut Context<'_>,
	) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
		let frame = Pin::new(&mut self.body).poll_frame(cx);
		if matches!(frame, Poll::Ready(None | Some(Err(_)))) || self.body.is_end_stream() {
			self.active = None;
		}
		frame
	}
	fn is_end_stream(&self) -> bool {
		self.body.is_end_stream()
	}
	fn size_hint(&self) -> http_body::SizeHint {
		self.body.size_hint()
	}
}

pub(crate) struct Io<I> {
	inner: I,
	state: Arc<State>,
	timer: Pin<Box<Sleep>>,
}
impl<I> Io<I> {
	fn poll_deadline(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
		self.state.wake.register(cx.waker());
		let activity = self.state.activity.lock().unwrap_or_else(PoisonError::into_inner);
		let at = self
			.state
			.timeout
			.filter(|_| activity.active == 0 && !activity.upgraded)
			.and_then(|timeout| activity.since.checked_add(timeout));
		drop(activity);
		if let Some(at) = at {
			self.timer.as_mut().reset(at);
			if self.timer.as_mut().poll(cx).is_ready() {
				return Err(io::Error::new(io::ErrorKind::TimedOut, "HTTP connection idle deadline"));
			}
		}
		Ok(())
	}
}
impl<I: AsyncRead + Unpin> AsyncRead for Io<I> {
	fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
		self.poll_deadline(cx)?;
		Pin::new(&mut self.inner).poll_read(cx, buf)
	}
}
impl<I: AsyncWrite + Unpin> AsyncWrite for Io<I> {
	fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
		self.poll_deadline(cx)?;
		Pin::new(&mut self.inner).poll_write(cx, buf)
	}
	fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		self.poll_deadline(cx)?;
		Pin::new(&mut self.inner).poll_flush(cx)
	}
	fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		Pin::new(&mut self.inner).poll_shutdown(cx)
	}
}

#[cfg(test)]
#[allow(
	dead_code,
	unused_imports,
	reason = "The benchmark includes this private module without a test harness"
)]
mod tests {
	use super::*;
	use tokio::io::{AsyncReadExt, AsyncWriteExt};

	const TIMEOUT: Duration = Duration::from_secs(10);

	async fn pair(timeout: Option<Duration>) -> (Io<tokio::io::DuplexStream>, tokio::io::DuplexStream, Arc<State>) {
		let (server, client) = tokio::io::duplex(1024);
		let acceptor = Acceptor {
			inner: axum_server::accept::DefaultAcceptor::new(),
			timeout,
		};
		let (io, service) = acceptor.accept(server, ()).await.unwrap();
		(io, client, service.state)
	}

	/// An inner acceptor whose handshake never completes, like a TLS peer that
	/// connects and never sends a ClientHello.
	#[derive(Clone)]
	struct Stalled;
	impl<I, S> Accept<I, S> for Stalled {
		type Stream = I;
		type Service = S;
		type Future = std::future::Pending<io::Result<(I, S)>>;
		fn accept(&self, _stream: I, _service: S) -> Self::Future {
			std::future::pending()
		}
	}

	#[tokio::test(start_paused = true)]
	async fn a_stalled_handshake_counts_toward_idle() {
		let (server, _client) = tokio::io::duplex(1024);
		let acceptor = Acceptor {
			inner: Stalled,
			timeout: Some(TIMEOUT),
		};
		let start = Instant::now();
		let result = moq_clock::timeout(TIMEOUT * 2, acceptor.accept(server, ()))
			.await
			.expect("stalled handshake survived its deadline");
		assert_eq!(result.err().unwrap().kind(), io::ErrorKind::TimedOut);
		assert_eq!(start.elapsed(), TIMEOUT);
	}

	#[tokio::test(start_paused = true)]
	async fn incoming_activity_does_not_extend_an_incomplete_request() {
		let (mut io, mut client, _) = pair(Some(TIMEOUT)).await;
		for _ in 0..9 {
			client.write_all(b"x").await.unwrap();
			assert_eq!(io.read_u8().await.unwrap(), b'x');
			tokio::time::advance(Duration::from_secs(1)).await;
		}
		tokio::time::advance(Duration::from_secs(1)).await;
		assert_eq!(io.read_u8().await.unwrap_err().kind(), io::ErrorKind::TimedOut);
	}

	#[tokio::test(start_paused = true)]
	async fn active_response_defers_idle_until_its_body_ends() {
		let (mut io, _client, state) = pair(Some(TIMEOUT)).await;
		let active = Active::new(state);
		let mut body = ResponseBody {
			body: Body::from("response"),
			active: Some(active),
		};
		tokio::time::advance(TIMEOUT * 2).await;
		assert!(
			io.poll_deadline(&mut Context::from_waker(std::task::Waker::noop()))
				.is_ok()
		);
		std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx))
			.await
			.unwrap()
			.unwrap();
		assert!(
			io.poll_deadline(&mut Context::from_waker(std::task::Waker::noop()))
				.is_ok()
		);
		tokio::time::advance(TIMEOUT).await;
		assert_eq!(io.read_u8().await.unwrap_err().kind(), io::ErrorKind::TimedOut);
	}

	#[tokio::test(start_paused = true)]
	async fn disabled_and_upgraded_connections_have_no_idle_deadline() {
		for upgraded in [false, true] {
			let (mut io, _client, state) = pair(upgraded.then_some(TIMEOUT)).await;
			state.activity.lock().unwrap().upgraded = upgraded;
			tokio::time::advance(TIMEOUT * 2).await;
			assert!(
				io.poll_deadline(&mut Context::from_waker(std::task::Waker::noop()))
					.is_ok()
			);
		}
	}
	#[tokio::test(start_paused = true)]
	async fn http2_pings_and_partial_headers_do_not_extend_idle() {
		for mode in [0, 1, 2] {
			let trickle = mode == 1;
			let active = mode == 2;
			let (server, mut client) = tokio::io::duplex(4096);
			let acceptor = Acceptor {
				inner: axum_server::accept::DefaultAcceptor::new(),
				timeout: Some(TIMEOUT),
			};
			let (started_tx, started_rx) = tokio::sync::oneshot::channel();
			let started = Arc::new(Mutex::new(Some(started_tx)));
			let router = axum::Router::new().route(
				"/",
				axum::routing::get(move || {
					let started = started.clone();
					async move {
						started.lock().unwrap().take().unwrap().send(()).unwrap();
						std::future::pending::<Response<Body>>().await
					}
				}),
			);
			let (io, service) = acceptor.accept(server, router).await.unwrap();
			let serving = tokio::spawn(async move {
				let mut builder =
					hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new()).http2_only();
				builder
					.http2()
					.timer(hyper_util::rt::TokioTimer::new())
					.keep_alive_interval((!trickle).then_some(Duration::from_secs(2)))
					.keep_alive_timeout(Duration::from_secs(2));
				builder
					.serve_connection(
						hyper_util::rt::TokioIo::new(io),
						hyper_util::service::TowerToHyperService::new(service),
					)
					.await
			});
			client
				.write_all(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n\0\0\0\x04\0\0\0\0\0")
				.await
				.unwrap();
			if trickle {
				// A HEADERS frame announcing 100 bytes but sending only one never
				// completes a request, even though the connection is receiving data.
				client.write_all(&[0, 0, 100, 1, 1, 0, 0, 0, 1, 0x82]).await.unwrap();
			}
			if active {
				// Indexed GET, http, and /, followed by a literal :authority.
				let headers = [
					0x82, 0x86, 0x84, 0x01, 9, b'l', b'o', b'c', b'a', b'l', b'h', b'o', b's', b't',
				];
				client
					.write_all(&[0, 0, headers.len() as u8, 1, 5, 0, 0, 0, 1])
					.await
					.unwrap();
				client.write_all(&headers).await.unwrap();
			}
			let pings = Arc::new(std::sync::atomic::AtomicUsize::new(0));
			let observed = pings.clone();
			let peer = tokio::spawn(async move {
				loop {
					let mut header = [0u8; 9];
					if client.read_exact(&mut header).await.is_err() {
						break;
					}
					let len = (usize::from(header[0]) << 16) | (usize::from(header[1]) << 8) | usize::from(header[2]);
					let mut payload = vec![0; len];
					client.read_exact(&mut payload).await.unwrap();
					if header[3] == 4 && header[4] == 0 && !trickle {
						client.write_all(&[0, 0, 0, 4, 1, 0, 0, 0, 0]).await.unwrap();
					}
					if header[3] == 6 && header[4] == 0 {
						observed.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
						header[4] = 1;
						if client.write_all(&header).await.is_err() || client.write_all(&payload).await.is_err() {
							break;
						}
					}
				}
			});
			if active {
				started_rx.await.unwrap();
				tokio::time::advance(TIMEOUT * 2).await;
				tokio::task::yield_now().await;
				assert!(!serving.is_finished(), "active HTTP/2 request was dropped");
				peer.abort();
				let _ = peer.await;
				serving.abort();
				let _ = serving.await;
				continue;
			}
			let start = Instant::now();
			let result = moq_clock::timeout(TIMEOUT * 2, serving)
				.await
				.expect("idle HTTP/2 connection survived its deadline")
				.unwrap();
			assert!(result.is_err(), "idle HTTP/2 connection succeeded");
			assert_eq!(start.elapsed(), TIMEOUT);
			if !trickle {
				assert!(
					pings.load(std::sync::atomic::Ordering::Relaxed) > 0,
					"client did not ACK a PING"
				);
			}
			peer.await.unwrap();
		}
	}
}
