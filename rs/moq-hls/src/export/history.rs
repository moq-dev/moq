use std::ops::Range;
use std::time::Duration;

use hang::timeline::Record;
use moq_json::window;
use moq_mux::timeline::Entry;

/// One edit to a durable timeline's window.
pub(super) enum Edit {
	/// A record became visible at this window index, which equals its sequence.
	Push { index: u64, entry: Entry },
	/// These window indices were removed.
	Pop(Range<u64>),
}

/// Read `track`'s durable timeline from group 0, FETCHing groups the cache evicted, then follow
/// live edits. Every reader of a history-mode timeline (the reference and each rendition's spans)
/// shares this, so all of them recover the same evicted history.
pub(super) async fn read(
	broadcast: &moq_net::broadcast::Consumer,
	section: &hang::catalog::Archive,
	track: &str,
	mut apply: impl FnMut(Edit),
) -> crate::Result<()> {
	let name = section
		.timelines
		.get(track)
		.ok_or_else(|| moq_mux::Error::TimelineMissing(track.to_string()))?;
	let scale = moq_net::Timescale::new(section.timescale as u64)
		.map_err(|_| moq_mux::Error::InvalidTimescale(section.timescale))?;
	let track = broadcast.track(name)?;
	// The largest delay the wire can carry (2^62 - 1 ms), so no history group is skipped as late.
	let subscription = moq_net::track::Subscription::default()
		.with_start(moq_net::track::Position::group(0))
		.with_max_delay(Duration::from_millis((1_u64 << 62) - 1));
	let mut live = track.subscribe(subscription).await?.ordered();
	let mut decoder = window::Decoder::<Record>::new(window::ConsumerConfig::default().with_compression(true));
	let mut next = 0;
	loop {
		let mut head = live.next_group().await?;
		let finished = head.is_none();
		// A finished track can have no groups left in cache. Its high-water mark
		// still tells us which stored groups must be fetched before returning.
		let end = head.as_ref().map(|group| group.sequence).or_else(|| track.latest());
		while end.is_some_and(|end| next <= end) {
			let mut group = if head.as_ref().is_some_and(|group| group.sequence == next) {
				head.take().unwrap()
			} else {
				match track.fetch_group(next, None).await {
					Ok(group) => group,
					// Retention prunes a recording's start, which this path cannot list from yet.
					Err(moq_net::Error::NotFound) if next == 0 => {
						return Err(crate::Error::HistoryPruned(name.clone()));
					}
					Err(err) => return Err(err.into()),
				}
			};
			let mut decoded = decoder.group();
			while let Some(frame) = group.read_frame().await? {
				decoded.decode(&frame.payload).map_err(moq_mux::Error::from)?;
				while let Some(event) = decoded.next_event() {
					match event {
						window::Event::Push { index, value } => {
							if index != value.sequence {
								return Err(moq_mux::Error::TimelineCheckpoint(index).into());
							}
							apply(Edit::Push {
								index,
								entry: Entry::decode(value, scale)?,
							});
						}
						window::Event::Pop(range) => apply(Edit::Pop(range)),
						// A durable path must not present missing history as a complete
						// recording. Cached/live consumption may skip; this path cannot.
						window::Event::Skip(_) => return Err(moq_net::Error::NotFound.into()),
						_ => unreachable!("unknown timeline window event"),
					}
				}
			}
			next += 1;
		}
		if finished {
			return Ok(());
		}
	}
}
