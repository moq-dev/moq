//! Keeping a cleanly ended broadcast up for subscribers that arrive late.

use std::{pin::Pin, task::Poll, time::Duration};

/// Keeps cleanly ended broadcasts announced, serving their finished tracks, for a fixed
/// duration, then closes them.
///
/// Subscribers already reading a track get its tail when the broadcast closes; this serves
/// the ones that arrive after the end, such as a relay or player that read the final catalog
/// late. Finish the catalog and media tracks first, then hand over the broadcasts and
/// `owner`, whatever holds their track producers: a finished track stops being served once
/// its producers drop, so the owner drops only after the broadcasts close.
///
/// The deadline is fixed at [`new`](Self::new), however late [`poll_wait`](Self::poll_wait) is
/// first polled. Dropping the handle closes the broadcasts at once, which is how an interrupt
/// ends it. A newer publisher announced at the same path does not end it: that one wins the
/// path while it is up, and readers that come back here find the finished catalog.
#[must_use = "dropping a Linger closes its broadcasts at once"]
pub struct Linger<T> {
	broadcasts: Vec<moq_net::broadcast::Producer>,
	// Dropped after `Drop::drop` closes the broadcasts.
	_owner: T,
	// `None` when the duration overflows the clock, so only an interrupt ends it.
	deadline: Option<moq_clock::Instant>,
	// Armed at the deadline, created on first poll.
	timer: Option<moq_clock::Sleep>,
}

impl Linger<()> {
	/// How long a publisher keeps a cleanly ended broadcast up unless configured otherwise.
	///
	/// Matches the default media max-age and a relay's idle track copy.
	pub const DEFAULT: Duration = Duration::from_secs(30);
}

impl<T> Linger<T> {
	/// Keep `broadcasts` up for `duration` from now, holding `owner` until they close.
	///
	/// A zero `duration` is due on the first poll.
	pub fn new(
		broadcasts: impl IntoIterator<Item = moq_net::broadcast::Producer>,
		owner: T,
		duration: Duration,
	) -> Self {
		Self {
			broadcasts: broadcasts.into_iter().collect(),
			_owner: owner,
			deadline: moq_clock::now().checked_add(duration),
			timer: None,
		}
	}

	/// Poll until the deadline passes on the ambient clock; drop the handle then to close the
	/// broadcasts.
	pub fn poll_wait(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let Some(deadline) = self.deadline else {
			return Poll::Pending;
		};
		let timer = self.timer.get_or_insert_with(|| moq_clock::sleep_until(deadline));
		waiter.poll_future(Pin::new(timer))
	}

	/// Wait for the deadline on the ambient clock, then close the broadcasts.
	pub async fn wait(mut self) {
		kio::wait(|waiter| self.poll_wait(waiter)).await
	}
}

impl<T> Drop for Linger<T> {
	fn drop(&mut self) {
		// Closed before the owner drops, so no lookup finds the broadcast without its tracks.
		for broadcast in &self.broadcasts {
			broadcast.close();
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const PATH: &str = "live";

	/// A broadcast at [`PATH`] whose one track, `video`, has three finished groups.
	fn finished(origin: &moq_net::origin::Producer) -> (moq_net::broadcast::Producer, moq_net::track::Producer) {
		let broadcast = origin.publish(PATH, moq_net::origin::Route::default()).unwrap();
		let track = broadcast.create_track("video", None).unwrap();
		for _ in 0..3 {
			let mut group = track.append_group().unwrap();
			group.write_frame(None, bytes::Bytes::from_static(b"frame")).unwrap();
			group.finish().unwrap();
		}
		track.finish().unwrap();
		(broadcast.broadcast().clone(), track)
	}

	/// Whether the origin has stopped routing [`PATH`].
	async fn unroutable(origin: &moq_net::origin::Producer) -> bool {
		matches!(
			origin.consume().request(PATH, None).await,
			Err(moq_net::Error::Unroutable)
		)
	}

	/// A subscriber arriving just before the deadline gets every group, the latest by
	/// subscribing and the earlier ones by fetching, and the path stops routing once it passes.
	#[tokio::test(start_paused = true)]
	async fn a_late_subscriber_drains_the_tail_until_the_deadline() {
		let origin = crate::source::produce_origin();
		let (broadcast, track) = finished(&origin);
		let start = moq_clock::now();
		let linger = tokio::spawn(Linger::new([broadcast], track, Duration::from_secs(30)).wait());

		moq_clock::sleep(Duration::from_secs(29)).await;
		let late = origin.consume().request(PATH, None).await.unwrap();
		let video = late.track("video").unwrap();
		for sequence in 0..3 {
			let mut group = video.fetch_group(sequence, None).await.unwrap();
			let frame = group.read_frame().await.unwrap().expect("a frame");
			assert_eq!(frame.payload.as_ref(), b"frame");
		}
		let mut live = video.subscribe(None).await.unwrap();
		assert!(live.recv_group().await.unwrap().is_some(), "the latest group");
		assert!(
			live.recv_group().await.unwrap().is_none(),
			"the track ends after its tail"
		);

		linger.await.unwrap();
		assert_eq!(moq_clock::now() - start, Duration::from_secs(30));
		assert!(unroutable(&origin).await);
	}

	/// The deadline counts from the end of the content, not from when the wait is polled.
	#[tokio::test(start_paused = true)]
	async fn the_deadline_is_fixed_at_the_end() {
		let origin = crate::source::produce_origin();
		let (broadcast, track) = finished(&origin);
		let start = moq_clock::now();
		let linger = Linger::new([broadcast], track, Duration::from_secs(30));

		moq_clock::sleep(Duration::from_secs(20)).await;
		linger.wait().await;
		assert_eq!(moq_clock::now() - start, Duration::from_secs(30));
	}

	/// Dropping the handle, as an interrupt does, closes the broadcast at once.
	#[tokio::test(start_paused = true)]
	async fn dropping_closes_at_once() {
		let origin = crate::source::produce_origin();
		let (broadcast, track) = finished(&origin);
		let linger = Linger::new([broadcast], track, Duration::from_secs(30));
		assert!(!unroutable(&origin).await);

		drop(linger);
		assert!(unroutable(&origin).await);
	}
}
