//! Compile these downstream futures at Rust's default recursion limit.
#![cfg(all(feature = "async", not(target_family = "wasm")))]

fn require_send(_: impl Send) {}

#[test]
fn announcement_next_is_send() {
	let _: fn(moq_net::announce::Consumer) = |mut consumer| {
		require_send(async move { consumer.next().await });
	};
}

#[test]
fn track_receive_is_send() {
	let _: fn(moq_net::track::Subscriber) = |mut consumer| {
		require_send(async move { consumer.recv_group().await });
	};
}

#[test]
fn group_read_is_send() {
	let _: fn(moq_net::group::Consumer) = |mut consumer| {
		require_send(async move { consumer.read_frame().await });
	};
}
