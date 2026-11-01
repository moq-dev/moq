//! The shutdown drain over moq-net's in-memory transport, on Tokio's paused clock.
//!
//! The deadline closes every established session the drain still holds, however
//! its GOAWAY is faring, and the drain returns only once each has recorded its end.
//! How long that end takes to reach an auth server is `auth_lifetime`'s to test.

use std::{
	sync::{
		Arc,
		atomic::{AtomicBool, Ordering},
	},
	task::Poll,
	time::Duration,
};

use futures::FutureExt;
use moq_auth::lease;
use moq_net::transport::memory;
use moq_relay::{Accepted, Connection, auth, shutdown};

const WINDOW: Duration = Duration::from_secs(10);

/// One session admitted through an embedded decider and served by a relay
/// [`Connection`] draining with `observer`.
struct Admitted {
	client: moq_net::Session,
	/// The decider's half of the lease, which learns how the session ended.
	lease: lease::Producer,
}

/// Admit a client speaking `version`. Once the relay supervises it, a `stalled`
/// client stops reading but keeps its transport open, so nothing the relay sends
/// afterwards leaves its stream.
async fn admit(observer: &shutdown::Observer, version: moq_net::Version, stalled: bool) -> Admitted {
	let (auth, mut admissions) = auth::Auth::embedded("test-relay");
	let mut config = memory::Config::default();
	config.protocol = Some(version.alpn().to_string());
	let (client_link, relay_link) = memory::pair(config);

	let observer = observer.clone();
	tokio::spawn(async move {
		let server = moq_net::Server::new().with_versions(version.into());
		let handshake = server
			.accept_request(moq_net::time::Instant::now(), relay_link)
			.await
			.expect("relay handshake");
		let request = Accepted::new(handshake, moq_auth::Transport::Quic);
		Connection::new(request, crate::memory::cluster(), auth)
			.with_shutdown(observer)
			.run()
			.await;
	});

	let all = moq_auth::Pattern::all;
	let (lease, consumer) = lease::Producer::new(moq_auth::Grant::new(all(), all()));
	tokio::spawn(async move {
		admissions.next().await.expect("an admission").grant(consumer);
	});
	let (client, driver) = moq_net::Client::new()
		.with_versions(version.into())
		.connect(moq_net::time::Instant::now(), client_link)
		.await
		.expect("client handshake");

	// Polled only while `reading`, and never dropped: a dropped driver would close
	// the transport rather than leave it open and unread.
	let reading = Arc::new(AtomicBool::new(true));
	tokio::spawn({
		let reading = reading.clone();
		async move {
			let mut driver = std::pin::pin!(moq_net::time::run(driver));
			std::future::poll_fn(|cx| match reading.load(Ordering::Relaxed) {
				true => driver.as_mut().poll(cx),
				false => Poll::Pending,
			})
			.await
		}
	});

	// Paused time advances only once every task is idle, so the relay is
	// supervising the session by the time this returns.
	kio::clock::sleep(Duration::from_millis(1)).await;
	assert!(
		lease.closed().now_or_never().is_none(),
		"the session ended before the drain"
	);
	reading.store(!stalled, Ordering::Relaxed);
	Admitted { client, lease }
}

/// Start the drain and return its end, which [`shutdown::Observer::drained`] reports.
fn drain(trigger: &shutdown::Trigger, observer: &shutdown::Observer) -> tokio::task::JoinHandle<()> {
	trigger.start();
	let observer = observer.clone();
	tokio::spawn(async move { observer.drained().await })
}

/// A peer that ignores the GOAWAY is closed at the deadline, and the drain returns
/// only after its lease recorded the shutdown.
#[tokio::test(start_paused = true)]
async fn a_session_ignoring_the_goaway_is_closed_at_the_deadline() {
	for alpn in ["moq-lite-06", "moq-lite-03", "moq-transport-18"] {
		let (trigger, observer) = shutdown::Observer::new(WINDOW);
		let session = admit(&observer, alpn.parse().unwrap(), false).await;
		let started = kio::clock::now();
		let drained = drain(&trigger, &observer);

		kio::clock::timeout(WINDOW + Duration::from_secs(1), drained)
			.await
			.unwrap_or_else(|_| panic!("{alpn}: the drain outlived its deadline"))
			.unwrap();
		assert!(started.elapsed() >= WINDOW, "{alpn}: drained before the deadline");
		let (reason, _) = session
			.lease
			.closed()
			.now_or_never()
			.unwrap_or_else(|| panic!("{alpn}: the drain returned before the lease recorded its end"));
		assert_eq!(reason, lease::Reason::Shutdown, "{alpn}");
		assert!(
			matches!(
				session.client.closed().await,
				moq_net::Error::Session(moq_net::SessionError::GoawayTimeout)
			),
			"{alpn}: closed for the deadline"
		);
	}
}

/// A peer that stops reading leaves the relay's GOAWAY stuck on its stream, so the
/// driver never arms its own timer. The drain deadline still closes the session and
/// waits for its end.
#[tokio::test(start_paused = true)]
async fn a_stalled_goaway_write_is_still_closed_at_the_deadline() {
	let (trigger, observer) = shutdown::Observer::new(WINDOW);
	let session = admit(&observer, "moq-lite-06".parse().unwrap(), true).await;
	let drained = drain(&trigger, &observer);

	kio::clock::timeout(WINDOW + Duration::from_secs(1), drained)
		.await
		.expect("a stalled GOAWAY held the drain past its deadline")
		.unwrap();
	let (reason, _) = session
		.lease
		.closed()
		.now_or_never()
		.expect("the drain returned before the lease recorded its end");
	assert_eq!(reason, lease::Reason::Shutdown);
}
