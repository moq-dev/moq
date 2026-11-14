//! Socket endpoints: two-way sessions between endpoints sharing an origin.
//!
//! A `tcp` session's halves are each one group: write with [crate::moq_publish_group_frame] and
//! read frames in order. A `track` session's halves are each a whole track: write with
//! [crate::moq_publish_track_group] and read it like [crate::moq_consume_track].

use std::ffi::{c_char, c_void};
use std::sync::Arc;

use moq_net::socket::{Endpoint, Incoming, TcpStream, TrackStream};
use tokio::sync::oneshot;

use crate::ffi::{self, Background, OnStatus};
use crate::publish::Children;
use crate::task::Task;
use crate::{Error, Id, NonZeroSlab, State, moq_string, moq_track_info};

/// Socket endpoints and the sessions they dial or accept.
#[derive(Default)]
pub struct Socket {
	endpoints: NonZeroSlab<EndpointEntry>,
	/// Dials, accept loops and accepts in progress.
	tasks: NonZeroSlab<Task>,
	incoming: NonZeroSlab<IncomingEntry>,
	streams: NonZeroSlab<StreamEntry>,
}

struct EndpointEntry {
	inner: Arc<Endpoint>,
	driver: Background,
	/// The endpoint's `track` halves handed to C, ended when it closes.
	children: Children,
}

enum IncomingKind {
	Tcp(Incoming<TcpStream>),
	Track(Incoming<TrackStream>),
}

struct IncomingEntry {
	inner: IncomingKind,
	peer: String,
	session: String,
	driver: Background,
	children: Children,
}

/// The peer's half of a stream, read through [Consume](crate::Consume).
pub(crate) enum Half {
	Track(Box<moq_net::track::Subscriber>),
	Group(moq_net::group::Consumer),
}

/// This side's half of a stream, until C takes it.
enum Writer {
	Track(moq_net::track::Producer),
	Group(moq_net::group::Producer),
}

struct StreamEntry {
	peer: String,
	session: String,
	writer: Option<Writer>,
	reader: Option<Half>,
	driver: Background,
	children: Children,
}

impl StreamEntry {
	fn tcp(stream: TcpStream, driver: Background, children: Children) -> Self {
		let (peer, session) = (stream.peer().to_string(), stream.session().to_string());
		Self {
			peer,
			session,
			writer: Some(Writer::Group(stream.producer)),
			reader: Some(Half::Group(stream.consumer)),
			driver,
			children,
		}
	}

	fn track(stream: TrackStream, driver: Background, children: Children) -> Self {
		let (peer, session) = (stream.peer().to_string(), stream.session().to_string());
		Self {
			peer,
			session,
			writer: Some(Writer::Track(stream.producer)),
			reader: Some(Half::Track(Box::new(stream.consumer))),
			driver,
			children,
		}
	}
}

/// Deliver `id` and end, or end without one when `close` fires first.
async fn deliver(
	callback: OnStatus,
	close: oneshot::Receiver<()>,
	pending: impl Future<Output = Result<StreamEntry, Error>>,
) -> Result<(), Error> {
	// `biased` so a pending close always wins over a ready stream.
	let stream = tokio::select! {
		biased;
		_ = close => return Ok(()),
		res = pending => res?,
	};
	let id = State::lock().socket.streams.insert(stream)?;
	callback.call(Ok(id));
	Ok(())
}

impl Socket {
	/// The open and kept counts of an endpoint's children.
	#[cfg(test)]
	pub(crate) fn endpoint_children(&self, id: Id) -> (usize, usize) {
		self.endpoints.get(id).expect("endpoint").children.counts()
	}

	fn endpoint(&self, id: Id) -> Result<&EndpointEntry, Error> {
		let endpoint = self.endpoints.get(id).ok_or(Error::NotFound)?;
		endpoint.driver.check()?;
		Ok(endpoint)
	}

	fn create(&mut self, origin: &crate::origin::Producer, prefix: &str, id: &str) -> Result<Id, Error> {
		let inner = Endpoint::new(origin, prefix, id)?;
		self.endpoints.insert(EndpointEntry {
			inner: Arc::new(inner),
			driver: origin.driver.clone(),
			children: Children::unkept(),
		})
	}

	fn dial(
		&mut self,
		endpoint: Id,
		peer: &str,
		track: Option<Option<moq_net::track::Info>>,
		callback: OnStatus,
	) -> Result<Id, Error> {
		let EndpointEntry {
			inner,
			driver,
			children,
		} = self.endpoint(endpoint)?;
		let (driver, children) = (driver.clone(), children.clone());
		let watch = driver.clone();
		let pending: std::pin::Pin<Box<dyn Future<Output = Result<StreamEntry, Error>> + Send>> = match track {
			None => {
				let dialing = inner.dial_tcp(peer, 0)?;
				Box::pin(async move { Ok(StreamEntry::tcp(dialing.await?, driver, children)) })
			}
			Some(info) => {
				let dialing = inner.dial_track(peer, info)?;
				Box::pin(async move { Ok(StreamEntry::track(dialing.await?, driver, children)) })
			}
		};
		self.tasks.spawn(
			callback,
			|state| &mut state.socket.tasks,
			|close| async move { watch.watch(deliver(callback, close, pending)).await },
		)
	}

	fn accept(&mut self, endpoint: Id, track: bool, callback: OnStatus) -> Result<Id, Error> {
		let EndpointEntry {
			inner,
			driver,
			children,
		} = self.endpoint(endpoint)?;
		let (inner, driver, children) = (inner.clone(), driver.clone(), children.clone());
		let poll = move |waiter: &kio::Waiter| match track {
			false => inner.poll_accept_tcp(waiter).map_ok(IncomingKind::Tcp),
			true => inner.poll_accept_track(waiter).map_ok(IncomingKind::Track),
		};
		// The first poll starts listening, so poll once before returning: a dial sent after
		// this call then queues for the loop instead of finding nobody listening.
		let mut first = match poll(&kio::Waiter::noop()) {
			std::task::Poll::Ready(Err(err)) if !matches!(err, moq_net::Error::Closed) => return Err(err.into()),
			std::task::Poll::Ready(res) => Some(res),
			std::task::Poll::Pending => None,
		};
		self.tasks.spawn(
			callback,
			|state| &mut state.socket.tasks,
			|mut close| async move {
				let watch = driver.clone();
				let run = async move {
					loop {
						// `biased` so a pending close always wins over a ready dial, the one the
						// first poll buffered included.
						let next = tokio::select! {
							biased;
							_ = &mut close => return Ok(()),
							res = async {
								match first.take() {
									Some(res) => res,
									None => kio::wait(&poll).await,
								}
							} => res,
						};
						let incoming = match next {
							Ok(incoming) => incoming,
							// The endpoint closed: no more dials will arrive.
							Err(moq_net::Error::Closed) => return Ok(()),
							Err(err) => return Err(err.into()),
						};
						let (peer, session) = match &incoming {
							IncomingKind::Tcp(incoming) => (incoming.peer(), incoming.session()),
							IncomingKind::Track(incoming) => (incoming.peer(), incoming.session()),
						};
						let entry = IncomingEntry {
							peer: peer.to_string(),
							session: session.to_string(),
							inner: incoming,
							driver: driver.clone(),
							children: children.clone(),
						};
						let id = State::lock().socket.incoming.insert(entry)?;
						callback.call(Ok(id));
					}
				};
				watch.watch(run).await
			},
		)
	}

	fn incoming_accept(
		&mut self,
		incoming: Id,
		info: Option<moq_net::track::Info>,
		callback: OnStatus,
	) -> Result<Id, Error> {
		let entry = self.incoming.get(incoming).ok_or(Error::NotFound)?;
		entry.driver.check()?;
		if info.is_some() && matches!(entry.inner, IncomingKind::Tcp(_)) {
			return Err(Error::InvalidConfig("a tcp session has no track info".into()));
		}
		let IncomingEntry {
			inner,
			driver,
			children,
			..
		} = self.incoming.remove(incoming).ok_or(Error::NotFound)?;
		let watch = driver.clone();
		let pending: std::pin::Pin<Box<dyn Future<Output = Result<StreamEntry, Error>> + Send>> = match inner {
			IncomingKind::Tcp(incoming) => {
				let accepting = incoming.accept(0);
				Box::pin(async move { Ok(StreamEntry::tcp(accepting.await?, driver, children)) })
			}
			IncomingKind::Track(incoming) => {
				let accepting = incoming.accept(info);
				Box::pin(async move { Ok(StreamEntry::track(accepting.await?, driver, children)) })
			}
		};
		self.tasks.spawn(
			callback,
			|state| &mut state.socket.tasks,
			|close| async move { watch.watch(deliver(callback, close, pending)).await },
		)
	}

	fn incoming_info(
		&self,
		incoming: Id,
		peer: Option<&mut moq_string>,
		session: Option<&mut moq_string>,
	) -> Result<(), Error> {
		let entry = self.incoming.get(incoming).ok_or(Error::NotFound)?;
		write_ids(&entry.peer, &entry.session, peer, session);
		Ok(())
	}

	fn stream_info(
		&self,
		stream: Id,
		peer: Option<&mut moq_string>,
		session: Option<&mut moq_string>,
	) -> Result<(), Error> {
		let entry = self.streams.get(stream).ok_or(Error::NotFound)?;
		write_ids(&entry.peer, &entry.session, peer, session);
		Ok(())
	}
}

fn write_ids(peer: &str, session: &str, peer_dst: Option<&mut moq_string>, session_dst: Option<&mut moq_string>) {
	let view = |s: &str| moq_string {
		data: s.as_ptr().cast::<c_char>(),
		len: s.len(),
	};
	if let Some(dst) = peer_dst {
		*dst = view(peer);
	}
	if let Some(dst) = session_dst {
		*dst = view(session);
	}
}

/// Open a socket endpoint named `id` under `prefix` on an origin, announcing it to the peers
/// reachable through that origin.
///
/// `id` names the endpoint: a well-known name for a server, or a random one for a client. It
/// must be one path segment. Peers find each other under the same `prefix`, so connect the
/// origin to a session (or share it locally) before dialing. Close it with [moq_socket_close].
///
/// Returns a positive endpoint handle, or a negative code, such as when the origin may not
/// publish `<prefix>/.socket/<id>`.
///
/// # Safety
/// - `prefix` must point at `prefix_len` bytes, and `id` at `id_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_socket_create(
	origin: u32,
	prefix: *const c_char,
	prefix_len: usize,
	id: *const c_char,
	id_len: usize,
) -> i32 {
	ffi::enter(move || {
		let origin = ffi::parse_id(origin)?;
		let prefix = unsafe { ffi::parse_str(prefix, prefix_len)? };
		let id = unsafe { ffi::parse_str(id, id_len)? };
		let mut state = State::lock();
		let origin = state.origin.get(origin)?.clone();
		state.socket.create(&origin, prefix, id)
	})
}

/// Close a socket endpoint and release its handle.
///
/// Withdraws the endpoint, ends its accept loops with a terminal `0`, fails its pending dials
/// and accepts, and resets its open sessions, aborting each `track` half still open. Handles
/// already delivered stay valid until released.
///
/// Returns zero, or a negative code if the endpoint is unknown.
#[unsafe(no_mangle)]
pub extern "C" fn moq_socket_close(endpoint: u32) -> i32 {
	ffi::enter(move || {
		let endpoint = ffi::parse_id(endpoint)?;
		let mut state = State::lock();
		let entry = state.socket.endpoints.remove(endpoint).ok_or(Error::NotFound)?;
		entry.inner.close();
		state.publish.socket_close(&entry.children);
		Ok(())
	})
}

/// Dial the endpoint `peer` in a new `tcp` session: one ordered, reliable byte-message stream
/// each way.
///
/// Waits for as long as the peer takes to accept, including for its endpoint to be announced.
/// `on_stream` fires with a positive stream handle once the peer accepts, then exactly once
/// more with a terminal code: `0` (including after [moq_socket_cancel], which gives up and
/// resets the session) or a negative error, `MOQ_ERROR_NOT_FOUND` when the peer rejects it
/// and `MOQ_ERROR_UNSUPPORTED` for a dial to this endpoint itself or across a hop that cannot
/// carry it. Release `user_data` at the terminal callback.
///
/// Returns a positive task handle, or a negative code.
///
/// # Safety
/// - `peer` must point at `peer_len` bytes.
/// - `on_stream` must be non-NULL, and `user_data` valid until its terminal callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_socket_dial_tcp(
	endpoint: u32,
	peer: *const c_char,
	peer_len: usize,
	on_stream: ffi::moq_status_callback,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let endpoint = ffi::parse_id(endpoint)?;
		let peer = unsafe { ffi::parse_str(peer, peer_len)? };
		let callback = unsafe { OnStatus::new(user_data, on_stream)? };
		State::lock().socket.dial(endpoint, peer, None, callback)
	})
}

/// Dial the endpoint `peer` in a new `track` session, whose halves are whole tracks; see
/// [moq_socket_dial_tcp].
///
/// `info` describes this side's half, or NULL to keep every message until read, with no
/// max age.
///
/// # Safety
/// - `peer` must point at `peer_len` bytes, and `info` be NULL or point at a [moq_track_info].
/// - `on_stream` must be non-NULL, and `user_data` valid until its terminal callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_socket_dial_track(
	endpoint: u32,
	peer: *const c_char,
	peer_len: usize,
	info: *const moq_track_info,
	on_stream: ffi::moq_status_callback,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let endpoint = ffi::parse_id(endpoint)?;
		let peer = unsafe { ffi::parse_str(peer, peer_len)? };
		let info = unsafe { info.as_ref() }.map(TryInto::try_into).transpose()?;
		let callback = unsafe { OnStatus::new(user_data, on_stream)? };
		State::lock().socket.dial(endpoint, peer, Some(info), callback)
	})
}

/// Accept `tcp` dials from peers.
///
/// `on_incoming` fires with a positive incoming handle for each dial, then exactly once more
/// with a terminal code: `0` once the endpoint closed or after [moq_socket_cancel], or a
/// negative error, `MOQ_ERROR_UNSUPPORTED` while no peer can reach the endpoint. Accept each
/// dial with [moq_socket_incoming_accept] or refuse it with [moq_socket_incoming_reject]. Dials
/// queue until delivered, or until their dialer gives up. Run one loop per protocol.
///
/// Returns a positive task handle, or a negative code.
///
/// # Safety
/// - `on_incoming` must be non-NULL, and `user_data` valid until its terminal callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_socket_accept_tcp(
	endpoint: u32,
	on_incoming: ffi::moq_status_callback,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let endpoint = ffi::parse_id(endpoint)?;
		let callback = unsafe { OnStatus::new(user_data, on_incoming)? };
		State::lock().socket.accept(endpoint, false, callback)
	})
}

/// Accept `track` dials from peers; see [moq_socket_accept_tcp].
///
/// # Safety
/// - `on_incoming` must be non-NULL, and `user_data` valid until its terminal callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_socket_accept_track(
	endpoint: u32,
	on_incoming: ffi::moq_status_callback,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let endpoint = ffi::parse_id(endpoint)?;
		let callback = unsafe { OnStatus::new(user_data, on_incoming)? };
		State::lock().socket.accept(endpoint, true, callback)
	})
}

/// Stop a dial, an accept loop, or an incoming accept.
///
/// Its callback still fires once more with a terminal `0`; release `user_data` there. A
/// stopped dial or accept resets its session. Returns zero, or a negative code if it already
/// ended.
#[unsafe(no_mangle)]
pub extern "C" fn moq_socket_cancel(task: u32) -> i32 {
	ffi::enter(move || {
		let task = ffi::parse_id(task)?;
		State::lock().socket.tasks.close(task, Error::NotFound)
	})
}

/// Read an incoming dial's claimed peer id and session id.
///
/// Either output may be NULL. The strings borrow the handle until it is accepted or rejected.
///
/// # Safety
/// - `peer` and `session` must each be NULL or point at a writable [moq_string].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_socket_incoming_info(
	incoming: u32,
	peer: *mut moq_string,
	session: *mut moq_string,
) -> i32 {
	ffi::enter(move || {
		let incoming = ffi::parse_id(incoming)?;
		let (peer, session) = unsafe { (peer.as_mut(), session.as_mut()) };
		State::lock().socket.incoming_info(incoming, peer, session)
	})
}

/// Accept an incoming dial, releasing its handle.
///
/// Verifies the dialer really dialed this session first. `info` describes this side's half of a
/// `track` session, or NULL to keep every message until read; a `tcp` session takes NULL.
/// `on_stream` fires like [moq_socket_dial_tcp]'s, and [moq_socket_cancel] gives up and resets
/// the session.
///
/// Returns a positive task handle, or a negative code.
///
/// # Safety
/// - `info` must be NULL or point at a [moq_track_info].
/// - `on_stream` must be non-NULL, and `user_data` valid until its terminal callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_socket_incoming_accept(
	incoming: u32,
	info: *const moq_track_info,
	on_stream: ffi::moq_status_callback,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let incoming = ffi::parse_id(incoming)?;
		let info = unsafe { info.as_ref() }.map(TryInto::try_into).transpose()?;
		let callback = unsafe { OnStatus::new(user_data, on_stream)? };
		State::lock().socket.incoming_accept(incoming, info, callback)
	})
}

/// Refuse an incoming dial as not found, releasing its handle.
#[unsafe(no_mangle)]
pub extern "C" fn moq_socket_incoming_reject(incoming: u32) -> i32 {
	ffi::enter(move || {
		let incoming = ffi::parse_id(incoming)?;
		let entry = State::lock().socket.incoming.remove(incoming).ok_or(Error::NotFound)?;
		match entry.inner {
			IncomingKind::Tcp(incoming) => incoming.reject(),
			IncomingKind::Track(incoming) => incoming.reject(),
		}
		Ok(())
	})
}

/// Read a stream's peer endpoint id and session id.
///
/// Either output may be NULL. The strings borrow the stream handle until [moq_socket_stream_free].
///
/// # Safety
/// - `peer` and `session` must each be NULL or point at a writable [moq_string].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_socket_stream_info(stream: u32, peer: *mut moq_string, session: *mut moq_string) -> i32 {
	ffi::enter(move || {
		let stream = ffi::parse_id(stream)?;
		let (peer, session) = unsafe { (peer.as_mut(), session.as_mut()) };
		State::lock().socket.stream_info(stream, peer, session)
	})
}

/// Take this side's half of a stream, which outlives the stream handle.
///
/// A `tcp` session's half is a group handle: write with [crate::moq_publish_group_frame], pacing
/// with [crate::moq_publish_group_ready], and end it with [crate::moq_publish_group_finish] (or
/// abort it). Dropping it unfinished resets the session. A `track` session's half is a raw track
/// handle, written and ended with the `moq_publish_track_*` calls.
///
/// Returns the positive handle, or a negative code once taken.
#[unsafe(no_mangle)]
pub extern "C" fn moq_socket_stream_writer(stream: u32) -> i32 {
	ffi::enter(move || {
		let stream = ffi::parse_id(stream)?;
		let mut state = State::lock();
		let entry = state.socket.streams.get_mut(stream).ok_or(Error::NotFound)?;
		let children = entry.children.clone();
		match entry.writer.take().ok_or(Error::Moq(moq_net::Error::Closed))? {
			Writer::Group(group) => state.publish.socket_group(group),
			Writer::Track(track) => state.publish.socket_track(track, &children),
		}
	})
}

/// Read the peer's half of a stream, which outlives the stream handle; callable once.
///
/// Works like [crate::moq_consume_track]: `on_frame` fires with a positive frame handle for each
/// frame in order, read with [crate::moq_consume_track_frame] and released with
/// [crate::moq_consume_track_frame_free], then exactly once more with a terminal code: `0` once
/// the peer finished (or after [crate::moq_consume_track_cancel]), or a negative error when it
/// reset. A `track` session's half is subscribed reliably and oldest first;
/// [crate::moq_consume_track_update] changes that, while a `tcp` session's refuses it with
/// `MOQ_ERROR_UNSUPPORTED`.
///
/// Returns a positive track handle, or a negative code once taken.
///
/// # Safety
/// - `on_frame` must be non-NULL, and `user_data` valid until its terminal callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_socket_stream_reader(
	stream: u32,
	on_frame: ffi::moq_status_callback,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let stream = ffi::parse_id(stream)?;
		let callback = unsafe { OnStatus::new(user_data, on_frame)? };
		let mut state = State::lock();
		let entry = state.socket.streams.get_mut(stream).ok_or(Error::NotFound)?;
		let driver = entry.driver.clone();
		let half = entry.reader.take().ok_or(Error::Moq(moq_net::Error::Closed))?;
		state.consume.socket_read(half, driver, callback)
	})
}

/// Release a stream handle.
///
/// A half not yet taken is dropped: an unfinished `tcp` writer resets the session, and an
/// unread reader stops reading. Taken halves are unaffected.
#[unsafe(no_mangle)]
pub extern "C" fn moq_socket_stream_free(stream: u32) -> i32 {
	ffi::enter(move || {
		let stream = ffi::parse_id(stream)?;
		let entry = State::lock().socket.streams.remove(stream).ok_or(Error::NotFound)?;
		drop(entry);
		Ok(())
	})
}
