use std::{
	collections::{hash_map::Entry, HashMap},
	sync::{atomic, Arc},
};

use crate::{
	coding::{Reader, Stream},
	lite::{self, Version},
	model::BroadcastProducer,
	AsPath, Broadcast, Error, Frame, FrameProducer, Group, GroupProducer, OriginProducer, Path, PathOwned,
	TrackProducer,
};

use tokio::sync::oneshot;
use web_async::Lock;

#[derive(Clone)]
pub(super) struct Subscriber<S: web_transport_trait::Session> {
	session: S,

	origin: Option<OriginProducer>,
	subscribes: Lock<HashMap<u64, TrackProducer>>,
	next_id: Arc<atomic::AtomicU64>,
	version: Version,
}

impl<S: web_transport_trait::Session> Subscriber<S> {
	pub fn new(session: S, origin: Option<OriginProducer>, version: Version) -> Self {
		Self {
			session,
			origin,
			subscribes: Default::default(),
			next_id: Default::default(),
			version,
		}
	}

	/// Send a signal when the subscriber is initialized.
	pub async fn run(self, init: oneshot::Sender<()>) -> Result<(), Error> {
		tokio::select! {
			Err(err) = self.clone().run_announce(init) => Err(err),
			res = self.run_uni() => res,
		}
	}

	async fn run_uni(self) -> Result<(), Error> {
		loop {
			let stream = self
				.session
				.accept_uni()
				.await
				.map_err(|err| Error::Transport(Arc::new(err)))?;

			let stream = Reader::new(stream, self.version);
			let this = self.clone();

			web_async::spawn(async move {
				if let Err(err) = this.run_uni_stream(stream).await {
					tracing::debug!(%err, "error running uni stream");
				}
			});
		}
	}

	async fn run_uni_stream(mut self, mut stream: Reader<S::RecvStream, Version>) -> Result<(), Error> {
		let kind = stream.decode().await?;

		let res = match kind {
			lite::DataType::Group => self.recv_group(&mut stream).await,
		};

		if let Err(err) = res {
			stream.abort(&err);
		}

		Ok(())
	}

	async fn run_announce(mut self, init: oneshot::Sender<()>) -> Result<(), Error> {
		if self.origin.is_none() {
			// Don't do anything if there's no origin configured.
			let _ = init.send(());
			return Ok(());
		}

		let mut stream = Stream::open(&self.session, self.version).await?;
		stream.writer.encode(&lite::ControlType::Announce).await?;

		tracing::trace!(root = %self.log_path(""), "announced start");

		// Ask for everything.
		// TODO This should actually ask for each root.
		let msg = lite::AnnouncePlease { prefix: "".into() };
		stream.writer.encode(&msg).await?;

		let mut producers = HashMap::new();

		let msg: lite::AnnounceInit = stream.reader.decode().await?;
		for path in msg.suffixes {
			self.start_announce(path, &mut producers)?;
		}

		let _ = init.send(());

		while let Some(announce) = stream.reader.decode_maybe::<lite::Announce>().await? {
			match announce {
				lite::Announce::Active { suffix: path } => {
					self.start_announce(path, &mut producers)?;
				}
				lite::Announce::Ended { suffix: path } => {
					tracing::debug!(broadcast = %self.log_path(&path), "unannounced");

					// Close the producer.
					let mut producer = producers.remove(&path.into_owned()).ok_or(Error::NotFound)?;
					producer.close()?;
				}
			}
		}

		// Close the stream when there's nothing more to announce.
		stream.writer.finish()?;
		stream.writer.closed().await
	}

	fn start_announce(
		&mut self,
		path: PathOwned,
		producers: &mut HashMap<PathOwned, BroadcastProducer>,
	) -> Result<(), Error> {
		tracing::debug!(broadcast = %self.log_path(&path), "announce");

		let broadcast = Broadcast::produce();

		// Make sure the peer doesn't double announce.
		match producers.entry(path.to_owned()) {
			Entry::Occupied(_) => return Err(Error::Duplicate),
			Entry::Vacant(entry) => entry.insert(broadcast.producer.clone()),
		};

		// Run the broadcast in the background until all consumers are dropped.
		self.origin
			.as_mut()
			.unwrap()
			.publish_broadcast(path.clone(), broadcast.consumer);

		web_async::spawn(self.clone().run_broadcast(path, broadcast.producer));

		Ok(())
	}

	async fn run_broadcast(self, path: PathOwned, mut broadcast: BroadcastProducer) {
		// Actually start serving subscriptions.
		loop {
			// Keep serving requests until there are no more consumers.
			// This way we'll clean up the task when the broadcast is no longer needed.
			let track = tokio::select! {
				track = broadcast.requested_track() => match track {
					Some(track) => track,
					None => break,
				},
				_ = self.session.closed() => break,
			};

			let id = self.next_id.fetch_add(1, atomic::Ordering::Relaxed);
			let mut this = self.clone();

			let path = path.clone();
			web_async::spawn(async move {
				if let Err(err) = this.run_subscribe(id, path, track).await {
					tracing::warn!(%err, id = %id, "error running subscribe");
				}

				this.subscribes.lock().remove(&id);
			});
		}
	}

	async fn run_subscribe(&mut self, id: u64, broadcast: Path<'_>, mut track: TrackProducer) -> Result<(), Error> {
		self.subscribes.lock().insert(id, track.clone());

		tracing::info!(id, broadcast = %self.log_path(&broadcast), track = ?track.name, "subscribe started");

		let res = tokio::select! {
			_ = track.unused() => Err(Error::Cancel),
			res = self.run_track(id, broadcast.clone(), track.clone()) => res,
		};

		match res {
			Err(Error::Cancel) | Err(Error::Transport(_)) => {
				tracing::info!(id, broadcast = %self.log_path(&broadcast), track = ?track.name, "subscribe cancelled");
				track.abort(Error::Cancel)?;
			}
			Err(err) => {
				tracing::warn!(id, broadcast = %self.log_path(&broadcast), track = ?track.name, %err, "subscribe error");
				track.abort(err)?;
			}
			_ => {
				tracing::info!(id, broadcast = %self.log_path(&broadcast), track = ?track.name, "subscribe complete");
			}
		}

		Ok(())
	}

	async fn run_track(&mut self, id: u64, broadcast: Path<'_>, track: TrackProducer) -> Result<(), Error> {
		let mut stream = Stream::open(&self.session, self.version).await?;
		stream.writer.encode(&lite::ControlType::Subscribe).await?;

		if let Err(err) = self.run_track_stream(&mut stream, id, broadcast, track).await {
			stream.writer.abort(&err);
			return Err(err);
		}

		stream.writer.finish()?;
		stream.writer.closed().await?;

		Ok(())
	}

	async fn run_track_stream(
		&mut self,
		stream: &mut Stream<S, Version>,
		id: u64,
		broadcast: Path<'_>,
		mut track: TrackProducer,
	) -> Result<(), Error> {
		let mut meta = track.meta();
		let current = meta.get().unwrap_or_default();

		let msg = lite::Subscribe {
			id,
			// NOTE: Makes a copy because we're not cool.
			broadcast: broadcast.to_owned(),
			// NOTE: Avoids making a copy because we're cool.
			track: track.name.as_ref().into(),
			priority: current.priority,
			max_latency: current.max_latency,
		};

		stream.writer.encode(&msg).await?;

		let _info: lite::SubscribeOk = stream.reader.decode().await?;

		loop {
			let meta = tokio::select! {
				// Wait until the priority/latency is updated
				meta = meta.changed() => meta,
				// Wait until the stream is closed
				res = stream.reader.closed() => return res,
			};

			if let Some(meta) = meta {
				tracing::trace!(subscribe = %id, track = ?track.name, ?meta, "subscribe update");

				stream
					.writer
					.encode(&lite::SubscribeUpdate {
						priority: meta.priority,
						max_latency: meta.max_latency,
					})
					.await?;
			} else {
				// No more consumers
				break;
			}
		}

		track.close()?;

		Ok(())
	}

	pub async fn recv_group(&mut self, stream: &mut Reader<S::RecvStream, Version>) -> Result<(), Error> {
		let hdr: lite::Group = stream.decode().await?;

		let mut group = {
			let mut subs = self.subscribes.lock();
			let track = subs.get_mut(&hdr.subscribe).ok_or(Error::Cancel)?;

			let group = Group { sequence: hdr.sequence };
			track.create_group(group)?
		};

		let res = tokio::select! {
			_ = group.unused() => Err(Error::Cancel),
			res = self.run_group(stream, group.clone()) => res,
		};

		match res {
			Err(Error::Cancel) | Err(Error::Transport(_)) => {
				tracing::trace!(group = %group.sequence, "group cancelled");
				group.abort(Error::Cancel)?;
			}
			Err(err) => {
				tracing::debug!(%err, group = %group.sequence, "group error");
				group.abort(err)?;
			}
			_ => {
				tracing::trace!(group = %group.sequence, "group complete");
			}
		}

		Ok(())
	}

	async fn run_group(
		&mut self,
		stream: &mut Reader<S::RecvStream, Version>,
		mut group: GroupProducer,
	) -> Result<(), Error> {
		while let Some(size) = stream.decode_maybe::<u64>().await? {
			let mut frame = group.create_frame(Frame { size })?;

			if let Err(err) = self.run_frame(stream, group.sequence, frame.clone()).await {
				frame.abort(err.clone())?;
				return Err(err);
			}
		}

		group.close()?;

		Ok(())
	}

	async fn run_frame(
		&mut self,
		stream: &mut Reader<S::RecvStream, Version>,
		_group: u64,
		mut frame: FrameProducer,
	) -> Result<(), Error> {
		let mut remain = frame.size;

		const MAX_CHUNK: usize = 1024 * 1024; // 1 MiB
		while remain > 0 {
			let chunk = stream
				.read(MAX_CHUNK.min(remain as usize))
				.await?
				.ok_or(Error::WrongSize)?;
			remain = remain.checked_sub(chunk.len() as u64).ok_or(Error::WrongSize)?;
			frame.write_chunk(chunk)?;
		}

		frame.close()?;

		Ok(())
	}

	fn log_path(&self, path: impl AsPath) -> Path<'_> {
		self.origin.as_ref().unwrap().root().join(path)
	}
}
