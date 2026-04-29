use std::time::Duration;

use futures::{FutureExt, StreamExt, stream::FuturesUnordered};
use web_async::FuturesExt;
use web_transport_trait::Stats;

use crate::{
	AsPath, BroadcastConsumer, Error, Origin, OriginConsumer, OriginList, Subscription, Track, TrackSubscriber,
	coding::{Stream, Writer},
	lite::{
		self,
		priority::{PriorityHandle, PriorityQueue},
	},
	model::GroupConsumer,
};

use super::Version;

pub(super) struct Publisher<S: web_transport_trait::Session> {
	session: S,
	origin: OriginConsumer,
	// The session-level origin id stamped onto outbound hop chains. Shared
	// with the Subscriber so it can optionally filter out reflected announces.
	self_origin: Origin,
	priority: PriorityQueue,
	version: Version,
}

impl<S: web_transport_trait::Session> Publisher<S> {
	pub fn new(session: S, origin: Option<OriginConsumer>, self_origin: Origin, version: Version) -> Self {
		// Default to a dummy origin that is immediately closed.
		let origin = origin.unwrap_or_else(|| Origin::random().produce().consume());
		Self {
			session,
			origin,
			self_origin,
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
				lite::ControlType::Probe => {
					self.recv_probe(stream);
					Ok(())
				}
				lite::ControlType::Goaway => {
					tracing::info!("received goaway stream");
					Ok(())
				}
				lite::ControlType::Session | lite::ControlType::Fetch => Err(Error::UnexpectedStream),
			} {
				tracing::warn!(%err, "control stream error");
			}
		}
	}

	fn recv_probe(&self, mut stream: Stream<S, Version>) {
		let session = self.session.clone();
		let version = self.version;

		web_async::spawn(async move {
			if let Err(err) = Self::run_probe(&session, &mut stream, version).await {
				match &err {
					Error::Cancel | Error::Transport(_) => {
						tracing::debug!("probe stream closed");
					}
					err => {
						tracing::warn!(%err, "probe stream error");
					}
				}
				stream.writer.abort(&err);
			} else {
				tracing::debug!("probe stream complete");
			}
		});
	}

	async fn run_probe(session: &S, stream: &mut Stream<S, Version>, _version: Version) -> Result<(), Error> {
		const PROBE_INTERVAL: Duration = Duration::from_millis(100);
		const PROBE_MAX_AGE: Duration = Duration::from_secs(10);
		const PROBE_MAX_DELTA: f64 = 0.25;

		let mut last_sent: Option<(u64, tokio::time::Instant)> = None;
		let mut interval = tokio::time::interval(PROBE_INTERVAL);

		loop {
			tokio::select! {
				res = stream.reader.closed() => return res,
				_ = interval.tick() => {}
			}

			let Some(bitrate) = session.stats().estimated_send_rate() else {
				continue;
			};

			let should_send = match last_sent {
				None => true,
				Some((0, _)) => bitrate > 0,
				Some((prev, at)) => {
					let elapsed = at.elapsed().as_secs_f64();
					let t = elapsed.clamp(PROBE_INTERVAL.as_secs_f64(), PROBE_MAX_AGE.as_secs_f64());
					let range = PROBE_MAX_AGE.as_secs_f64() - PROBE_INTERVAL.as_secs_f64();
					let threshold = PROBE_MAX_DELTA * (PROBE_MAX_AGE.as_secs_f64() - t) / range;
					let change = (bitrate as f64 - prev as f64).abs() / prev as f64;
					change >= threshold
				}
			};

			if should_send {
				let rtt = session.stats().rtt().map(|d| d.as_millis() as u64);
				stream.writer.encode(&lite::Probe { bitrate, rtt }).await?;
				last_sent = Some((bitrate, tokio::time::Instant::now()));
			}
		}
	}

	pub async fn recv_announce(&mut self, mut stream: Stream<S, Version>) -> Result<(), Error> {
		let interest = stream.reader.decode::<lite::AnnounceInterest>().await?;
		let prefix = interest.prefix.to_owned();

		let mut origin = self
			.origin
			.consume_only(&[prefix.as_path()])
			.ok_or(Error::Unauthorized)?;

		let version = self.version;
		let self_origin = self.self_origin;
		web_async::spawn(async move {
			if let Err(err) = Self::run_announce(&mut stream, &mut origin, &prefix, self_origin, version).await {
				match &err {
					Error::Cancel | Error::Transport(_) => {
						tracing::debug!(prefix = %origin.absolute(prefix), "announcing cancelled");
					}
					err => {
						tracing::warn!(%err, prefix = %origin.absolute(prefix), "announcing error");
					}
				}

				stream.writer.abort(&err);
			}
		});

		Ok(())
	}

	async fn run_announce(
		stream: &mut Stream<S, Version>,
		origin: &mut OriginConsumer,
		prefix: impl AsPath,
		self_origin: Origin,
		version: Version,
	) -> Result<(), Error> {
		let prefix = prefix.as_path();

		match version {
			Version::Lite01 | Version::Lite02 => {
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
			_ => {
				// Lite03+: no more announce init.
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

							if let Some(active) = active {
								tracing::debug!(broadcast = %origin.absolute(&path), "announce");
								// Append our origin id to the hops so the next relay can detect loops.
								// If the chain is already at MAX_HOPS, skip the announce — this link is
								// effectively unreachable and the peer will eventually prune the loop.
								let mut hops = active.hops.clone();
								if hops.push(self_origin).is_err() {
									tracing::warn!(
										broadcast = %origin.absolute(&path),
										"dropping announce; hop chain at MAX_HOPS (possible loop)",
									);
									continue;
								}
								let msg = lite::Announce::Active { suffix, hops };
								stream.writer.encode(&msg).await?;
							} else {
								tracing::debug!(broadcast = %origin.absolute(&path), "unannounce");
								// An ended announce doesn't need hops — the receiver matches on path only.
								let msg = lite::Announce::Ended {
									suffix,
									hops: OriginList::new(),
								};
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

		// We just received a subscribe for this exact path, so by definition the peer has
		// already seen an announcement for it — synchronous lookup is appropriate here.
		#[allow(deprecated)]
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
		consumer: Option<BroadcastConsumer>,
		priority: PriorityQueue,
		version: Version,
	) -> Result<(), Error> {
		let track = Track::new(subscribe.track.to_string());

		let broadcast = consumer.ok_or(Error::NotFound)?;
		let consumer = broadcast.consume_track(&track)?;

		// Pick the start group: explicit, else the latest cached group.
		let start = subscribe.start_group.or_else(|| consumer.latest());
		let subscriber = consumer.subscribe(Subscription {
			priority: subscribe.priority,
			ordered: subscribe.ordered,
			max_latency: subscribe.max_latency,
			start,
			end: subscribe.end_group,
		})?;

		let info = lite::SubscribeOk {
			priority: subscribe.priority,
			ordered: subscribe.ordered,
			max_latency: subscribe.max_latency,
			start_group: start,
			end_group: subscribe.end_group,
		};

		stream.writer.encode(&lite::SubscribeResponse::Ok(info)).await?;

		Self::run_track(session, subscriber, stream, subscribe.id, priority, version).await?;

		stream.writer.finish()?;
		stream.writer.closed().await
	}

	async fn run_track(
		session: S,
		mut subscriber: TrackSubscriber,
		stream: &mut Stream<S, Version>,
		subscribe_id: u64,
		priority: PriorityQueue,
		version: Version,
	) -> Result<(), Error> {
		let mut tasks = FuturesUnordered::new();

		loop {
			tokio::select! {
				// Poll all active group futures; never matches but keeps them running.
				true = async {
					while tasks.next().await.is_some() {}
					false
				} => unreachable!(),
				group = subscriber.recv_group().transpose() => match group {
					Some(group) => {
						let group = group?;
						let sequence = group.sequence;
						tracing::debug!(subscribe = %subscribe_id, track = %subscriber.name, sequence, "serving group");

						let msg = lite::Group {
							subscribe: subscribe_id,
							sequence,
						};

						let p = priority.insert(subscriber.subscription().priority, sequence);
						tasks.push(Self::serve_group(session.clone(), msg, p, group, version).map(|_| ()));
					}
					None => break,
				},
				upd = stream.reader.decode_maybe::<lite::SubscribeUpdate>() => match upd? {
					Some(upd) => {
						subscriber.update(Subscription {
							priority: upd.priority,
							ordered: upd.ordered,
							max_latency: upd.max_latency,
							start: upd.start_group,
							end: upd.end_group,
						});
					}
					None => break,
				},
			}
		}

		// Drain in-flight group futures so they finish writing before we close the stream.
		while tasks.next().await.is_some() {}

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

			stream.encode(&frame.size).await?;

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
		}

		stream.finish()?;
		stream.closed().await?;

		tracing::debug!(sequence = %msg.sequence, "finished group");

		Ok(())
	}
}
