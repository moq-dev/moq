use crate::{Connect, Error, State, ffi};

use std::ffi::c_char;
use std::ffi::c_void;
use std::str::FromStr;

use tracing::Level;

/// How a media track's frames are wrapped, independent of the codec.
///
/// The ABI carries this as a `uint32_t`, so an unknown discriminant from C is an
/// error rather than UB.
#[repr(C)]
#[allow(non_camel_case_types)]
#[derive(Clone, Copy, Debug)]
pub enum moq_container_kind {
	/// A QUIC VarInt timestamp prefix followed by the raw codec payload.
	/// Timestamps are in microseconds.
	MOQ_CONTAINER_KIND_LEGACY = 0,
	/// Fragmented MP4: each frame is a complete moof+mdat fragment, described by
	/// the init segment in `moq_container::init`.
	MOQ_CONTAINER_KIND_CMAF = 1,
	/// Low Overhead Container (draft-ietf-moq-loc): a small property block
	/// followed by the codec payload.
	MOQ_CONTAINER_KIND_LOC = 2,
	/// A container this build does not recognize, so the rendition must be
	/// ignored. Only ever read out of a catalog: publishing it is an error.
	MOQ_CONTAINER_KIND_UNKNOWN = 3,
}

/// The container of a video or audio rendition, plus whatever that container
/// needs to describe itself.
///
/// Zeroing this struct means `MOQ_CONTAINER_KIND_LEGACY` with no init segment,
/// which is what a rendition written by [moq_publish_audio] or [moq_publish_video] carries.
#[repr(C)]
#[allow(non_camel_case_types)]
#[derive(Clone, Copy)]
pub struct moq_container {
	/// `moq_container_kind` discriminant.
	pub kind: u32,

	/// The CMAF init segment (ftyp+moov), or NULL.
	/// Read only when `kind` is `MOQ_CONTAINER_KIND_CMAF`, where it is required.
	pub init: *const u8,
	pub init_len: usize,
}

impl Default for moq_container {
	fn default() -> Self {
		Self {
			kind: moq_container_kind::MOQ_CONTAINER_KIND_LEGACY as u32,
			init: std::ptr::null(),
			init_len: 0,
		}
	}
}

/// # Safety
/// - `container->init` must point to `container->init_len` bytes when
///   `container->kind` is `MOQ_CONTAINER_KIND_CMAF`.
pub(crate) unsafe fn parse_container(container: &moq_container) -> Result<hang::catalog::Container, Error> {
	use hang::catalog::Container;

	Ok(match container.kind {
		v if v == moq_container_kind::MOQ_CONTAINER_KIND_LEGACY as u32 => Container::Legacy,
		v if v == moq_container_kind::MOQ_CONTAINER_KIND_CMAF as u32 => {
			let init = unsafe { ffi::parse_slice(container.init, container.init_len)? };
			// A CMAF rendition is undecodable without its init segment, so an empty one
			// fails here rather than at every subscriber.
			if init.is_empty() {
				return Err(Error::InvalidPointer);
			}

			Container::Cmaf {
				init: bytes::Bytes::copy_from_slice(init),
			}
		}
		v if v == moq_container_kind::MOQ_CONTAINER_KIND_LOC as u32 => Container::Loc,
		// UNKNOWN included: we kept none of the original JSON, so there is nothing to republish.
		_ => return Err(Error::InvalidCode),
	})
}

/// Describe a catalog container for C, borrowing the CMAF init segment rather
/// than copying it, so the result lives only as long as the catalog snapshot.
pub(crate) fn borrow_container(container: &hang::catalog::Container) -> moq_container {
	use hang::catalog::Container;

	let (kind, init) = match container {
		Container::Legacy => (moq_container_kind::MOQ_CONTAINER_KIND_LEGACY, None),
		Container::Cmaf { init } => (moq_container_kind::MOQ_CONTAINER_KIND_CMAF, Some(init)),
		Container::Loc => (moq_container_kind::MOQ_CONTAINER_KIND_LOC, None),
		Container::Unknown(_) => (moq_container_kind::MOQ_CONTAINER_KIND_UNKNOWN, None),
	};

	moq_container {
		kind: kind as u32,
		init: init.map_or(std::ptr::null(), |init| init.as_ptr()),
		init_len: init.map_or(0, |init| init.len()),
	}
}

/// A single audio codec [moq_publish_audio] can parse.
#[repr(C)]
#[allow(non_camel_case_types)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum moq_audio_format {
	/// Advanced Audio Coding, configured by an AudioSpecificConfig.
	MOQ_AUDIO_FORMAT_AAC = 0,
	/// Opus, configured by an OpusHead.
	MOQ_AUDIO_FORMAT_OPUS = 1,
	/// FLAC, configured by the `fLaC` marker plus its STREAMINFO block.
	MOQ_AUDIO_FORMAT_FLAC = 2,
	/// MPEG-1/2 Audio Layer III.
	MOQ_AUDIO_FORMAT_MP3 = 3,
}

/// A single video codec [moq_publish_video] can parse.
///
/// H.264 and H.265 appear twice each because the framing differs, not just the
/// codec: AVC1/HVC1 are length-prefixed with an out-of-band config record,
/// while AVC3/HEV1 are Annex-B with the parameter sets inline.
#[repr(C)]
#[allow(non_camel_case_types)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum moq_video_format {
	/// H.264, length-prefixed NALUs with an out-of-band avcC.
	MOQ_VIDEO_FORMAT_AVC1 = 0,
	/// H.264, Annex-B with inline SPS/PPS.
	MOQ_VIDEO_FORMAT_AVC3 = 1,
	/// H.265, length-prefixed NALUs with an out-of-band hvcC.
	MOQ_VIDEO_FORMAT_HVC1 = 2,
	/// H.265, Annex-B with inline parameter sets.
	MOQ_VIDEO_FORMAT_HEV1 = 3,
	/// AV1.
	MOQ_VIDEO_FORMAT_AV01 = 4,
	/// VP8.
	MOQ_VIDEO_FORMAT_VP8 = 5,
	/// VP9.
	MOQ_VIDEO_FORMAT_VP9 = 6,
}

/// A container [moq_publish_container] can demux, which may publish several tracks.
#[repr(C)]
#[allow(non_camel_case_types)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum moq_container_format {
	/// Fragmented MP4 / CMAF.
	MOQ_CONTAINER_FORMAT_FMP4 = 0,
	/// Matroska / WebM.
	MOQ_CONTAINER_FORMAT_MKV = 1,
	/// MPEG-2 transport stream.
	MOQ_CONTAINER_FORMAT_TS = 2,
	/// Flash Video, as used by RTMP.
	MOQ_CONTAINER_FORMAT_FLV = 3,
}

/// Configuration for [moq_publish_audio].
///
/// Zero the struct, then set `format` and the required `init` bytes. New
/// optional fields are appended so existing initializers keep their meaning.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_audio_init {
	/// The audio codec, a [moq_audio_format] value.
	pub format: u32,

	/// Codec init bytes: an OpusHead, an AudioSpecificConfig, a STREAMINFO.
	/// Required, since audio has no in-band config to resolve from frames.
	pub init: *const u8,
	/// Length of `init` in bytes.
	pub init_len: usize,

	/// Human-readable rendition name for track pickers, or NULL if not used.
	pub label: *const c_char,
	/// Length of `label` in bytes.
	pub label_len: usize,
}

/// Configuration for [moq_publish_video].
///
/// Zero the struct, then set `format` and whatever else the codec needs. `init`
/// may stay NULL for a format that resolves in band.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_video_init {
	/// The video codec, a [moq_video_format] value.
	pub format: u32,

	/// Codec init bytes (an avcC, an hvcC), or NULL for a format that resolves
	/// from the stream itself.
	pub init: *const u8,
	/// Length of `init` in bytes.
	pub init_len: usize,

	/// Human-readable rendition name for track pickers, or NULL if not used.
	pub label: *const c_char,
	/// Length of `label` in bytes.
	pub label_len: usize,
}

/// Configuration for [moq_publish_container].
///
/// There is no label here: a container publishes and describes its own tracks,
/// so a rendition name would have no single track to land on.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_container_init {
	/// The container format, a [moq_container_format] value.
	pub format: u32,

	/// The leading chunk of the container, decoded immediately, or NULL.
	pub init: *const u8,
	/// Length of `init` in bytes.
	pub init_len: usize,
}

/// Validate an audio format code from C.
///
/// The field is a `u32` rather than the enum: C can put any integer there, and matching an
/// out-of-range discriminant as a Rust enum is UB. Same reason as [moq_audio_sample_format].
fn audio_format_from_u32(value: u32) -> Result<moq_mux::import::AudioFormat, Error> {
	use moq_mux::import::AudioFormat;
	Ok(match value {
		v if v == moq_audio_format::MOQ_AUDIO_FORMAT_AAC as u32 => AudioFormat::Aac,
		v if v == moq_audio_format::MOQ_AUDIO_FORMAT_OPUS as u32 => AudioFormat::Opus,
		v if v == moq_audio_format::MOQ_AUDIO_FORMAT_FLAC as u32 => AudioFormat::Flac,
		v if v == moq_audio_format::MOQ_AUDIO_FORMAT_MP3 as u32 => AudioFormat::Mp3,
		_ => return Err(Error::InvalidCode),
	})
}

/// Validate a video format code from C. See [audio_format_from_u32].
fn video_format_from_u32(value: u32) -> Result<moq_mux::import::VideoFormat, Error> {
	use moq_mux::import::VideoFormat;
	Ok(match value {
		v if v == moq_video_format::MOQ_VIDEO_FORMAT_AVC1 as u32 => VideoFormat::Avc1,
		v if v == moq_video_format::MOQ_VIDEO_FORMAT_AVC3 as u32 => VideoFormat::Avc3,
		v if v == moq_video_format::MOQ_VIDEO_FORMAT_HVC1 as u32 => VideoFormat::Hvc1,
		v if v == moq_video_format::MOQ_VIDEO_FORMAT_HEV1 as u32 => VideoFormat::Hev1,
		v if v == moq_video_format::MOQ_VIDEO_FORMAT_AV01 as u32 => VideoFormat::Av01,
		v if v == moq_video_format::MOQ_VIDEO_FORMAT_VP8 as u32 => VideoFormat::Vp8,
		v if v == moq_video_format::MOQ_VIDEO_FORMAT_VP9 as u32 => VideoFormat::Vp9,
		_ => return Err(Error::InvalidCode),
	})
}

/// Validate a container format code from C. See [audio_format_from_u32].
fn container_format_from_u32(value: u32) -> Result<moq_mux::import::ContainerFormat, Error> {
	use moq_mux::import::ContainerFormat;
	Ok(match value {
		v if v == moq_container_format::MOQ_CONTAINER_FORMAT_FMP4 as u32 => ContainerFormat::Fmp4,
		v if v == moq_container_format::MOQ_CONTAINER_FORMAT_MKV as u32 => ContainerFormat::Mkv,
		v if v == moq_container_format::MOQ_CONTAINER_FORMAT_TS as u32 => ContainerFormat::Ts,
		v if v == moq_container_format::MOQ_CONTAINER_FORMAT_FLV as u32 => ContainerFormat::Flv,
		_ => return Err(Error::InvalidCode),
	})
}

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

	/// The encoded width/height of the media, a hint so a decoder can size its
	/// buffers up front. Zero means absent, which no valid dimension is, so the
	/// two are independent: a catalog carrying only one round-trips unchanged.
	pub coded_width: u32,
	pub coded_height: u32,

	/// How the track's frames are wrapped.
	pub container: moq_container,

	/// Human-readable rendition name for track pickers, or NULL if not used.
	pub label: *const c_char,
	/// Length of `label` in bytes.
	pub label_len: usize,
}

/// Catalog properties shared by every video rendition.
///
/// A false `has_*` flag clears that field from the next catalog rather than preserving its previous value.
#[repr(C)]
#[allow(non_camel_case_types)]
#[derive(Clone, Copy, Default)]
pub struct moq_video_properties {
	/// Final rendered width in pixels when `has_display` is true.
	pub display_width: u32,

	/// Final rendered height in pixels when `has_display` is true.
	pub display_height: u32,

	/// Whether `display_width` and `display_height` are present.
	pub has_display: bool,

	/// Clockwise rotation in degrees when `has_rotation` is true.
	pub rotation: f64,

	/// Whether `rotation` is present.
	pub has_rotation: bool,

	/// Whether to flip horizontally after rotation when `has_flip` is true.
	pub flip: bool,

	/// Whether `flip` is present.
	pub has_flip: bool,
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

	/// How the track's frames are wrapped.
	pub container: moq_container,

	/// Human-readable rendition name for track pickers, or NULL if not used.
	pub label: *const c_char,
	/// Length of `label` in bytes.
	pub label_len: usize,
}

/// Options for a JSON snapshot track (lossy latest-value mode).
///
/// The same config is passed to a producer and its consumers, but the consumer reads only
/// `compression`; `delta_ratio` is producer-only.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_json_snapshot_config {
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

	/// Maximum age of a non-latest group before the publisher evicts it, in milliseconds.
	/// The publisher-side half of `moq_subscription.max_age_ms`.
	pub max_age_ms: u64,
	/// Whether `max_age_ms` is set. When false, the publisher's default applies.
	pub max_age_present: bool,

	/// Per-frame timescale in ticks per second.
	pub timescale: u64,
	/// Whether `timescale` is set. When false, the default microsecond timescale
	/// applies, matching the `timestamp_us` units used everywhere else in this ABI.
	pub timescale_present: bool,
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
		if info.max_age_present {
			out = out.with_max_age(std::time::Duration::from_millis(info.max_age_ms));
		}
		if info.timescale_present {
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

	/// Maximum age of a non-latest group before it is skipped, in milliseconds.
	/// Zero skips immediately. Enforced by the publisher's cache and by any local buffering.
	pub max_age_ms: u64,

	/// First group to deliver.
	pub group_start: u64,
	/// Whether `group_start` is present. When false, delivery starts at the latest group.
	pub group_start_present: bool,

	/// Last group to deliver, inclusive.
	pub group_end: u64,
	/// Whether `group_end` is present. When false, there is no end cap.
	pub group_end_present: bool,
}

impl From<&moq_subscription> for moq_net::track::Subscription {
	fn from(subscription: &moq_subscription) -> Self {
		let mut out = moq_net::track::Subscription::default()
			.with_priority(subscription.priority)
			.with_ordered(subscription.ordered)
			.with_max_age(std::time::Duration::from_millis(subscription.max_age_ms));
		if subscription.group_start_present {
			out = out.with_start(moq_net::track::Position::group(subscription.group_start));
		}
		if subscription.group_end_present {
			out = out.with_end(moq_net::track::Position::after_group(subscription.group_end));
		}
		out
	}
}

/// A borrowed UTF-8 string slice, NOT NULL terminated.
///
/// Used in both directions. As an output (e.g. a JSON document libmoq hands back) the
/// pointer borrows libmoq's own storage and is only valid until the owning resource is
/// freed; see the function that fills it for the exact lifetime. As an input (e.g. a
/// [moq_client_config] list) the pointer borrows the caller's storage and is only read
/// during the call.
#[repr(C)]
#[allow(non_camel_case_types)]
#[derive(Clone, Copy)]
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
			"" => moq_tokio::Log::default(),
			level => moq_tokio::Log::new(Level::from_str(level)?),
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

/// The protocol version names this build offers by default, spelled the way
/// [moq_client_config]'s `versions` expects. Built once; the slices are valid for the life of
/// the process.
static VERSION_NAMES: std::sync::LazyLock<Vec<String>> =
	std::sync::LazyLock::new(|| moq_net::Versions::all().iter().map(|v| v.to_string()).collect());

/// List the protocol versions offered during the handshake by default.
///
/// Writes up to `count` names into `dst` and returns the total number available, which
/// may be larger than `count`. Pass a NULL `dst` with a zero `count` to size the array
/// first. Each name borrows a static string valid for the life of the process, so a
/// caller building a menu can hold them indefinitely.
///
/// Work-in-progress versions are omitted, since they are not advertised unless pinned;
/// a dial still accepts them by name.
///
/// Returns the total count on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `dst` is either NULL with a zero `count`, or a valid
///   pointer to `count` writable [moq_string] values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_versions(dst: *mut moq_string, count: usize) -> i32 {
	ffi::enter(move || {
		if !dst.is_null() {
			let dst = unsafe { std::slice::from_raw_parts_mut(dst, count) };
			for (slot, name) in dst.iter_mut().zip(VERSION_NAMES.iter()) {
				slot.data = name.as_ptr().cast::<c_char>();
				slot.len = name.len();
			}
		} else if count != 0 {
			return Err(Error::InvalidPointer);
		}

		Ok(VERSION_NAMES.len())
	})
}

/// The QUIC backend names this build offers, spelled the way [moq_client_config]'s `backend`
/// expects. Built once; the slices are valid for the life of the process.
static BACKEND_NAMES: std::sync::LazyLock<Vec<&'static str>> =
	std::sync::LazyLock::new(|| moq_tokio::QuicBackend::compiled().iter().map(|b| b.as_str()).collect());

/// List the QUIC backends this build was compiled with.
///
/// Writes up to `count` names into `dst` and returns the total number available, which
/// may be larger than `count`. Pass a NULL `dst` with a zero `count` to size the array
/// first. Each name borrows a static string valid for the life of the process.
///
/// The backends are compile-time optional, so a caller building a menu must read this
/// rather than listing names: an option this build lacks is rejected by
/// a dial, which would leave a menu entry that can only fail.
///
/// Returns the total count on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `dst` is either NULL with a zero `count`, or a valid
///   pointer to `count` writable [moq_string] values.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_backends(dst: *mut moq_string, count: usize) -> i32 {
	ffi::enter(move || {
		if !dst.is_null() {
			let dst = unsafe { std::slice::from_raw_parts_mut(dst, count) };
			for (slot, name) in dst.iter_mut().zip(BACKEND_NAMES.iter()) {
				slot.data = name.as_ptr().cast::<c_char>();
				slot.len = name.len();
			}
		} else if count != 0 {
			return Err(Error::InvalidPointer);
		}

		Ok(BACKEND_NAMES.len())
	})
}

/// Whether this build can capture qlog traces.
///
/// Capture is compile-time optional. [moq_client_config]'s `quic_qlog` accepts a directory
/// either way, but dialing fails when the support is absent, so a caller offering the
/// knob should hide it rather than surface an option that cannot work.
#[unsafe(no_mangle)]
pub extern "C" fn moq_qlog_supported() -> bool {
	moq_tokio::qlog_supported()
}

/// A duration as the milliseconds the setters take, saturating rather than wrapping.
fn millis(duration: std::time::Duration) -> u64 {
	duration.as_millis().min(u64::MAX as u128) as u64
}

/// Settings for [moq_session_connect], or NULL to dial with the defaults.
///
/// Zero it (`memset`, or a `{0}` initializer) and set only what you need: a
/// zeroed struct means the defaults throughout. That is why the knobs whose
/// default is not zero carry a `has_*` flag rather than being read directly. The
/// WebSocket fallback is on by default and the reconnect backoff starts at one
/// second, so a caller who never touched them would otherwise silently turn them
/// off.
///
/// New settings are appended to the end of this struct, and a zeroed one keeps
/// the previous behavior, so adding one does not disturb existing callers.
#[repr(C)]
#[allow(non_camel_case_types)]
pub struct moq_client_config {
	/// Protocol versions to offer during the handshake, most preferred first.
	/// NULL/0 offers everything this build supports. Names are spelled the way
	/// the CLI spells them (`moq-lite-05`, `moq-transport-19`); [moq_versions]
	/// lists what is on offer.
	pub versions: *const moq_string,
	pub versions_len: usize,

	/// QUIC backend name, or NULL for this build's choice. [moq_backends] lists
	/// the names this build accepts, which are compile-time dependent.
	pub backend: *const c_char,
	pub backend_len: usize,

	/// Local socket address to bind, or NULL for the wildcard address.
	pub bind: *const c_char,
	pub bind_len: usize,

	/// How long a dial may take before it gives up.
	pub connect_timeout_ms: u64,
	pub has_connect_timeout: bool,

	/// Happy Eyeballs: how long before the next address is also dialed.
	pub failover_delay_ms: u64,
	pub has_failover_delay: bool,

	/// Happy Eyeballs: how long the first family waits for the AAAA answer.
	pub resolution_delay_ms: u64,
	pub has_resolution_delay: bool,

	/// Whether the WebSocket fallback may be raced, for a UDP-blocked network.
	/// Enabled unless you turn it off, hence the flag.
	pub websocket_enabled: bool,
	pub has_websocket_enabled: bool,

	/// How long QUIC gets before the WebSocket fallback is also dialed.
	pub websocket_delay_ms: u64,
	pub has_websocket_delay: bool,

	/// Accept any certificate. Development only: prefer `tls_fingerprints`,
	/// and pairing this with a fingerprint or a root is rejected at dial.
	pub tls_disable_verify: bool,

	/// Whether to trust the platform root store. Its default depends on the
	/// backend, so it needs the flag to distinguish "off" from "unset".
	pub tls_system_roots: bool,
	pub has_tls_system_roots: bool,

	/// Extra root certificate paths to trust.
	pub tls_roots: *const moq_string,
	pub tls_roots_len: usize,

	/// SHA-256 certificate fingerprints to pin, hex encoded. The native
	/// equivalent of the browser's `serverCertificateHashes`.
	pub tls_fingerprints: *const moq_string,
	pub tls_fingerprints_len: usize,

	/// SNI override, or NULL to use the host from the URL.
	pub tls_host_name: *const c_char,
	pub tls_host_name_len: usize,

	/// Client certificate and key paths for mTLS, or NULL for none.
	pub tls_cert: *const c_char,
	pub tls_cert_len: usize,
	pub tls_key: *const c_char,
	pub tls_key_len: usize,

	/// Reconnect pacing. Each must leave a non-zero delay or retrying would
	/// spin, which is rejected at dial.
	pub backoff_initial_ms: u64,
	pub has_backoff_initial: bool,
	pub backoff_multiplier: u32,
	pub has_backoff_multiplier: bool,
	pub backoff_max_ms: u64,
	pub has_backoff_max: bool,
	/// How long reconnection keeps trying before giving up for good.
	pub backoff_timeout_ms: u64,
	pub has_backoff_timeout: bool,

	/// QUIC transport tuning, all ignored by the WebSocket fallback.
	pub quic_max_streams: u64,
	pub has_quic_max_streams: bool,
	pub quic_idle_timeout_ms: u64,
	pub has_quic_idle_timeout: bool,
	pub quic_keep_alive_ms: u64,
	pub has_quic_keep_alive: bool,
	/// Generic segmentation offload and path MTU discovery. Both default to the
	/// backend's choice, so both need their flag.
	pub quic_gso: bool,
	pub has_quic_gso: bool,
	pub quic_mtu_discovery: bool,
	pub has_quic_mtu_discovery: bool,

	/// Congestion control family name, or NULL for the backend's choice.
	pub quic_congestion_control: *const c_char,
	pub quic_congestion_control_len: usize,

	/// Directory to write qlog traces into, or NULL for none. Capture is
	/// compile-time optional; see [moq_qlog_supported].
	pub quic_qlog: *const c_char,
	pub quic_qlog_len: usize,
}

/// The settings [moq_session_connect] dials with when given NULL.
///
/// Behaviorally the same as a zeroed struct, so this is for display rather than
/// for dialing: a settings UI can show the real numbers instead of hardcoding
/// ones that go stale when a default is retuned. The knobs whose default depends
/// on the backend (GSO, path MTU discovery, congestion control, the TLS root
/// store) come back with their `has_*` flag false, since there is no single value
/// to report.
///
/// Returned by value because there is nothing to fail: no handle to look up and
/// no pointer to reject. Prefer a zeroed struct when you only mean to set a knob
/// or two, and this when you want to read the numbers.
#[unsafe(no_mangle)]
pub extern "C" fn moq_client_defaults() -> moq_client_config {
	// SAFETY: every field is a scalar or a raw pointer, so all-zero is a valid
	// value, and it is the one that means "unset" throughout.
	let mut dst: moq_client_config = unsafe { std::mem::zeroed() };

	// A panic here would have no way to report itself, so fall back to the zeroed
	// struct: it is what "the defaults" means to a dial anyway, and only the
	// reported numbers would be wrong.
	let filled = std::panic::catch_unwind(|| {
		let mut dst: moq_client_config = unsafe { std::mem::zeroed() };
		let config = crate::client::Config::default();

		dst.connect_timeout_ms = millis(config.connect.resolved_timeout());
		dst.has_connect_timeout = true;
		dst.failover_delay_ms = millis(config.connect.resolved_race());
		dst.has_failover_delay = true;
		dst.resolution_delay_ms = millis(config.connect.resolved_resolution_delay());
		dst.has_resolution_delay = true;

		dst.websocket_enabled = config.connect.websocket.resolved_enabled();
		dst.has_websocket_enabled = true;
		dst.websocket_delay_ms = millis(config.connect.websocket.resolved_delay());
		dst.has_websocket_delay = true;

		dst.backoff_initial_ms = millis(config.connect.backoff.initial());
		dst.has_backoff_initial = true;
		dst.backoff_multiplier = config.connect.backoff.multiplier();
		dst.has_backoff_multiplier = true;
		dst.backoff_max_ms = millis(config.connect.backoff.max());
		dst.has_backoff_max = true;
		dst.backoff_timeout_ms = millis(config.connect.backoff.timeout());
		dst.has_backoff_timeout = true;

		let quic = config.quic.resolve();
		dst.quic_max_streams = quic.max_streams;
		dst.has_quic_max_streams = true;
		dst.quic_idle_timeout_ms = millis(quic.idle_timeout);
		dst.has_quic_idle_timeout = true;
		if let Some(keep_alive) = quic.keep_alive {
			dst.quic_keep_alive_ms = millis(keep_alive);
			dst.has_quic_keep_alive = true;
		}

		dst
	});

	if let Ok(value) = filled {
		dst = value;
	}

	dst
}

/// Resolve handles under the global lock, prepare the client without it, then insert
/// the ready session under a short second lock.
unsafe fn connect_session(
	url: *const c_char,
	url_len: usize,
	config: *const moq_client_config,
	origin_publish: u32,
	origin_consume: u32,
	on_status: Option<extern "C" fn(user_data: *mut c_void, code: i32)>,
	user_data: *mut c_void,
) -> Result<crate::Id, Error> {
	let url = ffi::parse_url(url, url_len)?;
	let origin_publish = ffi::parse_id_optional(origin_publish)?;
	let origin_consume = ffi::parse_id_optional(origin_consume)?;

	// Parse before taking the lock: it validates, and a rejected value should not
	// have blocked every other call while it was being read.
	let config = unsafe { crate::parse_client(config.as_ref())? };

	let (publish, consume) = {
		let state = State::lock();
		let publish = origin_publish.map(|id| state.origin.get(id)).transpose()?.cloned();
		let consume = origin_consume.map(|id| state.origin.get(id)).transpose()?.cloned();
		(publish, consume)
	};

	let callback = unsafe { ffi::OnStatus::new(user_data, on_status) };
	let request = Connect {
		config,
		url,
		publish,
		consume,
		callback,
	}
	.prepare()?;

	State::lock().session.connect(request)
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
/// Pass NULL for `config` to dial with the defaults. Fill in a
/// [moq_client_config] to pin a protocol version, adjust TLS trust, or tune the
/// transport; it is read during the call and not retained, so the same one can
/// dial any number of sessions.
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
/// - `config` must be NULL, or an aligned, readable [moq_client_config]. Every
///   non-NULL pointer inside it must be valid for its paired length, and all of
///   them must stay alive for the duration of this call: the config is read
///   here, not copied by whoever filled it in.
/// - The caller must keep `user_data` valid until the terminal (`<= 0`) `on_status` callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_session_connect(
	url: *const c_char,
	url_len: usize,
	config: *const moq_client_config,
	origin_publish: u32,
	origin_consume: u32,
	on_status: Option<extern "C" fn(user_data: *mut c_void, code: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || unsafe {
		connect_session(
			url,
			url_len,
			config,
			origin_publish,
			origin_consume,
			on_status,
			user_data,
		)
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

/// Create a broadcast at `path` on an origin, for publishing media tracks.
///
/// The broadcast starts live: the origin announces the path so consumers can discover it,
/// becoming visible shortly after this returns. Fill it with the `moq_publish_*` functions.
/// Toggle discoverability with [moq_publish_set_announce]; [moq_publish_finish] unpublishes
/// immediately.
///
/// Returns a non-zero broadcast handle on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that path is a valid pointer to path_len bytes of data.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_origin_publish(origin: u32, path: *const c_char, path_len: usize) -> i32 {
	ffi::enter(move || {
		let origin = ffi::parse_id(origin)?;
		let path = unsafe { ffi::parse_str(path, path_len)? };

		let mut state = State::lock();
		let broadcast = state.origin.publish(origin, path)?;
		state.publish.create(broadcast)
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

/// Set whether a broadcast created by [moq_origin_publish] is live: announced by its origin.
///
/// A non-live broadcast stays reachable by exact path for subscribes and fetches; it just is
/// not announced. This is how a publisher goes on and off the air without tearing down the
/// broadcast.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_set_announce(broadcast: u32, announce: bool) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		State::lock().publish.set_announce(broadcast, announce)
	})
}

/// Finish a broadcast and release it, ending its catalog cleanly.
///
/// Subscribers see a normal end of stream rather than an error, and the origin unpublishes
/// the path immediately.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_finish(broadcast: u32) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		State::lock().publish.finish(broadcast)
	})
}

/// Publish one audio codec as a new media track.
///
/// The track is named after the format (`0.opus`), so a subscriber finds it
/// through the catalog rather than by a name you choose.
/// [moq_audio_init::init] is required: audio resolves its whole rendition from
/// those bytes. Frames written with [moq_publish_media_frame] must be in decode
/// order.
///
/// Returns a non-zero handle to the track on success, or a negative code on failure.
///
/// # Safety
/// - `config` must be NULL, or point to an aligned, readable [moq_audio_init].
///   Every non-NULL pointer inside it must be valid for its paired length and
///   stay alive for the duration of this call. A NULL config is rejected with an
///   ordinary error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_audio(broadcast: u32, config: *const moq_audio_init) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let config = unsafe { config.as_ref() }.ok_or(Error::InvalidPointer)?;
		let init = unsafe { ffi::parse_slice(config.init, config.init_len)? };
		let label = unsafe { ffi::parse_str_optional(config.label, config.label_len)? };

		let mut audio = moq_mux::import::AudioInit::new(audio_format_from_u32(config.format)?, init.to_vec());
		audio.label = label.map(str::to_string);

		State::lock().publish.audio(broadcast, audio)
	})
}

/// Publish one video codec as a new media track.
///
/// Named as in [moq_publish_audio]. [moq_video_init::init] may be NULL for a
/// format that resolves in band.
///
/// Returns a non-zero handle to the track on success, or a negative code on failure.
///
/// # Safety
/// - As [moq_publish_audio], for a [moq_video_init].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_video(broadcast: u32, config: *const moq_video_init) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let config = unsafe { config.as_ref() }.ok_or(Error::InvalidPointer)?;
		let init = unsafe { ffi::parse_slice(config.init, config.init_len)? };
		let label = unsafe { ffi::parse_str_optional(config.label, config.label_len)? };

		let mut video = moq_mux::import::VideoInit::new(video_format_from_u32(config.format)?, init.to_vec());
		video.label = label.map(str::to_string);

		State::lock().publish.video(broadcast, video)
	})
}

/// Publish a container, which demuxes and publishes its own tracks.
///
/// Feed it whole chunks with [moq_publish_container_write]. Unlike the codec
/// entry points there is no label: a container describes each track it publishes
/// from its own metadata.
///
/// Returns a non-zero handle to the container on success, or a negative code on failure.
///
/// # Safety
/// - As [moq_publish_audio], for a [moq_container_init].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_container(broadcast: u32, config: *const moq_container_init) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let config = unsafe { config.as_ref() }.ok_or(Error::InvalidPointer)?;
		let init = unsafe { ffi::parse_slice(config.init, config.init_len)? };

		let container = moq_mux::import::ContainerInit::new(container_format_from_u32(config.format)?, init.to_vec());
		State::lock().publish.container(broadcast, container)
	})
}

/// Draw a group boundary on a media importer.
///
/// For a codec track this ends the open group; the next frame written starts a new one. Audio has
/// no boundary of its own (every packet is independently decodable), so this is the only thing
/// that gives it groups: call it after every frame for one group (one QUIC stream) the relay
/// forwards without waiting, or at a segment cadence to align with video for HLS/DASH. Video
/// groups at its own keyframes and needs this only to override that.
///
/// A container has its own [moq_publish_container_cut], since it rolls a group on every track it
/// publishes rather than ending one group.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_media_cut(media: u32) -> i32 {
	ffi::enter(move || {
		let media = ffi::parse_id(media)?;
		State::lock().publish.media_cut(media)
	})
}

/// Draw a group boundary and number the next group `sequence`.
///
/// [moq_publish_media_cut] with an explicit sequence, for a caller whose group numbers have to be
/// deterministic: two encoders publishing the same content align per GOP so a consumer can fail
/// over between them.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_media_seek(media: u32, sequence: u64) -> i32 {
	ffi::enter(move || {
		let media = ffi::parse_id(media)?;
		State::lock().publish.media_seek(media, sequence)
	})
}

/// Finish a media track, flushing any buffered frames. No more frames can be written.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_media_finish(export: u32) -> i32 {
	ffi::enter(move || {
		let export = ffi::parse_id(export)?;
		State::lock().publish.media_finish(export)
	})
}

/// Write a whole chunk of container bytes.
///
/// No timestamp: a container carries its tracks' timing itself, and the importer
/// reads it out rather than taking the caller's word for it.
///
/// Returns zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure `payload` is valid for `payload_size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_container_write(container: u32, payload: *const u8, payload_size: usize) -> i32 {
	ffi::enter(move || {
		let container = ffi::parse_id(container)?;
		let payload = unsafe { ffi::parse_slice(payload, payload_size)? };
		State::lock().publish.container_write(container, payload)
	})
}

/// Declare that the next chunk starts a new segment, rolling a group on every
/// track the container publishes.
///
/// An fMP4 source carrying `styp` atoms declares its own segments, so this is
/// only needed when it doesn't. Formats with no segment concept (MKV, TS, FLV)
/// ignore it.
///
/// Returns zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_container_cut(container: u32) -> i32 {
	ffi::enter(move || {
		let container = ffi::parse_id(container)?;
		State::lock().publish.container_cut(container)
	})
}

/// Start a new segment and number its groups `sequence`.
///
/// Returns zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_container_seek(container: u32, sequence: u64) -> i32 {
	ffi::enter(move || {
		let container = ffi::parse_id(container)?;
		State::lock().publish.container_seek(container, sequence)
	})
}

/// Finish every track the container publishes and release the handle.
///
/// Returns zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_container_finish(container: u32) -> i32 {
	ffi::enter(move || {
		let container = ffi::parse_id(container)?;
		State::lock().publish.container_finish(container)
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

/// Replace the catalog properties shared by every video rendition.
///
/// Rotation is clockwise and normalized to the nearest quarter turn. A field whose matching `has_*` flag is false is removed from the next catalog update.
///
/// Returns zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `properties` points to a valid [moq_video_properties].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_video_properties(broadcast: u32, properties: *const moq_video_properties) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let properties = unsafe { properties.as_ref() }.ok_or(Error::InvalidPointer)?;

		let mut value = hang::catalog::VideoProperties::default();
		value.display = properties.has_display.then_some(hang::catalog::Display {
			width: properties.display_width,
			height: properties.display_height,
		});
		value.rotation = properties.has_rotation.then_some(properties.rotation);
		value.flip = properties.has_flip.then_some(properties.flip);

		State::lock().publish.video_properties(broadcast, value)
	})
}

/// Add or replace a video rendition in a broadcast's catalog.
///
/// This is the producer counterpart to [moq_consume_video_config]: instead of
/// reading a rendition out of a catalog, it writes one into the catalog of a
/// broadcast created with [moq_origin_publish]. The rendition is keyed by
/// `config.name`; calling this again with the same name replaces the rendition
/// you declared, so a config can be refined in place. It fails only when a
/// [moq_publish_video] track owns the name, since that track publishes and
/// retires its own rendition. The updated catalog is published to subscribers
/// automatically.
///
/// The struct fields are read as inputs:
/// - `name` / `codec` are required (NOT NULL terminated) string slices.
/// - `label` may be NULL to omit the human-readable rendition name.
/// - `description` may be NULL to omit it.
/// - `coded_width` / `coded_height` may be zero to omit them.
/// - `container` describes how the frames written to the track are wrapped. A
///   zeroed one declares the legacy container, which is what [moq_publish_video]
///   writes; declare CMAF or LOC for a [moq_publish_track] whose frames you
///   already encode that way.
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
		let label = unsafe { ffi::parse_str_optional(config.label, config.label_len)? };
		let codec = unsafe { ffi::parse_str(config.codec, config.codec_len)? };
		let codec = hang::catalog::VideoCodec::from_str(codec).map_err(Error::Hang)?;

		let mut video = hang::catalog::VideoConfig::new(codec);
		video.label = label.map(str::to_string);
		if !config.description.is_null() {
			let description = unsafe { ffi::parse_slice(config.description, config.description_len)? };
			video.description = Some(bytes::Bytes::copy_from_slice(description));
		}
		video.coded_width = (config.coded_width > 0).then_some(config.coded_width);
		video.coded_height = (config.coded_height > 0).then_some(config.coded_height);
		video.container = unsafe { parse_container(&config.container)? };

		State::lock().publish.video_config(broadcast, name, video)
	})
}

/// Add or replace an audio rendition in a broadcast's catalog.
///
/// This is the producer counterpart to [moq_consume_audio_config]. The rendition
/// is keyed by `config.name`, on the same terms as [moq_publish_video_config]:
/// a repeat call replaces your own rendition, and a name a [moq_publish_audio]
/// track owns is refused. The updated catalog is published to subscribers
/// automatically.
///
/// The struct fields are read as inputs:
/// - `name` / `codec` are required (NOT NULL terminated) string slices.
/// - `label` may be NULL to omit the human-readable rendition name.
/// - `sample_rate` / `channel_count` are required.
/// - `description` may be NULL to omit it.
/// - `container` describes how the frames written to the track are wrapped, the
///   same as for [moq_publish_video_config].
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
		let label = unsafe { ffi::parse_str_optional(config.label, config.label_len)? };
		let codec = unsafe { ffi::parse_str(config.codec, config.codec_len)? };
		let codec = hang::catalog::AudioCodec::from_str(codec).map_err(Error::Hang)?;

		let mut audio = hang::catalog::AudioConfig::new(codec, config.sample_rate, config.channel_count);
		audio.label = label.map(str::to_string);
		audio.container = unsafe { parse_container(&config.container)? };
		if !config.description.is_null() {
			let description = unsafe { ffi::parse_slice(config.description, config.description_len)? };
			audio.description = Some(bytes::Bytes::copy_from_slice(description));
		}

		State::lock().publish.audio_config(broadcast, name, audio)
	})
}

/// Remove a video rendition from a broadcast's catalog by name.
///
/// Removes a rendition added by [moq_publish_video_config]. Any other name is a
/// no-op, including one a [moq_publish_video] track owns, which is retired by
/// [moq_publish_media_finish] instead. The updated catalog is published to
/// subscribers automatically.
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
/// Same rules as [moq_publish_video_remove].
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
/// This is the producer counterpart to [moq_consume_catalog_section] /
/// [moq_consume_catalog_section_at]: it writes an arbitrary top-level JSON key into the
/// catalog of a broadcast created with [moq_origin_publish], beyond the
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
pub unsafe extern "C" fn moq_publish_catalog_section_remove(
	broadcast: u32,
	name: *const c_char,
	name_len: usize,
) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let name = unsafe { ffi::parse_str(name, name_len)? };
		State::lock().publish.catalog_section_remove(broadcast, name)
	})
}

/// Create a raw track on a broadcast for arbitrary byte payloads.
///
/// Unlike [moq_publish_audio] and [moq_publish_video], this is the bare moq-net primitive: no
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

/// Create a raw group with an explicit sequence number.
///
/// Returns a non-zero group handle on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_track_group_at(track: u32, sequence: u64) -> i32 {
	ffi::enter(move || {
		let track = ffi::parse_id(track)?;
		State::lock().publish.track_group_at(track, sequence)
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
/// Takes `payload` then `timestamp_us`, matching [moq_publish_track_frame]. The payload must
/// be at most 1200 bytes. On success the datagram's per-track sequence number (shared with the
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
	payload: *const u8,
	payload_size: usize,
	timestamp_us: u64,
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
pub extern "C" fn moq_publish_track_finish(track: u32) -> i32 {
	ffi::enter(move || {
		let track = ffi::parse_id(track)?;
		State::lock().publish.track_finish(track)
	})
}

/// Declare a raw track's exclusive final group sequence.
///
/// Groups below `final_sequence` may still be created. Groups at or above it
/// are rejected. The track remains open for groups below the boundary. Call
/// [moq_publish_track_finish] after producing the remaining groups.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_track_finish_at(track: u32, final_sequence: u64) -> i32 {
	ffi::enter(move || {
		let track = ffi::parse_id(track)?;
		State::lock().publish.track_finish_at(track, final_sequence)
	})
}

/// Abort a raw track with an application error code.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_track_abort(track: u32, error_code: u16) -> i32 {
	ffi::enter(move || {
		let track = ffi::parse_id(track)?;
		State::lock().publish.track_abort(track, error_code)
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
pub extern "C" fn moq_publish_group_finish(group: u32) -> i32 {
	ffi::enter(move || {
		let group = ffi::parse_id(group)?;
		State::lock().publish.group_finish(group)
	})
}

/// Abort a raw group with an application error code.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_group_abort(group: u32, error_code: u16) -> i32 {
	ffi::enter(move || {
		let group = ffi::parse_id(group)?;
		State::lock().publish.group_abort(group, error_code)
	})
}

/// Create a JSON snapshot track (lossy latest-value) on a broadcast.
///
/// Values published via [moq_publish_json_snapshot_update] reach subscribers as a single latest
/// state; a late joiner only sees the newest. Advertise the track in the catalog with
/// [moq_publish_catalog_section] if consumers should discover it.
///
/// Returns a non-zero handle to the JSON producer on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure `name` is a valid pointer to `name_len` bytes and `config` a valid pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_publish_json_snapshot(
	broadcast: u32,
	name: *const c_char,
	name_len: usize,
	config: *const moq_json_snapshot_config,
) -> i32 {
	ffi::enter(move || {
		let broadcast = ffi::parse_id(broadcast)?;
		let name = unsafe { ffi::parse_str(name, name_len)? };
		let config = unsafe { config.as_ref() }.ok_or(Error::InvalidPointer)?;
		let mut producer = moq_json::snapshot::ProducerConfig::default();
		producer.delta_ratio = config.delta_ratio;
		producer.compression = config.compression;
		State::lock().publish.json_snapshot(broadcast, name, producer)
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
pub unsafe extern "C" fn moq_publish_json_snapshot_update(json: u32, value: *const c_char, value_len: usize) -> i32 {
	ffi::enter(move || {
		let json = ffi::parse_id(json)?;
		let value = unsafe { ffi::parse_slice(value.cast::<u8>(), value_len)? };
		let value = serde_json::from_slice(value)?;
		State::lock().publish.json_snapshot_update(json, value)
	})
}

/// Finish a JSON snapshot track. No more values can be published.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_publish_json_snapshot_finish(json: u32) -> i32 {
	ffi::enter(move || {
		let json = ffi::parse_id(json)?;
		State::lock().publish.json_snapshot_finish(json)
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
pub extern "C" fn moq_publish_json_stream_finish(stream: u32) -> i32 {
	ffi::enter(move || {
		let stream = ffi::parse_id(stream)?;
		State::lock().publish.json_stream_finish(stream)
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
/// The destination is filled with the video track information. `dst->container`
/// says how the track's frames are wrapped; skip a rendition whose kind is
/// `MOQ_CONTAINER_KIND_UNKNOWN`, since this build cannot parse it.
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

/// Query whether the publisher recommends temporarily avoiding a video rendition.
///
/// The track remains available. A false value also covers catalogs that omit the
/// optional field.
///
/// Returns zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `dst` points to properly aligned, writable storage for a `bool`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_video_stalled(catalog: u32, index: u32, dst: *mut bool) -> i32 {
	ffi::enter(move || {
		let catalog = ffi::parse_id(catalog)?;
		if dst.is_null() {
			return Err(Error::InvalidPointer);
		}

		let stalled = State::lock().consume.video_stalled(catalog, index as usize)?;
		unsafe { dst.write(stalled) };
		Ok(())
	})
}

/// Query the catalog properties shared by every video rendition.
///
/// The destination is filled by value and remains valid after the catalog snapshot is freed.
/// Inspect each `has_*` flag before reading its value.
///
/// Returns zero on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure that `dst` points to a valid [moq_video_properties].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_video_properties(catalog: u32, dst: *mut moq_video_properties) -> i32 {
	ffi::enter(move || {
		let catalog = ffi::parse_id(catalog)?;
		let dst = unsafe { dst.as_mut() }.ok_or(Error::InvalidPointer)?;
		State::lock().consume.video_properties(catalog, dst)
	})
}

/// Query information about an audio track in a catalog.
///
/// The destination is filled with the audio track information. `dst->container`
/// says how the track's frames are wrapped; skip a rendition whose kind is
/// `MOQ_CONTAINER_KIND_UNKNOWN`, since this build cannot parse it.
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
/// verbatim. Iterate them by index with [moq_consume_catalog_section_at], or look one up
/// directly by name with [moq_consume_catalog_section].
///
/// Returns the count (>= 0) on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_catalog_section_count(catalog: u32) -> i32 {
	ffi::enter(move || {
		let catalog = ffi::parse_id(catalog)?;
		State::lock().consume.catalog_section_count(catalog)
	})
}

/// Query an application catalog section by index, keyed by name.
///
/// Fills `dst` with the section's name and JSON value at `index`, in the range
/// `[0, moq_consume_catalog_section_count)`. Both pointers borrow the snapshot's storage
/// and stay valid until it is freed with [moq_consume_catalog_free].
///
/// Returns a zero on success, or a negative code on failure (e.g. `index` out of
/// range).
///
/// # Safety
/// - The caller must ensure that `dst` is a valid pointer to a [moq_section] struct.
/// - The caller must ensure that `dst` is not used after [moq_consume_catalog_free] is called.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_catalog_section_at(catalog: u32, index: u32, dst: *mut moq_section) -> i32 {
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
pub unsafe extern "C" fn moq_consume_catalog_section(
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
/// - `max_age_ms` controls the maximum amount of buffering allowed before skipping a GoP.
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
pub unsafe extern "C" fn moq_consume_video(
	catalog: u32,
	index: u32,
	max_age_ms: u64,
	on_frame: Option<extern "C" fn(user_data: *mut c_void, frame: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let catalog = ffi::parse_id(catalog)?;
		let index = index as usize;
		let max_age = std::time::Duration::from_millis(max_age_ms);
		let on_frame = unsafe { ffi::OnStatus::new(user_data, on_frame) };
		State::lock().consume.video(catalog, index, max_age, on_frame)
	})
}

/// Stop a video track consumer's background task.
///
/// Returns immediately: zero on success, or a negative code if already closed.
/// Does NOT free `user_data`; the [moq_consume_video] `on_frame` callback
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
/// The `max_age_ms` parameter controls how long to wait before skipping frames.
///
/// Returns a non-zero handle to the track on success, or a negative code on failure.
///
/// # Safety
/// - The caller must keep `user_data` valid until the terminal (`<= 0`) `on_frame` callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_audio(
	catalog: u32,
	index: u32,
	max_age_ms: u64,
	on_frame: Option<extern "C" fn(user_data: *mut c_void, frame: i32)>,
	user_data: *mut c_void,
) -> i32 {
	ffi::enter(move || {
		let catalog = ffi::parse_id(catalog)?;
		let index = index as usize;
		let max_age = std::time::Duration::from_millis(max_age_ms);
		let on_frame = unsafe { ffi::OnStatus::new(user_data, on_frame) };
		State::lock().consume.audio(catalog, index, max_age, on_frame)
	})
}

/// Stop an audio track consumer's background task.
///
/// Returns immediately: zero on success, or a negative code if already closed.
/// Does NOT free `user_data`; the [moq_consume_audio] `on_frame` callback
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
/// `dst.payload_size` in one call. The pointer is valid until [`moq_consume_frame_free`]
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

/// Free a decoded frame delivered via a [moq_consume_video] or [moq_consume_audio] callback.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_frame_free(frame: u32) -> i32 {
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
/// release it with [moq_consume_track_frame_free]. Pass NULL for `subscription`
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
/// frame is released with [moq_consume_frame_free]. `dst.timestamp_us` is the
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

/// Free a raw frame delivered via the [moq_consume_track] callback, releasing its payload.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_track_frame_free(frame: u32) -> i32 {
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
/// remain valid until released with [moq_consume_track_frame_free].
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
/// it with [moq_consume_datagram_free]. Datagrams arrive only over datagram-capable
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
/// [moq_consume_datagram_free]), plus `dst.timestamp_us` and `dst.sequence`.
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

/// Free a datagram delivered via the [moq_consume_datagrams] callback, releasing its payload.
///
/// Returns a zero on success, or a negative code on failure.
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_datagram_free(datagram: u32) -> i32 {
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
/// already delivered via the callback remain valid until released with [moq_consume_datagram_free].
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
/// release it with [moq_consume_json_value_free]. Pass the same compression the producer used.
///
/// Returns a non-zero handle to the task on success, or a negative code on failure.
///
/// # Safety
/// - The caller must ensure `name` is a valid pointer to `name_len` bytes and `config` a valid pointer.
/// - The caller must keep `user_data` valid until the terminal (`<= 0`) `on_value` callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn moq_consume_json_snapshot(
	broadcast: u32,
	name: *const c_char,
	name_len: usize,
	config: *const moq_json_snapshot_config,
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
		State::lock().consume.json_snapshot(broadcast, name, consumer, on_value)
	})
}

/// Subscribe to a JSON stream track (lossless append-log) by name.
///
/// `on_value` is called with a positive value ID for each record, in order, then once more with
/// a terminal `0` or negative error where `user_data` should be released. Read each value with
/// [moq_consume_json_value] and release it with [moq_consume_json_value_free].
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

/// Read a JSON value delivered via a [moq_consume_json_snapshot] or [moq_consume_json_stream] callback.
///
/// Fills `dst.json` / `dst.json_len`; the pointer is valid until the value is released with
/// [moq_consume_json_value_free].
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
pub extern "C" fn moq_consume_json_value_free(value: u32) -> i32 {
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
/// until released with [moq_consume_json_value_free].
#[unsafe(no_mangle)]
pub extern "C" fn moq_consume_json_close(task: u32) -> i32 {
	ffi::enter(move || {
		let task = ffi::parse_id(task)?;
		State::lock().consume.json_close(task)
	})
}
