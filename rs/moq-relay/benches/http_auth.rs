//! One public HTTP read's admission, swept over connections and the HTTP/2 streams each
//! holds waiting on the decider. A slope means admitting one request costs more as others
//! wait, rather than what the request itself touches.
//! The real web listener runs over loopback TCP; timings include one request round trip.
#![expect(clippy::disallowed_methods, reason = "host time measures benchmark throughput")]

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use moq_auth::{Grant, lease};
use moq_relay::{auth, cluster, web};
use std::sync::{
	Arc,
	atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

type Send = h2::client::SendRequest<bytes::Bytes>;

struct Fixture {
	measured: Send,
	_held: Vec<(Send, Vec<h2::client::ResponseFuture>)>,
	tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for Fixture {
	fn drop(&mut self) {
		for task in &self.tasks {
			task.abort();
		}
	}
}

impl Fixture {
	async fn new(connections: usize, streams: usize) -> Self {
		let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
		let (auth, mut admissions) = auth::Auth::embedded("bench");
		let waiting = Arc::new(AtomicUsize::new(0));
		let decider = tokio::spawn({
			let waiting = waiting.clone();
			async move {
				let mut held = Vec::new();
				while let Some(admission) = admissions.next().await {
					if admission.request.path.starts_with("/held") {
						held.push(admission);
						waiting.store(held.len(), Ordering::Release);
						continue;
					}
					let grant = Grant::new(moq_auth::Patterns::new(), moq_auth::Pattern::all());
					admission.grant(lease::Consumer::fixed(grant));
				}
			}
		});

		let pem = rcgen::generate_simple_self_signed(vec!["localhost".into()])
			.unwrap()
			.cert
			.pem();
		let certificates = moq_tokio::tls::Certificates::from_pem(pem.as_bytes()).unwrap();
		let mut config = web::Config::default();
		config.http.listen = Some("127.0.0.1:0".parse().unwrap());
		let cluster = cluster::Cluster::new(Default::default()).unwrap();
		// No admission timeout, so held requests stay waiting however long setup and the
		// measurement take.
		let web = web::Web::new(auth, cluster, certificates, config)
			.with_timeout(None)
			.bind()
			.unwrap();
		let addr = web.addrs().http.unwrap();
		let routes = web.routes();
		let server = tokio::spawn(async move {
			let _ = web.serve(routes).await;
		});

		let mut tasks = vec![decider, server];
		let mut held = Vec::new();
		for connection in 0..connections {
			let mut send = connect(addr, &mut tasks).await;
			let mut responses = Vec::new();
			for _ in 0..streams {
				send = send.ready().await.unwrap();
				let (response, _) = send.send_request(get(&format!("held{connection}")), true).unwrap();
				responses.push(response);
			}
			held.push((send, responses));
		}
		let measured = connect(addr, &mut tasks).await;
		while waiting.load(Ordering::Acquire) < connections * streams {
			tokio::time::sleep(Duration::from_millis(1)).await;
		}

		Self {
			measured,
			_held: held,
			tasks,
		}
	}

	async fn round(&mut self) {
		let mut send = self.measured.clone().ready().await.unwrap();
		let (response, _) = send.send_request(get("bench"), true).unwrap();
		let mut body = response.await.unwrap().into_body();
		while let Some(chunk) = body.data().await {
			let chunk = chunk.unwrap();
			body.flow_control().release_capacity(chunk.len()).unwrap();
		}
	}
}

async fn connect(addr: std::net::SocketAddr, tasks: &mut Vec<tokio::task::JoinHandle<()>>) -> Send {
	let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
	let (send, connection) = h2::client::handshake(tcp).await.unwrap();
	tasks.push(tokio::spawn(async move {
		let _ = connection.await;
	}));
	send
}

fn get(prefix: &str) -> axum::http::Request<()> {
	axum::http::Request::get(format!("http://localhost/announced/{prefix}"))
		.body(())
		.unwrap()
}

fn admission(c: &mut Criterion) {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let mut group = c.benchmark_group("relay_http_admission");
	group.sample_size(10);
	group.warm_up_time(Duration::from_millis(100));
	group.measurement_time(Duration::from_millis(300));
	// Each connection may hold up to the verification budget (128 by default) at once.
	for connections in [1, 16, 64] {
		for streams in [0, 16, 64] {
			let mut fixture = runtime.block_on(Fixture::new(connections, streams));
			group.bench_with_input(
				BenchmarkId::new(format!("connections={connections}"), streams),
				&(connections, streams),
				|b, _| {
					b.iter(|| runtime.block_on(fixture.round()));
				},
			);
		}
	}
	group.finish();
}

criterion_group!(benches, admission);
criterion_main!(benches);
