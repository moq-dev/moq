//! A consumer of the moq-net API as its documentation describes it, compiled
//! unchanged against the candidate archive.
//!
//! `cargo-semver-checks` compares two published APIs; nothing compares the
//! published API against code someone already wrote. This does: if a signature
//! moves, a name disappears, or an inherent method becomes generic, this stops
//! compiling and the break gets classified against the repo's main/dev
//! targeting rule before it ships rather than after.
//!
//! Purely model-layer, so it needs no runtime: tracks, groups, and frames never
//! touch a timer.
use std::task::Poll;

use moq_net::{Timestamp, broadcast, kio};

/// Publish one frame and read it back through a subscriber, in memory.
pub fn roundtrip(payload: &[u8]) -> Result<Vec<u8>, moq_net::Error> {
	let mut track = broadcast::Info::new().produce().create_track("probe", None)?;
	let mut subscriber = track.subscribe(None);

	let mut group = track.append_group()?;
	group.write_frame(Timestamp::ZERO, payload)?;
	group.finish()?;
	track.finish()?;

	let waiter = kio::Waiter::noop();
	let Poll::Ready(Ok(Some(mut group))) = subscriber.poll_next_group(&waiter) else {
		return Ok(Vec::new());
	};
	let Poll::Ready(Ok(Some(frame))) = group.poll_read_frame(&waiter) else {
		return Ok(Vec::new());
	};
	Ok(frame.payload.to_vec())
}
