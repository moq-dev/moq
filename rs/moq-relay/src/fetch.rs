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
	match subscriber.latest() {
		Some(sequence) => track.fetch_group(sequence, None).await,
		None => subscriber.recv_group().await?.ok_or(moq_net::Error::NotFound),
	}
}
