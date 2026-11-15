/// Fetch one group of `track`: the one that starts at `start`, or the newest when `None`.
///
/// The newest needs a live subscription to learn its start, since a fetch can
/// only retrieve a start already known. Once known it is fetched rather than
/// read off the subscription, so an evicted group is retrieved from upstream
/// instead of waited on forever. Fails before returning when the group can never
/// be served: [`moq_net::Error::NotFound`] locally, including a track that ends
/// before any group, or [`moq_net::StreamError::NotFound`] from upstream.
pub async fn fetch_group(
	track: &moq_net::track::Consumer,
	start: Option<u64>,
) -> moq_net::Result<moq_net::group::Consumer> {
	if let Some(start) = start {
		return track.fetch_group(start, None).await;
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
		let mut first = track
			.append_group(track.largest().map_or(0, |largest| largest.group + 1))
			.unwrap();
		first
			.write_frame(moq_net::Timestamp::unscaled(first.start), &b"a"[..])
			.unwrap();
		first.close().unwrap();
		let _open = track
			.append_group(track.largest().map_or(0, |largest| largest.group + 1))
			.unwrap();

		let handle = broadcast.consume().track("t").unwrap();
		let group = fetch_group(&handle, None)
			.now_or_never()
			.expect("the newest group is cached")
			.unwrap();
		assert_eq!(group.start, 1);
	}
}
