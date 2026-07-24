use crate::origin;
use crate::{
	Error, Origin,
	coding::{Decode, Encode, Reader, Stream, Writer},
	ietf::{self, FetchHeader, RequestId},
	setup,
	util::{MaybeBoxedExt, MaybeSendBox, TaskSet, err_only},
};

use super::{Control, Message, Publisher, Subscriber, Version, adapter::ControlStreamAdapter};

// Handshake dispatcher: each argument is an independent session parameter, so
// bundling them into a config struct would just add indirection.
#[allow(clippy::too_many_arguments)]
pub fn start<S: web_transport_trait::Session>(
	session: S,
	setup: Option<Stream<S, Version>>,
	request_id_max: Option<RequestId>,
	client: bool,
	// Traffic stats are attributed through these origin handles: tag them with
	// `origin::{Consumer, Producer}::with_stats` before calling `start`.
	publish: Option<origin::Consumer>,
	subscribe: Option<origin::Producer>,
	version: Version,
	// The request path we advertise in our SETUP (draft-17+ clients on URL-less
	// transports). A server passes `None`.
	path: Option<String>,
	// The peer's SETUP stream, when it was already read before `start` (a draft-17+
	// server that gated on the client's path via [`accept_setup`]). It becomes the
	// GOAWAY channel; `None` lets the uni loop read the SETUP itself.
	peer_setup: Option<Reader<S::RecvStream, crate::Version>>,
) -> Result<(MaybeSendBox<'static, Result<(), Error>>, crate::goaway::Handle), Error> {
	// GOAWAY wiring: the public Session holds one half (drain trigger, received
	// signal), the protocol tasks below hold the other.
	let (goaway_handle, goaway) = crate::goaway::Handle::new();
	let driver = async move {
		// moq-transport threads concrete origins through the publisher/subscriber.
		// An unset half gets an empty origin: an empty publish origin announces
		// nothing, and an empty subscribe origin issues no SUBSCRIBE_NAMESPACE.
		let publish = publish.unwrap_or_else(|| origin::Producer::empty(Origin::random()).consume());
		let subscribe = subscribe.unwrap_or_else(|| origin::Producer::empty(Origin::random()));
		let res = match version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 => {
				let Some(setup) = setup else {
					let err = Error::ProtocolViolation;
					session.close(err.to_code(), "setup stream required");
					return Err(err);
				};
				let control = Control::new(request_id_max, client);
				let adapter = ControlStreamAdapter::new(session.clone(), control.clone(), version);

				let publisher = Publisher::new(adapter.clone(), publish, control.clone(), version);
				let (tasks, mut task_set) = TaskSet::new();
				let subscriber = Subscriber::new(
					adapter.clone(),
					subscribe,
					control,
					version,
					tasks.clone(),
					goaway.going_away.clone(),
				);

				// GOAWAY send task: draft-14-16 carry GOAWAY on the shared control
				// stream. Parked on the drain trigger; races the transport close so
				// a parked trigger never blocks the task set draining.
				{
					let session = session.clone();
					let adapter = adapter.clone();
					let goaway = goaway.clone();
					tasks.push(async move {
						let payload = {
							let mut closed = std::pin::pin!(async { session.closed().await });
							let mut triggered = std::pin::pin!(goaway.triggered());
							kio::wait(|waiter| {
								if waiter.poll_future(closed.as_mut()).is_ready() {
									return std::task::Poll::Ready(None);
								}
								waiter.poll_future(triggered.as_mut())
							})
							.await
						};
						let Some(payload) = payload else {
							return;
						};
						let timeout_ms = payload.timeout.map(|d| d.as_millis() as u64).unwrap_or(0);
						adapter.send_goaway(&payload.uri, timeout_ms, version);
					});
				}
				drop(tasks);

				let dispatch_session = adapter.clone();
				let mut sub_ns = subscriber.clone();
				let sub_ns_adapter = adapter.clone();

				// Every half only ends the session on error (err_only parks on clean
				// completion); the task set draining is the one clean exit.
				let mut adapter_run = std::pin::pin!(err_only(adapter.run(setup.reader, setup.writer, goaway.clone())));
				let mut unis = std::pin::pin!(err_only(run_unis(adapter.clone(), subscriber.clone(), version, goaway)));
				let mut dispatch = std::pin::pin!(err_only(run_dispatch(
					dispatch_session,
					publisher.clone(),
					subscriber.clone(),
					version
				)));
				let mut publisher_run = std::pin::pin!(err_only(publisher.run()));
				let mut sub_ns_run = std::pin::pin!(err_only(async {
					let stream = match version {
						Version::Draft16 => {
							let (send, recv) = sub_ns_adapter.open_native_bi().await?;
							Stream {
								writer: crate::coding::Writer::new(send, version),
								reader: crate::coding::Reader::new(recv, version),
							}
						}
						_ => Stream::open(&sub_ns_adapter, version).await?,
					};
					if let Err(err) = sub_ns.run_subscribe_namespace(stream).await {
						tracing::warn!(%err, "subscribe_namespace failed, continuing without");
					}
					Ok(())
				}));

				kio::wait(|waiter| {
					use std::task::Poll;
					if let Poll::Ready(err) = waiter.poll_future(adapter_run.as_mut()) {
						return Poll::Ready(Err::<(), Error>(err));
					}
					if let Poll::Ready(err) = waiter.poll_future(unis.as_mut()) {
						return Poll::Ready(Err(err));
					}
					if let Poll::Ready(err) = waiter.poll_future(dispatch.as_mut()) {
						return Poll::Ready(Err(err));
					}
					if let Poll::Ready(err) = waiter.poll_future(publisher_run.as_mut()) {
						return Poll::Ready(Err(err));
					}
					if task_set.poll(waiter).is_ready() {
						return Poll::Ready(Ok(()));
					}
					if let Poll::Ready(err) = waiter.poll_future(sub_ns_run.as_mut()) {
						return Poll::Ready(Err(err));
					}
					Poll::Pending
				})
				.await
			}
			_ => {
				// Send SETUP and keep the stream alive: it is also our GOAWAY channel.
				let setup = {
					let session = session.clone();
					let goaway = goaway.clone();
					async move {
						if let Err(err) = run_setup(session, version, path, goaway).await {
							tracing::warn!(%err, "setup send error");
						}
						std::future::pending::<()>().await;
					}
				};

				let control = Control::new(None, client);
				let publisher = Publisher::new(session.clone(), publish, control.clone(), version);
				let (tasks, mut task_set) = TaskSet::new();
				let subscriber = Subscriber::new(
					session.clone(),
					subscribe,
					control,
					version,
					tasks,
					goaway.going_away.clone(),
				);

				let sub_ns_session = session.clone();
				let mut sub_ns = subscriber.clone();

				// When the peer's SETUP was pre-read (a gated server accept), monitor
				// GOAWAY on that stream here; otherwise `run_unis` does it when the SETUP
				// arrives on the wire.
				let goaway_recv = {
					let goaway = goaway.clone();
					async move {
						match peer_setup {
							Some(reader) => run_goaway(reader.with_version(version), version, goaway).await,
							None => std::future::pending().await,
						}
					}
				};

				// Every half only ends the session on error (err_only parks on clean
				// completion); `setup` never resolves (it holds the stream open) and the
				// task set draining is the one clean exit.
				let mut unis = std::pin::pin!(err_only(run_unis(session.clone(), subscriber.clone(), version, goaway)));
				let mut dispatch = std::pin::pin!(err_only(run_dispatch(
					session.clone(),
					publisher.clone(),
					subscriber.clone(),
					version
				)));
				let mut publisher_run = std::pin::pin!(err_only(publisher.run()));
				let mut goaway_recv = std::pin::pin!(err_only(goaway_recv));
				let mut setup = std::pin::pin!(setup);
				let mut sub_ns_run = std::pin::pin!(err_only(async {
					let stream = Stream::open(&sub_ns_session, version).await?;
					if let Err(err) = sub_ns.run_subscribe_namespace(stream).await {
						tracing::warn!(%err, "subscribe_namespace failed, continuing without");
					}
					Ok(())
				}));

				kio::wait(|waiter| {
					use std::task::Poll;
					if let Poll::Ready(err) = waiter.poll_future(unis.as_mut()) {
						return Poll::Ready(Err::<(), Error>(err));
					}
					if let Poll::Ready(err) = waiter.poll_future(dispatch.as_mut()) {
						return Poll::Ready(Err(err));
					}
					if let Poll::Ready(err) = waiter.poll_future(publisher_run.as_mut()) {
						return Poll::Ready(Err(err));
					}
					if let Poll::Ready(err) = waiter.poll_future(goaway_recv.as_mut()) {
						return Poll::Ready(Err(err));
					}
					if waiter.poll_future(setup.as_mut()).is_ready() {
						return Poll::Ready(Ok(()));
					}
					if task_set.poll(waiter).is_ready() {
						return Poll::Ready(Ok(()));
					}
					if let Poll::Ready(err) = waiter.poll_future(sub_ns_run.as_mut()) {
						return Poll::Ready(Err(err));
					}
					Poll::Pending
				})
				.await
			}
		};

		match &res {
			Err(Error::Transport(_)) => {
				tracing::info!("session terminated");
				session.close(1, "");
			}
			Err(err) => {
				tracing::warn!(%err, "session error");
				session.close(err.to_code(), err.to_string().as_ref());
			}
			_ => {
				tracing::info!("session closed");
				session.close(0, "");
			}
		}

		res
	}
	.maybe_boxed();

	Ok((driver, goaway_handle))
}

/// Server (draft-17+): read the peer's SETUP off its uni stream before starting the
/// session, returning that stream (it becomes the GOAWAY channel) and the request
/// path the peer advertised.
///
/// Blocks on the peer's Setup Stream; any other uni stream racing ahead of it is
/// `STOP_SENDING`-ed and skipped (group data needs a prior subscribe, so nothing
/// legitimate precedes the SETUP at connect). Pass the returned reader to [`start`]
/// as its `peer_setup` so GOAWAY monitoring continues without re-reading it.
pub async fn accept_setup<S: web_transport_trait::Session>(
	session: &S,
	version: Version,
) -> Result<(Reader<S::RecvStream, crate::Version>, Option<String>), Error> {
	let outer_version = crate::Version::Ietf(version);

	loop {
		let recv = session.accept_uni().await.map_err(Error::from_transport)?;
		let mut reader: Reader<S::RecvStream, crate::Version> = Reader::new(recv, outer_version);

		if reader.decode_peek::<u64>().await? != setup::SETUP_V17 {
			// Not the SETUP (group data this early is unexpected). Reject and keep waiting.
			reader.abort(&Error::UnexpectedStream);
			continue;
		}

		let setup: setup::Setup = reader.decode().await?;
		let mut bytes = setup.parameters;
		let path = match ietf::Parameters::decode(&mut bytes, version)?.get_bytes(ietf::ParameterBytes::Path) {
			Some(bytes) => Some(
				std::str::from_utf8(bytes)
					.map_err(|_| Error::Decode(crate::DecodeError::InvalidValue))?
					.to_owned(),
			),
			None => None,
		};

		return Ok((reader, path));
	}
}

/// Send our SETUP on a uni stream and keep it alive: on draft-17+ this stream is
/// also our GOAWAY channel, so a fired drain trigger encodes the GOAWAY here.
///
/// `path` is the request path we advertise (clients on URL-less transports); a
/// server passes `None`.
async fn run_setup<S: web_transport_trait::Session>(
	session: S,
	version: Version,
	path: Option<String>,
	goaway: crate::goaway::Protocol,
) -> Result<(), Error> {
	let outer_version = crate::Version::Ietf(version);

	let send = session.open_uni().await.map_err(Error::from_transport)?;
	let mut writer: Writer<S::SendStream, crate::Version> = Writer::new(send, outer_version);

	let mut parameters = ietf::Parameters::default();
	parameters.set_bytes(ietf::ParameterBytes::Implementation, b"moq-lite-rs".to_vec());
	if let Some(path) = path {
		parameters.set_bytes(ietf::ParameterBytes::Path, path.into_bytes());
	}
	let parameters = parameters.encode_bytes(version)?;

	writer.encode(&setup::Setup { parameters }).await?;

	// Hold the writer alive until the session closes, sending a GOAWAY if the
	// drain trigger fires meanwhile. The trigger resolves `None` when the session
	// drops without draining; keep holding either way (closing this stream
	// mid-session is a protocol violation on strict peers).
	let payload = {
		let mut closed = std::pin::pin!(session.closed());
		let mut triggered = std::pin::pin!(goaway.triggered());
		kio::wait(|waiter| {
			if waiter.poll_future(closed.as_mut()).is_ready() {
				return std::task::Poll::Ready(None);
			}
			waiter.poll_future(triggered.as_mut())
		})
		.await
	};

	if let Some(payload) = payload {
		let timeout_ms = payload.timeout.map(|d| d.as_millis() as u64).unwrap_or(0);
		let msg = ietf::GoAway {
			new_session_uri: std::borrow::Cow::Borrowed(payload.uri.as_ref()),
			timeout: timeout_ms,
		};

		// Frame as [type_id varint][size u16][body], the same shape as the
		// control-stream messages this channel otherwise carries.
		let mut body = bytes::BytesMut::new();
		msg.encode_msg(&mut body, version)?;
		let size: u16 = body
			.len()
			.try_into()
			.map_err(|_| Error::BoundsExceeded(crate::coding::BoundsExceeded))?;

		let mut writer = writer.with_version(version);
		writer.encode(&ietf::GoAway::ID).await?;
		writer.encode(&size).await?;
		writer.write_all(&mut std::io::Cursor::new(body)).await?;

		session.closed().await;
		writer.finish().ok();
	} else {
		writer.finish().ok();
	}

	Ok(())
}

/// Accept incoming uni streams and dispatch each to a handler.
///
/// For v17, this also handles the SETUP stream (0x2F00) and GOAWAY.
/// For v14-16, all uni streams are group data.
async fn run_unis<S: web_transport_trait::Session>(
	session: S,
	subscriber: Subscriber<S>,
	version: Version,
	goaway: crate::goaway::Protocol,
) -> Result<(), Error> {
	let outer_version = crate::Version::Ietf(version);
	let mut tasks = TaskSet::owned();

	loop {
		let recv = tasks.drive(session.accept_uni()).await.map_err(Error::from_transport)?;
		let mut reader: Reader<S::RecvStream, crate::Version> = Reader::new(recv, outer_version);
		let kind: u64 = tasks.drive(reader.decode_peek()).await?;

		// v17+: SETUP arrives on a uni stream, then becomes the GOAWAY channel.
		// We accept it in the background without blocking, since there are no
		// extensions that require waiting on the SETUP before proceeding.
		if kind == setup::SETUP_V17 {
			let goaway = goaway.clone();
			tasks.push(async move {
				// Decode and discard the unified SETUP message.
				if let Err(err) = reader.decode::<setup::Setup>().await {
					tracing::warn!(%err, "setup decode error");
					return;
				}

				// Monitor for GOAWAY after setup completes.
				if let Err(err) = run_goaway(reader.with_version(version), version, goaway).await {
					tracing::warn!(%err, "goaway error");
				}
			});

			continue;
		}

		// Poll one child handler for each group stream.
		let mut sub = subscriber.clone();
		tasks.push(async move {
			let mut reader = reader.with_version(version);
			if let Err(err) = run_uni_group(&mut sub, &mut reader).await {
				tracing::debug!(%err, "uni stream error");
				reader.abort(&err);
			}
		});
	}
}

async fn run_uni_group<S: web_transport_trait::Session>(
	subscriber: &mut Subscriber<S>,
	stream: &mut Reader<S::RecvStream, Version>,
) -> Result<(), Error> {
	let kind: u64 = stream.decode_peek().await?;

	// SUBGROUP_HEADER type bytes match the form 0b0XX1XXXX (spec §11.4.2):
	// draft-14-17 use 0x10-0x1D and 0x30-0x3D, draft-18 adds 0x40 (FIRST_OBJECT)
	// extending the form to also cover 0x50-0x5D and 0x70-0x7D. Per-version and
	// per-bit validation (e.g., FIRST_OBJECT must be 0 on draft-17) is done in
	// `GroupFlags::decode`.
	if kind <= 0xff && (kind & 0x90) == 0x10 {
		return subscriber.recv_group(stream).await;
	}

	match kind {
		FetchHeader::TYPE => Err(Error::Unsupported),
		_ => Err(Error::UnexpectedStream),
	}
}

/// Accept incoming bidi streams and dispatch to the correct handler based on message type.
async fn run_dispatch<S: web_transport_trait::Session>(
	session: S,
	publisher: Publisher<S>,
	mut subscriber: Subscriber<S>,
	version: Version,
) -> Result<(), Error> {
	let mut tasks = TaskSet::owned();
	loop {
		let mut stream = tasks.drive(Stream::accept(&session, version)).await?;

		let header = tasks
			.drive(async {
				let id: u64 = stream.reader.decode().await?;
				let size: u16 = stream.reader.decode().await?;
				let data = stream.reader.read_exact(size as usize).await?;
				Ok::<_, Error>((id, data))
			})
			.await;
		let (id, data) = header?;

		match id {
			// Publisher handles: Subscribe, Fetch, SubscribeNamespace (0x50 modern /
			// 0x11 legacy), TrackStatus
			ietf::Subscribe::ID
			| ietf::Fetch::ID
			| ietf::SubscribeNamespace::ID
			| ietf::SubscribeNamespaceLegacy::ID
			| ietf::TrackStatus::ID => {
				tasks.push(publisher.handle_stream(id, data, stream)?);
			}
			// Subscriber handles: Publish, PublishNamespace
			ietf::Publish::ID | ietf::PublishNamespace::ID => {
				tasks.push(subscriber.handle_stream(id, data, stream)?);
			}
			_ => {
				tracing::warn!(id, "unexpected bidi stream type");
				return Err(Error::UnexpectedStream);
			}
		}
	}
}

/// Monitor the peer's SETUP stream for a GOAWAY, surfacing it through
/// [`crate::Session::goaway`], then hold the stream until it FINs.
async fn run_goaway<R: web_transport_trait::RecvStream>(
	mut reader: Reader<R, Version>,
	version: Version,
	goaway: crate::goaway::Protocol,
) -> Result<(), Error> {
	let id: u64 = match reader.decode_maybe().await? {
		Some(id) => id,
		None => return Ok(()),
	};

	let size: u16 = reader.decode::<u16>().await?;
	let mut data = reader.read_exact(size as usize).await?;

	if id != ietf::GoAway::ID {
		return Err(Error::UnexpectedMessage);
	}

	let msg = ietf::GoAway::decode_msg(&mut data, version)?;
	tracing::info!(message = ?msg, "received GOAWAY");

	let timeout = (msg.timeout > 0).then(|| std::time::Duration::from_millis(msg.timeout));
	let received = crate::GoawayReceived {
		uri: std::sync::Arc::from(msg.new_session_uri.as_ref()),
		timeout,
	};
	if !goaway.record(received) {
		tracing::warn!("duplicate GOAWAY received; ignoring");
	}

	// Keep the reader alive until the peer FINs or the session closes. Dropping
	// it here would STOP_SENDING the peer's SETUP uni stream, which draft-19
	// sect 3.3 forbids closing at the transport layer mid-session, so a strict
	// peer would tear the session down as a PROTOCOL_VIOLATION right in the
	// middle of the drain we are trying to honor.
	//
	// Nothing else is expected on this stream: a peer sends at most one GOAWAY
	// per session. Read (and discard) any further framed messages so a
	// duplicate GOAWAY is surfaced in the logs rather than silently consumed.
	loop {
		let id: u64 = match reader.decode_maybe().await? {
			Some(id) => id,
			None => return Ok(()),
		};
		let size: u16 = reader.decode::<u16>().await?;
		let _ = reader.read_exact(size as usize).await?;
		tracing::warn!(
			id,
			duplicate_goaway = id == ietf::GoAway::ID,
			"unexpected message after GOAWAY on the SETUP stream; ignoring"
		);
	}
}
