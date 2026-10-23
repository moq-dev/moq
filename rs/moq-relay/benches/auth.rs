//! Revalidate one token while sweeping unrelated sessions and held credentials, and
//! cycle one verification slot while sweeping the waiting tokens and the slot count.
//! The real relay/controller runs over loopback TCP; timings include one AUTH round trip.
#![expect(clippy::disallowed_methods, reason = "host time measures benchmark throughput")]

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

/// One session with `verifications` slots held by an unanswered decider and `waiting`
/// tokens queued behind them. Each round refuses the oldest held check, which sends the
/// oldest waiting token, and presents one more so the queue keeps its depth.
struct Queue {
	_connection: moq_tokio::Connection,
	auth: moq_tokio::connection::auth::Handle,
	admissions: auth::Admissions,
	held: std::collections::VecDeque<auth::Admission>,
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
	async fn new(verifications: usize, waiting: usize) -> Self {
		let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
		let (auth, mut admissions) = auth::Auth::embedded("bench");
		let auth = auth.with_verifications(verifications);
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
		let mut queue = Self {
			auth: connection.auth(),
			_connection: connection,
			admissions,
			held: Default::default(),
			presented: 0,
			tasks: vec![listener],
		};
		for _ in 0..verifications {
			queue.present();
			let admission = queue.admissions.next().await.unwrap();
			queue.held.push_back(admission);
		}
		for _ in 0..waiting {
			queue.present();
		}
		// Let the waiting tokens reach the relay's queue before measuring.
		tokio::time::sleep(Duration::from_millis(50)).await;
		queue
	}

	fn present(&mut self) {
		let auth = self.auth.clone();
		let token = format!("token{}", self.presented);
		self.presented += 1;
		self.tasks.push(tokio::spawn(async move {
			let _ = auth.add(token).await;
		}));
	}

	async fn round(&mut self) {
		self.held
			.pop_front()
			.unwrap()
			.refuse(auth::Error::Refused(Default::default()));
		self.present();
		let admission = self.admissions.next().await.unwrap();
		self.held.push_back(admission);
		// Refused tokens finish; drop their handles so the table stays flat.
		self.tasks.retain(|task| !task.is_finished());
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
	// QMux grants 100 bidirectional streams, and every held or waiting token holds one.
	for verifications in [1, 8, 32] {
		for waiting in [0, 16, 48] {
			let mut fixture = runtime.block_on(Queue::new(verifications, waiting));
			group.bench_with_input(
				BenchmarkId::new(verifications.to_string(), waiting),
				&(verifications, waiting),
				|b, _| {
					b.iter(|| runtime.block_on(fixture.round()));
				},
			);
		}
	}
	group.finish();
}

criterion_group!(benches, refresh, queue);
criterion_main!(benches);
