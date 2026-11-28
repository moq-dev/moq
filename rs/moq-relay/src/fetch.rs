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

	// The largest names the newest group, possibly before it arrives.
	let mut subscriber = track.subscribe(None).await?;
	match subscriber.largest().await {
		Some(largest) => track.fetch_group(largest.group, None).await,
		None => Err(moq_net::Error::NotFound),
	}
}
