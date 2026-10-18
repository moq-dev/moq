/// Fetch one group of `track`: the one at `sequence`, or the newest when `None`.
///
/// The newest needs a live subscription to learn its sequence, since a fetch can
/// only retrieve a sequence already known. Once known it is fetched rather than
/// read off the subscription, so an evicted group is retrieved from upstream
/// instead of waited on forever. Fails before returning when the group can never
/// be served: [`moq_net::Error::NotFound`] locally, including a track that ends
/// before any group, or [`moq_net::StreamError::NotFound`] from upstream. A newest
/// sequence sent as a datagram is [`moq_net::Error::NotFetchable`] rather than an older
/// group passed off as the newest.
pub async fn fetch_group(
	track: &moq_net::track::Consumer,
	sequence: Option<u64>,
) -> moq_net::Result<moq_net::group::Consumer> {
	if let Some(sequence) = sequence {
		return track.fetch_group(sequence, None).await;
	}

	// The largest names the newest group, even an empty one, possibly before it arrives.
	let mut subscriber = track.subscribe(None).await?;
	match subscriber.largest().await {
		Some(largest) => track.fetch_group(largest.group, None).await,
		None => Err(moq_net::Error::NotFound),
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use futures::FutureExt;

	/// An empty newest group is still the newest group.
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

	/// A newest sequence sent as a datagram is refused, not served as an older group.
	#[test]
	fn a_newest_datagram_is_not_fetchable() {
		let broadcast = moq_net::broadcast::Info::default().produce();
		let mut track = broadcast.create_track("t", moq_net::track::Info::default()).unwrap();
		let mut first = track.append_group().unwrap();
		first.write_frame(None, &b"a"[..]).unwrap();
		first.finish().unwrap();
		assert_eq!(track.append_datagram(None, &b"d"[..]).unwrap(), 1);

		let handle = broadcast.consume().track("t").unwrap();
		let fetched = fetch_group(&handle, None)
			.now_or_never()
			.expect("the datagram is cached");
		assert!(matches!(fetched, Err(moq_net::Error::NotFetchable)));
	}
}
