//! Request accounting cost swept over connections and other requests in flight.

#[path = "../src/listener/idle.rs"]
mod idle;

use axum::{
	body::Body,
	http::{Request, Response},
};
use axum_server::accept::{Accept, DefaultAcceptor};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use std::{
	convert::Infallible,
	future::{Ready, ready},
	pin::Pin,
	task::{Context, Poll},
};
use tokio::io::AsyncWrite;
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

fn requests(c: &mut Criterion) {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_time()
		.build()
		.unwrap();
	let (mut baseline_io, _baseline_peer) = tokio::io::duplex(1024);
	let mut baseline_service = Respond;
	c.bench_function("http_idle_baseline", |b| {
		b.iter(|| {
			runtime.block_on(async {
				let mut cx = Context::from_waker(std::task::Waker::noop());
				assert!(matches!(
					Pin::new(&mut baseline_io).poll_write(&mut cx, &[]),
					Poll::Ready(Ok(0))
				));
				std::hint::black_box(baseline_service.call(Request::new(())).await.unwrap());
			})
		})
	});

	let mut group = c.benchmark_group("http_idle");
	for connections in [1, 16, 256] {
		for held in [0, 16, 256] {
			let (mut clients, _peers, _responses) = runtime.block_on(async {
				let acceptor = idle::Acceptor {
					inner: DefaultAcceptor::new(),
					timeout: Some(std::time::Duration::from_secs(30)),
				};
				let mut clients = Vec::new();
				let mut peers = Vec::new();
				let mut responses = Vec::new();
				for _ in 0..connections {
					let (io, peer) = tokio::io::duplex(1024);
					let (io, mut service) = acceptor.accept(io, Respond).await.unwrap();
					for _ in 0..held {
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
			let mut next = 0;
			group.bench_function(BenchmarkId::new(format!("connections={connections}"), held), |b| {
				b.iter(|| {
					runtime.block_on(async {
						let (io, service) = &mut clients[next];
						let mut cx = Context::from_waker(std::task::Waker::noop());
						assert!(matches!(Pin::new(io).poll_write(&mut cx, &[]), Poll::Ready(Ok(0))));
						let response = service.call(Request::new(())).await.unwrap();
						std::hint::black_box(response);
					});
					next = (next + 1) % connections;
				})
			});
		}
	}
	group.finish();
}

// The drain's shared counter must stay independent of the other connections
// and handshakes it already holds. Compare handler-only admission with the
// accept/first-header transfer, including a WebSocket's second transfer.
fn admission(c: &mut Criterion) {
	let mut group = c.benchmark_group("http_admission");
	for connections in [1, 16, 256] {
		for pending in [0, 16, 256] {
			let activity = moq_net::server::Activity::default();
			let _held = (0..connections + pending)
				.map(|_| activity.enter().unwrap())
				.collect::<Vec<_>>();
			let shape = format!("connections={connections}/pending={pending}");
			group.bench_function(BenchmarkId::new("handler", &shape), |b| {
				b.iter(|| std::hint::black_box(activity.enter().unwrap()));
			});
			group.bench_function(BenchmarkId::new("plain", &shape), |b| {
				b.iter(|| {
					let accepted = idle::Admission::new(activity.enter().unwrap());
					drop(std::hint::black_box(accepted.take().unwrap()));
				});
			});
			group.bench_function(BenchmarkId::new("upgrade", &shape), |b| {
				b.iter(|| {
					let accepted = idle::Admission::new(activity.enter().unwrap());
					let request = idle::Admission::new(accepted.take().unwrap());
					drop(std::hint::black_box(request.take().unwrap()));
				});
			});
		}
	}
	group.finish();
}
criterion_group!(benches, requests, admission);
criterion_main!(benches);
