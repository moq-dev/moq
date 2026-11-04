//! Compile these downstream futures at Rust's default recursion limit.
#![cfg(feature = "async")]

fn require_send(_: impl Send) {}

async fn watch_announcements(mut consumer: moq_net::announce::Consumer) {
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
	let _: fn(moq_net::announce::Consumer) = |consumer| {
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
