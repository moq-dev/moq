use std::task::Poll;

/// Fetch one group of `track`: the one at `sequence`, or the newest when `None`.
///
/// The newest needs a live subscription to learn its sequence, since a fetch can
/// only retrieve a sequence already known. Once known it is fetched rather than
/// read off the subscription, so an evicted group is retrieved from upstream
/// instead of waited on forever. Fails before returning when the group can never
/// be served: [`moq_net::Error::NotFound`] locally, including a track that ends
/// before any group, or [`moq_net::StreamError::NotFound`] from upstream.
pub async fn fetch_group(
	track: &moq_net::track::Consumer,
	sequence: Option<u64>,
) -> moq_net::Result<moq_net::group::Consumer> {
	if let Some(sequence) = sequence {
		return track.fetch_group(sequence, None).await;
	}

	let mut subscriber = track.subscribe(None).await?;
	// The subscription hands over a newest group already cached, even an empty one, which
	// has no largest frame. Otherwise the largest names it, possibly before it arrives.
	// Ok is the cached group; Err is the sequence to fetch, or none once the track ends.
	let newest = moq_net::kio::wait(|waiter| {
		if let Poll::Ready(group) = subscriber.poll_recv_group(waiter) {
			return Poll::Ready(group.map(|group| group.ok_or(None)));
		}
		subscriber
			.poll_largest(waiter)
			.map(|largest| Ok(Err(largest.map(|position| position.group))))
	})
	.await?;
	match newest {
		Ok(group) => Ok(group),
		Err(Some(sequence)) => track.fetch_group(sequence, None).await,
		Err(None) => Err(moq_net::Error::NotFound),
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use futures::FutureExt;

	/// An empty newest group has no largest frame, but it is still the newest group.
	#[test]
	fn the_newest_group_can_be_empty() {
		let broadcast = moq_net::broadcast::Info::default().produce();
		let track = broadcast.create_track("t", moq_net::track::Info::default()).unwrap();
		let mut first = track.append_group().unwrap();
		first.write_frame(None, &b"a"[..]).unwrap();
		first.finish().unwrap();
		let _open = track.append_group().unwrap();

		let handle = broadcast.consume().track("t").unwrap();
		let group = fetch_group(&handle, None)
			.now_or_never()
			.expect("the newest group is cached")
			.unwrap();
		assert_eq!(group.sequence, 1);
	}
}
