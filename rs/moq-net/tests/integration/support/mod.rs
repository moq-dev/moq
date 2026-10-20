#![cfg(feature = "async")]

//! Shared test support for moq-net integration tests.

pub mod harness;
pub mod mock;

use moq_net::track;

/// Wait until anyone wants the track, returning the group they want.
#[allow(dead_code)]
pub async fn wanted(mut demand: track::Demand) -> moq_net::Result<u64> {
	loop {
		if let Some(wanted) = demand.wanted_changed().await? {
			return Ok(wanted);
		}
	}
}

/// Wait until nobody wants the track.
#[allow(dead_code)]
pub async fn unwanted(mut demand: track::Demand) -> moq_net::Result<()> {
	while demand.wanted_changed().await?.is_some() {}
	Ok(())
}
