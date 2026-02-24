use futures::{FutureExt, StreamExt, stream::FuturesUnordered};
use web_async::FuturesExt;

use crate::{
	AsPath, BroadcastConsumer, Error, Origin, OriginConsumer, Track, TrackConsumer,
	coding::{Stream, Writer},
	lite::{
		self, Version,
		priority::{PriorityHandle, PriorityQueue},
	},
	model::GroupConsumer,
};

pub(super) struct Publisher<S: web_transport_trait::Session> {
	session: S,
	origin: OriginConsumer,
	priority: PriorityQueue,
	version: Version,
}

impl<S: web_transport_trait::Session> Publisher<S> {
	pub fn new(session: S, origin: Option<OriginConsumer>, version: Version) -> Self {
		// Default to a dummy origin that is immediately closed.
		let origin = origin.unwrap_or_else(|| Origin::produce().consume());
		Self {
			session,
			origin,
			priority: Default::default(),
			version,
		}
	}

	pub async fn run(mut self) -> Result<(), Error> {
		loop {
			let mut stream = Stream::accept(&self.session, self.version).await?;

			// To avoid cloning the origin, we process each control stream in received order.
			// This adds some head-of-line blocking but it delays an expensive clone.
			let kind = stream.reader.decode().await?;

			if let Err(err) = match kind {
				lite::ControlType::Announce => self.recv_announce(stream).await,
				lite::ControlType::Subscribe => self.recv_subscribe(stream).await,
				_ => Err(Error::UnexpectedStream),
			} {
				tracing::warn!(%err, "control stream error");
			}
		}
	}

	pub async fn recv_announce(&mut self, mut stream: Stream<S, Version>) -> Result<(), Error> {
		let interest = stream.reader.decode::<lite::AnnouncePlease>().await?;
		let prefix = interest.prefix.to_owned();

		// For logging, show the full path that we're announcing.
		tracing::trace!(root = %self.origin.absolute(&prefix), "announcing start");

		let mut origin = self
			.origin
			.consume_only(&[prefix.as_path()])
			.ok_or(Error::Unauthorized)?;

		let version = self.version;
		web_async::spawn(async move {
			if let Err(err) = Self::run_announce(&mut stream, &mut origin, &prefix, version).await {
				match &err {
					Error::Cancel => {
						tracing::debug!(prefix = %origin.absolute(prefix), "announcing cancelled");
					}
					Error::Transport => {
						tracing::debug!(prefix = %origin.absolute(prefix), "announcing cancelled");
					}
					err => {
						tracing::warn!(%err, prefix = %origin.absolute(prefix), "announcing error");
					}
				}

				stream.writer.abort(&err);
			} else {
				tracing::trace!(prefix = %origin.absolute(prefix), "announcing complete");
			}
		});

		Ok(())
	}

	async fn run_announce(
		stream: &mut Stream<S, Version>,
		origin: &mut OriginConsumer,
		prefix: impl AsPath,
		version: Version,
	) -> Result<(), Error> {
		let prefix = prefix.as_path();

		match version {
			Version::Draft01 | Version::Draft02 => {
				let mut init = Vec::new();

				// Send ANNOUNCE_INIT as the first message with all currently active paths
				// We use `try_next()` to synchronously get the initial updates.
				while let Some((path, active)) = origin.try_announced() {
					let suffix = path.strip_prefix(&prefix).expect("origin returned invalid path");

					if active.is_some() {
						tracing::debug!(broadcast = %origin.absolute(&path), "announce");
						init.push(suffix.to_owned());
					} else {
						// A potential race.
						tracing::debug!(broadcast = %origin.absolute(&path), "unannounce");
						init.retain(|path| path != &suffix);
					}
				}

				let announce_init = lite::AnnounceInit { suffixes: init };
				stream.writer.encode(&announce_init).await?;
			}
			Version::Draft03 => {
				// No more announce init in Draft03.
			}
		}

		// Send updates as they arrive.
		loop {
			tokio::select! {
				biased;
				res = stream.reader.closed() => return res,
				announced = origin.announced() => {
					match announced {
						Some((path, active)) => {
							let suffix = path.strip_prefix(&prefix).expect("origin returned invalid path").to_owned();

							if active.is_some() {
								tracing::debug!(broadcast = %origin.absolute(&path), "announce");
								let msg = lite::Announce::Active { suffix, hops: 0 };
								stream.writer.encode(&msg).await?;
							} else {
								tracing::debug!(broadcast = %origin.absolute(&path), "unannounce");
								let msg = lite::Announce::Ended { suffix, hops: 0 };
								stream.writer.encode(&msg).await?;
							}
						},
						None => {
							stream.writer.finish()?;
							return stream.writer.closed().await;
						}
					}
				}
			}
		}
	}

	pub async fn recv_subscribe(&mut self, mut stream: Stream<S, Version>) -> Result<(), Error> {
		let subscribe = stream.reader.decode::<lite::Subscribe>().await?;

		let id = subscribe.id;
		let track = subscribe.track.clone();
		let absolute = self.origin.absolute(&subscribe.broadcast).to_owned();

		tracing::info!(%id, broadcast = %absolute, %track, "subscribed started");

		let broadcast = self.origin.consume_broadcast(&subscribe.broadcast);
		let priority = self.priority.clone();
		let version = self.version;

		let session = self.session.clone();
		web_async::spawn(async move {
			if let Err(err) = Self::run_subscribe(session, &mut stream, &subscribe, broadcast, priority, version).await
			{
				match &err {
					// TODO better classify WebTransport errors.
					Error::Cancel | Error::Transport => {
						tracing::info!(%id, broadcast = %absolute, %track, "subscribed cancelled")
					}
					err => {
						tracing::warn!(%id, broadcast = %absolute, %track, %err, "subscribed error")
					}
				}
				stream.writer.abort(&err);
			} else {
				tracing::info!(%id, broadcast = %absolute, %track, "subscribed complete")
			}
		});

		Ok(())
	}

	async fn run_subscribe(
		session: S,
		stream: &mut Stream<S, Version>,
		subscribe: &lite::Subscribe<'_>,
		consumer: Option<BroadcastConsumer>,
		priority: PriorityQueue,
		version: Version,
	) -> Result<(), Error> {
		let track = Track {
			name: subscribe.track.to_string(),
			priority: subscribe.priority,
		};

		let broadcast = consumer.ok_or(Error::NotFound)?;
		let track = broadcast.subscribe_track(&track);

		// TODO wait until track.info() to get the *real* priority

		let info = lite::SubscribeOk {
			priority: track.info.priority,
			ordered: false,
			max_latency: std::time::Duration::ZERO,
			start_group: None,
			end_group: None,
		};

		stream.writer.encode(&lite::SubscribeResponse::Ok(info)).await?;

		tokio::select! {
			res = Self::run_track(session, track, subscribe, priority, version) => res?,
			res = stream.reader.closed() => res?,
		}

		stream.writer.finish()?;
		stream.writer.closed().await
	}

	async fn run_track(
		session: S,
		mut track: TrackConsumer,
		subscribe: &lite::Subscribe<'_>,
		priority: PriorityQueue,
		version: Version,
	) -> Result<(), Error> {
		let mut tasks = FuturesUnordered::new();

		loop {
			let group = tokio::select! {
				// Poll all active group futures; never matches but keeps them running.
				true = async {
					while tasks.next().await.is_some() {}
					false
				} => unreachable!(),
				Some(group) = track.next_group().transpose() => group,
				else => return Ok(()),
			}?;

			let sequence = group.info.sequence;
			tracing::debug!(subscribe = %subscribe.id, track = %track.info.name, sequence, "serving group");

			let msg = lite::Group {
				subscribe: subscribe.id,
				sequence,
			};

			let priority = priority.insert(track.info.priority, sequence);
			tasks.push(Self::serve_group(session.clone(), msg, priority, group, version).map(|_| ()));
		}
	}

	async fn serve_group(
		session: S,
		msg: lite::Group,
		mut priority: PriorityHandle,
		mut group: GroupConsumer,
		version: Version,
	) -> Result<(), Error> {
		// TODO add a way to open in priority order.
		let stream = session.open_uni().await.map_err(Error::from_transport)?;

		let mut stream = Writer::new(stream, version);
		stream.set_priority(priority.current());
		stream.encode(&lite::DataType::Group).await?;
		stream.encode(&msg).await?;

		loop {
			let frame = tokio::select! {
				biased;
				_ = stream.closed() => return Err(Error::Cancel),
				frame = group.next_frame() => frame,
				// Update the priority if it changes.
				priority = priority.next() => {
					stream.set_priority(priority);
					continue;
				}
			};

			let mut frame = match frame? {
				Some(frame) => frame,
				None => break,
			};

			tracing::trace!(size = %frame.info.size, "writing frame");

			stream.encode(&frame.info.size).await?;

			loop {
				let chunk = tokio::select! {
					biased;
					_ = stream.closed() => return Err(Error::Cancel),
					chunk = frame.read_chunk() => chunk,
					// Update the priority if it changes.
					priority = priority.next() => {
						stream.set_priority(priority);
						continue;
					}
				};

				match chunk? {
					Some(mut chunk) => stream.write_all(&mut chunk).await?,
					None => break,
				}
			}

			tracing::trace!(size = %frame.info.size, "wrote frame");
		}

		stream.finish()?;
		stream.closed().await?;

		tracing::debug!(sequence = %msg.sequence, "finished group");

		Ok(())
	}
}
