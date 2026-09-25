//! Integration test: a NO_CAPACITY refusal re-resolves once, over real sessions.
//!
//! Two workers each serve the root prefix on demand. A subscriber connected to
//! both requests a path: the cheaper worker is asked first, and when it refuses
//! for capacity the subscriber asks the other one exactly once.

use moq_tokio::moq_net::{self, Hop, Timestamp};
use std::sync::{
	Arc,
	atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

/// A worker: an origin with a root route answered on demand, behind a listening server.
struct Worker {
	hop: u64,
	port: u16,
	/// How many requests reached the worker's handler.
	requests: Arc<AtomicUsize>,
}

/// Spawn a worker whose root route costs `cost` and whose handler accepts with a
/// one-frame broadcast when `capacity` is set, and refuses with NO_CAPACITY otherwise.
async fn worker(hop: u64, cost: u64, capacity: bool) -> Worker {
	let origin = moq_tokio::origin::spawn_config(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	let dynamic = origin
		.dynamic("", moq_net::origin::Route::default().with_cost(cost))
		.expect("dynamic");

	// The content an accepting worker serves, held on an origin of its own.
	let store = moq_tokio::origin::spawn();
	let content = store.create_broadcast("job").expect("create broadcast");
	let track = content.create_track("video", None).expect("create track");
	let mut group = track
		.create_group(moq_net::group::Info { sequence: 0 })
		.expect("create group");
	group
		.write_frame(Timestamp::ZERO, format!("worker {hop}").into_bytes())
		.expect("write frame");
	group.finish().expect("finish group");

	let requests = Arc::new(AtomicUsize::new(0));
	let counted = requests.clone();
	tokio::spawn(async move {
		let _store = store;
		let _track = track;
		while let Ok(request) = dynamic.requested_broadcast().await {
			counted.fetch_add(1, Ordering::SeqCst);
			if capacity {
				request.accept(content.consume());
			} else {
				request.reject(moq_net::Error::NoCapacity);
			}
		}
	});

	let mut config = moq_tokio::listen::Config::default();
	config.bind = Some("[::]:0".parse().unwrap());
	config.tls.generate = vec!["localhost".into()];
	let server = config.init(Default::default()).expect("init server");
	let mut server = server.listen().await.expect("listen");
	let port = server.local_addr().expect("local addr").port();
	tokio::spawn(async move {
		while let Some(request) = server.accept().await {
			let Ok(session) = request.with_publisher(&origin).ok().await else {
				continue;
			};
			let _ = session.closed().await;
		}
	});

	Worker { hop, port, requests }
}

async fn connect(port: u16, origin: moq_net::origin::Producer) -> moq_tokio::Connection {
	let mut config = moq_tokio::connect::Config::default();
	config.tls.insecure = Some(true);
	let client = config.init(Default::default()).expect("init client");
	let url: url::Url = format!("moqt://localhost:{port}").parse().unwrap();
	tokio::time::timeout(
		TIMEOUT,
		client
			.with_subscriber(origin)
			.with_reconnect(false)
			.connect(url)
			.established(),
	)
	.await
	.expect("connect timeout")
	.expect("connect failed")
}

/// Wait until the root route's winner is the one `hop` originated.
async fn await_winner(announced: &mut moq_net::announce::Consumer, hop: u64) {
	loop {
		let update = tokio::time::timeout(TIMEOUT, announced.next())
			.await
			.expect("announce timeout")
			.expect("announcements ended");
		if update.kind.is_active() && update.route.hops.iter().next().map(|h| h.id()) == Some(hop) {
			return;
		}
	}
}

/// A subscriber origin connected to `standby` then the cheaper `preferred`, so both
/// routes stand before anything is requested.
async fn subscriber(preferred: &Worker, standby: &Worker) -> (moq_net::origin::Producer, [moq_tokio::Connection; 2]) {
	let origin = moq_tokio::origin::spawn();
	let mut announced = origin.consume().announced();
	let second = connect(standby.port, origin.clone()).await;
	await_winner(&mut announced, standby.hop).await;
	let first = connect(preferred.port, origin.clone()).await;
	await_winner(&mut announced, preferred.hop).await;
	(origin, [first, second])
}

/// A capacity refusal moves the request to the other advertiser.
#[tracing_test::traced_test]
#[tokio::test]
async fn capacity_refusal_reresolves_to_another_advertiser() {
	let full = worker(0xA, 1, false).await;
	let spare = worker(0xB, 2, true).await;
	let (origin, _sessions) = subscriber(&full, &spare).await;

	let broadcast = tokio::time::timeout(TIMEOUT, origin.consume().request_broadcast("job"))
		.await
		.expect("request timeout")
		.expect("a covered path resolves");
	let mut sub = broadcast
		.track("video")
		.unwrap()
		.subscribe(None)
		.await
		.expect("subscribe failed");
	let mut group = tokio::time::timeout(TIMEOUT, sub.recv_group())
		.await
		.expect("recv_group timeout")
		.expect("recv_group failed")
		.expect("track closed");
	let frame = tokio::time::timeout(TIMEOUT, group.read_frame())
		.await
		.expect("read_frame timeout")
		.expect("read_frame failed")
		.expect("group empty");
	assert_eq!(frame.payload.as_ref(), format!("worker {}", 0xB).as_bytes());

	assert_eq!(full.requests.load(Ordering::SeqCst), 1, "the full worker is asked once");
	assert_eq!(
		spare.requests.load(Ordering::SeqCst),
		1,
		"the spare worker is asked once"
	);
}

/// A second capacity refusal is terminal, and reported as unroutable rather than
/// forwarded as NO_CAPACITY.
#[tracing_test::traced_test]
#[tokio::test]
async fn second_capacity_refusal_is_unroutable() {
	let first = worker(0xA, 1, false).await;
	let second = worker(0xB, 2, false).await;
	let (origin, _sessions) = subscriber(&first, &second).await;

	let broadcast = tokio::time::timeout(TIMEOUT, origin.consume().request_broadcast("job"))
		.await
		.expect("request timeout")
		.expect("a covered path resolves");
	let refused = tokio::time::timeout(TIMEOUT, async {
		let mut sub = broadcast.track("video").unwrap().subscribe(None).await?;
		sub.recv_group().await.map(|_| ())
	})
	.await
	.expect("refusal timeout");
	assert!(matches!(refused, Err(moq_net::Error::Unroutable)), "{refused:?}");

	assert_eq!(first.requests.load(Ordering::SeqCst), 1, "never re-asked");
	assert_eq!(second.requests.load(Ordering::SeqCst), 1, "asked once, no loop");
}
