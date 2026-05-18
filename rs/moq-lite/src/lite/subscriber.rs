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
	// Session-level origin id shared with the Publisher. Used to filter out
	// reflected announces: we ask the peer (via AnnounceInterest.exclude_hop)
	// to skip broadcasts whose hop chain already passed through us, and we
	// double-check incoming announces against it as defense in depth.
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
		version: Version,
	) -> Self {
		// Identity for incoming-hop loop detection. Derived from the local
		// origin we publish into so it matches the relay identity across
		// every session sharing that origin — required for cross-session
		// loop detection. If no origin is attached (the announce loop is
		// inert anyway), fall back to a random session-local id.
		let self_origin = origin.as_deref().copied().unwrap_or_else(crate::Origin::random);
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

		// Ask the peer to filter out announces that already passed through us, so
		// reflected announces (the simple loop case) never hit the wire. Lite03
		// peers ignore this field, in which case start_announce below still drops.
		let msg = lite::AnnounceInterest {
			prefix: prefix.as_path(),
			exclude_hop: self.self_origin.id,
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

					// The matching Active may have been silently dropped by
					// start_announce as a reflected loop, in which case
					// `producers` has no entry; that's expected, not an error.
					if let Some(mut producer) = producers.remove(&path) {
						producer.abort(Error::Cancel).ok();
					}
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
		// Drop announces that already passed through us — this connection is
		// a reflection, not a new path. Peers should be filtering via
		// AnnounceInterest.exclude_hop, but Lite03 peers can't, so this is
		// the authoritative cluster-loop check on the receiver.
		if hops.contains(&self.self_origin) {
			tracing::debug!(broadcast = %self.log_path(&path), "dropping reflected announce");
			return Ok(());
		}

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
			let broadcast = broadcast.clone();
			web_async::spawn(async move {
				this.run_subscribe(id, path, broadcast, track).await;
				this.subscribes.lock().remove(&id);
			});
		}
	}

	async fn run_subscribe(&mut self, id: u64, path: PathOwned, broadcast: BroadcastDynamic, mut track: TrackProducer) {
		self.subscribes.lock().insert(id, track.clone());

		let msg = lite::Subscribe {
			id,
			broadcast: path.as_path(),
			track: (&track.name).into(),
			priority: track.priority,
			ordered: true,
			max_latency: std::time::Duration::ZERO,
			start_group: None,
			end_group: None,
		};

		tracing::info!(id, broadcast = %self.log_path(&path), track = %track.name, "subscribe started");

		tokio::select! {
			_ = track.unused() => {
				tracing::info!(id, broadcast = %self.log_path(&path), track = %track.name, "subscribe cancelled");
				let _ = track.abort(Error::Cancel);
			}
			err = broadcast.closed() => {
				tracing::info!(id, broadcast = %self.log_path(&path), track = %track.name, "broadcast closed");
				let _ = track.abort(err);
			}
			res = self.run_track(msg) => match res {
				Ok(()) => {
					tracing::info!(id, broadcast = %self.log_path(&path), track = %track.name, "subscribe complete");
					let _ = track.finish();
				}
				Err(err) => {
					tracing::warn!(id, broadcast = %self.log_path(&path), track = %track.name, %err, "subscribe error");
					let _ = track.abort(err);
				}
			},
		}
	}

	async fn run_track(&mut self, msg: lite::Subscribe<'_>) -> Result<(), Error> {
		let mut stream = Stream::open(&self.session, self.version).await?;
		stream.writer.encode(&lite::ControlType::Subscribe).await?;

		if let Err(err) = self.run_track_stream(&mut stream, msg).await {
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
	) -> Result<(), Error> {
		stream.writer.encode(&msg).await?;

		// The first response MUST be a SUBSCRIBE_OK.
		let resp: lite::SubscribeResponse = stream.reader.decode().await?;
		let lite::SubscribeResponse::Ok(_info) = resp else {
			return Err(Error::ProtocolViolation);
		};

		// TODO handle additional SUBSCRIBE_OK and SUBSCRIBE_DROP messages.
		stream.reader.closed().await?;

		Ok(())
	}

	pub async fn recv_group(&mut self, stream: &mut Reader<S::RecvStream, Version>) -> Result<(), Error> {
		let hdr: lite::Group = stream.decode().await?;

		let (mut group, track) = {
			let mut subs = self.subscribes.lock();
			let track = subs.get_mut(&hdr.subscribe).ok_or(Error::Cancel)?;

			let group_info = Group { sequence: hdr.sequence };
			let group = track.create_group(group_info)?;
			(group, track.clone())
		};

		let res = tokio::select! {
			err = track.closed() => Err(err),
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
