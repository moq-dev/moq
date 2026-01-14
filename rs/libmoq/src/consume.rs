use std::ffi::c_char;

use hang::TrackConsumer;
use moq_lite::coding::Buf;
use tokio::sync::oneshot;

use crate::ffi::OnStatus;
use crate::{Error, Id, NonZeroSlab, State, moq_audio_config, moq_frame, moq_video_config};

struct ConsumeCatalog {
	broadcast: hang::BroadcastConsumer,

	catalog: hang::catalog::Catalog,

	/// We need to store the codec information on the heap unfortunately.
	audio_codec: Vec<String>,
	video_codec: Vec<String>,
}

#[derive(Default)]
pub struct Consume {
	/// Active broadcast consumers.
	broadcast: NonZeroSlab<hang::BroadcastConsumer>,

	/// Active catalog consumers and their broadcast references.
	catalog: NonZeroSlab<ConsumeCatalog>,

	/// Catalog consumer task cancellation channels.
	catalog_task: NonZeroSlab<oneshot::Sender<()>>,

	/// Audio track consumer task cancellation channels.
	audio_task: NonZeroSlab<oneshot::Sender<()>>,

	/// Video track consumer task cancellation channels.
	video_task: NonZeroSlab<oneshot::Sender<()>>,

	/// Buffered frames ready for consumption.
	frame: NonZeroSlab<hang::Frame>,
}

impl Consume {
	pub fn start(&mut self, broadcast: hang::BroadcastConsumer) -> Id {
		self.broadcast.insert(broadcast)
	}

	pub fn catalog(&mut self, broadcast: Id, mut on_catalog: OnStatus) -> Result<Id, Error> {
		let broadcast = self.broadcast.get(broadcast).ok_or(Error::NotFound)?.clone();

		let channel = oneshot::channel();
		let id = self.catalog_task.insert(channel.0);

		tokio::spawn(async move {
			let res = tokio::select! {
				res = Self::run_catalog(broadcast, &mut on_catalog) => res,
				_ = channel.1 => Ok(()),
			};
			on_catalog.call(res);

			State::lock().consume.catalog_task.remove(id);
		});

		Ok(id)
	}

	async fn run_catalog(mut broadcast: hang::BroadcastConsumer, on_catalog: &mut OnStatus) -> Result<(), Error> {
		while let Some(catalog) = broadcast.catalog.next().await? {
			// Unfortunately we need to store the codec information on the heap.
			let audio_codec = catalog
				.audio
				.as_ref()
				.map(|audio| {
					audio
						.renditions
						.values()
						.map(|config| config.codec.to_string())
						.collect()
				})
				.unwrap_or_default();

			let video_codec = catalog
				.video
				.as_ref()
				.map(|video| {
					video
						.renditions
						.values()
						.map(|config| config.codec.to_string())
						.collect()
				})
				.unwrap_or_default();

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

	pub fn video_config(&mut self, catalog: Id, index: usize, dst: &mut moq_video_config) -> Result<(), Error> {
		let consume = self.catalog.get(catalog).ok_or(Error::NotFound)?;

		let video = consume.catalog.video.as_ref().ok_or(Error::NoIndex)?;
		let (rendition, config) = video.renditions.iter().nth(index).ok_or(Error::NoIndex)?;
		let codec = consume.video_codec.get(index).ok_or(Error::NoIndex)?;

		*dst = moq_video_config {
			name: rendition.as_str().as_ptr() as *const c_char,
			name_len: rendition.len(),
			codec: codec.as_str().as_ptr() as *const c_char,
			codec_len: codec.len(),
			description: config
				.description
				.as_ref()
				.map(|desc| desc.as_ptr())
				.unwrap_or(std::ptr::null()),
			description_len: config.description.as_ref().map(|desc| desc.len()).unwrap_or(0),
			coded_width: config
				.coded_width
				.as_ref()
				.map(|width| width as *const u32)
				.unwrap_or(std::ptr::null()),
			coded_height: config
				.coded_height
				.as_ref()
				.map(|height| height as *const u32)
				.unwrap_or(std::ptr::null()),
		};

		Ok(())
	}

	pub fn audio_config(&mut self, catalog: Id, index: usize, dst: &mut moq_audio_config) -> Result<(), Error> {
		let consume = self.catalog.get(catalog).ok_or(Error::NotFound)?;

		let audio = consume.catalog.audio.as_ref().ok_or(Error::NoIndex)?;
		let (rendition, config) = audio.renditions.iter().nth(index).ok_or(Error::NoIndex)?;
		let codec = consume.audio_codec.get(index).ok_or(Error::NoIndex)?;

		*dst = moq_audio_config {
			name: rendition.as_str().as_ptr() as *const c_char,
			name_len: rendition.len(),
			codec: codec.as_str().as_ptr() as *const c_char,
			codec_len: codec.len(),
			description: config
				.description
				.as_ref()
				.map(|desc| desc.as_ptr())
				.unwrap_or(std::ptr::null()),
			description_len: config.description.as_ref().map(|desc| desc.len()).unwrap_or(0),
			sample_rate: config.sample_rate,
			channel_count: config.channel_count,
		};

		Ok(())
	}

	pub fn catalog_close(&mut self, catalog: Id) -> Result<(), Error> {
		self.catalog.remove(catalog).ok_or(Error::NotFound)?;
		Ok(())
	}

	pub fn video_ordered(
		&mut self,
		catalog: Id,
		index: usize,
		latency: std::time::Duration,
		mut on_frame: OnStatus,
	) -> Result<Id, Error> {
		let consume = self.catalog.get(catalog).ok_or(Error::NotFound)?;
		let video = consume.catalog.video.as_ref().ok_or(Error::NotFound)?;
		let rendition = video.renditions.keys().nth(index).ok_or(Error::NotFound)?;

		let track = consume.broadcast.subscribe_track(&moq_lite::Track {
			name: rendition.clone(),
			priority: video.priority,
		});
		let track = TrackConsumer::new(track, latency);

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
		let consume = self.catalog.get(catalog).ok_or(Error::NotFound)?;
		let audio = consume.catalog.audio.as_ref().ok_or(Error::NotFound)?;
		let rendition = audio.renditions.keys().nth(index).ok_or(Error::NotFound)?;

		let track = consume.broadcast.subscribe_track(&moq_lite::Track {
			name: rendition.clone(),
			priority: audio.priority,
		});
		let track = TrackConsumer::new(track, latency);

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

	async fn run_track(mut track: TrackConsumer, on_frame: &mut OnStatus) -> Result<(), Error> {
		while let Some(mut frame) = track.read_frame().await? {
			// TODO add a chunking API so we don't have to (potentially) allocate a contiguous buffer for the frame.
			let mut new_payload = hang::BufList::new();
			new_payload.push_chunk(if frame.payload.num_chunks() == 1 {
				// We can avoid allocating
				frame.payload.get_chunk(0).expect("frame has zero chunks").clone()
			} else {
				// We need to allocate
				frame.payload.copy_to_bytes(frame.payload.num_bytes())
			});

			let new_frame = hang::Frame {
				payload: new_payload,
				timestamp: frame.timestamp,
				keyframe: frame.keyframe,
			};

			// Important: Don't hold the mutex during this callback.
			let id = State::lock().consume.frame.insert(new_frame);
			on_frame.call(Ok(id));
		}

		Ok(())
	}

	pub fn audio_close(&mut self, track: Id) -> Result<(), Error> {
		self.audio_task.remove(track).ok_or(Error::NotFound)?;
		Ok(())
	}

	pub fn video_close(&mut self, track: Id) -> Result<(), Error> {
		self.video_task.remove(track).ok_or(Error::NotFound)?;
		Ok(())
	}

	// NOTE: You're supposed to call this multiple times to get all of the chunks.
	pub fn frame_chunk(&self, frame: Id, index: usize, dst: &mut moq_frame) -> Result<(), Error> {
		let frame = self.frame.get(frame).ok_or(Error::NotFound)?;
		let chunk = frame.payload.get_chunk(index).ok_or(Error::NoIndex)?;

		let timestamp_us = frame
			.timestamp
			.as_micros()
			.try_into()
			.map_err(|_| moq_lite::TimeOverflow)?;

		*dst = moq_frame {
			payload: chunk.as_ptr(),
			payload_size: chunk.len(),
			timestamp_us,
			keyframe: frame.keyframe,
		};

		Ok(())
	}

	pub fn frame_close(&mut self, frame: Id) -> Result<(), Error> {
		self.frame.remove(frame).ok_or(Error::NotFound)?;
		Ok(())
	}

	pub fn close(&mut self, consume: Id) -> Result<(), Error> {
		self.broadcast.remove(consume).ok_or(Error::NotFound)?;
		Ok(())
	}
}
