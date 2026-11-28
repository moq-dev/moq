/// Fetch one group of `track`: the one that starts at `start`, or the newest when `None`.
///
/// The newest is read off a live subscription, the one request every copy answers, including
/// one received over moq-lite-05/06, whose upstream can't be asked by key. Fails before
/// returning when the group can never be served: [`moq_net::Error::NotFound`] locally,
/// including a track that ends before any group, or [`moq_net::StreamError::NotFound`] from
/// upstream.
pub async fn fetch_group(
	track: &moq_net::track::Consumer,
	start: Option<u64>,
) -> moq_net::Result<moq_net::group::Consumer> {
	if let Some(start) = start {
		return track.fetch_group(start, None).await;
	}

	let mut subscriber = track.subscribe(None).await?;
	subscriber.recv_group().await?.ok_or(moq_net::Error::NotFound)
}
