use std::time::Duration;

use hang::timeline::Record;
use moq_json::window;
use moq_mux::timeline::Entry;

use super::{Reference, renditions};

/// Read every durable timeline group, recovering gaps before following live edits.
pub(super) async fn watch(
	broadcast: &moq_net::broadcast::Consumer,
	section: &hang::catalog::Archive,
	reference: &Reference,
	renditions: &renditions::Fanout,
) -> crate::Result<()> {
	let name = section
		.timelines
		.get(&reference.1)
		.ok_or_else(|| moq_mux::Error::TimelineMissing(reference.1.clone()))?;
	let scale = moq_net::Timescale::new(section.timescale as u64)
		.map_err(|_| moq_mux::Error::InvalidTimescale(section.timescale))?;
	let track = broadcast.track(name)?;
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
				track.fetch_group(next, None).await?
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
							renditions.push(index, Entry::decode(value, scale)?, reference);
						}
						window::Event::Pop(range) => renditions.pop(range),
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
