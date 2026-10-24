//! Revalidate one token while sweeping unrelated sessions and held credentials, and
//! drain token batches while sweeping offered concurrency and payload size.
//! The real relay/controller runs over loopback TCP; timings include one AUTH round trip.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use moq_auth::{Grant, lease};
use moq_relay::{Connection, auth, cluster};
use std::time::Duration;

fn grant(prefix: &str) -> Grant {
	Grant::new(
		format!("{prefix}/**").parse::<moq_auth::Pattern>().unwrap(),
		moq_auth::Patterns::new(),
	)
}

struct Fixture {
	_connections: Vec<moq_tokio::Connection>,
	_tokens: Vec<moq_tokio::connection::auth::Auth>,
	producers: Vec<lease::Producer>,
	selected: usize,
	watch: moq_tokio::connection::auth::Watch,
	tasks: Vec<tokio::task::JoinHandle<()>>,
	alternate: bool,
}

impl Drop for Fixture {
	fn drop(&mut self) {
		for task in &self.tasks {
			task.abort();
		}
	}
}

impl Fixture {
	async fn new(sessions: usize, tokens: usize) -> Self {
		let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
		let (auth, mut admissions) = auth::Auth::embedded("bench");
		let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
		let decider = tokio::spawn(async move {
			while let Some(admission) = admissions.next().await {
				let prefix = admission
					.request
					.query
					.as_deref()
					.unwrap_or("initial")
					.strip_prefix("jwt=")
					.unwrap_or("initial")
					.to_owned();
				let (producer, consumer) = lease::Producer::new(grant(&prefix));
				admission.grant(consumer);
				if send.send((prefix, producer)).is_err() {
					break;
				}
			}
		});
		let mut config = moq_tokio::listen::Config::default();
		config.tcp.bind = Some("127.0.0.1:0".parse().unwrap());
		let mut server = config.init(Default::default()).unwrap().listen().await.unwrap();
		let port = server.tcp_local_addr().unwrap().port();
		let cluster = cluster::Cluster::new(Default::default()).unwrap();
		let listener = tokio::spawn(async move {
			let mut serving = tokio::task::JoinSet::new();
			while let Some(request) = server.accept().await {
				let connection = Connection::new(request, cluster.clone(), auth.clone());
				serving.spawn(async move {
					connection.run().await;
				});
			}
		});
		let mut config = moq_tokio::connect::Config::default();
		config.version = vec!["moq-transport-22".parse().unwrap()];
		let client = config.init(Default::default()).unwrap().with_reconnect(false);
		let url: url::Url = format!("tcp://127.0.0.1:{port}/").parse().unwrap();
		let mut connections = Vec::new();
		let mut retained = Vec::new();
		let mut producers = Vec::new();
		let mut selected = 0;
		let mut watch = None;
		for session in 0..sessions {
			let connection = client.clone().connect(url.clone()).established().await.unwrap();
			producers.push(receive.recv().await.unwrap().1);
			let auth = connection.auth();
			let added = futures::future::join_all((0..tokens).map(|index| auth.add(format!("token{index}")))).await;
			let added: Vec<_> = added.into_iter().map(Result::unwrap).collect();
			if session == 0 {
				let mut observed = added.last().unwrap().grant();
				observed.changed().await.unwrap();
				watch = Some(observed);
			}
			for _ in 0..tokens {
				let (prefix, producer) = receive.recv().await.unwrap();
				if session == 0 && prefix == format!("token{}", tokens - 1) {
					selected = producers.len();
				}
				producers.push(producer);
			}
			retained.extend(added);
			connections.push(connection);
		}
		Self {
			_connections: connections,
			_tokens: retained,
			producers,
			selected,
			watch: watch.unwrap(),
			tasks: vec![listener, decider],
			alternate: false,
		}
	}

	async fn round(&mut self) {
		self.alternate = !self.alternate;
		let expected = grant(if self.alternate { "changed" } else { "restored" });
		self.producers[self.selected].update(expected.clone());
		let actual = self.watch.changed().await.unwrap().unwrap();
		assert_eq!(actual.publish, expected.publish);
	}
}

fn refresh(c: &mut Criterion) {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let mut group = c.benchmark_group("relay_auth_refresh");
	group.sample_size(10);
	group.warm_up_time(Duration::from_millis(100));
	group.measurement_time(Duration::from_millis(300));
	for sessions in [1, 16, 64] {
		// QMux grants 100 bidirectional streams, including persistent control streams.
		for tokens in [1, 16, 64] {
			let mut fixture = runtime.block_on(Fixture::new(sessions, tokens));
			group.bench_with_input(
				BenchmarkId::new(sessions.to_string(), tokens),
				&(sessions, tokens),
				|b, _| {
					b.iter(|| runtime.block_on(fixture.round()));
				},
			);
		}
	}
	group.finish();
}

/// One real session; each measured batch offers tokens before the decider drains them,
/// so requests beyond the fixed production window wait for a verification slot.
struct Queue {
	_connection: moq_tokio::Connection,
	auth: moq_tokio::connection::auth::Handle,
	admissions: auth::Admissions,
	presented: usize,
	tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for Queue {
	fn drop(&mut self) {
		for task in &self.tasks {
			task.abort();
		}
	}
}

impl Queue {
	async fn new() -> Self {
		let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
		let (auth, mut admissions) = auth::Auth::embedded("bench");
		let mut config = moq_tokio::listen::Config::default();
		config.tcp.bind = Some("127.0.0.1:0".parse().unwrap());
		let mut server = config.init(Default::default()).unwrap().listen().await.unwrap();
		let port = server.tcp_local_addr().unwrap().port();
		let cluster = cluster::Cluster::new(Default::default()).unwrap();
		let listener = tokio::spawn(async move {
			let mut serving = tokio::task::JoinSet::new();
			while let Some(request) = server.accept().await {
				let connection = Connection::new(request, cluster.clone(), auth.clone());
				serving.spawn(async move {
					connection.run().await;
				});
			}
		});
		let mut config = moq_tokio::connect::Config::default();
		config.version = vec!["moq-transport-22".parse().unwrap()];
		let client = config.init(Default::default()).unwrap().with_reconnect(false);
		let url: url::Url = format!("tcp://127.0.0.1:{port}/").parse().unwrap();
		let (connection, ()) = tokio::join!(async { client.connect(url).established().await.unwrap() }, async {
			let admission = admissions.next().await.unwrap();
			admission.grant(lease::Consumer::fixed(grant("initial")));
		});
		Self {
			auth: connection.auth(),
			_connection: connection,
			admissions,
			presented: 0,
			tasks: vec![listener],
		}
	}

	async fn round(&mut self, offered: usize, bytes: usize) {
		let mut tasks = Vec::with_capacity(offered);
		for _ in 0..offered {
			let auth = self.auth.clone();
			let token = format!("{:0>width$}", self.presented, width = bytes);
			self.presented += 1;
			tasks.push(tokio::spawn(async move { auth.add(token).await }));
		}
		for _ in 0..offered {
			self.admissions
				.next()
				.await
				.unwrap()
				.refuse(auth::Error::Refused(Default::default()));
		}
		for task in tasks {
			assert!(task.await.unwrap().is_err());
		}
	}
}

fn queue(c: &mut Criterion) {
	let runtime = tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap();
	let mut group = c.benchmark_group("relay_auth_queue");
	group.sample_size(10);
	group.warm_up_time(Duration::from_millis(100));
	group.measurement_time(Duration::from_millis(300));
	for offered in [1, 128, 256] {
		for bytes in [16, 256, 4096] {
			let mut fixture = runtime.block_on(Queue::new());
			group.throughput(criterion::Throughput::Elements(offered as u64));
			group.bench_with_input(
				BenchmarkId::new(offered.to_string(), bytes),
				&(offered, bytes),
				|b, &(offered, bytes)| {
					b.iter(|| runtime.block_on(fixture.round(offered, bytes)));
				},
			);
		}
	}
	group.finish();
}

criterion_group!(benches, refresh, queue);
criterion_main!(benches);
