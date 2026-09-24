//! A broadcast exists for other people only while it is announced, and a consumer
//! in the same process sees exactly what a consumer across a session sees.
//!
//! Each scenario runs twice: once through a consumer of the publishing origin,
//! once through a subscriber origin fed by an in-memory mock session. Both runs
//! record what they observe, and the records must match.
//!
//! Deterministic: time is paused and the runtime is single-threaded, so settling
//! is a virtual sleep.

mod support;

use std::time::Duration;

use moq_net::{Error, Hop, Timestamp, Version, origin};
use support::harness::{MockConnectOptions, MockPair, connect_mock};
use tokio::sync::mpsc;

/// Long enough, in virtual time, for anything in flight to reach the far side.
const SETTLE: Duration = Duration::from_secs(1);

const PATH: &str = "bcast";

fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	tokio::spawn(support::harness::run(driver));
	producer
}

/// Where the observing consumer sits relative to the publisher.
enum Observer {
	/// A consumer of the publishing origin itself.
	Local,
	/// A consumer of another origin, fed over a mock session of this version.
	Remote(&'static str),
}

/// Drain every update the cursor has pending, as `kind prefix` lines.
fn drain(announced: &mut moq_net::announce::Consumer) -> Vec<String> {
	let mut seen = Vec::new();
	while let Some(update) = announced.try_next() {
		seen.push(format!("{:?} {}", update.kind, update.prefix));
	}
	seen
}

/// How a request for the path answers.
async fn request(consumer: &origin::Consumer) -> Result<moq_net::broadcast::Consumer, String> {
	consumer.request_broadcast(PATH).await.map_err(|err| match err {
		Error::Unroutable => "unroutable".to_string(),
		err => err.to_string(),
	})
}

fn outcome(result: &Result<moq_net::broadcast::Consumer, String>) -> String {
	match result {
		Ok(_) => "ok".to_string(),
		Err(err) => err.clone(),
	}
}

/// Subscribe to `name` from group 0 and read it on a task, reporting each
/// group's frames and then how the subscription ended. Read continuously, as a
/// real subscriber would, so the subscription is driven the whole time.
async fn read(broadcast: &moq_net::broadcast::Consumer, name: &str) -> mpsc::UnboundedReceiver<String> {
	let subscription = moq_net::track::Subscription::default().with_start(moq_net::track::Position::group(0));
	let mut sub = broadcast
		.track(name)
		.unwrap()
		.subscribe(subscription)
		.await
		.expect("subscribe");
	let (tx, rx) = mpsc::unbounded_channel();
	tokio::spawn(async move {
		loop {
			let mut group = match sub.recv_group().await {
				Ok(Some(group)) => group,
				Ok(None) => {
					let _ = tx.send("end".to_string());
					return;
				}
				Err(err) => {
					let _ = tx.send(format!("error {err}"));
					return;
				}
			};
			let mut frames = Vec::new();
			while let Ok(Some(frame)) = group.read_frame().await {
				frames.push(String::from_utf8_lossy(&frame.payload).into_owned());
			}
			if tx.send(frames.join(",")).is_err() {
				return;
			}
		}
	});
	rx
}

/// The reader's next report, or `stalled` if nothing arrives in time.
async fn next(rx: &mut mpsc::UnboundedReceiver<String>) -> String {
	match tokio::time::timeout(SETTLE, rx.recv()).await {
		Ok(Some(report)) => report,
		_ => "stalled".to_string(),
	}
}

fn write_group(track: &moq_net::track::Producer, payload: &'static str) {
	let mut group = track.append_group().unwrap();
	group.write_frame(Timestamp::ZERO, payload.as_bytes()).unwrap();
	group.finish().unwrap();
}

/// Walk one broadcast through its whole announce lifecycle and record what the
/// observer sees at each step.
async fn lifecycle(observer: Observer) -> Vec<String> {
	let publisher = produce_origin(1);
	let (consumer, _pair): (origin::Consumer, Option<MockPair>) = match observer {
		Observer::Local => (publisher.consume(), None),
		Observer::Remote(version) => {
			let subscriber = produce_origin(2);
			let mut options = MockConnectOptions::new(version.parse::<Version>().unwrap());
			options.server_publish = Some(publisher.clone());
			options.client_subscribe = Some(subscriber.clone());
			let pair = connect_mock(options).await;
			(subscriber.consume(), Some(pair))
		}
	};
	let mut announced = consumer.announced();
	let mut log = Vec::new();

	// Created, not yet announced: nobody can see or reach it.
	let broadcast = publisher.create_broadcast(PATH).unwrap();
	let track = broadcast.create_track("video", None).unwrap();
	tokio::time::sleep(SETTLE).await;
	log.push(format!(
		"created: {:?} {}",
		drain(&mut announced),
		outcome(&request(&consumer).await)
	));

	// Announced: listed, and a request resolves.
	broadcast.announce(Default::default()).unwrap();
	tokio::time::sleep(SETTLE).await;
	let first = request(&consumer).await;
	log.push(format!("announced: {:?} {}", drain(&mut announced), outcome(&first)));
	let first = first.expect("an announced broadcast resolves");

	// A track in flight across the unannounce.
	let mut reader = read(&first, "video").await;
	tokio::time::timeout(SETTLE, track.used())
		.await
		.expect("no subscriber appeared")
		.unwrap();
	write_group(&track, "before");
	log.push(format!("in flight: {}", next(&mut reader).await));

	// Unannounced: retracted for everyone. A fresh request is refused rather
	// than joining the broadcast still draining, which ends, while the track
	// already in flight carries on to its own end.
	broadcast.unannounce();
	tokio::time::sleep(SETTLE).await;
	log.push(format!(
		"unannounced: {:?} {} closed={}",
		drain(&mut announced),
		outcome(&request(&consumer).await),
		first.is_closed(),
	));
	write_group(&track, "after");
	track.finish().unwrap();
	log.push(format!(
		"draining: {} then {}",
		next(&mut reader).await,
		next(&mut reader).await
	));

	// Announced again: listed and servable, through a fresh broadcast.
	broadcast.announce(Default::default()).unwrap();
	tokio::time::sleep(SETTLE).await;
	let again = request(&consumer).await;
	log.push(format!(
		"reannounced: {:?} {} fresh={}",
		drain(&mut announced),
		outcome(&again),
		again.as_ref().is_ok_and(|again| !again.is_clone(&first)),
	));
	let again = again.expect("a reannounced broadcast resolves");

	let audio = broadcast.create_track("audio", None).unwrap();
	let mut reader = read(&again, "audio").await;
	tokio::time::timeout(SETTLE, audio.used())
		.await
		.expect("no subscriber appeared after reannouncing")
		.unwrap();
	write_group(&audio, "again");
	log.push(format!("serving again: {}", next(&mut reader).await));

	log
}

const EXPECTED: &[&str] = &[
	"created: [] unroutable",
	"announced: [\"Announced bcast\"] ok",
	"in flight: before",
	"unannounced: [\"Retracted bcast\"] unroutable closed=true",
	"draining: after then end",
	"reannounced: [\"Announced bcast\"] ok fresh=true",
	"serving again: again",
];

#[tokio::test]
async fn local_consumer_sees_only_announced_broadcasts() {
	tokio::time::pause();
	assert_eq!(lifecycle(Observer::Local).await, EXPECTED);
}

#[tokio::test]
async fn remote_lite_consumer_sees_what_a_local_one_does() {
	tokio::time::pause();
	assert_eq!(lifecycle(Observer::Remote("moq-lite-05")).await, EXPECTED);
}

#[tokio::test]
async fn remote_ietf_consumer_sees_what_a_local_one_does() {
	tokio::time::pause();
	let mut seen = lifecycle(Observer::Remote("moq-transport-19")).await;
	// Every IETF subscription ends in error today, announced or not: the subscriber
	// reads the publisher's PUBLISH_DONE as trailing bytes (/quest/m1/ietf-publish-done.md).
	// The track still carries on across the retraction; only its end differs.
	assert_eq!(seen.remove(4), "draining: after then error dropped");
	let mut expected = EXPECTED.to_vec();
	expected.remove(4);
	assert_eq!(seen, expected);
}
