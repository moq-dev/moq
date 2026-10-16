//! AUTH admission bursts across sessions and tokens per session, without sockets.
//!
//! Measures the pending queue and complete grant/withdrawal lifecycle. Run the same
//! matrix before and after queue changes so a per-session scan appears on its own axis.

#[path = "../tests/support/mod.rs"]
mod support;

use bytes::Bytes;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use futures::future::join_all;
use moq_net::{Session, Version, auth};
use std::time::Duration;
use support::{harness, mock::create_mock_session_pair};

struct Pair {
	client: Session,
	_server: Session,
	requests: auth::Requests,
	_initial: auth::Issued,
}

async fn pair(version: Version) -> Pair {
	let mut mock = moq_net::transport::memory::Config::default();
	mock.protocol = Some(version.alpn().to_string());
	let (client, server) = create_mock_session_pair(mock);
	let client = async {
		let (session, driver) = moq_net::Client::new()
			.with_versions(version.into())
			.connect(harness::now(), client)
			.await
			.unwrap();
		harness::spawn(driver);
		session
	};
	let server = async {
		let request = moq_net::Server::new()
			.with_versions(version.into())
			.accept_request(harness::now(), server)
			.await
			.unwrap();
		let requests = request.auth().requests().unwrap();
		let (session, driver) = request.ok().await.unwrap();
		harness::spawn(driver);
		(session, requests)
	};
	let (client, (server, mut requests)) = futures::join!(client, server);
	let initial = requests.next().await.unwrap().accept(auth::Grant::all());
	Pair {
		client,
		_server: server,
		requests,
		_initial: initial,
	}
}

impl Pair {
	async fn round(&mut self, count: usize) {
		let handle = self.client.auth();
		let present = join_all((0..count).map(|_| handle.add(Bytes::from_static(b"token"))));
		let accept = async {
			let mut issued = Vec::with_capacity(count);
			for _ in 0..count {
				issued.push(self.requests.next().await.unwrap().accept(auth::Grant::all()));
			}
			issued
		};
		let (tokens, issued) = futures::join!(present, accept);
		let tokens: Vec<_> = tokens.into_iter().map(Result::unwrap).collect();
		drop(tokens);
		join_all(issued.iter().map(auth::Issued::closed)).await;
	}
}

fn admission(c: &mut Criterion) {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	for version in ["moq-lite-07-wip", "moq-transport-22"] {
		let mut group = c.benchmark_group(format!("auth/{version}"));
		group.sample_size(10);
		group.warm_up_time(Duration::from_millis(100));
		group.measurement_time(Duration::from_millis(300));
		for sessions in [1, 16, 64] {
			for tokens in [1, 16, 128] {
				group.throughput(Throughput::Elements((sessions * tokens) as u64));
				group.bench_with_input(
					BenchmarkId::new(sessions.to_string(), tokens),
					&(sessions, tokens),
					|b, &(sessions, tokens)| {
						let mut pairs =
							runtime.block_on(join_all((0..sessions).map(|_| pair(version.parse().unwrap()))));
						b.iter(|| runtime.block_on(join_all(pairs.iter_mut().map(|pair| pair.round(tokens)))));
					},
				);
			}
		}
		group.finish();
	}
}

criterion_group!(benches, admission);
criterion_main!(benches);
