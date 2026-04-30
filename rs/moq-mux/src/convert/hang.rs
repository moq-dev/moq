use std::collections::HashMap;
use std::task::Poll;

use hang::catalog::Container;
use hang::container::Frame;

/// Converts a broadcast from any format to hang/Legacy format.
///
/// If tracks are already Legacy, they are inserted directly (no copy).
/// If tracks are CMAF, parses moof+mdat and converts to hang frames.
pub struct Convert {
	input: moq_lite::BroadcastConsumer,
	output: moq_lite::BroadcastProducer,
	catalog_consumer: Option<hang::CatalogConsumer>,
	catalog_producer: crate::import::CatalogProducer,
	tracks: HashMap<String, TrackState>,
}

enum TrackState {
	/// Legacy track passed through without conversion (pumps groups input → output).
	Passthrough(Box<PassthroughTrack>),
	/// CMAF track being converted to Legacy.
	Convert(Box<ConvertTrack>),
}

impl Convert {
	pub fn new(input: moq_lite::BroadcastConsumer, mut output: moq_lite::BroadcastProducer) -> anyhow::Result<Self> {
		let catalog_producer = crate::import::CatalogProducer::new(&mut output)?;

		let catalog_track =
			input.subscribe_track(&hang::Catalog::default_track(), moq_lite::Subscription::default())?;
		let catalog_consumer = hang::CatalogConsumer::new(catalog_track);

		Ok(Self {
			input,
			output,
			catalog_consumer: Some(catalog_consumer),
			catalog_producer,
			tracks: HashMap::new(),
		})
	}

	/// Poll the converter forward.
	pub fn poll(&mut self, waiter: &conducer::Waiter) -> Poll<anyhow::Result<()>> {
		if let Some(consumer) = &mut self.catalog_consumer {
			if let Poll::Ready(catalog) = consumer.poll_next(waiter)? {
				match catalog {
					Some(catalog) => self.update_catalog(&catalog)?,
					None => self.catalog_consumer = None,
				}
			}
		}

		let mut error = None;
		self.tracks.retain(|_, t| match t {
			TrackState::Passthrough(p) => match p.poll(waiter) {
				Poll::Pending => true,
				Poll::Ready(Ok(())) => false,
				Poll::Ready(Err(e)) => {
					error.get_or_insert(e);
					false
				}
			},
			TrackState::Convert(c) => match c.poll(waiter) {
				Poll::Pending => true,
				Poll::Ready(Ok(())) => false,
				Poll::Ready(Err(e)) => {
					error.get_or_insert(e);
					false
				}
			},
		});

		if let Some(e) = error {
			return Poll::Ready(Err(e));
		}

		if self.catalog_consumer.is_none() && self.tracks.is_empty() {
			let _ = self.catalog_producer.finish();
			return Poll::Ready(Ok(()));
		}

		Poll::Pending
	}

	fn update_catalog(&mut self, catalog: &hang::Catalog) -> anyhow::Result<()> {
		let mut guard = self.catalog_producer.lock();

		let mut active: HashMap<&str, ()> = HashMap::new();

		for (name, config) in &catalog.video.renditions {
			active.insert(name, ());

			if self.tracks.contains_key(name) {
				continue;
			}

			match &config.container {
				Container::Legacy => {
					let input_track = self
						.input
						.subscribe_track(&moq_lite::Track::new(name.clone()), moq_lite::Subscription::default())?;
					let output_track = self.output.create_track(moq_lite::Track::new(name.clone()))?;
					self.tracks.insert(
						name.clone(),
						TrackState::Passthrough(Box::new(PassthroughTrack::new(input_track, output_track))),
					);
					guard.video.renditions.insert(name.clone(), config.clone());
				}
				Container::Cmaf { init } => {
					let timescale = crate::container::cmaf::parse_timescale(init)?;

					let input_track = self
						.input
						.subscribe_track(&moq_lite::Track::new(name.clone()), moq_lite::Subscription::default())?;

					let mut legacy_config = config.clone();
					legacy_config.container = Container::Legacy;
					guard.video.renditions.insert(name.clone(), legacy_config);

					let output_track = self.output.create_track(moq_lite::Track::new(name.clone()))?;
					self.tracks.insert(
						name.clone(),
						TrackState::Convert(Box::new(ConvertTrack::new(input_track, output_track, timescale))),
					);
				}
			}
		}

		for (name, config) in &catalog.audio.renditions {
			active.insert(name, ());

			if self.tracks.contains_key(name) {
				continue;
			}

			match &config.container {
				Container::Legacy => {
					let input_track = self
						.input
						.subscribe_track(&moq_lite::Track::new(name.clone()), moq_lite::Subscription::default())?;
					let output_track = self.output.create_track(moq_lite::Track::new(name.clone()))?;
					self.tracks.insert(
						name.clone(),
						TrackState::Passthrough(Box::new(PassthroughTrack::new(input_track, output_track))),
					);
					guard.audio.renditions.insert(name.clone(), config.clone());
				}
				Container::Cmaf { init } => {
					let timescale = crate::container::cmaf::parse_timescale(init)?;

					let input_track = self
						.input
						.subscribe_track(&moq_lite::Track::new(name.clone()), moq_lite::Subscription::default())?;

					let mut legacy_config = config.clone();
					legacy_config.container = Container::Legacy;
					guard.audio.renditions.insert(name.clone(), legacy_config);

					let output_track = self.output.create_track(moq_lite::Track::new(name.clone()))?;
					self.tracks.insert(
						name.clone(),
						TrackState::Convert(Box::new(ConvertTrack::new(input_track, output_track, timescale))),
					);
				}
			}
		}

		// Remove tracks that are no longer in the catalog.
		self.tracks.retain(|name, _| {
			if active.contains_key(name.as_str()) {
				return true;
			}
			let _ = self.output.remove_track(name);
			false
		});
		guard
			.video
			.renditions
			.retain(|name, _| active.contains_key(name.as_str()));
		guard
			.audio
			.renditions
			.retain(|name, _| active.contains_key(name.as_str()));

		Ok(())
	}

	/// Run the converter to completion.
	pub async fn run(mut self) -> anyhow::Result<()> {
		conducer::wait(|w| self.poll(w)).await
	}
}

/// Poll-based passthrough for a single track that's already Legacy.
///
/// Pumps groups + frames from input → output without re-encoding.
struct PassthroughTrack {
	input: moq_lite::TrackSubscriber,
	output: moq_lite::TrackProducer,
	groups: Vec<(moq_lite::GroupConsumer, moq_lite::GroupProducer)>,
	finished: bool,
}

impl PassthroughTrack {
	fn new(input: moq_lite::TrackSubscriber, output: moq_lite::TrackProducer) -> Self {
		Self {
			input,
			output,
			groups: Vec::new(),
			finished: false,
		}
	}

	fn poll(&mut self, waiter: &conducer::Waiter) -> Poll<anyhow::Result<()>> {
		while !self.finished {
			match self.input.poll_recv_group(waiter) {
				Poll::Ready(Ok(Some(group))) => {
					let info = moq_lite::Group {
						sequence: group.sequence,
					};
					let out_group = self.output.create_group(info)?;
					self.groups.push((group, out_group));
				}
				Poll::Ready(Ok(None)) => self.finished = true,
				Poll::Ready(Err(e)) => return Poll::Ready(Err(e.into())),
				Poll::Pending => break,
			}
		}

		self.groups.retain_mut(|(reader, writer)| {
			loop {
				match reader.poll_read_frame(waiter) {
					Poll::Ready(Ok(Some(data))) => {
						if let Err(e) = writer.write_frame(data) {
							tracing::error!(%e, "passthrough write_frame failed");
							return false;
						}
					}
					Poll::Ready(Ok(None)) => {
						let _ = writer.finish();
						return false;
					}
					Poll::Ready(Err(_)) => return false,
					Poll::Pending => return true,
				}
			}
		});

		if self.finished && self.groups.is_empty() {
			self.output.finish()?;
			Poll::Ready(Ok(()))
		} else {
			Poll::Pending
		}
	}
}

/// Poll-based CMAF-to-Legacy converter for a single track.
///
/// Receives groups independently and converts each one without ordering across groups.
struct ConvertTrack {
	input: moq_lite::TrackSubscriber,
	output: moq_lite::TrackProducer,
	timescale: u64,
	/// Active input groups being read, each with its corresponding output group.
	groups: Vec<(moq_lite::GroupConsumer, moq_lite::GroupProducer)>,
	finished: bool,
}

impl ConvertTrack {
	fn new(input: moq_lite::TrackSubscriber, output: moq_lite::TrackProducer, timescale: u64) -> Self {
		Self {
			input,
			output,
			timescale,
			groups: Vec::new(),
			finished: false,
		}
	}

	fn poll(&mut self, waiter: &conducer::Waiter) -> Poll<anyhow::Result<()>> {
		// 1. Poll for new input groups
		while !self.finished {
			match self.input.poll_recv_group(waiter) {
				Poll::Ready(Ok(Some(group))) => {
					let out_group = self.output.append_group()?;
					self.groups.push((group, out_group));
				}
				Poll::Ready(Ok(None)) => self.finished = true,
				Poll::Ready(Err(e)) => return Poll::Ready(Err(e.into())),
				Poll::Pending => break,
			}
		}

		// 2. Poll all active groups for frames, converting each independently
		let timescale = self.timescale;

		self.groups.retain_mut(|(reader, writer)| {
			loop {
				match reader.poll_read_frame(waiter) {
					Poll::Ready(Ok(Some(data))) => {
						// Decode CMAF moof+mdat into media frames
						let frames = match crate::container::cmaf::decode(data, timescale) {
							Ok(f) => f,
							Err(e) => {
								tracing::error!(%e, "cmaf decode failed");
								return false;
							}
						};

						// Encode each as hang Legacy frame
						for decoded in frames {
							let frame = Frame {
								timestamp: decoded.timestamp,
								payload: decoded.payload.into(),
							};
							if let Err(e) = frame.encode(writer) {
								tracing::error!(%e, "legacy encode failed");
								return false;
							}
						}
					}
					Poll::Ready(Ok(None)) => {
						let _ = writer.finish();
						return false;
					}
					Poll::Ready(Err(_)) => return false,
					Poll::Pending => return true,
				}
			}
		});

		// 3. Done when input finished and all groups drained
		if self.finished && self.groups.is_empty() {
			self.output.finish()?;
			Poll::Ready(Ok(()))
		} else {
			Poll::Pending
		}
	}
}

#[cfg(test)]
mod test {
	use hang::container::Timestamp;

	use crate::convert::cmaf::test::*;

	fn ts(micros: u64) -> Timestamp {
		Timestamp::from_micros(micros).unwrap()
	}

	#[tokio::test]
	async fn cmaf_to_legacy_video() {
		tokio::time::pause();

		use hang::catalog::Container;

		let config = test_video_config();
		let init_data = crate::convert::cmaf::build_video_init(&config).unwrap();
		let timescale = config.framerate.map(|f| (f * 1000.0) as u64).unwrap();

		let cmaf_frames: Vec<(u64, Vec<u8>, bool)> = vec![
			(0, vec![0x01, 0x02, 0x03], true),
			(33_000u64 * timescale / 1_000_000, vec![0x04, 0x05], false),
			(66_000u64 * timescale / 1_000_000, vec![0x06, 0x07, 0x08], true),
		];

		let mut cmaf_config = config.clone();
		cmaf_config.container = Container::Cmaf { init: init_data.into() };

		let (consumer, mut video_track, _broadcast, mut catalog_track) = setup_input(&cmaf_config);
		let output = moq_lite::Broadcast::new().produce();
		let output_consumer = output.consume();
		let converter = super::Convert::new(consumer, output).unwrap();

		let cmaf_frames_clone = cmaf_frames.clone();
		tokio::spawn(async move {
			tokio::task::yield_now().await;
			write_cmaf_frames(&mut video_track, &cmaf_frames_clone);
			catalog_track.finish().unwrap();
		});

		let (convert_result, legacy_frames) = tokio::join!(converter.run(), async {
			let output_video = subscribe_video(&output_consumer).await;
			read_legacy_frames(output_video).await
		});
		convert_result.unwrap();

		assert_eq!(legacy_frames.len(), 3, "expected 3 Legacy frames");
		assert_eq!(legacy_frames[0].0, ts(0));
		assert_eq!(legacy_frames[0].1, vec![0x01, 0x02, 0x03]);
		assert_eq!(legacy_frames[1].0, ts(33_000));
		assert_eq!(legacy_frames[1].1, vec![0x04, 0x05]);
		assert_eq!(legacy_frames[2].0, ts(66_000));
		assert_eq!(legacy_frames[2].1, vec![0x06, 0x07, 0x08]);
	}

	#[tokio::test]
	async fn legacy_passthrough() {
		tokio::time::pause();
		let config = test_video_config();
		let frames = vec![(ts(0), vec![0xAA, 0xBB], true), (ts(33_000), vec![0xCC, 0xDD], false)];

		let (consumer, mut video_track, _broadcast, mut catalog_track) = setup_input(&config);
		let output = moq_lite::Broadcast::new().produce();
		let output_consumer = output.consume();

		let converter = super::Convert::new(consumer, output).unwrap();

		let frames_clone = frames.clone();
		tokio::spawn(async move {
			tokio::task::yield_now().await;
			write_legacy_frames(&mut video_track, &frames_clone);
			catalog_track.finish().unwrap();
		});

		let (convert_result, result) = tokio::join!(converter.run(), async {
			let output_video = subscribe_video(&output_consumer).await;
			read_legacy_frames(output_video).await
		});
		convert_result.expect("converter.run() failed");

		assert_eq!(result.len(), frames.len());

		for (i, (expected_ts, expected_payload, _)) in frames.iter().enumerate() {
			assert_eq!(result[i].0, *expected_ts, "timestamp mismatch at frame {i}");
			assert_eq!(result[i].1, *expected_payload, "payload mismatch at frame {i}");
		}
	}
}
