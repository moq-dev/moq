//! Shared test support for moq-net integration tests.

pub mod harness;
pub mod mock;

use moq_net::{announce, goaway, origin, track};

/// Wait until anyone wants the track, returning the group they want.
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub async fn wanted(mut demand: track::Demand) -> moq_net::Result<u64> {
	loop {
		if let Some(wanted) = demand.wanted_changed().await? {
			return Ok(wanted);
		}
	}
}

/// Wait until nobody wants the track.
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub async fn unwanted(mut demand: track::Demand) -> moq_net::Result<()> {
	while demand.wanted_changed().await?.is_some() {}
	Ok(())
}

/// Wait until `consumer` sees a route announced at exactly `path`, and return it.
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub async fn announced(consumer: &origin::Consumer, path: &str) -> origin::Route {
	let mut announced = consumer.announced();
	loop {
		match announced.next().await.expect("the origin closed") {
			announce::Event::Start(announce)
			| announce::Event::Update(announce)
			| announce::Event::Restart(announce)
				if announce.prefix.as_str() == path =>
			{
				return announce.route;
			}
			_ => {}
		}
	}
}

/// Reads of the peer's GOAWAY over [`goaway::Consumer::poll`].
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub trait Draining {
	/// The peer's GOAWAY, or `None` if it has not sent one.
	fn peek(&self) -> Option<goaway::Goaway>;

	/// Wait for the peer's GOAWAY, or `None` if the session closes without one.
	async fn recv(&self) -> Option<goaway::Goaway>;
}

impl Draining for goaway::Consumer {
	fn peek(&self) -> Option<goaway::Goaway> {
		match self.poll(&moq_net::kio::Waiter::noop()) {
			std::task::Poll::Ready(Ok(goaway)) => Some(goaway),
			_ => None,
		}
	}

	async fn recv(&self) -> Option<goaway::Goaway> {
		moq_net::kio::wait(|waiter| self.poll(waiter)).await.ok()
	}
}

/// Every version this crate speaks, found by wire code, including the opt-in `-wip`
/// drafts that [`moq_net::Versions::all`] leaves out.
#[allow(dead_code, reason = "shared by several test binaries, each using only part of it")]
pub fn every_version() -> Vec<moq_net::Version> {
	(0xff0dad00..=0xff0dadff)
		.chain(0xff000000..=0xff0000ff)
		.filter_map(moq_net::Version::from_code)
		.collect()
}
