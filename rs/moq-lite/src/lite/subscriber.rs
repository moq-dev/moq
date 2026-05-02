use std::{
	collections::{HashMap, hash_map::Entry},
	sync::{Arc, atomic},
};

use futures::{StreamExt, stream::FuturesUnordered};

use crate::{
	AsPath, BandwidthProducer, Broadcast, BroadcastDynamic, Error, Frame, FrameProducer, Group, GroupProducer,
	OriginProducer, Path, PathOwned, TrackProducer,
	coding::{Reader, Stream},
	lite,
	model::BroadcastProducer,
};

use super::Version;

use web_async::Lock;

#[derive(Clone)]
pub(super) struct Subscriber<S: web_transport_trait::Session> {
	session: S,

	origin: Option<OriginProducer>,
	recv_bandwidth: Option<BandwidthProducer>,
	// Session-level origin id shared with the Publisher. Kept so callers that
	// want to filter reflected announces can reuse the same id; for now only
	// plumbed through, not applied automatically (see hang.live's dependency
	// on seeing its own publishes as a confirmation signal).
	#[allow(dead_code)]
	self_origin: crate::Origin,
	subscribes: Lock<HashMap<u64, TrackProducer>>,
	next_id: Arc<atomic::AtomicU64>,
	version: Version,
}

impl<S: web_transport_trait::Session> Subscriber<S> {
	pub fn new(
		session: S,
		origin: Option<OriginProducer>,
		recv_bandwidth: Option<BandwidthProducer>,
		self_origin: crate::Origin,
		version: Version,
	) -> Self {
		Self {
			session,
			origin,
			recv_bandwidth,
			self_origin,
			subscribes: Default::default(),
			next_id: Default::default(),
			version,
		}
	}

	pub async fn run(self) -> Result<(), Error> {
		let bw = self.clone();
		tokio::select! {
			Err(err) = self.clone().run_announce() => Err(err),
			res = self.run_uni() => res,
			Err(err) = bw.run_recv_bandwidth() => Err(err),
		}
	}

	async fn run_uni(self) -> Result<(), Error> {
		loop {
			let stream = self.session.accept_uni().await.map_err(Error::from_transport)?;

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

	async fn run_announce(self) -> Result<(), Error> {
		let origin = match &self.origin {
			Some(origin) => origin,
			None => return Ok(()),
		};

		let prefixes: Vec<PathOwned> = origin.allowed().map(|p| p.to_owned()).collect();

		let mut tasks = FuturesUnordered::new();
		for prefix in prefixes {
			tasks.push(self.clone().run_announce_prefix(prefix));
		}

		while let Some(result) = tasks.next().await {
			result?;
		}

		Ok(())
	}

	async fn run_announce_prefix(mut self, prefix: PathOwned) -> Result<(), Error> {
		let mut stream = Stream::open(&self.session, self.version).await?;
		stream.writer.encode(&lite::ControlType::Announce).await?;

		let msg = lite::AnnounceInterest {
			prefix: prefix.as_path(),
			exclude_hop: 0,
		};
		stream.writer.encode(&msg).await?;

		let mut producers = HashMap::new();

		match self.version {
			Version::Lite01 | Version::Lite02 => {
				let msg: lite::AnnounceInit = stream.reader.decode().await?;
				for suffix in msg.suffixes {
					let path = prefix.join(&suffix);
					// Lite01/02 don't carry hop information; the broadcast starts with an empty chain.
					self.start_announce(path, crate::OriginList::new(), &mut producers)?;
				}
			}
			_ => {
				// Lite03+: no AnnounceInit, initial state comes via Announce messages.
			}
		}

		while let Some(announce) = stream.reader.decode_maybe::<lite::Announce>().await? {
			match announce {
				lite::Announce::Active { suffix, hops } => {
					let path = prefix.join(&suffix);
					self.start_announce(path, hops, &mut producers)?;
				}
				lite::Announce::Ended { suffix, .. } => {
					let path = prefix.join(&suffix);
					tracing::debug!(broadcast = %self.log_path(&path), "unannounced");

					// Abort the producer.
					let mut producer = producers.remove(&path).ok_or(Error::NotFound)?;
					producer.abort(Error::Cancel).ok();
				}
			}
		}

		// Close the stream when there's nothing more to announce.
		stream.writer.finish()?;
		stream.writer.closed().await
	}

	/// Opens a PROBE stream when consumers exist, reads bandwidth estimates.
	/// Returns Ok(()) only when recv_bandwidth is None (disabled).
	/// Stream-level errors (e.g. peer reset) are non-fatal and logged as debug.
	async fn run_recv_bandwidth(self) -> Result<(), Error> {
		let Some(bandwidth) = &self.recv_bandwidth else {
			return Ok(());
		};

		bandwidth.used().await?;

		let res = self.run_probe_stream(bandwidth).await;
		match res {
			Ok(()) | Err(Error::Cancel | Error::Transport(_) | Error::Decode(_) | Error::Remote(_)) => {
				tracing::debug!("probe stream closed");
				Ok(())
			}
			Err(err) => Err(err),
		}
	}

	async fn run_probe_stream(&self, bandwidth: &BandwidthProducer) -> Result<(), Error> {
		let mut stream = Stream::open(&self.session, self.version).await?;
		stream.writer.encode(&lite::ControlType::Probe).await?;

		loop {
			tokio::select! {
				biased;
				_ = bandwidth.closed() => {
					stream.writer.finish()?;
					return stream.writer.closed().await;
				}
				res = bandwidth.unused() => {
					res?;
					// No more consumers, close the probe stream.
					stream.writer.finish()?;
					return stream.writer.closed().await;
				}
				probe = stream.reader.decode::<lite::Probe>() => {
					let probe = probe?;
					bandwidth.set(Some(probe.bitrate))?;
				}
			}
		}
	}

	fn start_announce(
		&mut self,
		path: PathOwned,
		hops: crate::OriginList,
		producers: &mut HashMap<PathOwned, BroadcastProducer>,
	) -> Result<(), Error> {
		tracing::debug!(broadcast = %self.log_path(&path), hops = hops.len(), "announce");

		let broadcast = Broadcast { hops }.produce();

		// Make sure the peer doesn't double announce.
		match producers.entry(path.to_owned()) {
			Entry::Occupied(_) => return Err(Error::Duplicate),
			Entry::Vacant(entry) => entry.insert(broadcast.clone()),
		};

		// Create the dynamic handler BEFORE publishing, so that consumers
		// see dynamic >= 1 immediately when they receive the announcement.
		// Otherwise there's a race on multi-threaded runtimes where a consumer
		// can call subscribe_track() before dynamic is incremented, getting NotFound.
		let dynamic = broadcast.dynamic();

		// Run the broadcast in the background until all consumers are dropped.
		self.origin
			.as_mut()
			.unwrap()
			.publish_broadcast(path.clone(), broadcast.consume());

		web_async::spawn(self.clone().run_broadcast(path, dynamic));

		Ok(())
	}

	async fn run_broadcast(self, path: PathOwned, mut broadcast: BroadcastDynamic) {
		// Actually start serving subscriptions.
		loop {
			// Keep serving requests until there are no more consumers.
			// This way we'll clean up the task when the broadcast is no longer needed.
			let track = tokio::select! {
				producer = broadcast.requested_track() => match producer {
					Ok(producer) => producer,
					Err(err) => {
						tracing::debug!(%err, "broadcast closed");
						break;
					}
				},
				_ = self.session.closed() => break,
			};

			let id = self.next_id.fetch_add(1, atomic::Ordering::Relaxed);
			let mut this = self.clone();

			let path = path.clone();
			web_async::spawn(async move {
				this.run_subscribe(id, path, track).await;
				this.subscribes.lock().remove(&id);
			});
		}
	}

	async fn run_subscribe(&mut self, id: u64, broadcast: Path<'_>, mut track: TrackProducer) {
		self.subscribes.lock().insert(id, track.clone());

		tracing::info!(id, broadcast = %self.log_path(&broadcast), track = %track.name, "subscribe started");

		// Cancel as soon as the track has no consumers. Cloned so it doesn't conflict
		// with `track.subscription()`'s `&mut self` borrow inside `run_track`.
		let unused = track.clone();
		let res = tokio::select! {
			_ = unused.unused() => Err(Error::Cancel),
			res = self.run_track(id, &broadcast, &mut track) => res,
		};

		match res {
			Err(Error::Cancel) => {
				tracing::info!(id, broadcast = %self.log_path(&broadcast), track = %track.name, "subscribe cancelled");
				let _ = track.abort(Error::Cancel);
			}
			Err(err) => {
				tracing::warn!(id, broadcast = %self.log_path(&broadcast), track = %track.name, %err, "subscribe error");
				let _ = track.abort(err);
			}
			_ => {
				tracing::info!(id, broadcast = %self.log_path(&broadcast), track = %track.name, "subscribe complete");
				let _ = track.finish();
			}
		}
	}

	async fn run_track(&mut self, id: u64, broadcast: &Path<'_>, track: &mut TrackProducer) -> Result<(), Error> {
		// Wait for the first interested subscriber before opening an upstream
		// SUBSCRIBE stream — relays only relay when somebody's listening.
		let Some(initial) = track.subscription().await else {
			return Ok(());
		};

		let mut stream = Stream::open(&self.session, self.version).await?;
		stream.writer.encode(&lite::ControlType::Subscribe).await?;

		let msg = lite::Subscribe {
			id,
			broadcast: broadcast.to_owned(),
			track: track.name.clone().into(),
			priority: initial.priority,
			ordered: initial.ordered,
			max_latency: initial.max_latency,
			start_group: initial.start,
			end_group: initial.end,
		};

		if let Err(err) = self.run_track_stream(&mut stream, msg, track).await {
			stream.writer.abort(&err);
			return Err(err);
		}

		stream.writer.finish()?;
		stream.writer.closed().await
	}

	async fn run_track_stream(
		&mut self,
		stream: &mut Stream<S, Version>,
		msg: lite::Subscribe<'_>,
		track: &mut TrackProducer,
	) -> Result<(), Error> {
		stream.writer.encode(&msg).await?;

		// The first response MUST be a SUBSCRIBE_OK.
		let resp: lite::SubscribeResponse = stream.reader.decode().await?;
		let lite::SubscribeResponse::Ok(_info) = resp else {
			return Err(Error::ProtocolViolation);
		};

		// Forward subsequent aggregate-subscription changes upstream as
		// SUBSCRIBE_UPDATE. Exit cleanly when the upstream stream closes
		// (PublishDone) or when no live subscribers remain.
		// TODO handle additional SUBSCRIBE_OK and SUBSCRIBE_DROP messages.
		loop {
			tokio::select! {
				sub = track.subscription() => match sub {
					Some(sub) => {
						stream.writer.encode(&lite::SubscribeUpdate {
							priority: sub.priority,
							ordered: sub.ordered,
							max_latency: sub.max_latency,
							start_group: sub.start,
							end_group: sub.end,
						}).await?;
					}
					None => return Ok(()),
				},
				res = stream.reader.closed() => return res,
			}
		}
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
			err = group.closed() => Err(err),
			res = self.run_group(stream, group.clone()) => res,
		};

		match res {
			Err(Error::Cancel) => {
				let _ = group.abort(Error::Cancel);
			}
			Err(err) => {
				tracing::debug!(%err, group = %group.sequence, "group error");
				let _ = group.abort(err);
			}
			_ => {
				let _ = group.finish();
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

			if let Err(err) = self.run_frame(stream, &mut frame).await {
				let _ = frame.abort(err.clone());
				return Err(err);
			}

			frame.finish()?;
		}

		Ok(())
	}

	async fn run_frame(
		&mut self,
		stream: &mut Reader<S::RecvStream, Version>,
		frame: &mut FrameProducer,
	) -> Result<(), Error> {
		// FrameProducer impls BufMut over its pre-allocated per-frame buffer, so
		// read_buf writes QUIC stream bytes directly into the frame — no
		// intermediate Bytes allocations, and quinn's reassembly arena is freed
		// as we drain it.
		while bytes::BufMut::has_remaining_mut(frame) {
			match stream.read_buf(frame).await? {
				Some(n) if n > 0 => {}
				_ => return Err(Error::WrongSize),
			}
		}
		Ok(())
	}

	fn log_path(&self, path: impl AsPath) -> Path<'_> {
		self.origin.as_ref().unwrap().root().join(path)
	}
}
