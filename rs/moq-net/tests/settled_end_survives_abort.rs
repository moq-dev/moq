//! A track whose declared end has settled holds every group it promised: the end was
//! reached and each group below it finished. An abort that lands afterwards, such as
//! the session dying, ends the track cleanly but must not throw those groups away: a
//! consumer that has not read them yet still gets them, then the clean end.

mod support;

use moq_net::{Error, Hop, Timestamp};

fn produce_origin(hop: u64) -> moq_net::origin::Producer {
	let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::new(Hop::new(hop).unwrap()));
	tokio::spawn(support::harness::run(driver));
	producer
}

const GROUPS: u64 = 2;

/// Publish `GROUPS` finished groups, declare the end at `GROUPS`, then abort the track.
/// Returns the sequences a consumer that starts reading only now receives, and how the
/// read ended.
async fn round(abort: bool) -> (Vec<u64>, Option<Error>) {
	let origin = produce_origin(1);
	let broadcast = origin.create_broadcast("bcast").unwrap();
	let mut track = broadcast.create_track("video", None).unwrap();
	// From the first group with a replay window: a late reader is owed the whole track,
	// not the live edge.
	let subscription = moq_net::track::Subscription::default()
		.with_start(moq_net::track::Position::group(0))
		.with_max_age(std::time::Duration::from_secs(30));
	let mut consumer = track.subscribe(subscription);

	for _ in 0..GROUPS {
		let mut group = track.append_group().unwrap();
		group.write_frame(Timestamp::ZERO, b"frame".as_slice()).unwrap();
		group.finish().unwrap();
	}
	track.finish_at(GROUPS).unwrap();
	// Every group below the end is finished: the end has settled and the track is
	// complete. Nothing has been read yet.
	if abort {
		track.abort(Error::Session(moq_net::SessionError::Cancel)).unwrap();
	}

	let mut got = Vec::new();
	loop {
		match consumer.recv_group().await {
			Ok(Some(group)) => got.push(group.sequence),
			Ok(None) => return (got, None),
			Err(err) => return (got, Some(err)),
		}
	}
}

/// The groups a settled track holds outlive an abort: a late reader gets all of them.
#[tokio::test]
async fn an_abort_after_the_end_settled_keeps_the_groups_for_a_late_reader() {
	let (got, err) = round(true).await;
	assert!(
		err.is_none() && got == (0..GROUPS).collect::<Vec<_>>(),
		"got {got:?} of {GROUPS} groups, err={err:?} (every group had finished before the \
		 abort, so a late reader gets all of them and then the clean end)"
	);
}

/// Control: with no abort the same late reader gets every group, then the clean end.
#[tokio::test]
async fn a_settled_track_delivers_every_group_to_a_late_reader() {
	let (got, err) = round(false).await;
	assert!(
		err.is_none() && got == (0..GROUPS).collect::<Vec<_>>(),
		"no abort: got {got:?}, err={err:?}"
	);
}
