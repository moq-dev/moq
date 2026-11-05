//! One public HTTP read's admission, swept over connections and the HTTP/2 streams each
//! holds waiting on the decider. A slope means admitting one request costs more as others
//! wait, rather than what the request itself touches.
//! The real web listener runs over loopback TCP; timings include one request round trip.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use futures::{StreamExt, stream::FuturesUnordered};
use moq_auth::{Grant, lease};
use moq_relay::{auth, cluster, web};
use std::task::Poll;
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
	async fn new(connections: usize, streams: usize, server: bool) -> Self {
		let _ = rind_rustls::default_provider().unwrap().install_default();
		let waiting = kio::Shared::new(0usize);
		let (auth, decider) = if server {
			let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
			let mut config = auth::Config::default();
			config.url = Some(format!("http://{}/", listener.local_addr().unwrap()).parse().unwrap());
			let auth = config.init("bench", &Default::default(), false).unwrap();
			let app = axum::Router::new()
				.route("/", axum::routing::post(decide))
				.with_state(waiting.clone());
			let decider = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
			(auth, decider)
		} else {
			let (auth, mut admissions) = auth::Auth::embedded("bench");
			let waiting = waiting.clone();
			let decider = tokio::spawn(async move {
				let mut held = Vec::new();
				while let Some(admission) = admissions.next().await {
					if admission.request.path.starts_with("/held") {
						held.push(admission);
						*waiting.lock() = held.len();
						continue;
					}
					admission.grant(lease::Consumer::fixed(grant()));
				}
			});
			(auth, decider)
		};

		let pem = (|| -> std::result::Result<_, rcgen::Error> {
			let signing_key = rind_rcgen::KeyPair::generate(&rcgen::PKCS_ECDSA_P256_SHA256)?;
			let mut params = rcgen::CertificateParams::new(vec!["localhost".into()])?;
			params.serial_number = Some(1u64.into());
			let cert = params.self_signed(&signing_key)?;
			Ok(rcgen::CertifiedKey { cert, signing_key })
		})()
		.unwrap()
		.cert
		.pem();
		let certificates = moq_tokio::tls::Certificates::from_pem(pem.as_bytes()).unwrap();
		let mut config = web::Config::default();
		config.http.listen = Some("127.0.0.1:0".parse().unwrap());
		let (cluster, driver) = cluster::Cluster::new(Default::default()).unwrap();
		let driver = tokio::spawn(async move { driver.run().await.unwrap() });
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

		let mut tasks = vec![decider, server, driver];
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
		let mut responses: FuturesUnordered<_> = held
			.iter_mut()
			.flat_map(|(_, responses)| responses.iter_mut())
			.collect();
		tokio::select! {
			biased;
			result = responses.next(), if !responses.is_empty() => {
				panic!("a held admission completed during setup: {result:?}");
			}
			_ = waiting
				.wait(|count| {
					if **count == connections * streams {
						Poll::Ready(())
					} else {
						Poll::Pending
					}
				}) => {}
		}
		drop(responses);

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

fn grant() -> Grant {
	Grant::new(moq_auth::Patterns::new(), moq_auth::Pattern::all())
}

async fn decide(
	axum::extract::State(waiting): axum::extract::State<kio::Shared<usize>>,
	axum::Json(request): axum::Json<moq_auth::Request>,
) -> axum::Json<Grant> {
	if request.path.starts_with("/held") {
		*waiting.lock() += 1;
		std::future::pending().await
	} else {
		axum::Json(grant())
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
	for server in [false, true] {
		let name = if server {
			"relay_http_auth_server"
		} else {
			"relay_http_admission"
		};
		let mut group = c.benchmark_group(name);
		group.sample_size(10);
		group.warm_up_time(Duration::from_millis(100));
		group.measurement_time(Duration::from_millis(300));
		// A held HTTP/1.1 auth POST takes two sockets, unlike a multiplexed web
		// stream. Keep its full sweep below a process's standard 1024-fd budget.
		let (connections, streams) = if server {
			([1, 4, 16], [0, 4, 16])
		} else {
			([1, 16, 64], [0, 16, 64])
		};
		// Each connection may hold up to the verification budget (128 by default) at once.
		for connections in connections {
			for streams in streams {
				let mut fixture = runtime.block_on(Fixture::new(connections, streams, server));
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
}

criterion_group!(benches, admission);
criterion_main!(benches);
