use crate::{Error, State, ffi};

use std::ffi::c_char;
use std::ffi::c_void;
use std::str::FromStr;

use tracing::Level;

/// Information about a video rendition in the catalog.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_video_config {
	/// The name of the track, NOT NULL terminated.
	pub name: *const c_char,
	pub name_len: usize,

	/// The codec of the track, NOT NULL terminated
	pub codec: *const c_char,
	pub codec_len: usize,

	/// The description of the track, or NULL if not used.
	/// This is codec specific, for example H264:
	///   - NULL: annex.b encoded
	///   - Non-NULL: AVCC encoded
	pub description: *const u8,
	pub description_len: usize,

	/// The encoded width/height of the media, or NULL if not available
	pub coded_width: *const u32,
	pub coded_height: *const u32,
}

/// Information about an audio rendition in the catalog.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_audio_config {
	/// The name of the track, NOT NULL terminated
	pub name: *const c_char,
	pub name_len: usize,

	/// The codec of the track, NOT NULL terminated
	pub codec: *const c_char,
	pub codec_len: usize,

	/// The description of the track, or NULL if not used.
	pub description: *const u8,
	pub description_len: usize,

	/// The sample rate of the track in Hz
	pub sample_rate: u32,

	/// The number of channels in the track
	pub channel_count: u32,
}

/// Options for a JSON snapshot track (lossy latest-value mode).
///
/// The same config is passed to a producer and its consumers, but the consumer reads only
/// `compression`; `delta_ratio` is producer-only.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_json_config {
	/// How aggressively the producer emits deltas instead of full snapshots. `0` disables deltas
	/// (one snapshot per group); a positive value allows roughly that many snapshots' worth of
	/// deltas before rolling. Ignored by the consumer.
	pub delta_ratio: u32,

	/// DEFLATE-compress each group. Must match on the producer and consumer.
	pub compression: bool,
}

/// Options for a JSON stream track (lossless append-log mode).
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_json_stream_config {
	/// DEFLATE-compress the group. Must match on the producer and consumer.
	pub compression: bool,
}

/// A JSON value delivered by a consumer callback.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_json_value {
	/// The JSON document as UTF-8, NOT NULL terminated.
	pub json: *const c_char,
	pub json_len: usize,
}

/// Information about a frame of media.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_frame {
	/// The payload of the frame, or NULL/0 if the stream has ended
	pub payload: *const u8,
	pub payload_size: usize,

	/// The presentation timestamp of the frame in microseconds
	pub timestamp_us: u64,

	/// Whether the frame is a keyframe, aka the start of a new group.
	pub keyframe: bool,
}

/// A best-effort raw track datagram delivered via [moq_consume_datagrams].
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_datagram {
	/// The payload of the datagram, or NULL/0 if the track has ended.
	pub payload: *const u8,
	pub payload_size: usize,

	/// The presentation timestamp of the datagram in microseconds.
	pub timestamp_us: u64,

	/// Per-track sequence number, drawn from the same namespace as groups.
	pub sequence: u64,
}

/// Publisher-side raw track properties.
///
/// A null [moq_publish_track] `info` pointer uses the moq-net defaults.
/// A zero-initialized struct also uses those defaults, except `priority` where
/// zero is the default itself.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_track_info {
	/// Priority, used to break ties between subscriptions of equal subscriber priority.
	pub priority: u8,

	/// Whether groups are prioritized in sequence order.
	/// Groups may always arrive out-of-order (or not at all) over the network.
	pub ordered: bool,

	/// How long the relay should cache past groups, in milliseconds.
	pub cache_ms: u64,
	/// Whether `cache_ms` should override the default cache setting.
	pub cache_valid: bool,

	/// Per-frame timescale in ticks per second.
	pub timescale: u64,
	/// Whether `timescale` should override the default millisecond timescale.
	pub timescale_valid: bool,
}

impl TryFrom<&moq_track_info> for moq_net::track::Info {
	type Error = Error;

	fn try_from(info: &moq_track_info) -> Result<Self, Self::Error> {
		// Raw tracks default to a microsecond timescale, matching the C ABI's
		// timestamp_us units. An explicit timescale below overrides it.
		let mut out = moq_net::track::Info::default()
			.with_timescale(moq_net::Timescale::MICRO)
			.with_priority(info.priority)
			.with_ordered(info.ordered);
		if info.cache_valid {
			out = out.with_cache(std::time::Duration::from_millis(info.cache_ms));
		}
		if info.timescale_valid {
			out = out.with_timescale(moq_net::Timescale::new(info.timescale)?);
		}
		Ok(out)
	}
}

/// Subscriber-side raw track delivery preferences.
///
/// A null [moq_consume_track] or [moq_consume_track_update] `subscription`
/// pointer uses the moq-net defaults.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_subscription {
	/// Delivery priority. Higher values preempt lower ones under contention.
	pub priority: u8,

	/// Whether groups are prioritized in sequence order.
	/// Groups may always arrive out-of-order (or not at all) over the network.
	pub ordered: bool,

	/// How long to wait for an older group once a newer group has arrived, in milliseconds.
	pub stale_ms: u64,

	/// First group to deliver.
	pub group_start: u64,
	/// Whether `group_start` is present. When false, delivery starts at the latest group.
	pub group_start_valid: bool,

	/// Last group to deliver, inclusive.
	pub group_end: u64,
	/// Whether `group_end` is present. When false, there is no end cap.
	pub group_end_valid: bool,
}

impl From<&moq_subscription> for moq_net::track::Subscription {
	fn from(subscription: &moq_subscription) -> Self {
		let mut out = moq_net::track::Subscription::default()
			.with_priority(subscription.priority)
			.with_ordered(subscription.ordered)
			.with_stale(std::time::Duration::from_millis(subscription.stale_ms));
		if subscription.group_start_valid {
			out = out.with_group_start(subscription.group_start);
		}
		if subscription.group_end_valid {
			out = out.with_group_end(subscription.group_end);
		}
		out
	}
}

/// A borrowed UTF-8 string slice, NOT NULL terminated.
///
/// Used to hand a C caller a JSON document that lives inside libmoq's storage.
/// The pointer borrows that storage and is only valid until the owning resource
/// is freed (see the function that fills it for the exact lifetime).
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_string {
	/// Pointer to `len` bytes of UTF-8, NOT NULL terminated.
	pub data: *const c_char,
	pub len: usize,
}

/// One untyped application catalog section: a name and its JSON value.
///
/// Both `name` and `json` are UTF-8, NOT NULL terminated, and borrow the catalog
/// snapshot's storage. They stay valid until the snapshot is freed with
/// [moq_consume_catalog_free]. `json` is the section's value serialized as JSON
/// (parse it yourself); a top-level catalog key beyond `video`/`audio`.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_section {
	/// The section name, NOT NULL terminated.
	pub name: *const c_char,
	pub name_len: usize,

	/// The section value as a JSON document, NOT NULL terminated.
	pub json: *const c_char,
	pub json_len: usize,
}

/// Information about a broadcast announced by an origin.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_announced {
	/// The path of the broadcast, NOT NULL terminated
	pub path: *const c_char,
	pub path_len: usize,

	/// Whether the broadcast is active or has ended
	/// This MUST toggle between true and false over the lifetime of the broadcast
	pub active: bool,
}

/// A snapshot of connection statistics, filled in by [moq_session_stats].
///
/// Each metric has a `*_valid` flag: when `false`, the matching value is meaningless because
/// the transport backend doesn't report it (a `false` flag is NOT the same as a zero value).
/// Native QUIC reports every metric; the browser WebTransport reports few or none. Initialize
/// the struct to zero before the call; [moq_session_stats] overwrites every field.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_connection_stats {
	/// Smoothed round-trip time, in microseconds.
	pub rtt_us: u64,
	pub rtt_valid: bool,

	/// Estimated send bandwidth from the congestion controller, in bits per second.
	pub send_rate_bps: u64,
	pub send_rate_valid: bool,

	/// Estimated receive bandwidth from MoQ PROBE, in bits per second.
	pub recv_rate_bps: u64,
	pub recv_rate_valid: bool,

	/// Total bytes sent, including retransmissions and overhead.
	pub bytes_sent: u64,
	pub bytes_sent_valid: bool,

	/// Total bytes received, including duplicates and overhead.
	pub bytes_received: u64,
	pub bytes_received_valid: bool,

	/// Total bytes lost (detected via retransmission or acknowledgement).
	pub bytes_lost: u64,
	pub bytes_lost_valid: bool,

	/// Total datagrams sent.
	pub packets_sent: u64,
	pub packets_sent_valid: bool,

	/// Total datagrams received.
	pub packets_received: u64,
	pub packets_received_valid: bool,

	/// Total datagrams detected as lost.
	pub packets_lost: u64,
	pub packets_lost_valid: bool,
}

impl From<&moq_net::ConnectionStats> for moq_connection_stats {
	fn from(stats: &moq_net::ConnectionStats) -> Self {
		// An Option<u64> becomes a (value, valid) pair; absent metrics report 0/false.
		fn split(value: Option<u64>) -> (u64, bool) {
			(value.unwrap_or(0), value.is_some())
		}

		let (rtt_us, rtt_valid) = split(stats.rtt.map(|d| d.as_micros() as u64));
		let (send_rate_bps, send_rate_valid) = split(stats.estimated_send_rate);
		let (recv_rate_bps, recv_rate_valid) = split(stats.estimated_recv_rate);
		let (bytes_sent, bytes_sent_valid) = split(stats.bytes_sent);
		let (bytes_received, bytes_received_valid) = split(stats.bytes_received);
		let (bytes_lost, bytes_lost_valid) = split(stats.bytes_lost);
		let (packets_sent, packets_sent_valid) = split(stats.packets_sent);
		let (packets_received, packets_received_valid) = split(stats.packets_received);
		let (packets_lost, packets_lost_valid) = split(stats.packets_lost);

		Self {
			rtt_us,
			rtt_valid,
			send_rate_bps,
			send_rate_valid,
			recv_rate_bps,
			recv_rate_valid,
			bytes_sent,
			bytes_sent_valid,
			bytes_received,
			bytes_received_valid,
			bytes_lost,
			bytes_lost_valid,
			packets_sent,
			packets_sent_valid,
			packets_received,
			packets_received_valid,
			packets_lost,
			packets_lost_valid,
		}
	}
}

/// Initialize the library with a log level.
///
/// This should be called before any other functions.
/// The log_level is a string: "error", "warn", "info", "debug", "trace"
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that level is a valid pointer to level_len bytes of data.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_log_level(level: *const c_char, level_len: usize) -> i32 {
	ffi::enter(move || {
		match unsafe { ffi::parse_str(level, level_len)? } {
			"" => moq_native::Log::default(),
			level => moq_native::Log::new(Level::from_str(level)?),
		}
		.init()?;

		Ok(())
	})
}

/// Human-readable reason for the most recent failed call on the calling thread.
///
/// libmoq functions return only a negative code; this exposes the matching message
/// (including detail the code can't carry, e.g. which URL failed to parse or why a
/// decode failed). The string is only meaningful after a call returned a negative
/// code; check the code first.
///
/// Returns a NUL-terminated, UTF-8 pointer valid until the next libmoq call **on the
/// same thread**, or NULL if no error has been recorded on this thread. Copy it if you
/// need it to outlive the next call. Errors delivered through status callbacks carry
/// their code directly; read this from inside the callback to get their reason.
#[unsafe(no_mangle)]
pub extern "C" fn moq_error() -> *const c_char {
	ffi::last_error_ptr()
}

/// Start establishing a connection to a MoQ server.
///
/// Takes origin handles, which are used for publishing and consuming broadcasts respectively.
/// - Any broadcasts in `origin_publish` will be announced to the server.
/// - Any broadcasts announced by the server will be available in `origin_consume`.
/// - If an origin handle is 0, that functionality is completely disabled.
///
/// This may be called multiple times to connect to different servers.
/// Origins can be shared across sessions, useful for fanout or relaying.
///
/// Returns a non-zero handle to the session on success, or a negative code on (immediate) failure.
/// You should call [moq_session_close], even on error, to free up resources.
///
/// The session reconnects automatically with exponential backoff if the connection drops.
/// Published broadcasts are re-announced and consumers re-subscribed on each reconnect,
/// since the origins outlive the underlying connection.
///
/// `on_status` reports the session lifecycle through its status code:
/// - `> 0` on every (re)connect, carrying the connection epoch (`1` = first connect,
///   `2` = first reconnect, and so on), so a reconnect is distinguishable from the
///   initial connect. May fire repeatedly. Transient disconnects are not reported.
/// - `0` when the session is closed cleanly via [moq_session_close] (terminal).
/// - a negative error code if reconnection permanently gives up, e.g. the backoff
///   timeout is exceeded (terminal).
///
/// After a terminal (`<= 0`) status, `on_status` is never called again and `user_data`
/// is never touched again, so that final callback is the point to release `user_data`.
/// The terminal `0` fires even after [moq_session_close], so do not free `user_data` on
/// the close call itself.
///
/// # Safety
/// - The caller must ensure that url is a valid pointer to url_len bytes of data.
/// - The caller must keep `user_data` valid until the terminal (`<= 0`) `on_status` callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_session_connect(
	url: *const c_char,
	url_len: usize,
	origin_publish: u32,
	origin_consume: u32,
	on_status: Option<extern "C" fn(user_data: *mut c_void, code: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let url = ffi::parse_url(url, url_len)?;

		let mut state = State::lock();
		let publish = ffi::parse_id_optional(origin_publish)?
			.map(|id| state.origin.get(id))
			.transpose()?
			.cloned();
		let consume = ffi::parse_id_optional(origin_consume)?
			.map(|id| state.origin.get(id))
			.transpose()?
			.cloned();

		let on_status = unsafe { ffi::OnStatus::new(user_data, on_status) };
		state.session.connect(url, publish, consume, on_status)
	})
}

/// Request that a session shut down.
///
/// Returns immediately: zero on success, or a negative code if the session is
/// unknown or already closing. Does NOT free `user_data`. The
/// [moq_session_connect] `on_status` callback still fires once more with a
/// terminal `0` (or a negative error), and that final callback is where
/// `user_data` should be released. Safe to call from any thread, including from
/// within `on_status`.
#[unsafe(no_mangle)]
pub extern "C" fn moq_session_close(session: u32) -> i32 {
	ffi::enter(move || {
		let session = ffi::parse_id(session)?;
		State::lock().session.close(session)
	})
}

/// Snapshot the current connection statistics for a session.
///
/// Fills `dst` with a point-in-time view of the underlying QUIC/WebTransport connection
/// (RTT, bandwidth estimates, byte/packet counters). Each metric carries a `*_valid` flag
/// since availability depends on the transport backend; see [moq_connection_stats].
///
/// Returns zero on success, or a negative code on failure: the session handle is unknown, or
/// the session is currently reconnecting and has no live connection (in which case `dst` is
/// left untouched). Safe to call repeatedly to poll stats over the life of the session.
///
/// # Safety
/// - The caller must ensure that `dst` is a valid pointer to a [moq_connection_stats] struct.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_session_stats(session: u32, dst: *mut moq_connection_stats) -> i32 {
	ffi::enter(move || {
		let session = ffi::parse_id(session)?;
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		let stats = State::lock().session.stats(session)?;
		*dst = moq_connection_stats::from(&stats);
		Ok(())
	})
}

/// Create an origin for publishing broadcasts.
///
/// Origins contain any number of broadcasts addressed by path.
/// The same broadcast can be published to multiple origins under different paths.
///
/// [moq_origin_announced] can be used to discover broadcasts published to this origin.
/// This is extremely useful for discovering what is available on the server to [moq_origin_request].
///
/// Returns a non-zero handle to the origin on success.
#[unsafe(no_mangle)]
pub extern "C" fn moq_origin_create() -> i32 {
	ffi::enter(move || State::lock().origin.create())
}

/// Publish a broadcast to an origin.
///
/// The broadcast will be announced to any origin consumers, such as over the network.
///
/// Returns a positive publish handle on success, or a negative code on failure. The broadcast
/// stays announced until the handle is passed to [moq_origin_unpublish]; closing the broadcast
/// itself does not unannounce it.
///
/// # Safety
/// - The caller must ensure that path is a valid pointer to path_len bytes of data.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_origin_publish(origin: u32, path: *const c_char, path_len: usize, broadcast: u32) -> i32 {
	ffi::enter(move || {
		let origin = ffi::parse_id(origin)?;
		let path = unsafe { ffi::parse_str(path, path_len)? };
		let broadcast = ffi::parse_id(broadcast)?;

		let mut state = State::lock();
		let broadcast = state.publish.get(broadcast)?.consume();
		state.origin.publish(origin, path, broadcast)
	})
}

/// Unannounce a broadcast previously published with [moq_origin_publish].
///
/// Takes the publish handle returned by [moq_origin_publish]. Returns zero on success, or a
/// negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_origin_unpublish(publish: u32) -> i32 {
	ffi::enter(move || {
		let publish = ffi::parse_id(publish)?;
		State::lock().origin.unpublish(publish)
	})
}

/// Learn about all broadcasts published to an origin.
///
/// `on_announce` is invoked with a positive announced ID for each broadcast,
/// then exactly once more with a terminal code: `0` (stopped cleanly) or a
/// negative error. After the terminal (`<= 0`) callback, `on_announce` is never
/// called again and `user_data` is never touched again, so release `user_data`
/// there. The terminal callback fires even after [moq_origin_announced_close].
///
/// - [moq_origin_announced_info] is used to query information about the broadcast.
/// - [moq_origin_announced_free] releases each delivered announced ID once read.
/// - [moq_origin_announced_close] is used to stop receiving announcements.
///
/// Returns a non-zero handle on success, or a negative code on failure.
///
/// # Safety
/// - The caller must keep `user_data` valid until the terminal (`<= 0`) `on_announce` callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_origin_announced(
	origin: u32,
	on_announce: Option<extern "C" fn(user_data: *mut c_void, announced: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let origin = ffi::parse_id(origin)?;
		let on_announce = unsafe { ffi::OnStatus::new(user_data, on_announce) };
		State::lock().origin.announced(origin, on_announce)
	})
}

/// Query information about a broadcast discovered by [moq_origin_announced].
///
/// The destination is filled with the broadcast information. The `path` pointer borrows
/// the announcement's storage: copy it out before calling [moq_origin_announced_free], which
/// invalidates it.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `dst` is a valid pointer to a [moq_announced] struct.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_origin_announced_info(announced: u32, dst: *mut moq_announced) -> i32 {
	ffi::enter(move || {
		let announced = ffi::parse_id(announced)?;
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		State::lock().origin.announced_info(announced, dst)
	})
}

/// Free a single announcement delivered to a [moq_origin_announced] `on_announce` callback.
///
/// Each announce / unannounce event hands the callback a distinct announcement handle (read
/// with [moq_origin_announced_info]); release it here once done to avoid leaking one per event
/// over the life of the listener. This is per-announcement and distinct from
/// [moq_origin_announced_close], which stops the listener itself. After freeing, any `path`
/// pointer obtained from [moq_origin_announced_info] for this handle is dangling.
///
/// Returns zero on success, or a negative code if the handle is unknown.
#[unsafe(no_mangle)]
pub extern "C" fn moq_origin_announced_free(announced: u32) -> i32 {
	ffi::enter(move || {
		let announced = ffi::parse_id(announced)?;
		State::lock().origin.announced_free(announced)
	})
}

/// Stop receiving announcements for broadcasts published to an origin.
///
/// Returns immediately: zero on success, or a negative code if already closed.
/// Does NOT free `user_data`. The [moq_origin_announced] `on_announce` callback
/// still fires once more with a terminal `0` (or a negative error), and that
/// final callback is where `user_data` should be released.
#[unsafe(no_mangle)]
pub extern "C" fn moq_origin_announced_close(announced: u32) -> i32 {
	ffi::enter(move || {
		let announced = ffi::parse_id(announced)?;
		State::lock().origin.announced_close(announced)
	})
}

/// Consume a broadcast from an origin by path, waiting until it is announced.
///
/// Resolves against future announcements: it waits for the announcement to arrive (e.g. over the
/// network) and then delivers the broadcast handle via `on_broadcast`. Use it right after
/// [moq_session_connect] to avoid racing announcement gossip. To resolve against only what is
/// announced now (plus any dynamic fallback), use [moq_origin_request] instead.
///
/// `on_broadcast` is invoked with a positive broadcast handle once announced, then exactly once
/// more with a terminal code: `0` (the wait finished, including after
/// [moq_origin_consume_announced_close]) or a negative error. After the terminal (`<= 0`) callback,
/// `on_broadcast` is never called again and `user_data` is never touched again, so release
/// `user_data` there. The broadcast handle is usable with [moq_consume_catalog] / [moq_consume_track]
/// and must be freed separately with [moq_consume_close].
///
/// Returns a non-zero handle to the wait on success, or a negative code on (immediate) failure.
///
/// # Safety
/// - The caller must ensure that path is a valid pointer to path_len bytes of data.
/// - The caller must keep `user_data` valid until the terminal (`<= 0`) `on_broadcast` callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_origin_consume_announced(
	origin: u32,
	path: *const c_char,
	path_len: usize,
	on_broadcast: Option<extern "C" fn(user_data: *mut c_void, broadcast: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let origin = ffi::parse_id(origin)?;
		let path = unsafe { ffi::parse_str(path, path_len)? }.to_string();
		let on_broadcast = unsafe { ffi::OnStatus::new(user_data, on_broadcast) };
		State::lock().origin.consume_announced(origin, path, on_broadcast)
	})
}

/// Abort a wait started by [moq_origin_consume_announced].
///
/// Returns immediately: zero on success, or a negative code if already closed. Does NOT free
/// `user_data`. The [moq_origin_consume_announced] `on_broadcast` callback still fires once more
/// with a terminal `0` (or a negative error), and that final callback is where `user_data` should
/// be released. Any broadcast handle already delivered is unaffected and must still be freed with
/// [moq_consume_close].
#[unsafe(no_mangle)]
pub extern "C" fn moq_origin_consume_announced_close(task: u32) -> i32 {
	ffi::enter(move || {
		let task = ffi::parse_id(task)?;
		State::lock().origin.consume_announced_close(task)
	})
}

/// Request a broadcast from an origin by path, resolving as soon as it can be served.
///
/// Resolves against what is announced *now* plus any dynamic fallback, where
/// [moq_origin_consume_announced] waits indefinitely for a future announcement: it returns an
/// already-announced broadcast at once, otherwise falls back to a dynamic handler on the origin
/// (if any), and fails when neither can serve the path. It does NOT wait for a later
/// announcement.
///
/// `on_broadcast` is invoked with a positive broadcast handle once served, then exactly once more
/// with a terminal code: `0` (finished, including after [moq_origin_request_close]) or a negative
/// error. After the terminal (`<= 0`) callback, `user_data` is never touched again, so release it
/// there. The broadcast handle is usable with [moq_consume_catalog] / [moq_consume_track] and must
/// be freed separately with [moq_consume_close].
///
/// Returns a non-zero handle to the request on success, or a negative code on (immediate) failure.
///
/// # Safety
/// - The caller must ensure that path is a valid pointer to path_len bytes of data.
/// - The caller must keep `user_data` valid until the terminal (`<= 0`) `on_broadcast` callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_origin_request(
	origin: u32,
	path: *const c_char,
	path_len: usize,
	on_broadcast: Option<extern "C" fn(user_data: *mut c_void, broadcast: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let origin = ffi::parse_id(origin)?;
		let path = unsafe { ffi::parse_str(path, path_len)? }.to_string();
		let on_broadcast = unsafe { ffi::OnStatus::new(user_data, on_broadcast) };
		State::lock().origin.request(origin, path, on_broadcast)
	})
}

/// Abort a request started by [moq_origin_request].
///
/// Returns immediately: zero on success, or a negative code if already closed. Does NOT free
/// `user_data`; the [moq_origin_request] `on_broadcast` callback fires once more with a terminal
/// code, which is where `user_data` should be released. Any broadcast handle already delivered is
/// unaffected and must still be freed with [moq_consume_close].
#[unsafe(no_mangle)]
pub extern "C" fn moq_origin_request_close(task: u32) -> i32 {
	ffi::enter(move || {
		let task = ffi::parse_id(task)?;
		State::lock().origin.consume_announced_close(task)
	})
}

/// Close an origin and clean up its resources.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_origin_close(origin: u32) -> i32 {
	ffi::enter(move || {
		let origin = ffi::parse_id(origin)?;
		State::lock().origin.close(origin)
	})
}

/// Create a new broadcast for publishing media tracks.
///
/// Returns a non-zero handle to the broadcast on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_create() -> i32 {
	ffi::enter(move || State::lock().publish.create())
}

/// Close a broadcast and clean up its resources.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_close(broadcast: u32) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		State::lock().publish.close(broadcast)
	})
}

/// Create a new media track for a broadcast
///
/// All frames in [moq_publish_media_frame] must be written in decode order.
/// The `format` controls the encoding, both of `init` and frame payloads.
///
/// Returns a non-zero handle to the track on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that format is a valid pointer to format_len bytes of data.
/// - The caller must ensure that init is a valid pointer to init_size bytes of data.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_media_ordered(
	broadcast: u32,
	format: *const c_char,
	format_len: usize,
	init: *const u8,
	init_size: usize,
) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let format = unsafe { ffi::parse_str(format, format_len)? };
		let init = unsafe { ffi::parse_slice(init, init_size)? };

		State::lock().publish.media_ordered(broadcast, format, init)
	})
}

/// Remove a track from a broadcast.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_media_close(export: u32) -> i32 {
	ffi::enter(move || {
		let export = ffi::parse_id(export)?;
		State::lock().publish.media_close(export)
	})
}

/// Write data to a track.
///
/// The encoding of `data` depends on the track `format`.
/// The timestamp is in microseconds.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that payload is a valid pointer to payload_size bytes of data.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_media_frame(
	media: u32,
	payload: *const u8,
	payload_size: usize,
	timestamp_us: u64,
) -> i32 {
	ffi::enter(move || {
		let media = ffi::parse_id(media)?;
		let payload = unsafe { ffi::parse_slice(payload, payload_size)? };
		let timestamp = hang::container::Timestamp::from_micros(timestamp_us)?;
		State::lock().publish.media_frame(media, payload, timestamp)
	})
}

/// Add or replace a video rendition in a broadcast's catalog.
///
/// This is the producer counterpart to [moq_consume_video_config]: instead of
/// reading a rendition out of a catalog, it writes one into the catalog of a
/// broadcast created with [moq_publish_create]. The rendition is keyed by
/// `config.name`; calling this again with the same name replaces it. The
/// updated catalog is published to subscribers automatically.
///
/// The struct fields are read as inputs:
/// - `name` / `codec` are required (NOT NULL terminated) string slices.
/// - `description` may be NULL to omit it.
/// - `coded_width` / `coded_height` may be NULL to omit them.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `config` points to a valid [moq_video_config].
/// - The caller must ensure each non-NULL pointer inside `config` is valid for its length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_video_config(broadcast: u32, config: *const moq_video_config) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let config = unsafe { config.as_ref() }.ok_or(Error::InvalidPointer)?;

		let name = unsafe { ffi::parse_str(config.name, config.name_len)? };
		let codec = unsafe { ffi::parse_str(config.codec, config.codec_len)? };
		let codec = hang::catalog::VideoCodec::from_str(codec).map_err(Error::Hang)?;

		let mut video = hang::catalog::VideoConfig::new(codec);
		if !config.description.is_null() {
			let description = unsafe { ffi::parse_slice(config.description, config.description_len)? };
			video.description = Some(bytes::Bytes::copy_from_slice(description));
		}
		video.coded_width = unsafe { config.coded_width.as_ref() }.copied();
		video.coded_height = unsafe { config.coded_height.as_ref() }.copied();

		State::lock().publish.video_config(broadcast, name, video)
	})
}

/// Add or replace an audio rendition in a broadcast's catalog.
///
/// This is the producer counterpart to [moq_consume_audio_config]. The rendition
/// is keyed by `config.name`; calling this again with the same name replaces it.
/// The updated catalog is published to subscribers automatically.
///
/// The struct fields are read as inputs:
/// - `name` / `codec` are required (NOT NULL terminated) string slices.
/// - `sample_rate` / `channel_count` are required.
/// - `description` may be NULL to omit it.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `config` points to a valid [moq_audio_config].
/// - The caller must ensure each non-NULL pointer inside `config` is valid for its length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_audio_config(broadcast: u32, config: *const moq_audio_config) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let config = unsafe { config.as_ref() }.ok_or(Error::InvalidPointer)?;

		let name = unsafe { ffi::parse_str(config.name, config.name_len)? };
		let codec = unsafe { ffi::parse_str(config.codec, config.codec_len)? };
		let codec = hang::catalog::AudioCodec::from_str(codec).map_err(Error::Hang)?;

		let mut audio = hang::catalog::AudioConfig::new(codec, config.sample_rate, config.channel_count);
		if !config.description.is_null() {
			let description = unsafe { ffi::parse_slice(config.description, config.description_len)? };
			audio.description = Some(bytes::Bytes::copy_from_slice(description));
		}

		State::lock().publish.audio_config(broadcast, name, audio)
	})
}

/// Remove a video rendition from a broadcast's catalog by name.
///
/// This is a no-op if no rendition with that name exists. The updated catalog is
/// published to subscribers automatically.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that name is a valid pointer to name_len bytes of data.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_video_remove(broadcast: u32, name: *const c_char, name_len: usize) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let name = unsafe { ffi::parse_str(name, name_len)? };
		State::lock().publish.video_remove(broadcast, name)
	})
}

/// Remove an audio rendition from a broadcast's catalog by name.
///
/// This is a no-op if no rendition with that name exists. The updated catalog is
/// published to subscribers automatically.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that name is a valid pointer to name_len bytes of data.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_audio_remove(broadcast: u32, name: *const c_char, name_len: usize) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let name = unsafe { ffi::parse_str(name, name_len)? };
		State::lock().publish.audio_remove(broadcast, name)
	})
}

/// Set (or replace) a top-level application catalog section by name.
///
/// This is the producer counterpart to [moq_catalog_get_section] /
/// [moq_catalog_section_at]: it writes an arbitrary top-level JSON key into the
/// catalog of a broadcast created with [moq_publish_create], beyond the
/// `video`/`audio` keys owned by the media pipeline. Calling it again with the
/// same name replaces the section. The updated catalog is published to
/// subscribers automatically.
///
/// `json` is a JSON document (object, array, string, ...) as `json_len` bytes of
/// UTF-8. Returns a zero on success, or a negative code on failure: invalid JSON
/// yields a Json error (-37); a reserved `name` (`video`/`audio`) yields a mux error.
///
/// # Safety
/// - The caller must ensure that name is a valid pointer to name_len bytes of data.
/// - The caller must ensure that json is a valid pointer to json_len bytes of data.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_catalog_section(
	broadcast: u32,
	name: *const c_char,
	name_len: usize,
	json: *const c_char,
	json_len: usize,
) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let name = unsafe { ffi::parse_str(name, name_len)? };
		let json = unsafe { ffi::parse_str(json, json_len)? };
		let value: serde_json::Value = serde_json::from_str(json)?;
		State::lock().publish.catalog_section_set(broadcast, name, value)
	})
}

/// Remove a top-level application catalog section by name.
///
/// This is a no-op if no section with that name exists. The updated catalog is
/// published to subscribers automatically.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that name is a valid pointer to name_len bytes of data.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_remove_catalog_section(broadcast: u32, name: *const c_char, name_len: usize) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let name = unsafe { ffi::parse_str(name, name_len)? };
		State::lock().publish.catalog_section_remove(broadcast, name)
	})
}

/// Create a raw track on a broadcast for arbitrary byte payloads.
///
/// Unlike [moq_publish_media_ordered], this is the bare moq-net primitive: no
/// codec, container, or catalog framing. Frames written to it are delivered
/// as-is to subscribers using [moq_consume_track]. Use it for non-media tracks
/// (control channels, JSON metadata, etc.), or pair it with
/// [moq_publish_video_config] / [moq_publish_audio_config] to also describe the
/// track in the catalog. Pass NULL for `info` to use moq-net defaults.
///
/// Returns a non-zero handle to the track on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that name is a valid pointer to name_len bytes of data.
/// - The caller must ensure that info is either NULL or a valid pointer to a [moq_track_info] struct.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_track(
	broadcast: u32,
	name: *const c_char,
	name_len: usize,
	info: *const moq_track_info,
) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let name = unsafe { ffi::parse_str(name, name_len)? };
		// Default raw tracks to a microsecond timescale even when no info is given.
		let info = match unsafe { info.as_ref() } {
			Some(info) => moq_net::track::Info::try_from(info)?,
			None => moq_net::track::Info::default().with_timescale(moq_net::Timescale::MICRO),
		};
		State::lock().publish.track(broadcast, name, Some(info))
	})
}

/// Append a new group to a raw track, returning a group producer.
///
/// Groups are delivered independently and each may contain any number of frames
/// written via [moq_publish_group_frame]. Sequence numbers auto-increment.
///
/// Returns a non-zero handle to the group on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_track_group(track: u32) -> i32 {
	ffi::enter(move || {
		let track = ffi::parse_id(track)?;
		State::lock().publish.track_group(track)
	})
}

/// Write a single-frame group to a raw track with a timestamp.
///
/// Convenience for the common one-frame-per-group pattern. Equivalent to
/// appending a group, writing one frame, and finishing it.
/// The timestamp is in microseconds.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that payload is a valid pointer to payload_size bytes of data.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_track_frame(
	track: u32,
	payload: *const u8,
	payload_size: usize,
	timestamp_us: u64,
) -> i32 {
	ffi::enter(move || {
		let track = ffi::parse_id(track)?;
		let payload = unsafe { ffi::parse_slice(payload, payload_size)? };
		let timestamp = moq_net::Timestamp::from_micros(timestamp_us)?;
		State::lock().publish.track_frame(track, timestamp, payload)
	})
}

/// Send a best-effort datagram on a raw track created by [moq_publish_track].
///
/// `timestamp_us` is the presentation timestamp in microseconds. The payload must be at
/// most 1200 bytes. On success the datagram's per-track sequence number (shared with the
/// group namespace) is written to `out_sequence` when it is non-NULL. Datagrams are
/// delivered only on transports and wire versions with a datagram channel; there is no
/// group fallback.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that payload is a valid pointer to payload_size bytes of data.
/// - `out_sequence` must be NULL or a valid pointer to a `uint64_t`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_track_datagram(
	track: u32,
	timestamp_us: u64,
	payload: *const u8,
	payload_size: usize,
	out_sequence: *mut u64,
) -> i32 {
	ffi::enter(move || {
		let track = ffi::parse_id(track)?;
		let payload = unsafe { ffi::parse_slice(payload, payload_size)? };
		let sequence = State::lock().publish.track_datagram(track, timestamp_us, payload)?;
		if let Some(out) = unsafe { out_sequence.as_mut() } {
			*out = sequence;
		}
		Ok(())
	})
}

/// Finish a raw track. No more groups or frames can be written.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_track_close(track: u32) -> i32 {
	ffi::enter(move || {
		let track = ffi::parse_id(track)?;
		State::lock().publish.track_finish(track)
	})
}

/// Write a frame into a raw group created by [moq_publish_track_group].
///
/// The timestamp is in microseconds.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that payload is a valid pointer to payload_size bytes of data.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_group_frame(
	group: u32,
	payload: *const u8,
	payload_size: usize,
	timestamp_us: u64,
) -> i32 {
	ffi::enter(move || {
		let group = ffi::parse_id(group)?;
		let payload = unsafe { ffi::parse_slice(payload, payload_size)? };
		let timestamp = moq_net::Timestamp::from_micros(timestamp_us)?;
		State::lock().publish.group_frame(group, timestamp, payload)
	})
}

/// Finish a raw group. No more frames can be written.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_group_close(group: u32) -> i32 {
	ffi::enter(move || {
		let group = ffi::parse_id(group)?;
		State::lock().publish.group_finish(group)
	})
}

/// Create a JSON snapshot track (lossy latest-value) on a broadcast.
///
/// Values published via [moq_publish_json_update] reach subscribers as a single latest state; a
/// late joiner only sees the newest. Advertise the track in the catalog with
/// [moq_publish_catalog_section] if consumers should discover it.
///
/// Returns a non-zero handle to the JSON producer on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure `name` is a valid pointer to `name_len` bytes and `config` a valid pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_json(
	broadcast: u32,
	name: *const c_char,
	name_len: usize,
	config: *const moq_json_config,
) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let name = unsafe { ffi::parse_str(name, name_len)? };
		let config = unsafe { config.as_ref() }.ok_or(Error::InvalidPointer)?;
		let mut producer = moq_json::snapshot::ProducerConfig::default();
		producer.delta_ratio = config.delta_ratio;
		producer.compression = config.compression;
		State::lock().publish.json(broadcast, name, producer)
	})
}

/// Publish a new value to a JSON snapshot track. `value` is a UTF-8 JSON document. A no-op if
/// unchanged from the previous update.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure `value` is a valid pointer to `value_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_json_update(json: u32, value: *const c_char, value_len: usize) -> i32 {
	ffi::enter(move || {
		let json = ffi::parse_id(json)?;
		let value = unsafe { ffi::parse_slice(value.cast::<u8>(), value_len)? };
		let value = serde_json::from_slice(value)?;
		State::lock().publish.json_update(json, value)
	})
}

/// Finish a JSON snapshot track. No more values can be published.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_json_close(json: u32) -> i32 {
	ffi::enter(move || {
		let json = ffi::parse_id(json)?;
		State::lock().publish.json_close(json)
	})
}

/// Create a JSON stream track (lossless append-log) on a broadcast.
///
/// Every record appended via [moq_publish_json_stream_append] is preserved and delivered in order.
///
/// Returns a non-zero handle to the JSON stream producer on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure `name` is a valid pointer to `name_len` bytes and `config` a valid pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_json_stream(
	broadcast: u32,
	name: *const c_char,
	name_len: usize,
	config: *const moq_json_stream_config,
) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let name = unsafe { ffi::parse_str(name, name_len)? };
		let config = unsafe { config.as_ref() }.ok_or(Error::InvalidPointer)?;
		let producer = moq_json::stream::ProducerConfig::default().with_compression(config.compression);
		State::lock().publish.json_stream(broadcast, name, producer)
	})
}

/// Append one record to a JSON stream track. `value` is a UTF-8 JSON document.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure `value` is a valid pointer to `value_len` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_json_stream_append(stream: u32, value: *const c_char, value_len: usize) -> i32 {
	ffi::enter(move || {
		let stream = ffi::parse_id(stream)?;
		let value = unsafe { ffi::parse_slice(value.cast::<u8>(), value_len)? };
		let value = serde_json::from_slice(value)?;
		State::lock().publish.json_stream_append(stream, value)
	})
}

/// Finish a JSON stream track. No more records can be appended.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_json_stream_close(stream: u32) -> i32 {
	ffi::enter(move || {
		let stream = ffi::parse_id(stream)?;
		State::lock().publish.json_stream_close(stream)
	})
}

/// Create a catalog consumer for a broadcast.
///
/// `on_catalog` is invoked with a positive catalog ID for each catalog update
/// (usable to query video/audio track information), then exactly once more with
/// a terminal code: `0` (closed cleanly) or a negative error. After the terminal
/// (`<= 0`) callback, `on_catalog` is never called again and `user_data` is never
/// touched again, so release `user_data` there. The terminal callback fires even
/// after [moq_consume_catalog_close].
///
/// Returns a non-zero handle on success, or a negative code on failure.
///
/// # Safety
/// - The caller must keep `user_data` valid until the terminal (`<= 0`) `on_catalog` callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_catalog(
	broadcast: u32,
	on_catalog: Option<extern "C" fn(user_data: *mut c_void, catalog: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let on_catalog = unsafe { ffi::OnStatus::new(user_data, on_catalog) };
		State::lock().consume.catalog(broadcast, on_catalog)
	})
}

/// Stop a catalog consumer's background subscription.
///
/// Returns immediately: zero on success, or a negative code if already closed.
/// Does NOT free `user_data`; the [moq_consume_catalog] callback still fires once
/// more with a terminal `0` (or a negative error), which is where `user_data`
/// should be released. Catalog snapshots previously delivered via the callback
/// remain valid until freed with [moq_consume_catalog_free].
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_catalog_close(catalog: u32) -> i32 {
	ffi::enter(move || {
		let catalog = ffi::parse_id(catalog)?;
		State::lock().consume.catalog_close(catalog)
	})
}

/// Free a catalog snapshot received via the [moq_consume_catalog] callback.
///
/// This releases the snapshot and invalidates any borrowed references (e.g. pointers
/// returned by [moq_consume_video_config] or [moq_consume_audio_config]).
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_catalog_free(catalog: u32) -> i32 {
	ffi::enter(move || {
		let catalog = ffi::parse_id(catalog)?;
		State::lock().consume.catalog_free(catalog)
	})
}

/// Query information about a video track in a catalog.
///
/// The destination is filled with the video track information.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `dst` is a valid pointer to a [moq_video_config] struct.
/// - The caller must ensure that `dst` is not used after [moq_consume_catalog_free] is called.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_video_config(catalog: u32, index: u32, dst: *mut moq_video_config) -> i32 {
	ffi::enter(move || {
		let catalog = ffi::parse_id(catalog)?;
		let index = index as usize;
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		State::lock().consume.video_config(catalog, index, dst)
	})
}

/// Query information about an audio track in a catalog.
///
/// The destination is filled with the audio track information.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `dst` is a valid pointer to a [moq_audio_config] struct.
/// - The caller must ensure that `dst` is not used after [moq_consume_catalog_free] is called.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_audio_config(catalog: u32, index: u32, dst: *mut moq_audio_config) -> i32 {
	ffi::enter(move || {
		let catalog = ffi::parse_id(catalog)?;
		let index = index as usize;
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		State::lock().consume.audio_config(catalog, index, dst)
	})
}

/// Number of untyped application catalog sections in a catalog snapshot.
///
/// These are the top-level catalog keys beyond `video`/`audio`, carried through
/// verbatim. Iterate them by index with [moq_catalog_section_at], or look one up
/// directly by name with [moq_catalog_get_section].
///
/// Returns the count (>= 0) on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_catalog_section_count(catalog: u32) -> i32 {
	ffi::enter(move || {
		let catalog = ffi::parse_id(catalog)?;
		State::lock().consume.catalog_section_count(catalog)
	})
}

/// Query an application catalog section by index, keyed by name.
///
/// Fills `dst` with the section's name and JSON value at `index`, in the range
/// `[0, moq_catalog_section_count)`. Both pointers borrow the snapshot's storage
/// and stay valid until it is freed with [moq_consume_catalog_free].
///
/// Returns a zero on success, or a negative code on failure (e.g. `index` out of
/// range).
///
/// # Safety
/// - The caller must ensure that `dst` is a valid pointer to a [moq_section] struct.
/// - The caller must ensure that `dst` is not used after [moq_consume_catalog_free] is called.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_catalog_section_at(catalog: u32, index: u32, dst: *mut moq_section) -> i32 {
	ffi::enter(move || {
		let catalog = ffi::parse_id(catalog)?;
		let index = index as usize;
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		State::lock().consume.catalog_section_at(catalog, index, dst)
	})
}

/// Look up an application catalog section by name.
///
/// Fills `dst` with the section's JSON value (the document to parse yourself).
/// The pointer borrows the snapshot's storage and stays valid until it is freed
/// with [moq_consume_catalog_free].
///
/// Returns a zero on success, or a negative code on failure: no section with that
/// name yields a not-found error.
///
/// # Safety
/// - The caller must ensure that name is a valid pointer to name_len bytes of data.
/// - The caller must ensure that `dst` is a valid pointer to a [moq_string] struct.
/// - The caller must ensure that `dst` is not used after [moq_consume_catalog_free] is called.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_catalog_get_section(
	catalog: u32,
	name: *const c_char,
	name_len: usize,
	dst: *mut moq_string,
) -> i32 {
	ffi::enter(move || {
		let catalog = ffi::parse_id(catalog)?;
		let name = unsafe { ffi::parse_str(name, name_len)? };
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		State::lock().consume.catalog_section_get(catalog, name, dst)
	})
}

/// Consume a video track from a broadcast, delivering frames in order.
///
/// - `max_latency_ms` controls the maximum amount of buffering allowed before skipping a GoP.
/// - `on_frame` is called with a positive frame ID per frame, then exactly once
///   more with a terminal code: `0` (closed cleanly) or a negative error. After
///   the terminal (`<= 0`) callback, `on_frame` is never called again and
///   `user_data` is never touched again, so release `user_data` there. The
///   terminal callback fires even after [moq_consume_video_close].
///
/// Returns a non-zero handle to the track on success, or a negative code on failure.
///
/// # Safety
/// - The caller must keep `user_data` valid until the terminal (`<= 0`) `on_frame` callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_video_ordered(
	catalog: u32,
	index: u32,
	max_latency_ms: u64,
	on_frame: Option<extern "C" fn(user_data: *mut c_void, frame: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let catalog = ffi::parse_id(catalog)?;
		let index = index as usize;
		let max_latency = std::time::Duration::from_millis(max_latency_ms);
		let on_frame = unsafe { ffi::OnStatus::new(user_data, on_frame) };
		State::lock()
			.consume
			.video_ordered(catalog, index, max_latency, on_frame)
	})
}

/// Stop a video track consumer's background task.
///
/// Returns immediately: zero on success, or a negative code if already closed.
/// Does NOT free `user_data`; the [moq_consume_video_ordered] `on_frame` callback
/// still fires once more with a terminal `0` (or a negative error), which is
/// where `user_data` should be released.
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_video_close(track: u32) -> i32 {
	ffi::enter(move || {
		let track = ffi::parse_id(track)?;
		State::lock().consume.track_close(track)
	})
}

/// Consume an audio track from a broadcast, emitting the frames in order.
///
/// `on_frame` is called with a positive frame ID per frame, then exactly once
/// more with a terminal code: `0` (closed cleanly) or a negative error. After
/// the terminal (`<= 0`) callback, `on_frame` is never called again and
/// `user_data` is never touched again, so release `user_data` there. The
/// terminal callback fires even after [moq_consume_audio_close].
/// The `max_latency_ms` parameter controls how long to wait before skipping frames.
///
/// Returns a non-zero handle to the track on success, or a negative code on failure.
///
/// # Safety
/// - The caller must keep `user_data` valid until the terminal (`<= 0`) `on_frame` callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_audio_ordered(
	catalog: u32,
	index: u32,
	max_latency_ms: u64,
	on_frame: Option<extern "C" fn(user_data: *mut c_void, frame: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let catalog = ffi::parse_id(catalog)?;
		let index = index as usize;
		let max_latency = std::time::Duration::from_millis(max_latency_ms);
		let on_frame = unsafe { ffi::OnStatus::new(user_data, on_frame) };
		State::lock()
			.consume
			.audio_ordered(catalog, index, max_latency, on_frame)
	})
}

/// Stop an audio track consumer's background task.
///
/// Returns immediately: zero on success, or a negative code if already closed.
/// Does NOT free `user_data`; the [moq_consume_audio_ordered] `on_frame` callback
/// still fires once more with a terminal `0` (or a negative error), which is
/// where `user_data` should be released.
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_audio_close(track: u32) -> i32 {
	ffi::enter(move || {
		let track = ffi::parse_id(track)?;
		State::lock().consume.track_close(track)
	})
}

/// Get a chunk of a frame's payload.
///
/// Read the payload of a frame as a single contiguous slice.
///
/// Frames are not chunked; the entire payload is delivered through `dst.payload` /
/// `dst.payload_size` in one call. The pointer is valid until [`moq_consume_frame_close`]
/// is called for this frame.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `dst` is a valid pointer to a [moq_frame] struct.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_frame(frame: u32, dst: *mut moq_frame) -> i32 {
	ffi::enter(move || {
		let frame = ffi::parse_id(frame)?;
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		State::lock().consume.frame(frame, dst)
	})
}

/// Close a frame and clean up its resources.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_frame_close(frame: u32) -> i32 {
	ffi::enter(move || {
		let frame = ffi::parse_id(frame)?;
		State::lock().consume.frame_close(frame)
	})
}

/// Close a broadcast consumer and clean up its resources.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_close(consume: u32) -> i32 {
	ffi::enter(move || {
		let consume = ffi::parse_id(consume)?;
		State::lock().consume.close(consume)
	})
}

/// Subscribe to a raw track by name, delivering each frame's payload as-is.
///
/// This is the counterpart to [moq_publish_track]: no catalog lookup or
/// container parsing. `on_frame` is called with a positive raw frame ID for each
/// frame in sequence order, then exactly once more with a terminal code: `0`
/// (closed cleanly) or a negative error. After the terminal (`<= 0`) callback,
/// `on_frame` is never called again and `user_data` is never touched again, so
/// release `user_data` there. The terminal callback fires even after
/// [moq_consume_track_close]. Read each frame with [moq_consume_track_frame] and
/// release it with [moq_consume_track_frame_close]. Pass NULL for `subscription`
/// to use moq-net defaults.
///
/// Returns a non-zero handle to the track on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that name is a valid pointer to name_len bytes of data.
/// - The caller must ensure that subscription is either NULL or a valid pointer to a [moq_subscription] struct.
/// - The caller must keep `user_data` valid until the terminal (`<= 0`) `on_frame` callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_track(
	broadcast: u32,
	name: *const c_char,
	name_len: usize,
	subscription: *const moq_subscription,
	on_frame: Option<extern "C" fn(user_data: *mut c_void, frame: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let name = unsafe { ffi::parse_str(name, name_len)? };
		let subscription = unsafe { subscription.as_ref() }.map(moq_net::track::Subscription::from);
		let on_frame = unsafe { ffi::OnStatus::new(user_data, on_frame) };
		State::lock().consume.raw_track(broadcast, name, subscription, on_frame)
	})
}

/// Update a raw track subscription's delivery preferences.
///
/// Pass NULL for `subscription` to reset to moq-net defaults.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that subscription is either NULL or a valid pointer to a [moq_subscription] struct.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_track_update(track: u32, subscription: *const moq_subscription) -> i32 {
	ffi::enter(move || {
		let track = ffi::parse_id(track)?;
		let subscription = unsafe { subscription.as_ref() }.map(moq_net::track::Subscription::from);
		State::lock().consume.raw_track_update(track, subscription)
	})
}

/// Read a raw frame's payload delivered via the [moq_consume_track] callback.
///
/// Fills `dst.payload` / `dst.payload_size`; the pointer is valid until the
/// frame is released with [moq_consume_frame_close]. `dst.timestamp_us` is the
/// frame presentation timestamp in microseconds. `dst.keyframe` is reported as
/// false because raw tracks do not parse codec metadata.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `dst` is a valid pointer to a [moq_frame] struct.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_track_frame(frame: u32, dst: *mut moq_frame) -> i32 {
	ffi::enter(move || {
		let frame = ffi::parse_id(frame)?;
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		State::lock().consume.raw_frame(frame, dst)
	})
}

/// Close a raw frame and clean up its resources.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_track_frame_close(frame: u32) -> i32 {
	ffi::enter(move || {
		let frame = ffi::parse_id(frame)?;
		State::lock().consume.raw_frame_close(frame)
	})
}

/// Stop a raw track consumer's background task.
///
/// Returns immediately: zero on success, or a negative code if already closed.
/// Does NOT free `user_data`; the [moq_consume_track] `on_frame` callback still
/// fires once more with a terminal `0` (or a negative error), which is where
/// `user_data` should be released. Frames already delivered via the callback
/// remain valid until released with [moq_consume_track_frame_close].
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_track_close(track: u32) -> i32 {
	ffi::enter(move || {
		let track = ffi::parse_id(track)?;
		State::lock().consume.raw_track_close(track)
	})
}

/// Subscribe to a raw track's best-effort datagrams by name.
///
/// The datagram counterpart to [moq_consume_track], on its own subscription. `on_datagram`
/// is called with a positive datagram ID for each datagram in arrival order, then exactly
/// once more with a terminal code: `0` (closed cleanly) or a negative error. After the
/// terminal (`<= 0`) callback, `on_datagram` is never called again and `user_data` is never
/// touched again, so release `user_data` there. The terminal callback fires even after
/// [moq_consume_datagrams_close]. Read each datagram with [moq_consume_datagram] and release
/// it with [moq_consume_datagram_close]. Datagrams arrive only over datagram-capable
/// transports and lite-05 or newer moq-lite; there is no stream fallback.
///
/// Returns a non-zero handle to the subscription on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that name is a valid pointer to name_len bytes of data.
/// - The caller must keep `user_data` valid until the terminal (`<= 0`) `on_datagram` callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_datagrams(
	broadcast: u32,
	name: *const c_char,
	name_len: usize,
	on_datagram: Option<extern "C" fn(user_data: *mut c_void, datagram: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let name = unsafe { ffi::parse_str(name, name_len)? };
		let on_datagram = unsafe { ffi::OnStatus::new(user_data, on_datagram) };
		State::lock().consume.datagram_track(broadcast, name, on_datagram)
	})
}

/// Read a datagram delivered via the [moq_consume_datagrams] callback.
///
/// Fills `dst.payload` / `dst.payload_size` (valid until the datagram is released with
/// [moq_consume_datagram_close]), plus `dst.timestamp_us` and `dst.sequence`.
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `dst` is a valid pointer to a [moq_datagram] struct.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_datagram(datagram: u32, dst: *mut moq_datagram) -> i32 {
	ffi::enter(move || {
		let datagram = ffi::parse_id(datagram)?;
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		State::lock().consume.datagram(datagram, dst)
	})
}

/// Close a datagram and clean up its resources.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_datagram_close(datagram: u32) -> i32 {
	ffi::enter(move || {
		let datagram = ffi::parse_id(datagram)?;
		State::lock().consume.datagram_close(datagram)
	})
}

/// Stop a datagram subscription's background task.
///
/// Returns immediately: zero on success, or a negative code if already closed. Does NOT free
/// `user_data`; the [moq_consume_datagrams] `on_datagram` callback still fires once more with a
/// terminal `0` (or a negative error), which is where `user_data` should be released. Datagrams
/// already delivered via the callback remain valid until released with [moq_consume_datagram_close].
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_datagrams_close(task: u32) -> i32 {
	ffi::enter(move || {
		let task = ffi::parse_id(task)?;
		State::lock().consume.datagram_track_close(task)
	})
}

/// Subscribe to a JSON snapshot track (lossy latest-value) by name.
///
/// `on_value` is called with a positive value ID for each new latest value; a consumer that
/// falls behind collapses the backlog and only sees the newest. It is called exactly once more
/// with a terminal `0` (track ended / closed) or a negative error, after which `user_data` is
/// never touched again, so release it there. Read each value with [moq_consume_json_value] and
/// release it with [moq_consume_json_value_close]. Pass the same compression the producer used.
///
/// Returns a non-zero handle to the task on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure `name` is a valid pointer to `name_len` bytes and `config` a valid pointer.
/// - The caller must keep `user_data` valid until the terminal (`<= 0`) `on_value` callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_json(
	broadcast: u32,
	name: *const c_char,
	name_len: usize,
	config: *const moq_json_config,
	on_value: Option<extern "C" fn(user_data: *mut c_void, value: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let name = unsafe { ffi::parse_str(name, name_len)? };
		let config = unsafe { config.as_ref() }.ok_or(Error::InvalidPointer)?;
		let mut consumer = moq_json::snapshot::ConsumerConfig::default();
		consumer.compression = config.compression;
		let on_value = unsafe { ffi::OnStatus::new(user_data, on_value) };
		State::lock().consume.json(broadcast, name, consumer, on_value)
	})
}

/// Subscribe to a JSON stream track (lossless append-log) by name.
///
/// `on_value` is called with a positive value ID for each record, in order, then once more with
/// a terminal `0` or negative error where `user_data` should be released. Read each value with
/// [moq_consume_json_value] and release it with [moq_consume_json_value_close].
///
/// Returns a non-zero handle to the task on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure `name` is a valid pointer to `name_len` bytes and `config` a valid pointer.
/// - The caller must keep `user_data` valid until the terminal (`<= 0`) `on_value` callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_json_stream(
	broadcast: u32,
	name: *const c_char,
	name_len: usize,
	config: *const moq_json_stream_config,
	on_value: Option<extern "C" fn(user_data: *mut c_void, value: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let name = unsafe { ffi::parse_str(name, name_len)? };
		let config = unsafe { config.as_ref() }.ok_or(Error::InvalidPointer)?;
		let consumer = moq_json::stream::ConsumerConfig::default().with_compression(config.compression);
		let on_value = unsafe { ffi::OnStatus::new(user_data, on_value) };
		State::lock().consume.json_stream(broadcast, name, consumer, on_value)
	})
}

/// Read a JSON value delivered via a [moq_consume_json] or [moq_consume_json_stream] callback.
///
/// Fills `dst.json` / `dst.json_len`; the pointer is valid until the value is released with
/// [moq_consume_json_value_close].
///
/// Returns a zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure `dst` is a valid pointer to a [moq_json_value] struct.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_json_value(value: u32, dst: *mut moq_json_value) -> i32 {
	ffi::enter(move || {
		let value = ffi::parse_id(value)?;
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		State::lock().consume.json_value(value, dst)
	})
}

/// Release a JSON value delivered via a consumer callback.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_json_value_close(value: u32) -> i32 {
	ffi::enter(move || {
		let value = ffi::parse_id(value)?;
		State::lock().consume.json_value_close(value)
	})
}

/// Stop a JSON consumer's background task (snapshot or stream).
///
/// Returns immediately: zero on success, or a negative code if already closed. Does NOT free
/// `user_data`; the `on_value` callback still fires once more with a terminal `0` (or a negative
/// error), which is where `user_data` should be released. Values already delivered remain valid
/// until released with [moq_consume_json_value_close].
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_json_close(task: u32) -> i32 {
	ffi::enter(move || {
		let task = ffi::parse_id(task)?;
		State::lock().consume.json_close(task)
	})
}
