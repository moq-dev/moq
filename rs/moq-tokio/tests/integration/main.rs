//! Every integration test, linked into one binary.
//!
//! Each `tests/*.rs` file is its own crate that links this package and its
//! dependencies again, so add new integration tests here as a module.
#![expect(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "tests read the host clock directly"
)]

mod backend;
mod broadcast;
mod close_code;
mod keylog;
mod max_age;
mod receive_credit;
mod reconnect;
mod reset_secret;
mod runtime;
mod stream_code;
mod subscription_end_integrity;
mod tokens;
mod unannounce;
mod worker;

/// Wait until some subscriber wants the track.
async fn wanted(mut demand: moq_net::track::Demand) -> moq_net::Result<()> {
	while demand.wanted_changed().await?.is_none() {}
	Ok(())
}

/// Wait until no subscriber wants the track.
async fn unwanted(mut demand: moq_net::track::Demand) -> moq_net::Result<()> {
	while demand.wanted_changed().await?.is_some() {}
	Ok(())
}
