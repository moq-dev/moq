use std::sync::Arc;

use futures::{stream::FuturesUnordered, FutureExt, StreamExt};
use web_async::FuturesExt;

use crate::{
	coding::{Stream, Writer},
	lite::{
		self,
		priority::{PriorityHandle, PriorityQueue},
		Version,
	},
	model::GroupConsumer,
	AsPath, BroadcastConsumer, Delivery, Error, OriginConsumer, OriginProducer, Time, Track,
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
		let origin = origin.unwrap_or_else(|| OriginProducer::new().consume());
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

		web_async::spawn(async move {
			if let Err(err) = Self::run_announce(&mut stream, &mut origin, &prefix).await {
				match &err {
					Error::Cancel | Error::Transport(_) => {
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
	) -> Result<(), Error> {
		let prefix = prefix.as_path();
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

		// Flush any synchronously announced paths
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
								let msg = lite::Announce::Active { suffix };
								stream.writer.encode(&msg).await?;
							} else {
								tracing::debug!(broadcast = %origin.absolute(&path), "unannounce");
								let msg = lite::Announce::Ended { suffix };
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

		let broadcast = self.origin.consume_broadcast(&subscribe.broadcast);
		let priority = self.priority.clone();
		let version = self.version;

		let session = self.session.clone();
		web_async::spawn(async move {
			if let Err(err) = Self::run_subscribe(session, &mut stream, &subscribe, broadcast, priority, version).await
			{
				match &err {
					// TODO better classify WebTransport errors.
					Error::Cancel | Error::Transport(_) => {
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
		broadcast: Option<BroadcastConsumer>,
		priority: PriorityQueue,
		version: Version,
	) -> Result<(), Error> {
		let track = Track::from(subscribe.track.to_string());

		let delivery = Delivery {
			priority: subscribe.priority,
			max_latency: subscribe.max_latency,
			ordered: subscribe.ordered,
		};

		tracing::info!(id = %subscribe.id, broadcast = %subscribe.broadcast, track = %track.name, ?delivery, "subscribed started");

		let mut track = broadcast.ok_or(Error::NotFound)?.subscribe_track(track, delivery)?;
		let delivery = track.delivery().current();

		let info = lite::SubscribeOk {
			priority: delivery.priority,
			max_latency: delivery.max_latency,
			ordered: delivery.ordered,
		};

		tracing::trace!(subscribe = %subscribe.id, broadcast = %subscribe.broadcast, track = %track.name, ?delivery, "subscribed ok");

		stream.writer.encode(&info).await?;

		// Just to get around ownership issues.
		let mut delivery = track.delivery().clone();

		// All of the groups we're currently serving.
		let mut tasks = FuturesUnordered::new();

		loop {
			let group = tokio::select! {
				Some(group) = track.next_group().transpose() => group,
				update = stream.reader.decode_maybe::<lite::SubscribeUpdate>() => {
					// The stream is closed, so we're done.
					// TODO also cancel outstanding groups.
					let Some(update) = update? else {break};

					let delivery = Delivery {
						priority: update.priority,
						max_latency: update.max_latency,
						ordered: update.ordered,
					};

					tracing::info!(subscribe = %subscribe.id, broadcast = %subscribe.broadcast, track = %track.name, ?delivery, "subscribed update");
					track.subscriber().update(delivery);

					// TODO update the priority of all outstanding groups.

					continue;
				},
				Some(delivery) = delivery.changed() => {
					let info = lite::SubscribeOk {
						priority: delivery.priority,
						max_latency: delivery.max_latency,
						ordered: delivery.ordered,
					};

					tracing::info!(subscribe = %subscribe.id, broadcast = %subscribe.broadcast, track = %track.name, ?delivery, "subscribed ok");
					stream.writer.encode(&info).await?;

					continue;

				},
				// This is a hack to avoid waking up the select! loop each time a group completes.
				// We poll all of the groups until they're all complete, only matching `else` when all are complete.
				// We don't use tokio::spawn so we can wait until done, clean up on Drop, and support non-tokio.
				true = async {
					// Constantly poll all of the groups until they're all complete.
					while tasks.next().await.is_some() {}
					// Never match
					false
				} => unreachable!("never match"),
				else => break,
			}?;

			tracing::debug!(subscribe = %subscribe.id, broadcast = %subscribe.broadcast, track = %track.name, group = %group.sequence, "serving group");

			let msg = lite::Group {
				subscribe: subscribe.id,
				sequence: group.sequence,
			};

			// TODO factor in ordered.
			let priority = priority.insert(track.subscriber().current().priority, group.sequence);

			// Run the group until it's closed or expires.
			tasks.push(Self::serve_group(session.clone(), msg, priority, group, version).map(|_| ()));
		}

		stream.writer.finish()?;
		stream.writer.closed().await?;

		Ok(())
	}

	async fn serve_group(
		session: S,
		msg: lite::Group,
		mut priority: PriorityHandle,
		mut group: GroupConsumer,
		version: Version,
	) -> Result<(), Error> {
		// TODO add a way to open in priority order.
		let stream = session
			.open_uni()
			.await
			.map_err(|err| Error::Transport(Arc::new(err)))?;

		let mut stream = Writer::new(stream, version);
		stream.set_priority(priority.current());
		stream.encode(&lite::DataType::Group).await?;
		stream.encode(&msg).await?;

		// The maximum instant of the frames in the group.
		let mut instant_max = Time::ZERO;

		loop {
			let frame = tokio::select! {
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

			let delta = match frame.instant.checked_sub(instant_max) {
				Ok(delta) => {
					instant_max = frame.instant;
					delta
				}
				Err(_) => {
					tracing::warn!("frame instant went backwards");
					Default::default()
				}
			};

			stream
				.encode(&lite::FrameHeader {
					delta,
					size: frame.size,
				})
				.await?;

			loop {
				let chunk = tokio::select! {
					_ = stream.closed() => return Err(Error::Cancel),
					chunk = frame.read_chunk() => chunk,
					// Update the priority if it changes.
					priority = priority.next() => {
						stream.set_priority(priority);
						continue;
					}
				};

				let Some(mut chunk) = chunk? else {
					break;
				};
				stream.write_all(&mut chunk).await?;
			}
		}

		stream.finish()?;
		stream.closed().await?;

		Ok(())
	}
}
