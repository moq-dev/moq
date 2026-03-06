use bytes::Buf;
use tokio::sync::oneshot;

use crate::ffi::OnStatus;
use crate::{Error, Id, NonZeroSlab, State};

pub struct VideoConfigData {
	pub name: String,
	pub codec: String,
	pub description: Option<Vec<u8>>,
	pub coded_width: Option<u32>,
	pub coded_height: Option<u32>,
}

pub struct AudioConfigData {
	pub name: String,
	pub codec: String,
	pub description: Option<Vec<u8>>,
	pub sample_rate: u32,
	pub channel_count: u32,
}

struct ConsumeCatalog {
	broadcast: moq_lite::BroadcastConsumer,

	catalog: hang::catalog::Catalog,

	/// We need to store the codec information on the heap unfortunately.
	audio_codec: Vec<String>,
	video_codec: Vec<String>,
}

#[derive(Default)]
pub struct Consume {
	/// Active broadcast consumers.
	broadcast: NonZeroSlab<moq_lite::BroadcastConsumer>,

	/// Active catalog consumers and their broadcast references.
	catalog: NonZeroSlab<ConsumeCatalog>,

	/// Catalog consumer task cancellation channels.
	catalog_task: NonZeroSlab<oneshot::Sender<()>>,

	/// Audio track consumer task cancellation channels.
	audio_task: NonZeroSlab<oneshot::Sender<()>>,

	/// Video track consumer task cancellation channels.
	video_task: NonZeroSlab<oneshot::Sender<()>>,

	/// Buffered frames ready for consumption.
	frame: NonZeroSlab<hang::container::OrderedFrame>,
}

impl Consume {
	pub fn start(&mut self, broadcast: moq_lite::BroadcastConsumer) -> Id {
		self.broadcast.insert(broadcast)
	}

	pub fn catalog(&mut self, broadcast: Id, mut on_catalog: OnStatus) -> Result<Id, Error> {
		let broadcast = self.broadcast.get(broadcast).ok_or(Error::BroadcastNotFound)?.clone();
		let catalog = broadcast.subscribe_track(&hang::catalog::Catalog::default_track())?;

		let channel = oneshot::channel();
		let id = self.catalog_task.insert(channel.0);

		tokio::spawn(async move {
			let res = tokio::select! {
				res = Self::run_catalog(broadcast, catalog.into(), &mut on_catalog) => res,
				_ = channel.1 => Ok(()),
			};
			on_catalog.call(res);

			State::lock().consume.catalog_task.remove(id);
		});

		Ok(id)
	}

	async fn run_catalog(
		broadcast: moq_lite::BroadcastConsumer,
		mut catalog: hang::CatalogConsumer,
		on_catalog: &mut OnStatus,
	) -> Result<(), Error> {
		while let Some(catalog) = catalog.next().await? {
			// Unfortunately we need to store the codec information on the heap.
			let audio_codec = catalog
				.audio
				.renditions
				.values()
				.map(|config| config.codec.to_string())
				.collect();

			let video_codec = catalog
				.video
				.renditions
				.values()
				.map(|config| config.codec.to_string())
				.collect();

			let catalog = ConsumeCatalog {
				broadcast: broadcast.clone(),
				catalog,
				audio_codec,
				video_codec,
			};

			let id = State::lock().consume.catalog.insert(catalog);

			// Important: Don't hold the mutex during this callback.
			on_catalog.call(Ok(id));
		}

		Ok(())
	}

	/// Returns video config fields as owned Rust values.
	pub fn video_config_data(&self, catalog: Id, index: usize) -> Result<VideoConfigData, Error> {
		let consume = self.catalog.get(catalog).ok_or(Error::CatalogNotFound)?;
		let (rendition, config) = consume
			.catalog
			.video
			.renditions
			.iter()
			.nth(index)
			.ok_or(Error::NoIndex)?;
		let codec = consume.video_codec.get(index).ok_or(Error::NoIndex)?;
		Ok(VideoConfigData {
			name: rendition.clone(),
			codec: codec.clone(),
			description: config.description.as_ref().map(|d| d.to_vec()),
			coded_width: config.coded_width,
			coded_height: config.coded_height,
		})
	}

	/// Returns audio config fields as owned Rust values.
	pub fn audio_config_data(&self, catalog: Id, index: usize) -> Result<AudioConfigData, Error> {
		let consume = self.catalog.get(catalog).ok_or(Error::CatalogNotFound)?;
		let (rendition, config) = consume
			.catalog
			.audio
			.renditions
			.iter()
			.nth(index)
			.ok_or(Error::NoIndex)?;
		let codec = consume.audio_codec.get(index).ok_or(Error::NoIndex)?;
		Ok(AudioConfigData {
			name: rendition.clone(),
			codec: codec.clone(),
			description: config.description.as_ref().map(|d| d.to_vec()),
			sample_rate: config.sample_rate,
			channel_count: config.channel_count,
		})
	}

	pub fn catalog_close(&mut self, catalog: Id) -> Result<(), Error> {
		self.catalog_task.remove(catalog).ok_or(Error::CatalogNotFound)?;
		Ok(())
	}

	pub fn catalog_snapshot_close(&mut self, catalog: Id) -> Result<(), Error> {
		self.catalog.remove(catalog).ok_or(Error::CatalogNotFound)?;
		Ok(())
	}

	pub fn video_ordered(
		&mut self,
		catalog: Id,
		index: usize,
		latency: std::time::Duration,
		mut on_frame: OnStatus,
	) -> Result<Id, Error> {
		let consume = self.catalog.get(catalog).ok_or(Error::CatalogNotFound)?;
		let rendition = consume
			.catalog
			.video
			.renditions
			.keys()
			.nth(index)
			.ok_or(Error::TrackNotFound)?;

		let track = consume.broadcast.subscribe_track(&moq_lite::Track {
			name: rendition.clone(),
			priority: 1, // TODO: Remove priority
		})?;
		let track = hang::container::OrderedConsumer::new(track, latency);

		let channel = oneshot::channel();
		let id = self.video_task.insert(channel.0);

		tokio::spawn(async move {
			let res = tokio::select! {
				res = Self::run_track(track, &mut on_frame) => res,
				_ = channel.1 => Ok(()),
			};
			on_frame.call(res);

			// Make sure we clean up the task on exit.
			State::lock().consume.video_task.remove(id);
		});

		Ok(id)
	}

	pub fn audio_ordered(
		&mut self,
		catalog: Id,
		index: usize,
		latency: std::time::Duration,
		mut on_frame: OnStatus,
	) -> Result<Id, Error> {
		let consume = self.catalog.get(catalog).ok_or(Error::CatalogNotFound)?;
		let rendition = consume
			.catalog
			.audio
			.renditions
			.keys()
			.nth(index)
			.ok_or(Error::TrackNotFound)?;

		let track = consume.broadcast.subscribe_track(&moq_lite::Track {
			name: rendition.clone(),
			priority: 2, // TODO: Remove priority
		})?;
		let track = hang::container::OrderedConsumer::new(track, latency);

		let channel = oneshot::channel();
		let id = self.audio_task.insert(channel.0);

		tokio::spawn(async move {
			let res = tokio::select! {
				res = Self::run_track(track, &mut on_frame) => res,
				_ = channel.1 => Ok(()),
			};
			on_frame.call(res);

			// Make sure we clean up the task on exit.
			State::lock().consume.audio_task.remove(id);
		});

		Ok(id)
	}

	async fn run_track(mut track: hang::container::OrderedConsumer, on_frame: &mut OnStatus) -> Result<(), Error> {
		while let Some(mut ordered) = track.read().await? {
			// TODO add a chunking API so we don't have to (potentially) allocate a contiguous buffer for the frame.
			let mut new_payload = hang::container::BufList::new();
			new_payload.push_chunk(if ordered.payload.num_chunks() == 1 {
				// We can avoid allocating
				ordered.payload.get_chunk(0).expect("frame has zero chunks").clone()
			} else {
				// We need to allocate
				ordered.payload.copy_to_bytes(ordered.payload.num_bytes())
			});

			let new_frame = hang::container::OrderedFrame {
				timestamp: ordered.timestamp,
				payload: new_payload,
				group: ordered.group,
				index: ordered.index,
			};

			// Important: Don't hold the mutex during this callback.
			let id = State::lock().consume.frame.insert(new_frame);
			on_frame.call(Ok(id));
		}

		Ok(())
	}

	pub fn audio_close(&mut self, track: Id) -> Result<(), Error> {
		self.audio_task.remove(track).ok_or(Error::TrackNotFound)?;
		Ok(())
	}

	pub fn video_close(&mut self, track: Id) -> Result<(), Error> {
		self.video_task.remove(track).ok_or(Error::TrackNotFound)?;
		Ok(())
	}

	/// Returns frame data as owned Rust values.
	pub fn frame_data(&self, frame: Id) -> Result<(Vec<u8>, u64, bool), Error> {
		let frame = self.frame.get(frame).ok_or(Error::FrameNotFound)?;

		let payload: Vec<u8> = (0..frame.payload.num_chunks())
			.filter_map(|i| frame.payload.get_chunk(i))
			.flat_map(|chunk| chunk.iter().copied())
			.collect();

		let timestamp_us = frame
			.timestamp
			.as_micros()
			.try_into()
			.map_err(|_| moq_lite::TimeOverflow)?;

		Ok((payload, timestamp_us, frame.is_keyframe()))
	}

	pub fn frame_close(&mut self, frame: Id) -> Result<(), Error> {
		self.frame.remove(frame).ok_or(Error::FrameNotFound)?;
		Ok(())
	}

	pub fn close(&mut self, consume: Id) -> Result<(), Error> {
		self.broadcast.remove(consume).ok_or(Error::BroadcastNotFound)?;
		Ok(())
	}
}
