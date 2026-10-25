//! Consuming opaque values and their timestamps from a track.

use std::task::Poll;

use bytes::Bytes;

use crate::Result;

pub use super::Config;

/// Consumes opaque states in order with `next`, or skips to the newest with `latest`.
pub struct Consumer {
	track: moq_net::track::Ordered,
	group: Option<moq_net::group::Consumer>,
	/// The DEFLATE decoder for the current group, `Some` while decompressing. A snapshot group is
	/// normally one frame, but the window is per group either way.
	flate: Option<crate::Decoder>,
	compression: bool,
}

impl Consumer {
	/// Create a consumer reading from the given track subscriber.
	///
	/// Set [`Config::compression`] to match the producer that wrote the track.
	pub fn new(track: moq_net::track::Subscriber, config: Config) -> Self {
		Self {
			track: track.ordered(),
			group: None,
			flate: None,
			compression: config.compression.is_deflate(),
		}
	}

	/// Get the next state and its exact frame timestamp, or `None` once the track ends.
	pub async fn next(&mut self) -> Result<Option<moq_net::Timed<Bytes>>> {
		kio::wait(|waiter| self.poll_next(waiter)).await
	}

	/// Poll for each state in order, retaining the subscription's drift budget.
	///
	/// Finish the current group before taking the next. Lost groups resynchronize from a
	/// later snapshot. A timed playhead reader sets its subscription's `max_delay` to its
	/// distance behind live; the default zero budget keeps only groups not yet superseded.
	pub fn poll_next(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<moq_net::Timed<Bytes>>>> {
		self.poll_read(waiter, false)
	}

	/// Skip to the newest buffered state and its timestamp, or wait for a new one.
	pub async fn latest(&mut self) -> Result<Option<moq_net::Timed<Bytes>>> {
		kio::wait(|waiter| self.poll_latest(waiter)).await
	}

	/// Poll for the newest state, discarding superseded groups and intermediate yields.
	///
	/// Frames inside the newest group are still decoded in order. What this skips is gone;
	/// a subsequent `next` resumes after it. An untimed frame keeps `at: None`.
	pub fn poll_latest(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<moq_net::Timed<Bytes>>>> {
		self.poll_read(waiter, true)
	}

	fn poll_read(&mut self, waiter: &kio::Waiter, skip: bool) -> Poll<Result<Option<moq_net::Timed<Bytes>>>> {
		if skip {
			// Jump to the newest group the reader already has; a newer one supersedes the rest.
			// Taking groups, not the track's largest position, ignores datagrams (which share
			// the sequence space) and stays inside any cap the caller set on the reader.
			while let Poll::Ready(Some(group)) = self.track.poll_next_group(waiter)? {
				self.group = Some(group);
				self.flate = self.compression.then(crate::Decoder::new);
			}
		}

		let mut latest = None;
		loop {
			let Some(group) = &mut self.group else {
				match self.track.poll_next_group(waiter)? {
					Poll::Ready(Some(group)) => {
						self.group = Some(group);
						self.flate = self.compression.then(crate::Decoder::new);
						continue;
					}
					Poll::Ready(None) => return Poll::Ready(Ok(latest)),
					Poll::Pending => break,
				}
			};

			match group.poll_read_frame(waiter) {
				Poll::Ready(Ok(Some(frame))) => {
					let value = moq_net::Timed {
						value: self.decode(&frame.payload)?,
						at: frame.timestamp,
					};
					if !skip {
						return Poll::Ready(Ok(Some(value)));
					}
					latest = Some(value);
				}
				Poll::Ready(Ok(None)) => self.group = None,
				// Judged by stream code, so a group the publisher reset as old is the same loss as
				// one this side dropped.
				Poll::Ready(Err(err))
					if matches!(
						moq_net::StreamError::from(&err),
						moq_net::StreamError::Old
							| moq_net::StreamError::Evicted
							| moq_net::StreamError::TooFarBehind
							| moq_net::StreamError::GroupTooLarge
					) =>
				{
					// A later group starts a fresh snapshot and compression window.
					tracing::warn!(track = self.track.name(), group = group.sequence, error = ?err,
						"snapshot group lost; waiting for a newer one");
					self.group = None;
				}
				Poll::Ready(Err(err)) => return Poll::Ready(Err(err.into())),
				Poll::Pending => break,
			}
		}
		match latest {
			Some(value) => Poll::Ready(Ok(Some(value))),
			None => Poll::Pending,
		}
	}

	/// Decompress one frame, if the track is compressed.
	fn decode(&mut self, payload: &Bytes) -> Result<Bytes> {
		Ok(match self.flate.as_mut() {
			Some(flate) => flate.frame(payload)?,
			None => payload.clone(),
		})
	}
}

#[cfg(test)]
mod timestamps {
	use super::*;

	#[test]
	fn exact_timestamps_survive_reads() {
		for compression in [crate::Compression::None, crate::Compression::Deflate] {
			let scale = moq_net::Timescale::new(90_000).unwrap();
			let track = moq_net::broadcast::Info::new()
				.produce()
				.create_track("data", moq_net::track::Info::default().with_timescale(scale))
				.unwrap();
			let mut subscriber = track.subscribe(None);
			let mut subscription = subscriber.subscription();
			subscription.max_delay = None;
			subscriber.update(subscription).unwrap();
			let config = crate::snapshot::Config { compression };
			let mut producer = crate::snapshot::Producer::new(track, config.clone());
			let mut consumer = Consumer::new(subscriber, config);
			let mut newest = Consumer::new(producer.consume(), crate::snapshot::Config { compression });
			let times = [0, 810_001, 990_007];
			for (n, ticks) in times.into_iter().enumerate() {
				let at = moq_net::Timestamp::new(ticks, scale).unwrap();
				producer
					.update(moq_net::Timed {
						value: Bytes::from(vec![n as u8; 32]),
						at: Some(at),
					})
					.unwrap();
			}
			producer.finish().unwrap();
			for (n, ticks) in times.into_iter().enumerate() {
				let Poll::Ready(Ok(Some(value))) = consumer.poll_next(&kio::Waiter::noop()) else {
					panic!("missing state {n}")
				};
				assert_eq!(value.value[0] as u64, n as u64);
				let at = value.at.unwrap();
				assert_eq!(at.value(), ticks);
				assert_eq!(at.scale(), scale);
			}
			assert!(matches!(
				consumer.poll_next(&kio::Waiter::noop()),
				Poll::Ready(Ok(None))
			));
			let Poll::Ready(Ok(Some(value))) = newest.poll_latest(&kio::Waiter::noop()) else {
				panic!("missing latest state")
			};
			assert_eq!(value.value[0] as u64, 2);
			assert_eq!(value.at.unwrap().value(), 990_007);
			assert_eq!(value.at.unwrap().scale(), scale);
		}
	}

	#[test]
	fn latest_advances_the_same_cursor_as_next() {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("data", moq_net::track::Info::default())
			.unwrap();
		let subscriber = track.subscribe(None);
		let config = crate::snapshot::Config {
			compression: crate::Compression::Deflate,
		};
		let mut producer = crate::snapshot::Producer::new(track, config.clone());
		let mut consumer = Consumer::new(subscriber, config);
		let waiter = kio::Waiter::noop();
		for n in 0..3 {
			producer.update(Bytes::from(vec![n as u8; 32])).unwrap();
			if n == 0 {
				let Poll::Ready(Ok(Some(value))) = consumer.poll_next(&waiter) else {
					panic!("missing first state")
				};
				assert_eq!(value.value[0] as u64, 0);
				assert_eq!(value.at, None);
			}
		}
		let Poll::Ready(Ok(Some(value))) = consumer.poll_latest(&waiter) else {
			panic!("missing latest state")
		};
		assert_eq!(value.value[0] as u64, 2);
		assert_eq!(value.at, None);
		assert!(consumer.poll_next(&waiter).is_pending());
		let n = 3;
		producer.update(Bytes::from(vec![n as u8; 32])).unwrap();
		let Poll::Ready(Ok(Some(value))) = consumer.poll_next(&waiter) else {
			panic!("missing new state")
		};
		assert_eq!(value.value[0] as u64, 3);
		producer.finish().unwrap();
		assert!(matches!(consumer.poll_latest(&waiter), Poll::Ready(Ok(None))));
	}

	#[test]
	fn latest_keeps_a_cap_set_before_the_consumer() {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("data", moq_net::track::Info::default())
			.unwrap();
		let mut subscriber = track.subscribe(None);
		subscriber = subscriber.with_range(..5);
		let config = crate::snapshot::Config::default();
		let mut producer = crate::snapshot::Producer::new(track, config.clone());
		let mut consumer = Consumer::new(subscriber, config);
		let waiter = kio::Waiter::noop();
		for n in 0..4u8 {
			producer.update(Bytes::from(vec![n; 32])).unwrap();
		}
		let Poll::Ready(Ok(Some(value))) = consumer.poll_latest(&waiter) else {
			panic!("missing latest state")
		};
		assert_eq!(value.value[0], 3);
		for n in 4..7u8 {
			producer.update(Bytes::from(vec![n; 32])).unwrap();
		}
		let Poll::Ready(Ok(Some(value))) = consumer.poll_latest(&waiter) else {
			panic!("missing capped state")
		};
		assert_eq!(value.value[0], 4, "latest crossed the cap");
		assert!(consumer.poll_next(&waiter).is_pending());
	}

	#[test]
	fn latest_ignores_datagrams() {
		let track = moq_net::broadcast::Info::new()
			.produce()
			.create_track("data", moq_net::track::Info::default())
			.unwrap();
		let mut raw = track.clone();
		let subscriber = track.subscribe(None);
		let config = crate::snapshot::Config::default();
		let mut producer = crate::snapshot::Producer::new(track, config.clone());
		let mut consumer = Consumer::new(subscriber, config);
		let waiter = kio::Waiter::noop();
		for n in 0..2u8 {
			producer.update(Bytes::from(vec![n; 32])).unwrap();
		}
		// A datagram shares the sequence space, so it outnumbers every group.
		raw.append_datagram(None, &b"noise"[..]).unwrap();
		let Poll::Ready(Ok(Some(value))) = consumer.poll_latest(&waiter) else {
			panic!("a datagram hid the latest state")
		};
		assert_eq!(value.value[0], 1);
	}
}
