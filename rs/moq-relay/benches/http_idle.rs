//! Request accounting cost swept over connections and other requests in flight.
//!
//! Run with `cargo bench -p moq-relay --bench http_idle`.

#[path = "../src/listener/idle.rs"]
mod idle;

use axum::{
	body::Body,
	http::{Request, Response},
};
use axum_server::accept::{Accept, DefaultAcceptor};
use std::{
	convert::Infallible,
	fmt,
	future::{Ready, ready},
	pin::Pin,
	task::{Context, Poll},
};
use swole::bench;
use tokio::io::{AsyncWrite, DuplexStream};
use tower_service::Service;

#[derive(Clone)]
struct Respond;
impl Service<Request<()>> for Respond {
	type Response = Response<Body>;
	type Error = Infallible;
	type Future = Ready<Result<Self::Response, Self::Error>>;
	fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		Poll::Ready(Ok(()))
	}
	fn call(&mut self, request: Request<()>) -> Self::Future {
		let body = if request.uri().path() == "/held" {
			Body::from_stream(futures::stream::pending::<Result<bytes::Bytes, Infallible>>())
		} else {
			Body::empty()
		};
		ready(Ok(Response::new(body)))
	}
}

fn runtime() -> tokio::runtime::Runtime {
	tokio::runtime::Builder::new_current_thread()
		.enable_time()
		.build()
		.unwrap()
}

/// One connection's write side and the service answering it, without request accounting.
struct Baseline {
	io: DuplexStream,
	_peer: DuplexStream,
	service: Respond,
	runtime: tokio::runtime::Runtime,
}

fn baseline() -> Baseline {
	let (io, peer) = tokio::io::duplex(1024);
	Baseline {
		io,
		_peer: peer,
		service: Respond,
		runtime: runtime(),
	}
}

#[bench(setup = baseline)]
fn http_idle_baseline(baseline: &mut Baseline) -> Response<Body> {
	let Baseline {
		io, service, runtime, ..
	} = baseline;
	runtime.block_on(async {
		let mut cx = Context::from_waker(std::task::Waker::noop());
		assert!(matches!(Pin::new(io).poll_write(&mut cx, &[]), Poll::Ready(Ok(0))));
		service.call(Request::new(())).await.unwrap()
	})
}

/// Connections and the requests each holds open, named `connections=<connections>/<held>`.
#[derive(Clone, Copy)]
struct Held {
	connections: usize,
	held: usize,
}

impl fmt::Display for Held {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "connections={}/{}", self.connections, self.held)
	}
}

fn held() -> Vec<Held> {
	[1, 16, 256]
		.into_iter()
		.flat_map(|connections| [0, 16, 256].map(|held| Held { connections, held }))
		.collect()
}

type Client = (idle::Io<DuplexStream>, idle::Requests<Respond>);

/// Accepted connections, each holding its requests open, served round-robin.
struct Idle {
	clients: Vec<Client>,
	_peers: Vec<DuplexStream>,
	_responses: Vec<Response<Body>>,
	next: usize,
	runtime: tokio::runtime::Runtime,
}

fn idle(held: Held) -> Idle {
	let runtime = runtime();
	let (clients, peers, responses) = runtime.block_on(async {
		let acceptor = idle::Acceptor {
			inner: DefaultAcceptor::new(),
			timeout: Some(std::time::Duration::from_secs(30)),
		};
		let mut clients = Vec::new();
		let mut peers = Vec::new();
		let mut responses = Vec::new();
		for _ in 0..held.connections {
			let (io, peer) = tokio::io::duplex(1024);
			let (io, mut service) = acceptor.accept(io, Respond).await.unwrap();
			for _ in 0..held.held {
				responses.push(
					service
						.call(Request::builder().uri("/held").body(()).unwrap())
						.await
						.unwrap(),
				);
			}
			clients.push((io, service));
			peers.push(peer);
		}
		(clients, peers, responses)
	});
	Idle {
		clients,
		_peers: peers,
		_responses: responses,
		next: 0,
		runtime,
	}
}

#[bench(args = held(), setup = idle)]
fn http_idle(idle: &mut Idle) -> Response<Body> {
	let (io, service) = &mut idle.clients[idle.next];
	let response = idle.runtime.block_on(async {
		let mut cx = Context::from_waker(std::task::Waker::noop());
		assert!(matches!(Pin::new(io).poll_write(&mut cx, &[]), Poll::Ready(Ok(0))));
		service.call(Request::new(())).await.unwrap()
	});
	idle.next = (idle.next + 1) % idle.clients.len();
	response
}

// The drain's shared counter must stay independent of the other connections
// and handshakes it already holds. Compare handler-only admission with the
// accept/first-header transfer, including a WebSocket's second transfer.
mod http_admission {
	use std::fmt;
	use std::hint::black_box;

	use swole::bench;

	use super::idle;

	/// Connections already admitted and handshakes pending, named
	/// `connections=<connections>/pending=<pending>`.
	#[derive(Clone, Copy)]
	struct Shape {
		connections: usize,
		pending: usize,
	}

	impl fmt::Display for Shape {
		fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
			write!(f, "connections={}/pending={}", self.connections, self.pending)
		}
	}

	fn shapes() -> Vec<Shape> {
		[1, 16, 256]
			.into_iter()
			.flat_map(|connections| [0, 16, 256].map(|pending| Shape { connections, pending }))
			.collect()
	}

	/// The drain's admission with every connection and handshake already entered.
	struct Admission {
		admission: moq_net::server::Admission,
		_held: Vec<moq_net::server::Admitted>,
	}

	fn admission(shape: Shape) -> Admission {
		let admission = moq_net::server::Admission::default();
		let held = (0..shape.connections + shape.pending)
			.map(|_| admission.enter().unwrap())
			.collect();
		Admission { admission, _held: held }
	}

	// Each drops what it admitted inside the call: leaving releases the drain's counter,
	// which is half of the accounting being compared.
	#[bench(args = shapes(), setup = admission)]
	fn handler(admission: &Admission) {
		drop(black_box(admission.admission.enter().unwrap()));
	}

	#[bench(args = shapes(), setup = admission)]
	fn plain(admission: &Admission) {
		let accepted = idle::Admission::new(admission.admission.enter().unwrap());
		drop(black_box(accepted.take().unwrap()));
	}

	#[bench(args = shapes(), setup = admission)]
	fn upgrade(admission: &Admission) {
		let accepted = idle::Admission::new(admission.admission.enter().unwrap());
		let request = idle::Admission::new(accepted.take().unwrap());
		drop(black_box(request.take().unwrap()));
	}
}

fn main() {
	swole::main();
}
