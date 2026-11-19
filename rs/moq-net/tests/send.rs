//! Compile these downstream futures at Rust's default recursion limit.
#![cfg(feature = "async")]

fn require_send(_: impl Send) {}

async fn watch_announcements(mut consumer: moq_net::origin::Routes) {
	while let Some(event) = consumer.next().await {
		drop(event);
	}
}

async fn receive_groups(mut consumer: moq_net::track::Subscriber) {
	while let Ok(Some(group)) = consumer.recv_group().await {
		drop(group);
	}
}

async fn read_frames(mut consumer: moq_net::group::Consumer) {
	while let Ok(Some(frame)) = consumer.read_frame().await {
		drop(frame);
	}
}

#[test]
fn announcement_next_is_send() {
	let _: fn(moq_net::origin::Routes) = |consumer| {
		require_send(watch_announcements(consumer));
	};
}

#[test]
fn track_receive_is_send() {
	let _: fn(moq_net::track::Subscriber) = |consumer| {
		require_send(receive_groups(consumer));
	};
}

#[test]
fn group_read_is_send() {
	let _: fn(moq_net::group::Consumer) = |consumer| {
		require_send(read_frames(consumer));
	};
}

// Exporters such as moq-ffi's ask whether `Driver` is `Sync` to pick a wrapper, so asking must
// settle within the limit. Method resolution evaluates the `T: Sync` bound on the first
// candidate and falls back to the second when it fails.
struct Probe<T>(std::marker::PhantomData<T>);

trait Synced {
	fn synced(&self) -> bool {
		true
	}
}

impl<T: Sync> Synced for Probe<T> {}

trait Unsynced {
	fn synced(&self) -> bool {
		false
	}
}

impl<T> Unsynced for &Probe<T> {}

#[test]
#[allow(
	clippy::needless_borrow,
	reason = "the borrow picks which candidate method resolution tries first"
)]
fn driver_is_not_sync() {
	assert!(!(&Probe::<moq_net::Driver>(std::marker::PhantomData)).synced());
	// The control proves the probe can answer yes.
	assert!((&Probe::<moq_net::Session>(std::marker::PhantomData)).synced());
}

#[test]
fn driver_is_send() {
	let _: fn(moq_net::Driver) = require_send;
}
