use super::*;

use crate::ffi::ReturnCode;
use std::ffi::{c_char, c_void};
use std::sync::mpsc;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

/// Convert a positive `i32` return value to `u32`, panicking on error.
fn id(raw: i32) -> u32 {
	assert!(raw > 0, "expected positive id, got {raw}");
	raw as u32
}

/// Create a live broadcast at `path` on `origin` via `moq_origin_publish`.
fn publish_broadcast(origin: u32, path: &[u8]) -> u32 {
	id(unsafe { moq_origin_publish(origin, path.as_ptr() as *const c_char, path.len()) })
}

/// Request a published broadcast via `moq_origin_request` and return its handle.
///
/// A broadcast created with `moq_origin_publish` becomes visible asynchronously, so an
/// early request can race the attach and fail as unroutable; retry until the deadline.
fn request_broadcast(origin: u32, path: &[u8]) -> u32 {
	let deadline = std::time::Instant::now() + TIMEOUT;
	loop {
		let cb = Callback::new();
		let _task = id(unsafe {
			moq_origin_request(
				origin,
				path.as_ptr() as *const c_char,
				path.len(),
				Some(channel_callback),
				cb.ptr,
			)
		});
		let code = cb.recv();
		if code > 0 {
			cb.recv_terminal();
			return code as u32;
		}
		// A failed request already delivered its terminal code; back off and retry.
		assert!(
			std::time::Instant::now() < deadline,
			"timed out requesting broadcast: {code}"
		);
		std::thread::sleep(Duration::from_millis(10));
	}
}

/// RAII guard that calls a closure on drop.
struct Guard<F: FnOnce()>(Option<F>);
impl<F: FnOnce()> Drop for Guard<F> {
	fn drop(&mut self) {
		if let Some(f) = self.0.take() {
			f();
		}
	}
}

/// Heap-allocated callback sender with RAII cleanup.
struct Callback {
	rx: mpsc::Receiver<i32>,
	ptr: *mut c_void,
}

impl Callback {
	fn new() -> Self {
		let (tx, rx) = mpsc::channel();
		let ptr = Box::into_raw(Box::new(tx)) as *mut c_void;
		Self { rx, ptr }
	}

	fn recv(&self) -> i32 {
		self.rx.recv_timeout(TIMEOUT).expect("callback timed out")
	}

	/// Wait for the terminal callback (code <= 0) the task delivers after close
	/// or stream end. Must be drained before the Callback (user_data) drops,
	/// since user_data must outlive the final callback.
	fn recv_terminal(&self) -> i32 {
		let code = self.recv();
		assert!(code <= 0, "expected terminal code <= 0, got {code}");
		code
	}

	/// Like [`recv_terminal`](Self::recv_terminal), but first drains any mid-stream catalog
	/// snapshot ids, freeing each. Auto-detected metrics (jitter, bitrate) republish the catalog
	/// while frames flow, so the callback delivers extra snapshots before the terminal.
	fn recv_catalog_terminal(&self) -> i32 {
		loop {
			let code = self.recv();
			if code <= 0 {
				return code;
			}
			assert_eq!(moq_consume_catalog_free(id(code)), 0);
		}
	}
}

impl Drop for Callback {
	fn drop(&mut self) {
		unsafe { drop(Box::from_raw(self.ptr as *mut mpsc::Sender<i32>)) };
	}
}

/// FFI callback that forwards the status code through an `mpsc::Sender`.
extern "C" fn channel_callback(user_data: *mut c_void, code: i32) {
	let tx = unsafe { &*(user_data as *const mpsc::Sender<i32>) };
	let _ = tx.send(code);
}

/// Build a valid OpusHead init buffer (RFC 7845 §5.1).
fn opus_head() -> Vec<u8> {
	let mut head = Vec::with_capacity(19);
	head.extend_from_slice(b"OpusHead");
	head.push(1); // version
	head.push(2); // channel count (stereo)
	head.extend_from_slice(&0u16.to_le_bytes()); // pre-skip
	head.extend_from_slice(&48000u32.to_le_bytes()); // sample rate
	head.extend_from_slice(&0u16.to_le_bytes()); // output gain
	head.push(0); // channel mapping family
	head
}

/// H.264 Annex B init with SPS + PPS extracted from Big Buck Bunny (1280x720, High profile, Level 3.1).
fn h264_init() -> Vec<u8> {
	let mut init = Vec::new();
	init.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
	init.extend_from_slice(&[
		0x67, 0x64, 0x00, 0x1f, 0xac, 0x24, 0x84, 0x01, 0x40, 0x16, 0xec, 0x04, 0x40, 0x00, 0x00, 0x03, 0x00, 0x40,
		0x00, 0x00, 0x0c, 0x23, 0xc6, 0x0c, 0x92,
	]);
	init.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
	init.extend_from_slice(&[0x68, 0xee, 0x32, 0xc8, 0xb0]);
	init
}

fn label_ptr(label: Option<&[u8]>) -> (*const c_char, usize) {
	label
		.map(|label| (label.as_ptr() as *const c_char, label.len()))
		.unwrap_or((std::ptr::null(), 0))
}

fn init_ptr(init: &[u8]) -> *const u8 {
	if init.is_empty() {
		std::ptr::null()
	} else {
		init.as_ptr()
	}
}

fn publish_audio(broadcast: u32, format: moq_audio_format, init: &[u8], label: Option<&[u8]>) -> i32 {
	let (label, label_len) = label_ptr(label);
	let config = moq_audio_init {
		format: format as u32,
		init: init_ptr(init),
		init_len: init.len(),
		label,
		label_len,
	};

	unsafe { moq_publish_audio(broadcast, &config) }
}

fn publish_video(broadcast: u32, format: moq_video_format, init: &[u8], label: Option<&[u8]>) -> i32 {
	let (label, label_len) = label_ptr(label);
	let config = moq_video_init {
		format: format as u32,
		init: init_ptr(init),
		init_len: init.len(),
		label,
		label_len,
	};

	unsafe { moq_publish_video(broadcast, &config) }
}

fn publish_container(broadcast: u32, format: moq_container_format, init: &[u8]) -> i32 {
	let config = moq_container_init {
		format: format as u32,
		init: init_ptr(init),
		init_len: init.len(),
	};

	unsafe { moq_publish_container(broadcast, &config) }
}

#[test]
fn origin_lifecycle() {
	let origin = id(moq_origin_create());
	assert_eq!(moq_origin_close(origin), 0, "moq_origin_close should succeed");
	assert!(moq_origin_close(origin) < 0, "double-close should fail");
}

#[test]
fn last_error_reports_reason() {
	// A failed call records a retrievable reason string for moq_error().
	assert!(moq_origin_close(9999) < 0);
	let ptr = moq_error();
	assert!(!ptr.is_null(), "expected a recorded error message");
	let msg = unsafe { std::ffi::CStr::from_ptr(ptr) }.to_str().unwrap();
	assert_eq!(msg, "origin not found");
}

#[test]
fn last_error_set_before_callback() {
	use crate::Error;
	use crate::ffi::OnStatus;

	// A binding reads moq_error() from inside the callback; the reason for a
	// negative status must already be recorded by the time the callback runs.
	extern "C" fn capture(user_data: *mut c_void, code: i32) {
		assert!(code < 0, "expected a negative status, got {code}");
		let slot = unsafe { &mut *(user_data as *mut Option<String>) };
		let ptr = moq_error();
		*slot = (!ptr.is_null()).then(|| unsafe { std::ffi::CStr::from_ptr(ptr) }.to_str().unwrap().to_owned());
	}

	let mut captured: Option<String> = None;
	let cb = unsafe { OnStatus::new(&mut captured as *mut _ as *mut c_void, Some(capture)) };
	cb.call(Err::<(), Error>(Error::OriginNotFound));

	assert_eq!(captured.as_deref(), Some("origin not found"));
}

#[test]
fn publish_media_lifecycle() {
	let origin = id(moq_origin_create());
	let broadcast = publish_broadcast(origin, b"publish-media-lifecycle");
	let _guard = Guard(Some(|| {
		moq_publish_finish(broadcast);
	}));

	let init = opus_head();
	let format = moq_audio_format::MOQ_AUDIO_FORMAT_OPUS;
	let media = id(publish_audio(broadcast, format, &init, None));

	let payload = b"opus frame";
	let ret = unsafe { moq_publish_media_frame(media, payload.as_ptr(), payload.len(), 1000) };
	assert_eq!(ret, 0, "moq_publish_media_frame should succeed");

	assert_eq!(moq_publish_media_finish(media), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
}

#[test]
fn publish_media_rejects_a_null_config() {
	let origin = id(moq_origin_create());
	let broadcast = publish_broadcast(origin, b"publish-media-null-config");

	assert!(unsafe { moq_publish_audio(broadcast, std::ptr::null()) } < 0);

	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

/// A container gets its own handle space, so a handle from one entry point cannot be fed to the
/// other's calls. That is what stops a container from being handed a frame timestamp it would drop.
#[test]
fn container_and_media_handles_are_not_interchangeable() {
	let origin = id(moq_origin_create());
	let broadcast = publish_broadcast(origin, b"publish-container-handles");

	let container = id(publish_container(
		broadcast,
		moq_container_format::MOQ_CONTAINER_FORMAT_FMP4,
		&[],
	));
	let media = id(publish_audio(
		broadcast,
		moq_audio_format::MOQ_AUDIO_FORMAT_OPUS,
		&opus_head(),
		None,
	));

	// Each handle is rejected by the other's calls rather than acted on.
	assert!(
		moq_publish_media_finish(container) < 0,
		"a container is not a media track"
	);
	assert!(
		moq_publish_container_finish(media) < 0,
		"a media track is not a container"
	);

	assert_eq!(moq_publish_container_finish(container), 0);
	assert_eq!(moq_publish_media_finish(media), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

fn borrowed_string(ptr: *const c_char, len: usize) -> Option<String> {
	if ptr.is_null() {
		return None;
	}

	Some(
		unsafe { std::str::from_utf8(std::slice::from_raw_parts(ptr.cast::<u8>(), len)) }
			.unwrap()
			.to_string(),
	)
}

/// A label describes the rendition without changing its generated track name.
/// Duplicate labels remain valid because the transport identifiers stay unique.
#[test]
fn publish_media_labels_config_without_naming_track() {
	let origin = id(moq_origin_create());
	let path = b"labeled-track";
	let broadcast = publish_broadcast(origin, path);

	let init = opus_head();
	let format = moq_audio_format::MOQ_AUDIO_FORMAT_OPUS;
	let label = b"English";

	let media1 = id(publish_audio(broadcast, format, &init, Some(label)));

	let consume = request_broadcast(origin, path);
	let catalog_cb = Callback::new();
	let catalog_task = id(unsafe { moq_consume_catalog(consume, Some(channel_callback), catalog_cb.ptr) });
	let catalog_id1 = id(catalog_cb.recv());

	let mut audio_cfg = moq_audio_config {
		name: std::ptr::null(),
		name_len: 0,
		label: std::ptr::null(),
		label_len: 0,
		codec: std::ptr::null(),
		codec_len: 0,
		description: std::ptr::null(),
		description_len: 0,
		sample_rate: 0,
		channel_count: 0,
		container: moq_container::default(),
	};
	assert_eq!(unsafe { moq_consume_audio_config(catalog_id1, 0, &mut audio_cfg) }, 0);
	assert_eq!(
		borrowed_string(audio_cfg.name, audio_cfg.name_len).as_deref(),
		Some("0.opus")
	);
	assert_eq!(
		borrowed_string(audio_cfg.label, audio_cfg.label_len).as_deref(),
		Some("English")
	);

	let media2 = id(publish_audio(broadcast, format, &init, Some(label)));

	let catalog_id2 = id(catalog_cb.recv());
	assert_eq!(unsafe { moq_consume_audio_config(catalog_id2, 0, &mut audio_cfg) }, 0);
	assert_eq!(
		borrowed_string(audio_cfg.name, audio_cfg.name_len).as_deref(),
		Some("0.opus")
	);
	assert_eq!(
		borrowed_string(audio_cfg.label, audio_cfg.label_len).as_deref(),
		Some("English")
	);
	assert_eq!(unsafe { moq_consume_audio_config(catalog_id2, 1, &mut audio_cfg) }, 0);
	assert_eq!(
		borrowed_string(audio_cfg.name, audio_cfg.name_len).as_deref(),
		Some("1.opus")
	);
	assert_eq!(
		borrowed_string(audio_cfg.label, audio_cfg.label_len).as_deref(),
		Some("English")
	);

	assert_eq!(moq_consume_catalog_free(catalog_id1), 0);
	assert_eq!(moq_consume_catalog_free(catalog_id2), 0);
	assert_eq!(moq_consume_catalog_close(catalog_task), 0);
	assert_eq!(catalog_cb.recv_terminal(), 0, "catalog close delivers terminal 0");
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_media_finish(media1), 0);
	assert_eq!(moq_publish_media_finish(media2), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn publish_catalog_config_invalid_broadcast() {
	let name = "video";
	let codec = "vp8";
	let video = moq_video_config {
		name: name.as_ptr() as *const c_char,
		name_len: name.len(),
		label: std::ptr::null(),
		label_len: 0,
		codec: codec.as_ptr() as *const c_char,
		codec_len: codec.len(),
		description: std::ptr::null(),
		description_len: 0,
		coded_width: 0,
		coded_height: 0,
		container: moq_container::default(),
	};
	assert!(unsafe { moq_publish_video_config(0, &video) } < 0);
	assert!(unsafe { moq_publish_video_properties(0, &moq_video_properties::default()) } < 0);

	let audio_codec = "opus";
	let audio = moq_audio_config {
		name: name.as_ptr() as *const c_char,
		name_len: name.len(),
		label: std::ptr::null(),
		label_len: 0,
		codec: audio_codec.as_ptr() as *const c_char,
		codec_len: audio_codec.len(),
		description: std::ptr::null(),
		description_len: 0,
		sample_rate: 48000,
		channel_count: 2,
		container: moq_container::default(),
	};
	assert!(unsafe { moq_publish_audio_config(0, &audio) } < 0);

	assert!(unsafe { moq_publish_video_remove(0, name.as_ptr() as *const c_char, name.len()) } < 0);
	assert!(unsafe { moq_publish_audio_remove(0, name.as_ptr() as *const c_char, name.len()) } < 0);
}

/// An avc3 track has no catalog rendition until its first SPS arrives, but it owns the name from
/// the moment it's published. Writing into that gap used to succeed, then get overwritten by the
/// importer and deleted when the media handle finished, silently taking the caller's entry with it.
#[test]
fn publish_media_owns_its_rendition_before_the_first_keyframe() {
	let origin = id(moq_origin_create());
	let broadcast = publish_broadcast(origin, b"media-owns-its-rendition");

	// Annex-B with an empty init, so the parameter sets arrive in band and the importer publishes
	// nothing until a keyframe lands.
	let media = id(publish_video(
		broadcast,
		moq_video_format::MOQ_VIDEO_FORMAT_AVC3,
		&[],
		None,
	));

	let name = "0.avc3";
	let codec = "avc1.42c01e";
	let video = moq_video_config {
		name: name.as_ptr() as *const c_char,
		name_len: name.len(),
		label: std::ptr::null(),
		label_len: 0,
		codec: codec.as_ptr() as *const c_char,
		codec_len: codec.len(),
		description: std::ptr::null(),
		description_len: 0,
		coded_width: 0,
		coded_height: 0,
		container: moq_container::default(),
	};
	assert_eq!(
		unsafe { moq_publish_video_config(broadcast, &video) },
		-18,
		"the media track owns 0.avc3 even before its config resolves"
	);
	assert_eq!(
		unsafe { moq_publish_video_remove(broadcast, name.as_ptr() as *const c_char, name.len()) },
		0,
		"removing a name the caller never authored is a no-op"
	);
	assert_eq!(
		unsafe { moq_publish_video_config(broadcast, &video) },
		-18,
		"and the no-op left the media track's rendition alone"
	);

	// Once the media handle is gone the name is free again.
	assert_eq!(moq_publish_media_finish(media), 0);
	assert_eq!(
		unsafe { moq_publish_video_config(broadcast, &video) },
		0,
		"finishing the media track releases its rendition name"
	);

	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

/// A caller owns the renditions it authored, so re-declaring one refines it in place rather than
/// failing, and removing one actually retires it.
#[test]
fn publish_video_config_replaces_its_own_rendition() {
	let origin = id(moq_origin_create());
	let broadcast = publish_broadcast(origin, b"catalog-config-replace");

	let name = "authored";
	let codec = "vp8";
	let mut video = moq_video_config {
		name: name.as_ptr() as *const c_char,
		name_len: name.len(),
		label: std::ptr::null(),
		label_len: 0,
		codec: codec.as_ptr() as *const c_char,
		codec_len: codec.len(),
		description: std::ptr::null(),
		description_len: 0,
		coded_width: 640,
		coded_height: 360,
		container: moq_container::default(),
	};

	assert_eq!(unsafe { moq_publish_video_config(broadcast, &video) }, 0);
	video.coded_width = 1920;
	video.coded_height = 1080;
	assert_eq!(
		unsafe { moq_publish_video_config(broadcast, &video) },
		0,
		"a caller can refine a rendition it owns"
	);

	assert_eq!(
		unsafe { moq_publish_video_remove(broadcast, name.as_ptr() as *const c_char, name.len()) },
		0
	);
	assert_eq!(
		unsafe { moq_publish_video_config(broadcast, &video) },
		0,
		"the name is free once the caller removes its rendition"
	);

	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn publish_catalog_config_null_pointer() {
	let origin = id(moq_origin_create());
	let broadcast = publish_broadcast(origin, b"publish-catalog-config-null-pointer");
	assert_eq!(
		unsafe { moq_publish_video_config(broadcast, std::ptr::null()) },
		-6,
		"null config should return InvalidPointer (-6)"
	);
	assert_eq!(
		unsafe { moq_publish_video_properties(broadcast, std::ptr::null()) },
		-6,
		"null properties should return InvalidPointer (-6)"
	);
	assert_eq!(
		unsafe { moq_publish_audio_config(broadcast, std::ptr::null()) },
		-6,
		"null config should return InvalidPointer (-6)"
	);
	assert_eq!(moq_publish_finish(broadcast), 0);
}

#[test]
fn publish_catalog_roundtrip() {
	let origin = id(moq_origin_create());
	let path = b"catalog-producer";
	let broadcast = publish_broadcast(origin, path);

	// Author the catalog directly instead of via moq_publish_video.
	let video_name = "video";
	let video_label = "Main camera";
	let video_codec = "vp8";
	let width: u32 = 1920;
	let height: u32 = 1080;
	let description: &[u8] = &[0x01, 0x02, 0x03];
	let video = moq_video_config {
		name: video_name.as_ptr() as *const c_char,
		name_len: video_name.len(),
		label: video_label.as_ptr() as *const c_char,
		label_len: video_label.len(),
		codec: video_codec.as_ptr() as *const c_char,
		codec_len: video_codec.len(),
		description: description.as_ptr(),
		description_len: description.len(),
		coded_width: width,
		coded_height: height,
		container: moq_container::default(),
	};
	assert_eq!(unsafe { moq_publish_video_config(broadcast, &video) }, 0);
	let stalled_video_name = "video-stalled";
	let stalled_video = moq_video_config {
		name: stalled_video_name.as_ptr() as *const c_char,
		name_len: stalled_video_name.len(),
		label: std::ptr::null(),
		label_len: 0,
		codec: video_codec.as_ptr() as *const c_char,
		codec_len: video_codec.len(),
		description: description.as_ptr(),
		description_len: description.len(),
		coded_width: width,
		coded_height: height,
		container: moq_container::default(),
	};
	assert_eq!(unsafe { moq_publish_video_config(broadcast, &stalled_video) }, 0);
	{
		let mut state = State::lock();
		let (_, catalog) = state.publish.pair_mut(Id::try_from(broadcast).unwrap()).unwrap();
		catalog
			.lock()
			.video
			.renditions
			.get_mut(stalled_video_name)
			.unwrap()
			.stalled = Some(true);
	}
	let properties = moq_video_properties {
		display_width: 1080,
		display_height: 1920,
		has_display: true,
		rotation: 315.0,
		has_rotation: true,
		flip: true,
		has_flip: true,
	};
	assert_eq!(unsafe { moq_publish_video_properties(broadcast, &properties) }, 0);

	let audio_name = "audio";
	let audio_label = "English";
	let audio_codec = "opus";
	let audio = moq_audio_config {
		name: audio_name.as_ptr() as *const c_char,
		name_len: audio_name.len(),
		label: audio_label.as_ptr() as *const c_char,
		label_len: audio_label.len(),
		codec: audio_codec.as_ptr() as *const c_char,
		codec_len: audio_codec.len(),
		description: std::ptr::null(),
		description_len: 0,
		sample_rate: 48000,
		channel_count: 2,
		container: moq_container::default(),
	};
	assert_eq!(unsafe { moq_publish_audio_config(broadcast, &audio) }, 0);

	// Consume the broadcast to verify the catalog round-trips.
	let consume = request_broadcast(origin, path);
	let catalog_cb = Callback::new();
	let catalog_task = id(unsafe { moq_consume_catalog(consume, Some(channel_callback), catalog_cb.ptr) });
	let catalog_id = id(catalog_cb.recv());

	// The video rendition we authored comes back through the consume API.
	let mut video_cfg = moq_video_config {
		name: std::ptr::null(),
		name_len: 0,
		label: std::ptr::null(),
		label_len: 0,
		codec: std::ptr::null(),
		codec_len: 0,
		description: std::ptr::null(),
		description_len: 0,
		coded_width: 0,
		coded_height: 0,
		container: moq_container::default(),
	};
	assert_eq!(unsafe { moq_consume_video_config(catalog_id, 0, &mut video_cfg) }, 0);
	let codec = unsafe {
		std::str::from_utf8(std::slice::from_raw_parts(
			video_cfg.codec.cast::<u8>(),
			video_cfg.codec_len,
		))
	}
	.unwrap();
	assert_eq!(codec, "vp8");
	assert_eq!(
		borrowed_string(video_cfg.label, video_cfg.label_len).as_deref(),
		Some("Main camera")
	);
	assert_eq!(video_cfg.coded_width, 1920);
	assert_eq!(video_cfg.coded_height, 1080);
	let mut stalled = std::mem::MaybeUninit::<bool>::uninit();
	assert_eq!(
		unsafe { moq_consume_video_stalled(catalog_id, 0, stalled.as_mut_ptr()) },
		0
	);
	assert!(!unsafe { stalled.assume_init() });

	let mut stalled = std::mem::MaybeUninit::<bool>::uninit();
	assert_eq!(
		unsafe { moq_consume_video_stalled(catalog_id, 1, stalled.as_mut_ptr()) },
		0
	);
	assert!(unsafe { stalled.assume_init() });
	assert_eq!(
		unsafe { moq_consume_video_stalled(catalog_id, 0, std::ptr::null_mut()) },
		-6,
		"null stalled pointer should return InvalidPointer (-6)"
	);
	assert_eq!(
		unsafe {
			moq_publish_video_remove(
				broadcast,
				stalled_video_name.as_ptr() as *const c_char,
				stalled_video_name.len(),
			)
		},
		0
	);
	let active_catalog_id = id(catalog_cb.recv());

	let mut properties = moq_video_properties::default();
	assert_eq!(unsafe { moq_consume_video_properties(catalog_id, &mut properties) }, 0);
	assert!(properties.has_display);
	assert_eq!(properties.display_width, 1080);
	assert_eq!(properties.display_height, 1920);
	assert!(properties.has_rotation);
	assert_eq!(properties.rotation, 0.0);
	assert!(properties.has_flip);
	assert!(properties.flip);

	// And so does the audio rendition.
	let mut audio_cfg = moq_audio_config {
		name: std::ptr::null(),
		name_len: 0,
		label: std::ptr::null(),
		label_len: 0,
		codec: std::ptr::null(),
		codec_len: 0,
		description: std::ptr::null(),
		description_len: 0,
		sample_rate: 0,
		channel_count: 0,
		container: moq_container::default(),
	};
	assert_eq!(unsafe { moq_consume_audio_config(catalog_id, 0, &mut audio_cfg) }, 0);
	assert_eq!(
		borrowed_string(audio_cfg.label, audio_cfg.label_len).as_deref(),
		Some("English")
	);
	assert_eq!(audio_cfg.sample_rate, 48000);
	assert_eq!(audio_cfg.channel_count, 2);

	// Removing the video rendition republishes a catalog without it.
	assert_eq!(
		unsafe { moq_publish_video_remove(broadcast, video_name.as_ptr() as *const c_char, video_name.len()) },
		0
	);
	let catalog_id2 = id(catalog_cb.recv());
	assert!(
		unsafe { moq_consume_video_config(catalog_id2, 0, &mut video_cfg) } < 0,
		"video rendition should be gone after remove"
	);
	assert_eq!(unsafe { moq_consume_audio_config(catalog_id2, 0, &mut audio_cfg) }, 0);

	assert_eq!(moq_consume_catalog_free(catalog_id), 0);
	assert_eq!(moq_consume_catalog_free(active_catalog_id), 0);
	assert_eq!(moq_consume_catalog_free(catalog_id2), 0);
	assert_eq!(moq_consume_catalog_close(catalog_task), 0);
	assert_eq!(catalog_cb.recv_terminal(), 0, "catalog close delivers terminal 0");
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

/// hang carries coded_width and coded_height as independent options, so a catalog
/// that declares only one must survive a consume/publish round trip. Collapsing
/// them behind one presence flag would invent a zero for the missing half.
#[test]
fn a_half_specified_coded_size_round_trips() {
	let origin = id(moq_origin_create());
	let path = b"half-coded-size";
	let broadcast = publish_broadcast(origin, path);

	let name = "video";
	let codec = "vp8";
	let mut video = moq_video_config {
		name: name.as_ptr() as *const c_char,
		name_len: name.len(),
		label: std::ptr::null(),
		label_len: 0,
		codec: codec.as_ptr() as *const c_char,
		codec_len: codec.len(),
		description: std::ptr::null(),
		description_len: 0,
		coded_width: 1920,
		coded_height: 0, // absent, not "zero pixels tall"
		container: moq_container::default(),
	};
	assert_eq!(unsafe { moq_publish_video_config(broadcast, &video) }, 0);

	let consume = request_broadcast(origin, path);
	let catalog_cb = Callback::new();
	let catalog_task = id(unsafe { moq_consume_catalog(consume, Some(channel_callback), catalog_cb.ptr) });
	let catalog = id(catalog_cb.recv());

	let mut read = moq_video_config {
		name: std::ptr::null(),
		name_len: 0,
		label: std::ptr::null(),
		label_len: 0,
		codec: std::ptr::null(),
		codec_len: 0,
		description: std::ptr::null(),
		description_len: 0,
		coded_width: 0,
		coded_height: 0,
		container: moq_container::default(),
	};
	assert_eq!(unsafe { moq_consume_video_config(catalog, 0, &mut read) }, 0);
	assert_eq!(read.coded_width, 1920);
	assert_eq!(read.coded_height, 0, "the absent height must not come back invented");

	// Forwarding what we read into another broadcast carries the half-specified
	// size across unchanged, which is the case a shared presence flag would break
	// by inventing a zero height.
	let forward = publish_broadcast(origin, b"half-coded-size-forwarded");
	video.coded_width = read.coded_width;
	video.coded_height = read.coded_height;
	assert_eq!(unsafe { moq_publish_video_config(forward, &video) }, 0);

	let forwarded = request_broadcast(origin, b"half-coded-size-forwarded");
	let forwarded_cb = Callback::new();
	let forwarded_task = id(unsafe { moq_consume_catalog(forwarded, Some(channel_callback), forwarded_cb.ptr) });
	let forwarded_catalog = id(forwarded_cb.recv());
	assert_eq!(unsafe { moq_consume_video_config(forwarded_catalog, 0, &mut read) }, 0);
	assert_eq!(read.coded_width, 1920);
	assert_eq!(read.coded_height, 0, "forwarding must not invent the absent height");

	assert_eq!(moq_consume_catalog_free(forwarded_catalog), 0);
	assert_eq!(moq_consume_catalog_close(forwarded_task), 0);
	assert_eq!(forwarded_cb.recv_catalog_terminal(), 0);
	assert_eq!(moq_consume_close(forwarded), 0);
	assert_eq!(moq_publish_finish(forward), 0);

	assert_eq!(moq_consume_catalog_free(catalog), 0);
	assert_eq!(moq_consume_catalog_close(catalog_task), 0);
	assert_eq!(catalog_cb.recv_catalog_terminal(), 0);
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn raw_loc_video_uses_the_declared_catalog_container() {
	let origin = id(moq_origin_create());
	let path = b"raw-loc-video";
	let broadcast = publish_broadcast(origin, path);

	let name = b"video";
	let track =
		id(unsafe { moq_publish_track(broadcast, name.as_ptr() as *const c_char, name.len(), std::ptr::null()) });

	let codec = b"vp8";
	let video = moq_video_config {
		name: name.as_ptr() as *const c_char,
		name_len: name.len(),
		label: std::ptr::null(),
		label_len: 0,
		codec: codec.as_ptr() as *const c_char,
		codec_len: codec.len(),
		description: std::ptr::null(),
		description_len: 0,
		coded_width: 0,
		coded_height: 0,
		container: moq_container {
			kind: moq_container_kind::MOQ_CONTAINER_KIND_LOC as u32,
			init: std::ptr::null(),
			init_len: 0,
		},
	};
	assert_eq!(unsafe { moq_publish_video_config(broadcast, &video) }, 0);

	let consume = request_broadcast(origin, path);
	let catalog_cb = Callback::new();
	let catalog_task = id(unsafe { moq_consume_catalog(consume, Some(channel_callback), catalog_cb.ptr) });
	let catalog = id(catalog_cb.recv());

	// The declaration survives the round trip, so a consumer knows to parse LOC.
	let mut video_cfg = moq_video_config {
		name: std::ptr::null(),
		name_len: 0,
		label: std::ptr::null(),
		label_len: 0,
		codec: std::ptr::null(),
		codec_len: 0,
		description: std::ptr::null(),
		description_len: 0,
		coded_width: 0,
		coded_height: 0,
		container: moq_container::default(),
	};
	assert_eq!(unsafe { moq_consume_video_config(catalog, 0, &mut video_cfg) }, 0);
	assert_eq!(
		video_cfg.container.kind,
		moq_container_kind::MOQ_CONTAINER_KIND_LOC as u32
	);
	assert!(video_cfg.container.init.is_null());

	let frame_cb = Callback::new();
	let consumer = id(unsafe { moq_consume_video(catalog, 0, 10_000, Some(channel_callback), frame_cb.ptr) });

	let timestamp_us = 42_000;
	let payload = b"codec frame";
	let loc = moq_loc::encode(timestamp_us, payload).unwrap();
	let group = id(moq_publish_track_group(track));
	assert_eq!(
		unsafe { moq_publish_group_frame(group, loc.as_ptr(), loc.len(), timestamp_us) },
		0
	);
	assert_eq!(moq_publish_group_finish(group), 0);

	let frame_id = id(frame_cb.recv());
	let mut frame = moq_frame {
		payload: std::ptr::null(),
		payload_size: 0,
		timestamp_us: 0,
		keyframe: false,
	};
	assert_eq!(unsafe { moq_consume_frame(frame_id, &mut frame) }, 0);
	assert_eq!(frame.timestamp_us, timestamp_us);
	assert!(frame.keyframe);
	assert_eq!(
		unsafe { std::slice::from_raw_parts(frame.payload, frame.payload_size) },
		payload
	);

	assert_eq!(moq_consume_frame_free(frame_id), 0);
	assert_eq!(moq_consume_video_close(consumer), 0);
	assert_eq!(frame_cb.recv_terminal(), 0);
	assert_eq!(moq_consume_catalog_free(catalog), 0);
	assert_eq!(moq_consume_catalog_close(catalog_task), 0);
	assert_eq!(catalog_cb.recv_terminal(), 0);
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_track_finish(track), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn cmaf_catalog_container_carries_its_init_segment() {
	let origin = id(moq_origin_create());
	let path = b"cmaf-container";
	let broadcast = publish_broadcast(origin, path);

	let name = "audio";
	let codec = "opus";
	let init: &[u8] = &[0x00, 0x01, 0x02, 0x03];
	let audio = moq_audio_config {
		name: name.as_ptr() as *const c_char,
		name_len: name.len(),
		label: std::ptr::null(),
		label_len: 0,
		codec: codec.as_ptr() as *const c_char,
		codec_len: codec.len(),
		description: std::ptr::null(),
		description_len: 0,
		sample_rate: 48000,
		channel_count: 2,
		container: moq_container {
			kind: moq_container_kind::MOQ_CONTAINER_KIND_CMAF as u32,
			init: init.as_ptr(),
			init_len: init.len(),
		},
	};
	assert_eq!(unsafe { moq_publish_audio_config(broadcast, &audio) }, 0);

	let consume = request_broadcast(origin, path);
	let catalog_cb = Callback::new();
	let catalog_task = id(unsafe { moq_consume_catalog(consume, Some(channel_callback), catalog_cb.ptr) });
	let catalog = id(catalog_cb.recv());

	let mut audio_cfg = moq_audio_config {
		name: std::ptr::null(),
		name_len: 0,
		label: std::ptr::null(),
		label_len: 0,
		codec: std::ptr::null(),
		codec_len: 0,
		description: std::ptr::null(),
		description_len: 0,
		sample_rate: 0,
		channel_count: 0,
		container: moq_container::default(),
	};
	assert_eq!(unsafe { moq_consume_audio_config(catalog, 0, &mut audio_cfg) }, 0);
	assert_eq!(
		audio_cfg.container.kind,
		moq_container_kind::MOQ_CONTAINER_KIND_CMAF as u32
	);
	assert_eq!(
		unsafe { std::slice::from_raw_parts(audio_cfg.container.init, audio_cfg.container.init_len) },
		init
	);

	assert_eq!(moq_consume_catalog_free(catalog), 0);
	assert_eq!(moq_consume_catalog_close(catalog_task), 0);
	assert_eq!(catalog_cb.recv_terminal(), 0);
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn unpublishable_catalog_containers_are_rejected() {
	let origin = id(moq_origin_create());
	let broadcast = publish_broadcast(origin, b"container-reject");

	let name = "video";
	let codec = "vp8";
	let config = |container| moq_video_config {
		name: name.as_ptr() as *const c_char,
		name_len: name.len(),
		label: std::ptr::null(),
		label_len: 0,
		codec: codec.as_ptr() as *const c_char,
		codec_len: codec.len(),
		description: std::ptr::null(),
		description_len: 0,
		coded_width: 0,
		coded_height: 0,
		container,
	};

	// UNKNOWN only ever comes out of a catalog: we keep none of the original JSON, so
	// there is nothing to write back.
	let unknown = config(moq_container {
		kind: moq_container_kind::MOQ_CONTAINER_KIND_UNKNOWN as u32,
		init: std::ptr::null(),
		init_len: 0,
	});
	assert_eq!(
		unsafe { moq_publish_video_config(broadcast, &unknown) },
		-15,
		"unknown container should return InvalidCode (-15)"
	);

	let garbage = config(moq_container {
		kind: 12345,
		init: std::ptr::null(),
		init_len: 0,
	});
	assert_eq!(
		unsafe { moq_publish_video_config(broadcast, &garbage) },
		-15,
		"out of range container should return InvalidCode (-15)"
	);

	let cmaf = config(moq_container {
		kind: moq_container_kind::MOQ_CONTAINER_KIND_CMAF as u32,
		init: std::ptr::null(),
		init_len: 0,
	});
	assert_eq!(
		unsafe { moq_publish_video_config(broadcast, &cmaf) },
		-6,
		"cmaf without an init segment should return InvalidPointer (-6)"
	);

	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn catalog_section_roundtrip() {
	let origin = id(moq_origin_create());
	let path = b"catalog-sections";
	let broadcast = publish_broadcast(origin, path);

	// Set two untyped application sections on the publish-side catalog.
	let name_a = b"viewers";
	let json_a = br#"{"count":42}"#;
	assert_eq!(
		unsafe {
			moq_publish_catalog_section(
				broadcast,
				name_a.as_ptr() as *const c_char,
				name_a.len(),
				json_a.as_ptr() as *const c_char,
				json_a.len(),
			)
		},
		0
	);

	let name_b = b"title";
	let json_b = br#""hello world""#;
	assert_eq!(
		unsafe {
			moq_publish_catalog_section(
				broadcast,
				name_b.as_ptr() as *const c_char,
				name_b.len(),
				json_b.as_ptr() as *const c_char,
				json_b.len(),
			)
		},
		0
	);

	// A reserved section name (video/audio) is rejected.
	let reserved = b"video";
	let empty = b"{}";
	assert!(
		unsafe {
			moq_publish_catalog_section(
				broadcast,
				reserved.as_ptr() as *const c_char,
				reserved.len(),
				empty.as_ptr() as *const c_char,
				empty.len(),
			)
		} < 0,
		"reserved section name should fail"
	);

	// Invalid JSON is rejected with the Json error code (-37).
	let bad = b"not json";
	assert_eq!(
		unsafe {
			moq_publish_catalog_section(
				broadcast,
				name_a.as_ptr() as *const c_char,
				name_a.len(),
				bad.as_ptr() as *const c_char,
				bad.len(),
			)
		},
		-37,
		"invalid JSON should return the Json error code"
	);

	// Consume to verify the sections survive the wire.
	let consume = request_broadcast(origin, path);
	let catalog_cb = Callback::new();
	let catalog_task = id(unsafe { moq_consume_catalog(consume, Some(channel_callback), catalog_cb.ptr) });
	let catalog_id = id(catalog_cb.recv());

	// Both sections come back; iterate by index to find each by name.
	let count = moq_consume_catalog_section_count(catalog_id);
	assert_eq!(count, 2, "expected two sections, got {count}");

	let mut found_a = false;
	let mut found_b = false;
	for index in 0..count as u32 {
		let mut section = moq_section {
			name: std::ptr::null(),
			name_len: 0,
			json: std::ptr::null(),
			json_len: 0,
		};
		assert_eq!(
			unsafe { moq_consume_catalog_section_at(catalog_id, index, &mut section) },
			0
		);
		let name = unsafe { std::slice::from_raw_parts(section.name.cast::<u8>(), section.name_len) };
		let json = unsafe { std::slice::from_raw_parts(section.json.cast::<u8>(), section.json_len) };
		match name {
			n if n == name_a => {
				found_a = true;
				assert_eq!(json, json_a);
			}
			n if n == name_b => {
				found_b = true;
				assert_eq!(json, json_b);
			}
			other => panic!("unexpected section name: {:?}", std::str::from_utf8(other)),
		}
	}
	assert!(found_a && found_b, "both sections should be present");

	// Direct lookup by name returns the JSON value.
	let mut value = moq_string {
		data: std::ptr::null(),
		len: 0,
	};
	assert_eq!(
		unsafe { moq_consume_catalog_section(catalog_id, name_a.as_ptr() as *const c_char, name_a.len(), &mut value) },
		0
	);
	let got = unsafe { std::slice::from_raw_parts(value.data.cast::<u8>(), value.len) };
	assert_eq!(got, json_a);

	// A missing section fails.
	let missing = b"nope";
	assert!(
		unsafe {
			moq_consume_catalog_section(catalog_id, missing.as_ptr() as *const c_char, missing.len(), &mut value)
		} < 0,
		"missing section should fail"
	);

	// Removing a section republishes the catalog without it.
	assert_eq!(
		unsafe { moq_publish_catalog_section_remove(broadcast, name_a.as_ptr() as *const c_char, name_a.len()) },
		0
	);
	let catalog_id2 = id(catalog_cb.recv());
	assert_eq!(
		moq_consume_catalog_section_count(catalog_id2),
		1,
		"one section should remain after remove"
	);
	assert!(
		unsafe { moq_consume_catalog_section(catalog_id2, name_a.as_ptr() as *const c_char, name_a.len(), &mut value) }
			< 0,
		"removed section should be gone"
	);

	assert_eq!(moq_consume_catalog_free(catalog_id), 0);
	assert_eq!(moq_consume_catalog_free(catalog_id2), 0);
	assert_eq!(moq_consume_catalog_close(catalog_task), 0);
	assert_eq!(catalog_cb.recv_terminal(), 0, "catalog close delivers terminal 0");
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn publish_track_invalid_broadcast() {
	let name = b"data";
	assert!(unsafe { moq_publish_track(0, name.as_ptr() as *const c_char, name.len(), std::ptr::null()) } < 0);
	let info = moq_track_info {
		priority: 1,
		ordered: true,
		max_age_ms: 0,
		max_age_present: false,
		timescale: 0,
		timescale_present: false,
	};
	assert!(unsafe { moq_publish_track(0, name.as_ptr() as *const c_char, name.len(), &info) } < 0);
	assert!(moq_publish_track_group(9999) < 0);
	assert!(unsafe { moq_publish_track_frame(9999, name.as_ptr(), name.len(), 0) } < 0);
	assert!(unsafe { moq_publish_group_frame(9999, name.as_ptr(), name.len(), 0) } < 0);
	assert!(moq_publish_track_finish(9999) < 0);
	assert!(moq_publish_group_finish(9999) < 0);

	let subscription = moq_subscription {
		priority: 1,
		ordered: true,
		max_age_ms: 0,
		group_start: 0,
		group_start_present: false,
		group_end: 0,
		group_end_present: false,
	};
	assert!(unsafe { moq_consume_track_update(9999, &subscription) } < 0);
}

#[test]
fn publish_track_with_info_rejects_invalid_timescale() {
	let origin = id(moq_origin_create());
	let broadcast = publish_broadcast(origin, b"publish-track-with-info-rejects-invalid-timescale");
	let name = b"data";
	let info = moq_track_info {
		priority: 0,
		ordered: false,
		max_age_ms: 0,
		max_age_present: false,
		timescale: 0,
		timescale_present: true,
	};

	assert!(unsafe { moq_publish_track(broadcast, name.as_ptr() as *const c_char, name.len(), &info) } < 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
}

#[test]
fn raw_track_options_preserve_ordering_priority() {
	let mut info = moq_track_info {
		priority: 0,
		ordered: false,
		max_age_ms: 0,
		max_age_present: false,
		timescale: 0,
		timescale_present: false,
	};

	assert!(!moq_net::track::Info::try_from(&info).unwrap().ordered);
	info.ordered = true;
	assert!(moq_net::track::Info::try_from(&info).unwrap().ordered);

	let mut subscription = moq_subscription {
		priority: 0,
		ordered: false,
		max_age_ms: 0,
		group_start: 0,
		group_start_present: false,
		group_end: 0,
		group_end_present: false,
	};

	assert!(!moq_net::track::Subscription::from(&subscription).ordered);
	subscription.ordered = true;
	assert!(moq_net::track::Subscription::from(&subscription).ordered);
}

#[test]
fn raw_track_publish_consume() {
	let origin = id(moq_origin_create());
	let path = b"raw-track";
	let broadcast = publish_broadcast(origin, path);

	// A raw, non-media track: arbitrary bytes, no codec/container/catalog.
	let track_name = b"data";
	let track = id(unsafe {
		moq_publish_track(
			broadcast,
			track_name.as_ptr() as *const c_char,
			track_name.len(),
			std::ptr::null(),
		)
	});

	let consume = request_broadcast(origin, path);

	let frame_cb = Callback::new();
	// This round trip verifies every published frame. Allow the first group to
	// finish draining if the second becomes visible while the callback runs.
	let subscription = moq_subscription {
		priority: 0,
		ordered: false,
		max_age_ms: 1_000,
		group_start: 0,
		group_start_present: false,
		group_end: 0,
		group_end_present: false,
	};
	let consumer = id(unsafe {
		moq_consume_track(
			consume,
			track_name.as_ptr() as *const c_char,
			track_name.len(),
			&subscription,
			Some(channel_callback),
			frame_cb.ptr,
		)
	});

	// One-frame-per-group convenience write with an explicit timestamp.
	let payload = b"hello raw track";
	let timestamp_us = 12_345;
	assert_eq!(
		unsafe { moq_publish_track_frame(track, payload.as_ptr(), payload.len(), timestamp_us) },
		0
	);

	let frame_id = id(frame_cb.recv());
	let mut frame = moq_frame {
		payload: std::ptr::null(),
		payload_size: 0,
		timestamp_us: 0,
		keyframe: true, // should be overwritten with false
	};
	assert_eq!(unsafe { moq_consume_track_frame(frame_id, &mut frame) }, 0);
	let received = unsafe { std::slice::from_raw_parts(frame.payload, frame.payload_size) };
	assert_eq!(received, payload);
	assert_eq!(frame.timestamp_us, timestamp_us);
	assert!(!frame.keyframe, "raw frames have no keyframe flag");
	assert_eq!(moq_consume_track_frame_free(frame_id), 0);

	// Multi-frame group via the explicit group API.
	let group = id(moq_publish_track_group(track));
	let parts: [(&[u8], u64); 2] = [(b"part-0", 20_000), (b"part-1", 30_000)];
	for (part, timestamp_us) in parts {
		assert_eq!(
			unsafe { moq_publish_group_frame(group, part.as_ptr(), part.len(), timestamp_us) },
			0
		);
	}
	assert_eq!(moq_publish_group_finish(group), 0);

	for (expected, timestamp_us) in parts {
		let frame_id = id(frame_cb.recv());
		let mut frame = moq_frame {
			payload: std::ptr::null(),
			payload_size: 0,
			timestamp_us: 0,
			keyframe: false,
		};
		assert_eq!(unsafe { moq_consume_track_frame(frame_id, &mut frame) }, 0);
		let received = unsafe { std::slice::from_raw_parts(frame.payload, frame.payload_size) };
		assert_eq!(received, expected);
		assert_eq!(frame.timestamp_us, timestamp_us);
		assert_eq!(moq_consume_track_frame_free(frame_id), 0);
	}

	assert_eq!(moq_consume_track_close(consumer), 0);
	// The task delivers one final terminal callback after close; drain it
	// before the Callback (user_data) drops.
	assert_eq!(frame_cb.recv_terminal(), 0, "clean close delivers terminal 0");
	assert!(moq_consume_track_close(consumer) < 0, "double-close should fail");
	assert_eq!(moq_publish_track_finish(track), 0);
	assert!(moq_publish_track_finish(track) < 0, "double-close should fail");
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn raw_track_datagram_publish_consume() {
	let origin = id(moq_origin_create());
	let path = b"raw-datagram";
	let broadcast = publish_broadcast(origin, path);

	let track_name = b"events";
	let track = id(unsafe {
		moq_publish_track(
			broadcast,
			track_name.as_ptr() as *const c_char,
			track_name.len(),
			std::ptr::null(),
		)
	});

	let consume = request_broadcast(origin, path);

	let dg_cb = Callback::new();
	let consumer = id(unsafe {
		moq_consume_datagrams(
			consume,
			track_name.as_ptr() as *const c_char,
			track_name.len(),
			Some(channel_callback),
			dg_cb.ptr,
		)
	});

	// Millisecond-aligned so the value survives the default (millisecond) timescale exactly.
	let payload = b"hello datagram";
	let mut sequence: u64 = u64::MAX;
	assert_eq!(
		unsafe { moq_publish_track_datagram(track, payload.as_ptr(), payload.len(), 120_000, &mut sequence) },
		0
	);

	let dg_id = id(dg_cb.recv());
	let mut datagram = moq_datagram {
		payload: std::ptr::null(),
		payload_size: 0,
		timestamp_us: 0,
		sequence: 0,
	};
	assert_eq!(unsafe { moq_consume_datagram(dg_id, &mut datagram) }, 0);
	let received = unsafe { std::slice::from_raw_parts(datagram.payload, datagram.payload_size) };
	assert_eq!(received, payload);
	assert_eq!(datagram.timestamp_us, 120_000);
	assert_eq!(datagram.sequence, sequence);
	assert_eq!(moq_consume_datagram_free(dg_id), 0);

	assert_eq!(moq_consume_datagrams_close(consumer), 0);
	// The task delivers one final terminal callback after close; drain it
	// before the Callback (user_data) drops.
	assert_eq!(dg_cb.recv_terminal(), 0, "clean close delivers terminal 0");
	assert!(moq_consume_datagrams_close(consumer) < 0, "double-close should fail");
	assert_eq!(moq_publish_track_finish(track), 0);
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn raw_track_sparse_groups_and_known_end() {
	let origin = id(moq_origin_create());
	let broadcast = publish_broadcast(origin, b"raw-track-sparse-groups-and-known-end");
	let name = b"sparse";
	let track =
		id(unsafe { moq_publish_track(broadcast, name.as_ptr() as *const c_char, name.len(), std::ptr::null()) });

	let group = id(moq_publish_track_group_at(track, 2));
	assert_eq!(moq_publish_group_finish(group), 0);
	assert_eq!(moq_publish_track_finish_at(track, 5), 0);
	let group = id(moq_publish_track_group_at(track, 4));
	assert_eq!(moq_publish_group_finish(group), 0);
	assert!(moq_publish_track_group_at(track, 5) < 0);
	assert_eq!(moq_publish_track_finish(track), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
}

#[test]
fn raw_track_and_group_abort_consume_their_handles() {
	let origin = id(moq_origin_create());
	let broadcast = publish_broadcast(origin, b"raw-track-and-group-abort-consume-their-handles");
	let name = b"aborted";
	let track =
		id(unsafe { moq_publish_track(broadcast, name.as_ptr() as *const c_char, name.len(), std::ptr::null()) });
	let group = id(moq_publish_track_group(track));
	assert_eq!(moq_publish_group_abort(group, 409), 0);
	assert!(moq_publish_group_finish(group) < 0);
	assert_eq!(moq_publish_track_abort(track, 410), 0);
	assert!(moq_publish_track_finish(track) < 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
}

#[test]
fn raw_track_subscription_options_and_update() {
	let origin = id(moq_origin_create());
	let path = b"raw-track-options";
	let broadcast = publish_broadcast(origin, path);

	let track_name = b"data";
	let info = moq_track_info {
		priority: 3,
		ordered: false,
		max_age_ms: 1_000,
		max_age_present: true,
		timescale: 1_000_000,
		timescale_present: true,
	};
	let track =
		id(unsafe { moq_publish_track(broadcast, track_name.as_ptr() as *const c_char, track_name.len(), &info) });

	let payloads: [&[u8]; 3] = [b"zero", b"one", b"two"];
	for (i, payload) in payloads.into_iter().enumerate() {
		assert_eq!(
			unsafe { moq_publish_track_frame(track, payload.as_ptr(), payload.len(), i as u64 * 20_000) },
			0
		);
	}

	let consume = request_broadcast(origin, path);

	let frame_cb = Callback::new();
	let subscription = moq_subscription {
		priority: 5,
		ordered: true,
		max_age_ms: 25,
		group_start: 1,
		group_start_present: true,
		group_end: 1,
		group_end_present: true,
	};
	let consumer = id(unsafe {
		moq_consume_track(
			consume,
			track_name.as_ptr() as *const c_char,
			track_name.len(),
			&subscription,
			Some(channel_callback),
			frame_cb.ptr,
		)
	});

	let frame_id = id(frame_cb.recv());
	let mut frame = moq_frame {
		payload: std::ptr::null(),
		payload_size: 0,
		timestamp_us: 0,
		keyframe: false,
	};
	assert_eq!(unsafe { moq_consume_track_frame(frame_id, &mut frame) }, 0);
	let received = unsafe { std::slice::from_raw_parts(frame.payload, frame.payload_size) };
	assert_eq!(received, b"one");
	assert_eq!(frame.timestamp_us, 20_000);
	assert_eq!(moq_consume_track_frame_free(frame_id), 0);

	let update = moq_subscription {
		group_end: 2,
		..subscription
	};
	assert_eq!(unsafe { moq_consume_track_update(consumer, &update) }, 0);

	let frame_id = id(frame_cb.recv());
	let mut frame = moq_frame {
		payload: std::ptr::null(),
		payload_size: 0,
		timestamp_us: 0,
		keyframe: false,
	};
	assert_eq!(unsafe { moq_consume_track_frame(frame_id, &mut frame) }, 0);
	let received = unsafe { std::slice::from_raw_parts(frame.payload, frame.payload_size) };
	assert_eq!(received, b"two");
	assert_eq!(frame.timestamp_us, 40_000);
	assert_eq!(moq_consume_track_frame_free(frame_id), 0);

	assert_eq!(moq_consume_track_close(consumer), 0);
	assert_eq!(frame_cb.recv_terminal(), 0);
	assert_eq!(moq_publish_track_finish(track), 0);
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn json_snapshot_publish_consume() {
	let origin = id(moq_origin_create());
	let path = b"json-snapshot";
	let broadcast = publish_broadcast(origin, path);

	let track_name = b"meta";
	let config = moq_json_snapshot_config {
		delta_ratio: 8,
		compression: true,
	};
	let producer = id(unsafe {
		moq_publish_json_snapshot(
			broadcast,
			track_name.as_ptr() as *const c_char,
			track_name.len(),
			&config,
		)
	});

	let consume = request_broadcast(origin, path);

	let value_cb = Callback::new();
	let consumer = id(unsafe {
		moq_consume_json_snapshot(
			consume,
			track_name.as_ptr() as *const c_char,
			track_name.len(),
			&config,
			Some(channel_callback),
			value_cb.ptr,
		)
	});

	for expected in [r#"{"a":1}"#, r#"{"a":2}"#] {
		assert_eq!(
			unsafe { moq_publish_json_snapshot_update(producer, expected.as_ptr() as *const c_char, expected.len()) },
			0
		);
		let value_id = id(value_cb.recv());
		let mut value = moq_json_value {
			json: std::ptr::null(),
			json_len: 0,
		};
		assert_eq!(unsafe { moq_consume_json_value(value_id, &mut value) }, 0);
		let received = unsafe { std::slice::from_raw_parts(value.json.cast::<u8>(), value.json_len) };
		assert_eq!(
			serde_json::from_slice::<serde_json::Value>(received).unwrap(),
			serde_json::from_str::<serde_json::Value>(expected).unwrap()
		);
		assert_eq!(moq_consume_json_value_free(value_id), 0);
	}

	assert_eq!(moq_consume_json_close(consumer), 0);
	assert_eq!(value_cb.recv_terminal(), 0, "clean close delivers terminal 0");
	assert!(moq_consume_json_close(consumer) < 0, "double-close should fail");
	assert_eq!(moq_publish_json_snapshot_finish(producer), 0);
	assert!(
		moq_publish_json_snapshot_finish(producer) < 0,
		"double-close should fail"
	);
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn json_stream_publish_consume() {
	let origin = id(moq_origin_create());
	let path = b"json-stream";
	let broadcast = publish_broadcast(origin, path);

	let track_name = b"events";
	let config = moq_json_stream_config { compression: true };
	let producer = id(unsafe {
		moq_publish_json_stream(
			broadcast,
			track_name.as_ptr() as *const c_char,
			track_name.len(),
			&config,
		)
	});

	let consume = request_broadcast(origin, path);

	let value_cb = Callback::new();
	let consumer = id(unsafe {
		moq_consume_json_stream(
			consume,
			track_name.as_ptr() as *const c_char,
			track_name.len(),
			&config,
			Some(channel_callback),
			value_cb.ptr,
		)
	});

	for expected in [r#"{"n":0}"#, r#"{"n":1}"#, r#"{"n":2}"#] {
		assert_eq!(
			unsafe { moq_publish_json_stream_append(producer, expected.as_ptr() as *const c_char, expected.len()) },
			0
		);
		let value_id = id(value_cb.recv());
		let mut value = moq_json_value {
			json: std::ptr::null(),
			json_len: 0,
		};
		assert_eq!(unsafe { moq_consume_json_value(value_id, &mut value) }, 0);
		let received = unsafe { std::slice::from_raw_parts(value.json.cast::<u8>(), value.json_len) };
		assert_eq!(
			serde_json::from_slice::<serde_json::Value>(received).unwrap(),
			serde_json::from_str::<serde_json::Value>(expected).unwrap()
		);
		assert_eq!(moq_consume_json_value_free(value_id), 0);
	}

	assert_eq!(moq_consume_json_close(consumer), 0);
	assert_eq!(value_cb.recv_terminal(), 0, "clean close delivers terminal 0");
	assert!(moq_consume_json_close(consumer) < 0, "double-close should fail");
	assert_eq!(moq_publish_json_stream_finish(producer), 0);
	assert!(moq_publish_json_stream_finish(producer) < 0, "double-close should fail");
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn close_invalid_or_zero_ids() {
	assert!(moq_origin_close(9999) < 0);
	assert!(moq_session_close(9999) < 0);
	assert!(moq_publish_finish(9999) < 0);
	assert!(moq_consume_close(9999) < 0);
	assert!(moq_consume_frame_free(9999) < 0);

	assert!(moq_origin_close(0) < 0);
	assert!(moq_session_close(0) < 0);
	assert!(moq_publish_finish(0) < 0);
}

#[test]
fn announced_free_lifecycle() {
	let origin = id(moq_origin_create());
	let path = b"announced-free";
	let broadcast = publish_broadcast(origin, path);

	let ann_cb = Callback::new();
	let ann_task = id(unsafe { moq_origin_announced(origin, Some(channel_callback), ann_cb.ptr) });

	// The first callback is the announcement for our broadcast.
	let announced = id(ann_cb.recv());

	// Its info reports our path, active.
	let mut info = moq_announced {
		path: std::ptr::null(),
		path_len: 0,
		active: false,
	};
	assert_eq!(unsafe { moq_origin_announced_info(announced, &mut info) }, 0);
	assert!(info.active, "broadcast should be active");
	let got = unsafe { std::slice::from_raw_parts(info.path.cast::<u8>(), info.path_len) };
	assert_eq!(got, path, "announced path should match");

	// Freeing the record succeeds once; the handle is then unknown.
	assert_eq!(moq_origin_announced_free(announced), 0);
	assert!(moq_origin_announced_free(announced) < 0, "double-free should fail");
	assert!(
		unsafe { moq_origin_announced_info(announced, &mut info) } < 0,
		"info on a freed handle should fail"
	);

	// Stop the listener and drain its terminal callback before the Callback drops.
	assert_eq!(moq_origin_announced_close(ann_task), 0);
	ann_cb.recv_terminal();

	assert_eq!(moq_origin_close(origin), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
}

#[test]
fn double_close_all_resource_types() {
	let origin = id(moq_origin_create());
	assert_eq!(moq_origin_close(origin), 0);
	assert!(moq_origin_close(origin) < 0);

	let origin = id(moq_origin_create());
	let broadcast = publish_broadcast(origin, b"double-close-all-resource-types");
	let init = opus_head();
	let format = moq_audio_format::MOQ_AUDIO_FORMAT_OPUS;
	let media = id(publish_audio(broadcast, format, &init, None));

	assert_eq!(moq_publish_media_finish(media), 0);
	assert!(moq_publish_media_finish(media) < 0);
	assert_eq!(moq_publish_finish(broadcast), 0);

	let origin = id(moq_origin_create());
	let path = b"double-close-test";
	let broadcast = publish_broadcast(origin, path);
	let init = opus_head();
	let media = id(publish_audio(broadcast, format, &init, None));

	let consume = request_broadcast(origin, path);
	let catalog_cb = Callback::new();
	let catalog_task = id(unsafe { moq_consume_catalog(consume, Some(channel_callback), catalog_cb.ptr) });

	let catalog_id = id(catalog_cb.recv());

	let frame_cb = Callback::new();
	let track = id(unsafe { moq_consume_audio(catalog_id, 0, 10_000, Some(channel_callback), frame_cb.ptr) });

	let payload = b"test";
	assert_eq!(
		unsafe { moq_publish_media_frame(media, payload.as_ptr(), payload.len(), 1_000_000) },
		0
	);
	let frame_id = id(frame_cb.recv());

	assert_eq!(moq_consume_frame_free(frame_id), 0);
	assert!(moq_consume_frame_free(frame_id) < 0);

	assert_eq!(moq_consume_audio_close(track), 0);
	assert_eq!(frame_cb.recv_terminal(), 0, "audio close delivers terminal 0");
	assert!(moq_consume_audio_close(track) < 0);

	assert_eq!(moq_consume_catalog_free(catalog_id), 0);
	assert!(moq_consume_catalog_free(catalog_id) < 0);

	assert_eq!(moq_consume_catalog_close(catalog_task), 0);
	assert_eq!(catalog_cb.recv_terminal(), 0, "catalog close delivers terminal 0");
	assert!(moq_consume_catalog_close(catalog_task) < 0);

	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_media_finish(media), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

/// Audio has no keyframes, so `moq_publish_media_cut` is the only thing that gives it group
/// boundaries: without it every packet lands in one group that never closes. Cutting per packet is
/// what a live publisher does, and `_seek` does the same with a chosen sequence number.
#[test]
fn media_cut_bounds_audio_groups() {
	let origin = id(moq_origin_create());
	let broadcast = publish_broadcast(origin, b"media-cut");
	let init = opus_head();
	let format = moq_audio_format::MOQ_AUDIO_FORMAT_OPUS;
	let media = id(publish_audio(broadcast, format, &init, None));

	let payload = b"test";
	for i in 0..3u64 {
		assert_eq!(
			unsafe { moq_publish_media_frame(media, payload.as_ptr(), payload.len(), i * 20_000) },
			0
		);
		assert_eq!(moq_publish_media_cut(media), 0, "each packet is its own group");
	}

	// The same boundary, with the next group explicitly numbered.
	assert_eq!(
		unsafe { moq_publish_media_frame(media, payload.as_ptr(), payload.len(), 60_000) },
		0
	);
	assert_eq!(moq_publish_media_seek(media, 42), 0);

	// Both report a missing importer rather than panicking on an unknown id.
	assert!(moq_publish_media_cut(9999) < 0);
	assert!(moq_publish_media_seek(9999, 0) < 0);

	assert_eq!(moq_publish_media_finish(media), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn unknown_format() {
	let origin = id(moq_origin_create());
	let broadcast = publish_broadcast(origin, b"unknown-format");
	let _guard = Guard(Some(|| {
		moq_publish_finish(broadcast);
	}));

	// A format is an enum now, so the only bad value C can still supply is an out-of-range
	// code. That must be an error rather than a transmute into an invalid discriminant.
	let config = moq_audio_init {
		format: 9999,
		init: std::ptr::null(),
		init_len: 0,
		label: std::ptr::null(),
		label_len: 0,
	};
	let ret = unsafe { moq_publish_audio(broadcast, &config) };
	assert!(ret < 0, "an out-of-range format code should fail");
}

#[test]
fn local_announce() {
	let origin = id(moq_origin_create());

	let cb = Callback::new();
	let announced_task = id(unsafe { moq_origin_announced(origin, Some(channel_callback), cb.ptr) });

	let path = b"test/broadcast";
	let broadcast = publish_broadcast(origin, path);

	let announced_id = id(cb.recv());

	let mut info = moq_announced {
		path: std::ptr::null(),
		path_len: 0,
		active: false,
	};
	assert_eq!(unsafe { moq_origin_announced_info(announced_id, &mut info) }, 0);
	assert!(info.active, "broadcast should be active");

	let announced_path =
		unsafe { std::str::from_utf8(std::slice::from_raw_parts(info.path.cast::<u8>(), info.path_len)).unwrap() };
	assert_eq!(announced_path, "test/broadcast");

	assert_eq!(moq_origin_announced_close(announced_task), 0);
	assert_eq!(cb.recv_terminal(), 0, "announced close delivers terminal 0");
	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn announced_deactivation() {
	let origin = id(moq_origin_create());
	let cb = Callback::new();
	let announced_task = id(unsafe { moq_origin_announced(origin, Some(channel_callback), cb.ptr) });

	let path = b"deactivate/test";
	let broadcast = publish_broadcast(origin, path);

	let announced_id = id(cb.recv());
	let mut info = moq_announced {
		path: std::ptr::null(),
		path_len: 0,
		active: false,
	};
	assert_eq!(unsafe { moq_origin_announced_info(announced_id, &mut info) }, 0);
	assert!(info.active);

	// Going non-live unannounces the broadcast without tearing it down: it stays
	// reachable by exact path for subscribes and fetches.
	assert_eq!(moq_publish_set_announce(broadcast, false), 0);

	let deactivated_id = id(cb.recv());
	assert_eq!(unsafe { moq_origin_announced_info(deactivated_id, &mut info) }, 0);
	assert!(!info.active, "broadcast should be inactive after unannounce");

	assert_eq!(moq_origin_announced_close(announced_task), 0);
	assert_eq!(cb.recv_terminal(), 0, "announced close delivers terminal 0");
	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn local_publish_consume() {
	let origin = id(moq_origin_create());
	let path = b"live";
	let broadcast = publish_broadcast(origin, path);

	let init = opus_head();
	let format = moq_audio_format::MOQ_AUDIO_FORMAT_OPUS;
	let media = id(publish_audio(broadcast, format, &init, None));

	let consume = request_broadcast(origin, path);
	let catalog_cb = Callback::new();
	let catalog_task = id(unsafe { moq_consume_catalog(consume, Some(channel_callback), catalog_cb.ptr) });

	let catalog_id = id(catalog_cb.recv());

	let mut audio_cfg = moq_audio_config {
		name: std::ptr::null(),
		name_len: 0,
		label: std::ptr::null(),
		label_len: 0,
		codec: std::ptr::null(),
		codec_len: 0,
		description: std::ptr::null(),
		description_len: 0,
		sample_rate: 0,
		channel_count: 0,
		container: moq_container::default(),
	};
	assert_eq!(unsafe { moq_consume_audio_config(catalog_id, 0, &mut audio_cfg) }, 0);
	assert_eq!(audio_cfg.sample_rate, 48000);
	assert_eq!(audio_cfg.channel_count, 2);

	let codec = unsafe {
		std::str::from_utf8(std::slice::from_raw_parts(
			audio_cfg.codec.cast::<u8>(),
			audio_cfg.codec_len,
		))
	}
	.unwrap();
	assert_eq!(codec, "opus");

	let mut video_cfg = moq_video_config {
		name: std::ptr::null(),
		name_len: 0,
		label: std::ptr::null(),
		label_len: 0,
		codec: std::ptr::null(),
		codec_len: 0,
		description: std::ptr::null(),
		description_len: 0,
		coded_width: 0,
		coded_height: 0,
		container: moq_container::default(),
	};
	assert!(
		unsafe { moq_consume_video_config(catalog_id, 0, &mut video_cfg) } < 0,
		"video config should fail (no video tracks)"
	);

	let frame_cb = Callback::new();
	let track = id(unsafe { moq_consume_audio(catalog_id, 0, 10_000, Some(channel_callback), frame_cb.ptr) });

	let payload = b"opus audio payload data";
	let timestamp_us: u64 = 1_000_000;
	assert_eq!(
		unsafe { moq_publish_media_frame(media, payload.as_ptr(), payload.len(), timestamp_us) },
		0
	);

	let frame_id = id(frame_cb.recv());

	let mut frame = moq_frame {
		payload: std::ptr::null(),
		payload_size: 0,
		timestamp_us: 0,
		keyframe: false,
	};
	assert_eq!(unsafe { moq_consume_frame(frame_id, &mut frame) }, 0);
	assert_eq!(frame.payload_size, payload.len());
	assert_eq!(frame.timestamp_us, timestamp_us);

	let received = unsafe { std::slice::from_raw_parts(frame.payload, frame.payload_size) };
	assert_eq!(received, payload, "frame payload should match");

	assert_eq!(moq_consume_frame_free(frame_id), 0);
	assert_eq!(moq_consume_audio_close(track), 0);
	assert_eq!(frame_cb.recv_terminal(), 0, "audio close delivers terminal 0");
	assert_eq!(moq_consume_catalog_free(catalog_id), 0);
	assert_eq!(moq_consume_catalog_close(catalog_task), 0);
	assert_eq!(catalog_cb.recv_terminal(), 0, "catalog close delivers terminal 0");
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_media_finish(media), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn consume_announced_local() {
	let origin = id(moq_origin_create());

	// Start waiting before the broadcast exists: the announcement arrives afterwards.
	let cb = Callback::new();
	let path = b"live";
	let _task = id(unsafe {
		moq_origin_consume_announced(
			origin,
			path.as_ptr() as *const c_char,
			path.len(),
			Some(channel_callback),
			cb.ptr,
		)
	});

	let broadcast = publish_broadcast(origin, path);
	let init = opus_head();
	let format = moq_audio_format::MOQ_AUDIO_FORMAT_OPUS;
	let media = id(publish_audio(broadcast, format, &init, None));

	// First the broadcast handle, then a terminal 0 once the wait finishes.
	let consume = id(cb.recv());
	assert_eq!(cb.recv_terminal(), 0, "wait delivers terminal 0 after the handle");

	// The delivered handle behaves like one from moq_origin_request.
	let catalog_cb = Callback::new();
	let catalog_task = id(unsafe { moq_consume_catalog(consume, Some(channel_callback), catalog_cb.ptr) });
	let catalog_id = id(catalog_cb.recv());

	let mut audio_cfg = moq_audio_config {
		name: std::ptr::null(),
		name_len: 0,
		label: std::ptr::null(),
		label_len: 0,
		codec: std::ptr::null(),
		codec_len: 0,
		description: std::ptr::null(),
		description_len: 0,
		sample_rate: 0,
		channel_count: 0,
		container: moq_container::default(),
	};
	assert_eq!(unsafe { moq_consume_audio_config(catalog_id, 0, &mut audio_cfg) }, 0);
	assert_eq!(audio_cfg.sample_rate, 48000);
	assert_eq!(audio_cfg.channel_count, 2);

	assert_eq!(moq_consume_catalog_free(catalog_id), 0);
	assert_eq!(moq_consume_catalog_close(catalog_task), 0);
	assert_eq!(catalog_cb.recv_terminal(), 0, "catalog close delivers terminal 0");
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_media_finish(media), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

/// A catalog rendition may name a sibling broadcast (`./source`), and the track then lives
/// there, not on the broadcast the catalog came from. Ignoring the reference subscribes on the
/// catalog's own broadcast: `NotFound`, or a same-named local track with mismatched metadata.
#[test]
fn consume_audio_follows_a_sibling_broadcast_reference() {
	let origin = id(moq_origin_create());

	// Only the sibling serves the track; the catalog broadcast just describes it.
	let source = publish_broadcast(origin, b"a/source");
	let init = opus_head();
	let media = id(publish_audio(
		source,
		moq_audio_format::MOQ_AUDIO_FORMAT_OPUS,
		&init,
		None,
	));

	// The importer picks the track name, and the catalog rendition must key on the same one.
	let name = {
		let mut state = State::lock();
		let (_, catalog) = state.publish.pair_mut(Id::try_from(source).unwrap()).unwrap();
		let catalog = catalog.lock();
		catalog
			.audio
			.renditions
			.keys()
			.next()
			.expect("the importer publishes one audio rendition")
			.clone()
	};

	let broadcast = publish_broadcast(origin, b"a/pub");
	let codec = "opus";
	let config = moq_audio_config {
		name: name.as_ptr() as *const c_char,
		name_len: name.len(),
		label: std::ptr::null(),
		label_len: 0,
		codec: codec.as_ptr() as *const c_char,
		codec_len: codec.len(),
		description: std::ptr::null(),
		description_len: 0,
		sample_rate: 48000,
		channel_count: 2,
		container: moq_container::default(),
	};
	assert_eq!(unsafe { moq_publish_audio_config(broadcast, &config) }, 0);

	// The C config struct has no broadcast field, so point the rendition at the sibling here.
	{
		let mut state = State::lock();
		let (_, catalog) = state.publish.pair_mut(Id::try_from(broadcast).unwrap()).unwrap();
		catalog.lock().audio.renditions.get_mut(&name).unwrap().broadcast =
			Some(moq_net::PathRelative::new("./source").into_owned());
	}

	let consume = request_broadcast(origin, b"a/pub");
	let catalog_cb = Callback::new();
	let catalog_task = id(unsafe { moq_consume_catalog(consume, Some(channel_callback), catalog_cb.ptr) });
	let catalog_id = id(catalog_cb.recv());

	let frame_cb = Callback::new();
	let track = id(unsafe { moq_consume_audio(catalog_id, 0, 10_000, Some(channel_callback), frame_cb.ptr) });

	// Published on the sibling, so it only arrives if the reference was followed.
	let payload = b"opus audio payload data";
	let timestamp_us: u64 = 1_000_000;
	assert_eq!(
		unsafe { moq_publish_media_frame(media, payload.as_ptr(), payload.len(), timestamp_us) },
		0
	);

	let frame_id = id(frame_cb.recv());
	let mut frame = moq_frame {
		payload: std::ptr::null(),
		payload_size: 0,
		timestamp_us: 0,
		keyframe: false,
	};
	assert_eq!(unsafe { moq_consume_frame(frame_id, &mut frame) }, 0);
	let received = unsafe { std::slice::from_raw_parts(frame.payload, frame.payload_size) };
	assert_eq!(received, payload, "the sibling broadcast's frame should arrive");
	assert_eq!(frame.timestamp_us, timestamp_us);

	assert_eq!(moq_consume_frame_free(frame_id), 0);
	assert_eq!(moq_consume_audio_close(track), 0);
	assert_eq!(frame_cb.recv_terminal(), 0, "audio close delivers terminal 0");
	assert_eq!(moq_consume_catalog_free(catalog_id), 0);
	assert_eq!(moq_consume_catalog_close(catalog_task), 0);
	assert_eq!(catalog_cb.recv_terminal(), 0, "catalog close delivers terminal 0");
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_media_finish(media), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_publish_finish(source), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn consume_announced_close_cancels() {
	let origin = id(moq_origin_create());

	// Wait for a broadcast that never arrives, then cancel it.
	let cb = Callback::new();
	let path = b"never";
	let task = id(unsafe {
		moq_origin_consume_announced(
			origin,
			path.as_ptr() as *const c_char,
			path.len(),
			Some(channel_callback),
			cb.ptr,
		)
	});

	assert_eq!(moq_origin_consume_announced_close(task), 0);
	assert_eq!(cb.recv_terminal(), 0, "close delivers terminal 0");
	assert!(moq_origin_consume_announced_close(task) < 0, "double-close should fail");

	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn video_publish_consume() {
	let origin = id(moq_origin_create());
	let path = b"video-test";
	let broadcast = publish_broadcast(origin, path);

	let init = h264_init();
	let format = moq_video_format::MOQ_VIDEO_FORMAT_AVC3;
	let media = id(publish_video(broadcast, format, &init, None));

	let consume = request_broadcast(origin, path);
	let catalog_cb = Callback::new();
	let catalog_task = id(unsafe { moq_consume_catalog(consume, Some(channel_callback), catalog_cb.ptr) });

	let catalog_id = id(catalog_cb.recv());

	let mut video_cfg = moq_video_config {
		name: std::ptr::null(),
		name_len: 0,
		label: std::ptr::null(),
		label_len: 0,
		codec: std::ptr::null(),
		codec_len: 0,
		description: std::ptr::null(),
		description_len: 0,
		coded_width: 0,
		coded_height: 0,
		container: moq_container::default(),
	};
	assert_eq!(
		unsafe { moq_consume_video_config(catalog_id, 0, &mut video_cfg) },
		0,
		"video config should succeed for avc3 H.264 track"
	);

	let codec = unsafe {
		std::str::from_utf8(std::slice::from_raw_parts(
			video_cfg.codec.cast::<u8>(),
			video_cfg.codec_len,
		))
	}
	.unwrap();
	assert!(
		codec.starts_with("avc1.") || codec.starts_with("avc3."),
		"codec should be avc1/avc3, got {codec}"
	);

	assert_eq!(video_cfg.coded_width, 1280);
	assert_eq!(video_cfg.coded_height, 720);

	let mut audio_cfg = moq_audio_config {
		name: std::ptr::null(),
		name_len: 0,
		label: std::ptr::null(),
		label_len: 0,
		codec: std::ptr::null(),
		codec_len: 0,
		description: std::ptr::null(),
		description_len: 0,
		sample_rate: 0,
		channel_count: 0,
		container: moq_container::default(),
	};
	assert!(
		unsafe { moq_consume_audio_config(catalog_id, 0, &mut audio_cfg) } < 0,
		"audio config should fail (no audio tracks)"
	);

	let frame_cb = Callback::new();
	let track = id(unsafe { moq_consume_video(catalog_id, 0, 10_000, Some(channel_callback), frame_cb.ptr) });

	let keyframe = [0x00, 0x00, 0x00, 0x01, 0x65, 0xAA, 0xBB, 0xCC];
	assert_eq!(
		unsafe { moq_publish_media_frame(media, keyframe.as_ptr(), keyframe.len(), 0) },
		0
	);

	let frame_id = id(frame_cb.recv());
	let mut frame = moq_frame {
		payload: std::ptr::null(),
		payload_size: 0,
		timestamp_us: 0,
		keyframe: false,
	};
	assert_eq!(unsafe { moq_consume_frame(frame_id, &mut frame) }, 0);
	assert_eq!(frame.timestamp_us, 0);
	assert!(frame.payload_size > 0, "frame should have payload data");

	assert_eq!(moq_consume_frame_free(frame_id), 0);
	assert_eq!(moq_consume_video_close(track), 0);
	assert_eq!(frame_cb.recv_terminal(), 0, "video close delivers terminal 0");
	assert_eq!(moq_consume_catalog_free(catalog_id), 0);
	assert_eq!(moq_consume_catalog_close(catalog_task), 0);
	assert_eq!(catalog_cb.recv_terminal(), 0, "catalog close delivers terminal 0");
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_media_finish(media), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

/// The raw audio publish path: PCM in, an Opus track out. The decode half has
/// its own coverage in moq-audio; this pins the producer lifecycle the C surface
/// owns, including that a finished handle is gone.
#[test]
fn audio_raw_publish() {
	let origin = id(moq_origin_create());
	let broadcast = publish_broadcast(origin, b"audio-raw-publish-test");

	let name = b"audio";
	let input = moq_audio_encoder_input {
		format: moq_audio_sample_format::MOQ_AUDIO_SAMPLE_FORMAT_F32 as u32,
		sample_rate: 48_000,
		channels: 2,
	};
	let codec = b"opus";
	let output = moq_audio_encoder_output {
		codec: codec.as_ptr() as *const c_char,
		codec_len: codec.len(),
		sample_rate: 0,
		channels: 0,
		bitrate: 0,
		frame_duration_ms: 20,
	};
	let producer =
		id(unsafe { moq_encode_audio(broadcast, name.as_ptr() as *const c_char, name.len(), &input, &output) });

	// 20 ms of silence: interleaved stereo f32 at 48 kHz, one encoded frame's worth.
	let samples = vec![0.0f32; 960 * 2];
	let pcm = unsafe { std::slice::from_raw_parts(samples.as_ptr().cast::<u8>(), std::mem::size_of_val(&samples[..])) };
	let frame = moq_audio_frame {
		timestamp_us: 0,
		data: pcm.as_ptr(),
		data_size: pcm.len(),
	};
	assert_eq!(unsafe { moq_encode_audio_frame(producer, &frame) }, 0);

	assert_eq!(moq_encode_audio_finish(producer), 0);
	assert!(moq_encode_audio_finish(producer) < 0, "double-finish should fail");
	assert!(
		unsafe { moq_encode_audio_frame(producer, &frame) } < 0,
		"a finished producer should take no more frames"
	);

	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

/// A mid-gray RGBA frame, encodable without a camera.
fn gray_rgba(width: u32, height: u32) -> Vec<u8> {
	vec![0x80u8; width as usize * height as usize * 4]
}

/// The publish-side mirror of [`video_raw_decode`]: hand raw RGBA to
/// `moq_encode_video` and read decoded I420 back out of
/// `moq_decode_video`, so the encode and decode halves meet on the wire.
#[test]
fn video_raw_publish_consume() {
	let origin = id(moq_origin_create());
	let path = b"video-raw-publish-test";
	let broadcast = publish_broadcast(origin, path);

	let input = moq_video_encoder_input {
		format: moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_RGBA as u32,
		width: 320,
		height: 240,
		framerate: 30,
	};
	// Software so the test is deterministic everywhere: `Auto` would reach for a
	// hardware backend that CI runners don't have.
	let output = moq_video_encoder_output {
		codec: moq_video_codec::MOQ_VIDEO_CODEC_H264 as u32,
		bitrate: 0,
		gop: 0,
		kind: moq_video_encoder_kind::MOQ_VIDEO_ENCODER_KIND_SOFTWARE as u32,
		encoder: std::ptr::null(),
		encoder_len: 0,
	};
	let producer = id(unsafe { moq_encode_video(broadcast, &input, &output) });

	let rgba = gray_rgba(320, 240);
	let publish = |index: u64| {
		let frame = moq_video_encoder_frame {
			timestamp_us: index * 33_333,
			data: rgba.as_ptr(),
			data_size: rgba.len(),
		};
		assert_eq!(unsafe { moq_encode_video_frame(producer, &frame) }, 0);
	};

	// The catalog rendition only exists once the importer has parsed the codec
	// config out of an encoded keyframe, so publish before subscribing.
	assert_eq!(moq_encode_video_cut(producer), 0);
	for i in 0..5u64 {
		publish(i);
	}

	let consume = request_broadcast(origin, path);
	let catalog_cb = Callback::new();
	let catalog_task = id(unsafe { moq_consume_catalog(consume, Some(channel_callback), catalog_cb.ptr) });
	let catalog_id = id(catalog_cb.recv());

	let decoder = moq_video_decoder_output { max_age_ms: 10_000 };
	let frame_cb = Callback::new();
	let consumer = id(unsafe { moq_decode_video(catalog_id, 0, &decoder, Some(channel_callback), frame_cb.ptr) });

	// Keep feeding the encoder so the subscriber has frames to decode after it
	// joins, whatever the group boundary it landed on.
	for i in 5..20u64 {
		publish(i);
	}

	let frame_id = id(frame_cb.recv());
	let mut frame = moq_video_frame {
		timestamp_us: 0,
		width: 0,
		height: 0,
		data: std::ptr::null(),
		data_size: 0,
	};
	assert_eq!(unsafe { moq_decode_video_frame(frame_id, &mut frame) }, 0);
	assert_eq!(frame.width, 320);
	assert_eq!(frame.height, 240);
	assert_eq!(frame.data_size, 320 * 240 * 3 / 2, "tightly-packed I420");

	assert_eq!(moq_decode_video_frame_free(frame_id), 0);
	assert_eq!(moq_decode_video_close(consumer), 0);
	loop {
		let code = frame_cb.recv();
		if code > 0 {
			assert_eq!(moq_decode_video_frame_free(id(code)), 0);
		} else {
			assert_eq!(code, 0, "raw video close delivers terminal 0");
			break;
		}
	}

	assert_eq!(moq_consume_catalog_free(catalog_id), 0);
	assert_eq!(moq_consume_catalog_close(catalog_task), 0);
	assert_eq!(catalog_cb.recv_catalog_terminal(), 0);
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_encode_video_finish(producer), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

/// Regression: a producer handle is just an integer, so a C caller may drive it
/// from any thread, and each call runs on the thread that made it. Holding a bare
/// `Encoder` was therefore unsound on Windows, where the codec's COM apartment is
/// per-thread: it was opened on the publishing thread and closed on whichever
/// thread called finish. The confinement itself is asserted in moq-video
/// (`encode::sink`); this pins that the C surface supports the usage, including
/// the drain on finish.
#[test]
fn video_raw_publish_from_many_threads() {
	let origin = id(moq_origin_create());
	let broadcast = publish_broadcast(origin, b"video-raw-threads-test");

	let input = moq_video_encoder_input {
		format: moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_RGBA as u32,
		width: 320,
		height: 240,
		framerate: 30,
	};
	let output = moq_video_encoder_output {
		codec: moq_video_codec::MOQ_VIDEO_CODEC_H264 as u32,
		// An explicit ceiling so the retunes below stay under the rate the encoder
		// opened at, which openh264 requires.
		bitrate: 1_000_000,
		gop: 0,
		kind: moq_video_encoder_kind::MOQ_VIDEO_ENCODER_KIND_SOFTWARE as u32,
		encoder: std::ptr::null(),
		encoder_len: 0,
	};
	let producer = id(unsafe { moq_encode_video(broadcast, &input, &output) });

	// A fresh caller thread per frame, never the one that published.
	let rgba = std::sync::Arc::new(gray_rgba(320, 240));
	for i in 0..8u64 {
		let rgba = rgba.clone();
		std::thread::spawn(move || {
			if i == 0 {
				assert_eq!(moq_encode_video_cut(producer), 0);
			}
			let frame = moq_video_encoder_frame {
				timestamp_us: i * 33_333,
				data: rgba.as_ptr(),
				data_size: rgba.len(),
			};
			assert_eq!(unsafe { moq_encode_video_frame(producer, &frame) }, 0);
			assert_eq!(moq_encode_video_bitrate(producer, 900_000 - i), 0);
		})
		.join()
		.unwrap();
	}

	// ...and finished, so the encoder is drained and dropped, from yet another.
	std::thread::spawn(move || assert_eq!(moq_encode_video_finish(producer), 0))
		.join()
		.unwrap();

	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

/// Publish one mid-gray frame to a raw video producer, returning the status code.
fn publish_gray(producer: u32, rgba: &[u8]) -> i32 {
	let frame = moq_video_encoder_frame {
		timestamp_us: 0,
		data: rgba.as_ptr(),
		data_size: rgba.len(),
	};
	unsafe { moq_encode_video_frame(producer, &frame) }
}

/// Regression: an encode is a round trip to the codec thread, and a wedged codec
/// never comes back from it. It used to run under both of libmoq's process-wide
/// locks, the `State` mutex and one wrapping the runtime handle, so a single
/// stalled producer parked every unrelated call in the process behind it: another
/// broadcast's publish, a consumer's frame free, a session close.
///
/// Stalling the codec itself would take a test-only backend, so this holds the
/// per-producer lock an in-flight encode holds instead. From every other caller's
/// point of view that is the same wait.
#[test]
fn a_stalled_encode_does_not_block_unrelated_calls() {
	let origin = id(moq_origin_create());
	let broadcast = publish_broadcast(origin, b"video-raw-stall-test");

	let input = moq_video_encoder_input {
		format: moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_RGBA as u32,
		width: 320,
		height: 240,
		framerate: 30,
	};
	let output = moq_video_encoder_output {
		codec: moq_video_codec::MOQ_VIDEO_CODEC_H264 as u32,
		bitrate: 0,
		gop: 0,
		kind: moq_video_encoder_kind::MOQ_VIDEO_ENCODER_KIND_SOFTWARE as u32,
		encoder: std::ptr::null(),
		encoder_len: 0,
	};
	let stalled = id(unsafe { moq_encode_video(broadcast, &input, &output) });
	let other = id(unsafe { moq_encode_video(broadcast, &input, &output) });

	// Hold the lock a publish takes for the duration of its encode.
	let handle = State::lock().video.producer(Id::try_from(stalled).unwrap()).unwrap();
	let held = handle.lock();

	let rgba = std::sync::Arc::new(gray_rgba(320, 240));
	let stalling = {
		let rgba = rgba.clone();
		std::thread::spawn(move || publish_gray(stalled, &rgba))
	};

	// Wait until it has resolved the handle, so it is genuinely inside the stalled
	// call rather than still on its way in: the slab holds one reference, this test
	// a second, and the parked publish is the third.
	let deadline = std::time::Instant::now() + TIMEOUT;
	while handle.holders() < 3 {
		assert!(
			std::time::Instant::now() < deadline,
			"the publish never reached the encoder"
		);
		std::thread::yield_now();
	}

	// Unrelated work must not be queued behind it. On its own thread so a
	// regression fails this assertion rather than hanging the test.
	let (tx, rx) = mpsc::channel();
	let unrelated = {
		let rgba = rgba.clone();
		std::thread::spawn(move || {
			let _ = tx.send((moq_origin_create(), publish_gray(other, &rgba)));
		})
	};
	let (created, published) = rx
		.recv_timeout(TIMEOUT)
		.expect("an unrelated call was waiting on the stalled encode");
	assert!(created > 0, "creating an origin failed while a producer was stalled");
	assert_eq!(
		published, 0,
		"a second producer could not encode while the first stalled"
	);
	unrelated.join().unwrap();

	// ...and the stalled publish was still in flight the whole time, so the calls
	// above really did overlap it.
	assert_eq!(handle.holders(), 3, "the stalled publish finished early");

	drop(held);
	assert_eq!(stalling.join().unwrap(), 0);

	assert_eq!(moq_origin_close(id(created)), 0);
	assert_eq!(moq_encode_video_finish(stalled), 0);
	assert_eq!(moq_encode_video_finish(other), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

/// A raw video producer rejects a buffer that isn't one picture at the
/// configured resolution, rather than reinterpreting it.
#[test]
fn video_raw_publish_rejects_frame_size_mismatch() {
	let origin = id(moq_origin_create());
	let broadcast = publish_broadcast(origin, b"video-raw-mismatch-test");

	let input = moq_video_encoder_input {
		format: moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_RGBA as u32,
		width: 320,
		height: 240,
		framerate: 30,
	};
	let output = moq_video_encoder_output {
		codec: moq_video_codec::MOQ_VIDEO_CODEC_H264 as u32,
		bitrate: 0,
		gop: 0,
		kind: moq_video_encoder_kind::MOQ_VIDEO_ENCODER_KIND_SOFTWARE as u32,
		encoder: std::ptr::null(),
		encoder_len: 0,
	};
	let producer = id(unsafe { moq_encode_video(broadcast, &input, &output) });

	// A 640x480 buffer against a 320x240 encoder: the frame carries no dimensions
	// of its own, so this is caught as a wrong-sized picture.
	let rgba = gray_rgba(640, 480);
	let frame = moq_video_encoder_frame {
		timestamp_us: 0,
		data: rgba.as_ptr(),
		data_size: rgba.len(),
	};
	assert!(unsafe { moq_encode_video_frame(producer, &frame) } < 0);

	assert_eq!(moq_encode_video_finish(producer), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

/// Bad discriminants and pointers are rejected at the boundary rather than
/// reaching moq-video.
#[test]
fn video_raw_publish_rejects_invalid_config() {
	let origin = id(moq_origin_create());
	let broadcast = publish_broadcast(origin, b"video-raw-invalid-test");

	let valid_input = moq_video_encoder_input {
		format: moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_I420 as u32,
		width: 320,
		height: 240,
		framerate: 30,
	};
	let valid_output = moq_video_encoder_output {
		codec: moq_video_codec::MOQ_VIDEO_CODEC_H264 as u32,
		bitrate: 0,
		gop: 0,
		kind: moq_video_encoder_kind::MOQ_VIDEO_ENCODER_KIND_SOFTWARE as u32,
		encoder: std::ptr::null(),
		encoder_len: 0,
	};

	assert!(unsafe { moq_encode_video(broadcast, std::ptr::null(), &valid_output) } < 0);
	assert!(unsafe { moq_encode_video(broadcast, &valid_input, std::ptr::null()) } < 0);

	let bad_format = moq_video_encoder_input {
		format: 99,
		..valid_input
	};
	assert!(unsafe { moq_encode_video(broadcast, &bad_format, &valid_output) } < 0);

	let zero_framerate = moq_video_encoder_input {
		framerate: 0,
		..valid_input
	};
	assert!(unsafe { moq_encode_video(broadcast, &zero_framerate, &valid_output) } < 0);

	// Regression: dimensions arrive as a raw `u32` pair, and their product used to
	// overflow the default-bitrate estimate inside the encoder. A panic here is an
	// aborted host process, not an error return, since release builds are
	// `panic = "abort"`. It has to come back as a negative code.
	let unrepresentable = moq_video_encoder_input {
		width: u32::MAX - 1,
		height: u32::MAX - 1,
		..valid_input
	};
	assert!(unsafe { moq_encode_video(broadcast, &unrepresentable, &valid_output) } < 0);

	// A size no encoder can take, but whose arithmetic is fine, is the backend's
	// call rather than the boundary's: it must not be swept up by the check above.
	// Asserted on the reason, not just the code, since a backend refusing it looks
	// the same from the outside as the boundary refusing it.
	let merely_huge = moq_video_encoder_input {
		width: 65534,
		height: 65534,
		..valid_input
	};
	let huge = unsafe { moq_encode_video(broadcast, &merely_huge, &valid_output) };
	if huge > 0 {
		assert_eq!(moq_encode_video_finish(id(huge)), 0);
	} else {
		let reason = unsafe { std::ffi::CStr::from_ptr(moq_error()) }.to_str().unwrap();
		assert!(
			!reason.contains("too large to represent"),
			"the representability check rejected a size it should have left to the backend: {reason}"
		);
	}

	let bad_codec = moq_video_encoder_output {
		codec: 99,
		..valid_output
	};
	assert!(unsafe { moq_encode_video(broadcast, &valid_input, &bad_codec) } < 0);

	let bad_kind = moq_video_encoder_output {
		kind: 99,
		..valid_output
	};
	assert!(unsafe { moq_encode_video(broadcast, &valid_input, &bad_kind) } < 0);

	// Handles for a producer that was never created.
	assert!(moq_encode_video_cut(0) < 0);
	assert!(moq_encode_video_bitrate(0, 1_000_000) < 0);
	assert!(moq_encode_video_finish(0) < 0);

	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

/// End-to-end native decode: publish real H.264 (encoded by moq-video) and
/// consume it through `moq_decode_video`, asserting decoded I420 frames.
#[test]
fn video_raw_decode() {
	// Encode a few gray frames to Annex-B (avc3, SPS/PPS inline on the keyframe).
	let mut config = moq_video::encode::Config::new(320, 240, 30);
	config.kind = moq_video::encode::Kind::Software;
	let mut encoder = moq_video::encode::Encoder::new(&config).expect("openh264 encoder");
	let gray = vec![0x80u8; 320 * 240 * 4];
	let mut frames: Vec<moq_video::encode::Encoded> = Vec::new();
	for i in 0..5u64 {
		if i == 0 {
			encoder.keyframe();
		}
		let surface = moq_video::Surface::rgba(&gray, moq_video::Size::new(320, 240)).unwrap();
		let frame = moq_video::Frame::new(surface, moq_net::Timestamp::from_micros(i * 33_333).unwrap());
		frames.extend(encoder.encode(&frame).unwrap());
	}
	frames.extend(encoder.finish().unwrap());
	assert!(!frames.is_empty(), "encoder produced no frames");

	let origin = id(moq_origin_create());
	let path = b"video-raw-test";
	let broadcast = publish_broadcast(origin, path);

	// The init's SPS/PPS only seed catalog metadata; avc3 frames carry their own
	// inline parameter sets, so the decoder reads the true 320x240 from the wire.
	let init = h264_init();
	let format = moq_video_format::MOQ_VIDEO_FORMAT_AVC3;
	let media = id(publish_video(broadcast, format, &init, None));

	let consume = request_broadcast(origin, path);
	let catalog_cb = Callback::new();
	let catalog_task = id(unsafe { moq_consume_catalog(consume, Some(channel_callback), catalog_cb.ptr) });
	let catalog_id = id(catalog_cb.recv());

	// Subscribe + decode before publishing frames so the keyframe group is delivered.
	let output = moq_video_decoder_output { max_age_ms: 10_000 };
	let frame_cb = Callback::new();
	let consumer = id(unsafe { moq_decode_video(catalog_id, 0, &output, Some(channel_callback), frame_cb.ptr) });

	for (i, frame) in frames.iter().enumerate() {
		assert_eq!(
			unsafe { moq_publish_media_frame(media, frame.payload.as_ptr(), frame.payload.len(), (i as u64) * 33_000) },
			0
		);
	}

	// First decoded frame: packed I420 at the encoder resolution.
	let frame_id = id(frame_cb.recv());
	let mut frame = moq_video_frame {
		timestamp_us: 0,
		width: 0,
		height: 0,
		data: std::ptr::null(),
		data_size: 0,
	};
	assert_eq!(unsafe { moq_decode_video_frame(frame_id, &mut frame) }, 0);
	assert_eq!(frame.width, 320);
	assert_eq!(frame.height, 240);
	assert_eq!(frame.data_size, 320 * 240 * 3 / 2, "tightly-packed I420");
	assert!(!frame.data.is_null());

	assert_eq!(moq_decode_video_frame_free(frame_id), 0);
	assert_eq!(moq_decode_video_close(consumer), 0);

	// Drain any other decoded frames already queued, then expect the terminal 0.
	loop {
		let code = frame_cb.recv();
		if code > 0 {
			assert_eq!(moq_decode_video_frame_free(id(code)), 0);
		} else {
			assert_eq!(code, 0, "raw video close delivers terminal 0");
			break;
		}
	}
	assert_eq!(moq_consume_catalog_free(catalog_id), 0);
	assert_eq!(moq_consume_catalog_close(catalog_task), 0);
	// The publisher may emit more than one catalog snapshot (e.g. as the track's
	// stats settle), so drain any extra snapshots before the terminal.
	loop {
		let code = catalog_cb.recv();
		if code > 0 {
			assert_eq!(moq_consume_catalog_free(id(code)), 0);
		} else {
			assert_eq!(code, 0, "catalog close delivers terminal 0");
			break;
		}
	}
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_media_finish(media), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn multiple_frames_ordering() {
	let origin = id(moq_origin_create());
	let path = b"ordering-test";
	let broadcast = publish_broadcast(origin, path);

	let init = opus_head();
	let format = moq_audio_format::MOQ_AUDIO_FORMAT_OPUS;
	let media = id(publish_audio(broadcast, format, &init, None));

	let consume = request_broadcast(origin, path);
	let catalog_cb = Callback::new();
	let catalog_task = id(unsafe { moq_consume_catalog(consume, Some(channel_callback), catalog_cb.ptr) });
	let catalog_id = id(catalog_cb.recv());

	let frame_cb = Callback::new();
	let track = id(unsafe { moq_consume_audio(catalog_id, 0, 10_000, Some(channel_callback), frame_cb.ptr) });

	let timestamps: [u64; 5] = [0, 20_000, 40_000, 60_000, 80_000];
	for (i, &ts) in timestamps.iter().enumerate() {
		let payload = format!("frame-{i}");
		assert_eq!(
			unsafe { moq_publish_media_frame(media, payload.as_ptr(), payload.len(), ts) },
			0
		);
	}

	for (i, &expected_ts) in timestamps.iter().enumerate() {
		let frame_id = id(frame_cb.recv());
		let mut frame = moq_frame {
			payload: std::ptr::null(),
			payload_size: 0,
			timestamp_us: 0,
			keyframe: false,
		};
		assert_eq!(unsafe { moq_consume_frame(frame_id, &mut frame) }, 0);
		assert_eq!(frame.timestamp_us, expected_ts, "frame {i} has wrong timestamp");

		let received = unsafe { std::slice::from_raw_parts(frame.payload, frame.payload_size) };
		let expected = format!("frame-{i}");
		assert_eq!(received, expected.as_bytes(), "frame {i} has wrong payload");

		assert_eq!(moq_consume_frame_free(frame_id), 0);
	}

	assert_eq!(moq_consume_audio_close(track), 0);
	assert_eq!(frame_cb.recv_terminal(), 0, "audio close delivers terminal 0");
	assert_eq!(moq_consume_catalog_free(catalog_id), 0);
	assert_eq!(moq_consume_catalog_close(catalog_task), 0);
	assert_eq!(
		catalog_cb.recv_catalog_terminal(),
		0,
		"catalog close delivers terminal 0"
	);
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_media_finish(media), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn catalog_update_on_new_track() {
	let origin = id(moq_origin_create());
	let path = b"catalog-update";
	let broadcast = publish_broadcast(origin, path);

	let init = opus_head();
	let format = moq_audio_format::MOQ_AUDIO_FORMAT_OPUS;
	let media1 = id(publish_audio(broadcast, format, &init, None));

	let consume = request_broadcast(origin, path);
	let catalog_cb = Callback::new();
	let catalog_task = id(unsafe { moq_consume_catalog(consume, Some(channel_callback), catalog_cb.ptr) });

	let catalog_id1 = id(catalog_cb.recv());
	let mut audio_cfg = moq_audio_config {
		name: std::ptr::null(),
		name_len: 0,
		label: std::ptr::null(),
		label_len: 0,
		codec: std::ptr::null(),
		codec_len: 0,
		description: std::ptr::null(),
		description_len: 0,
		sample_rate: 0,
		channel_count: 0,
		container: moq_container::default(),
	};
	assert_eq!(unsafe { moq_consume_audio_config(catalog_id1, 0, &mut audio_cfg) }, 0);
	assert!(unsafe { moq_consume_audio_config(catalog_id1, 1, &mut audio_cfg) } < 0);

	let media2 = id(publish_audio(broadcast, format, &init, None));

	let catalog_id2 = id(catalog_cb.recv());

	assert_eq!(unsafe { moq_consume_audio_config(catalog_id2, 0, &mut audio_cfg) }, 0);
	assert_eq!(unsafe { moq_consume_audio_config(catalog_id2, 1, &mut audio_cfg) }, 0);

	assert_eq!(moq_consume_catalog_free(catalog_id1), 0);
	assert_eq!(moq_consume_catalog_free(catalog_id2), 0);
	assert_eq!(moq_consume_catalog_close(catalog_task), 0);
	assert_eq!(catalog_cb.recv_terminal(), 0, "catalog close delivers terminal 0");
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_media_finish(media1), 0);
	assert_eq!(moq_publish_media_finish(media2), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn null_pointer_handling() {
	assert_eq!(
		unsafe { moq_consume_frame(9999, std::ptr::null_mut()) },
		-6,
		"null dst should return InvalidPointer (-6)"
	);
	assert_eq!(
		unsafe { moq_consume_video_config(9999, 0, std::ptr::null_mut()) },
		-6,
		"null dst should return InvalidPointer (-6)"
	);
	assert_eq!(
		unsafe { moq_consume_audio_config(9999, 0, std::ptr::null_mut()) },
		-6,
		"null dst should return InvalidPointer (-6)"
	);
	assert_eq!(
		unsafe { moq_origin_announced_info(9999, std::ptr::null_mut()) },
		-6,
		"null dst should return InvalidPointer (-6)"
	);
}

#[test]
fn session_connect_invalid_url() {
	let url = b"not a valid url!!!";
	let ret = unsafe {
		moq_session_connect(
			url.as_ptr() as *const c_char,
			url.len(),
			std::ptr::null(),
			0,
			0,
			None,
			std::ptr::null_mut(),
		)
	};
	assert!(ret < 0, "connecting with an invalid URL should fail immediately");
}

#[test]
fn session_connect_and_close() {
	let cb = Callback::new();
	let url = b"moqt://localhost:1";
	let session = id(unsafe {
		moq_session_connect(
			url.as_ptr() as *const c_char,
			url.len(),
			std::ptr::null(),
			0,
			0,
			Some(channel_callback),
			cb.ptr,
		)
	});

	// close() requests shutdown; the task still delivers exactly one terminal
	// callback (0 = clean close, or a negative connect error), after which
	// user_data is safe to free.
	assert_eq!(moq_session_close(session), 0);
	assert!(cb.recv() <= 0, "session close delivers a terminal code");
}

/// Borrow a `&str` as the `moq_string` the list setters take.
fn moq_str(s: &str) -> moq_string {
	moq_string {
		data: s.as_ptr() as *const c_char,
		len: s.len(),
	}
}

/// A zeroed config, which is what a C caller gets from `memset` or `{0}` and must
/// mean "the defaults" for every knob.
fn client_config() -> moq_client_config {
	unsafe { std::mem::zeroed() }
}

/// Dial `moqt://localhost:1` with `config` and return the raw status.
fn dial(config: Option<&moq_client_config>) -> i32 {
	let url = b"moqt://localhost:1";
	unsafe {
		moq_session_connect(
			url.as_ptr() as *const c_char,
			url.len(),
			config.map_or(std::ptr::null(), |c| c as *const _),
			0,
			0,
			None,
			std::ptr::null_mut(),
		)
	}
}

/// The native config a zeroed struct produces, which is the thing every default
/// assertion below is really about.
fn parsed(config: &moq_client_config) -> crate::client::Config {
	unsafe { crate::parse_client(Some(config)) }.expect("config should parse")
}

#[test]
fn a_null_config_dials_with_the_defaults() {
	let defaults = crate::client::Config::default();
	let parsed = unsafe { crate::parse_client(None) }.expect("NULL is the defaults");
	assert_eq!(parsed.connect.backoff.initial(), defaults.connect.backoff.initial());
	assert_eq!(
		parsed.connect.websocket.resolved_enabled(),
		defaults.connect.websocket.resolved_enabled()
	);
}

/// The whole point of the `has_*` flags: a caller who zeroes the struct and sets
/// nothing must land on the defaults, not on zero. The backoff and the WebSocket
/// fallback are the ones with non-zero defaults, so they are what would break.
#[test]
fn a_zeroed_config_is_the_defaults() {
	let defaults = crate::client::Config::default();
	let parsed = parsed(&client_config());

	assert_eq!(parsed.connect.backoff.initial(), defaults.connect.backoff.initial());
	assert_eq!(
		parsed.connect.backoff.multiplier(),
		defaults.connect.backoff.multiplier()
	);
	assert_eq!(parsed.connect.backoff.max(), defaults.connect.backoff.max());
	assert_eq!(parsed.connect.backoff.timeout(), defaults.connect.backoff.timeout());
	assert_eq!(
		parsed.connect.websocket.resolved_enabled(),
		defaults.connect.websocket.resolved_enabled()
	);
	assert_eq!(
		parsed.connect.websocket.resolved_delay(),
		defaults.connect.websocket.resolved_delay()
	);
	assert_eq!(parsed.connect.version, defaults.connect.version);
	assert_eq!(parsed.connect.bind, defaults.connect.bind);
	assert!(parsed.connect.tls.fingerprint.is_empty());
	assert!(parsed.connect.tls.root.is_empty());
	assert!(parsed.quic.gso.is_none());
	assert!(parsed.quic.mtu_discovery.is_none());
}

/// `moq_client_defaults` has to agree with what a zeroed struct actually dials,
/// or a settings UI shows numbers the library won't use. Pinned against the
/// config each value comes from, so retuning a default without following through
/// to C fails right here.
#[test]
fn defaults_report_what_a_zeroed_config_dials() {
	let config = moq_client_defaults();

	let expected = crate::client::Config::default();
	let quic = moq_tokio::quic::Resolved::default();

	assert!(config.has_connect_timeout);
	assert_eq!(
		config.connect_timeout_ms,
		expected.connect.resolved_timeout().as_millis() as u64
	);
	assert!(config.has_failover_delay);
	assert_eq!(
		config.failover_delay_ms,
		expected.connect.resolved_race().as_millis() as u64
	);
	assert!(config.has_resolution_delay);
	assert_eq!(
		config.resolution_delay_ms,
		expected.connect.resolved_resolution_delay().as_millis() as u64
	);

	assert!(config.has_backoff_initial);
	assert_eq!(
		config.backoff_initial_ms,
		expected.connect.backoff.initial().as_millis() as u64
	);
	assert!(config.has_backoff_multiplier);
	assert_eq!(config.backoff_multiplier, expected.connect.backoff.multiplier());
	assert!(config.has_backoff_max);
	assert_eq!(config.backoff_max_ms, expected.connect.backoff.max().as_millis() as u64);
	assert!(config.has_backoff_timeout);
	assert_eq!(
		config.backoff_timeout_ms,
		expected.connect.backoff.timeout().as_millis() as u64
	);

	assert!(config.has_websocket_enabled);
	assert_eq!(config.websocket_enabled, expected.connect.websocket.resolved_enabled());
	assert!(config.has_websocket_delay);
	assert_eq!(
		config.websocket_delay_ms,
		expected.connect.websocket.resolved_delay().as_millis() as u64
	);

	assert!(config.has_quic_max_streams);
	assert_eq!(config.quic_max_streams, quic.max_streams);
	assert!(config.has_quic_idle_timeout);
	assert_eq!(config.quic_idle_timeout_ms, quic.idle_timeout.as_millis() as u64);
	assert_eq!(
		config.has_quic_keep_alive.then_some(config.quic_keep_alive_ms),
		quic.keep_alive.map(|d| d.as_millis() as u64)
	);

	// The backend-dependent knobs have no single value to report, so they come
	// back unset rather than guessing one.
	assert!(!config.has_quic_gso);
	assert!(!config.has_quic_mtu_discovery);
	assert!(!config.has_tls_system_roots);
	assert!(config.quic_congestion_control.is_null());

	// And what it reports must round-trip: dialing with it is dialing with the defaults.
	let expected = crate::client::Config::default();
	let reparsed = parsed(&config);
	assert_eq!(reparsed.connect.backoff.initial(), expected.connect.backoff.initial());
	assert_eq!(
		reparsed.connect.websocket.resolved_enabled(),
		expected.connect.websocket.resolved_enabled()
	);
}

/// Zero is a real setting for these two, not "unset": retry forever, and no
/// keep-alive pings. The flag is what carries that distinction.
#[test]
fn zero_with_a_flag_set_is_a_real_value() {
	let mut config = client_config();
	config.backoff_timeout_ms = 0;
	config.has_backoff_timeout = true;
	config.quic_keep_alive_ms = 0;
	config.has_quic_keep_alive = true;

	let explicit = parsed(&config);
	assert_eq!(explicit.connect.backoff.timeout(), std::time::Duration::ZERO);
	assert_eq!(explicit.quic.keep_alive, Some(std::time::Duration::ZERO));

	// Without the flags the same zeroes mean nothing at all.
	let defaults = parsed(&client_config());
	assert_ne!(defaults.connect.backoff.timeout(), std::time::Duration::ZERO);
	assert_eq!(defaults.quic.keep_alive, None);
}

#[test]
fn config_versions_round_trip() {
	let versions = [moq_str("moq-lite-05"), moq_str("moq-transport-19")];
	let mut config = client_config();
	config.versions = versions.as_ptr();
	config.versions_len = versions.len();
	assert_eq!(parsed(&config).connect.version.len(), 2);

	// An empty list is no pin at all, so the default set is offered.
	let mut config = client_config();
	config.versions_len = 0;
	assert_eq!(
		parsed(&config).connect.version,
		crate::client::Config::default().connect.version
	);
}

#[test]
fn config_rejects_an_unknown_version() {
	let versions = [moq_str("moq-lite-05"), moq_str("moq-carrier-pigeon-01")];
	let mut config = client_config();
	config.versions = versions.as_ptr();
	config.versions_len = versions.len();

	assert_eq!(dial(Some(&config)), Error::InvalidConfig(String::new()).code());
}

#[test]
fn config_rejects_a_bad_bind_address() {
	let good = "127.0.0.1:0";
	let mut config = client_config();
	config.bind = good.as_ptr() as *const c_char;
	config.bind_len = good.len();
	assert_eq!(parsed(&config).connect.bind, Some(good.parse().unwrap()));

	let bad = "not-an-address";
	let mut config = client_config();
	config.bind = bad.as_ptr() as *const c_char;
	config.bind_len = bad.len();
	assert_eq!(dial(Some(&config)), Error::InvalidConfig(String::new()).code());
}

#[test]
fn config_optional_strings_are_unset_when_null_or_empty() {
	let name = "relay.example.com";
	let mut config = client_config();
	config.tls_host_name = name.as_ptr() as *const c_char;
	config.tls_host_name_len = name.len();
	assert_eq!(parsed(&config).connect.tls.host_name.as_deref(), Some(name));

	// NULL and empty both mean "unset".
	let config = client_config();
	assert_eq!(parsed(&config).connect.tls.host_name, None);

	let mut config = client_config();
	config.tls_host_name = "".as_ptr() as *const c_char;
	config.tls_host_name_len = 0;
	assert_eq!(parsed(&config).connect.tls.host_name, None);
}

#[test]
fn config_rejects_malformed_tls_fingerprints() {
	for invalid in ["not-hex", "abcd"] {
		let fingerprints = [moq_str(invalid)];
		let mut config = client_config();
		config.tls_fingerprints = fingerprints.as_ptr();
		config.tls_fingerprints_len = fingerprints.len();
		assert_eq!(dial(Some(&config)), Error::InvalidConfig(String::new()).code());
	}

	let valid_value = "ab".repeat(32);
	let valid = [moq_str(&valid_value)];
	let mut config = client_config();
	config.tls_fingerprints = valid.as_ptr();
	config.tls_fingerprints_len = valid.len();
	assert_eq!(parsed(&config).connect.tls.fingerprint, vec![valid_value]);
}

#[test]
fn config_rejects_unknown_congestion_control() {
	let bogus = "sideways";
	let mut config = client_config();
	config.quic_congestion_control = bogus.as_ptr() as *const c_char;
	config.quic_congestion_control_len = bogus.len();
	assert_eq!(dial(Some(&config)), Error::InvalidConfig(String::new()).code());

	let delay = "delay";
	let mut config = client_config();
	config.quic_congestion_control = delay.as_ptr() as *const c_char;
	config.quic_congestion_control_len = delay.len();
	assert!(parsed(&config).quic.congestion_control.is_some());

	// NULL leaves it on the backend default, so the knob can stay automatic.
	assert!(parsed(&client_config()).quic.congestion_control.is_none());
}

/// Every QUIC and backoff knob lands where it should. A new one is a new field on
/// the end of the struct, which a zeroed caller never notices.
#[test]
fn config_quic_and_backoff_knobs_apply() {
	let mut config = client_config();
	config.backoff_initial_ms = 500;
	config.has_backoff_initial = true;
	config.backoff_multiplier = 3;
	config.has_backoff_multiplier = true;
	config.backoff_max_ms = 10_000;
	config.has_backoff_max = true;

	config.quic_max_streams = 4096;
	config.has_quic_max_streams = true;
	config.quic_idle_timeout_ms = 15_000;
	config.has_quic_idle_timeout = true;
	config.quic_gso = false;
	config.has_quic_gso = true;
	config.quic_mtu_discovery = true;
	config.has_quic_mtu_discovery = true;

	let dir = "/tmp/qlog";
	config.quic_qlog = dir.as_ptr() as *const c_char;
	config.quic_qlog_len = dir.len();

	let parsed = parsed(&config);
	assert_eq!(parsed.connect.backoff.initial(), std::time::Duration::from_millis(500));
	assert_eq!(parsed.connect.backoff.multiplier(), 3);
	assert_eq!(parsed.connect.backoff.max(), std::time::Duration::from_millis(10_000));
	assert_eq!(parsed.quic.max_streams, Some(4096));
	assert_eq!(parsed.quic.idle_timeout, Some(std::time::Duration::from_millis(15_000)));
	assert_eq!(parsed.quic.gso, Some(false));
	assert_eq!(parsed.quic.mtu_discovery, Some(true));
	assert_eq!(parsed.quic.qlog.as_deref(), Some(std::path::Path::new(dir)));
}

/// An idle timeout outside QUIC's millisecond varint is an ordinary configuration
/// error, and later calls remain usable.
#[test]
fn dial_rejects_an_unrepresentable_idle_timeout() {
	let mut config = client_config();
	config.quic_idle_timeout_ms = u64::MAX;
	config.has_quic_idle_timeout = true;

	assert_eq!(dial(Some(&config)), Error::InvalidConfig(String::new()).code());

	// A rejected dial leaves the library usable.
	assert!(moq_client_defaults().has_connect_timeout);
}

/// The backend variants are feature-gated, so a hardcoded menu offers options this
/// build rejects. Every name reported must be one a dial takes, same contract as
/// `moq_versions`.
#[test]
fn backends_lists_only_what_a_dial_accepts() {
	let count = unsafe { moq_backends(std::ptr::null_mut(), 0) };
	assert!(count > 0, "expected at least one compiled backend, got {count}");

	let mut names = vec![
		moq_string {
			data: std::ptr::null(),
			len: 0
		};
		count as usize
	];
	assert_eq!(unsafe { moq_backends(names.as_mut_ptr(), names.len()) }, count);

	let accepts = |name: &str| {
		let mut config = client_config();
		config.backend = name.as_ptr() as *const c_char;
		config.backend_len = name.len();
		unsafe { crate::parse_client(Some(&config)) }.is_ok()
	};

	for name in &names {
		let name = unsafe { ffi::parse_str(name.data, name.len) }.expect("backend name is UTF-8");
		assert!(accepts(name), "listed backend {name} must be settable");
	}

	// And the converse: a backend this build lacks is not listed, so the menu can't
	// offer a dead option.
	for candidate in ["quinn", "quiche", "noq"] {
		let listed = names.iter().any(|n| {
			unsafe { ffi::parse_str(n.data, n.len) }
				.map(|s| s == candidate)
				.unwrap_or(false)
		});
		assert_eq!(
			listed,
			accepts(candidate),
			"{candidate}: listed and accepted must agree"
		);
	}
}

/// Whether a qlog directory works at all is a compile-time feature, so the capability
/// has to agree with what a dial does. Both branches matter: `just check` runs without
/// `--all-features` and only ever sees the unsupported one, while CI runs with them and
/// only sees the supported one.
#[test]
fn qlog_support_matches_what_a_dial_accepts() {
	// A real directory: with capture compiled in, the dial creates a trace file inside
	// it, so a path that doesn't exist would fail for that reason instead of the one
	// under test. The pid keeps concurrent test binaries out of each other's way.
	let dir = std::env::temp_dir().join(format!("moq-qlog-test-{}", std::process::id()));
	std::fs::create_dir_all(&dir).expect("create the qlog directory");
	let path = dir.to_str().expect("temp dir is UTF-8").to_string();

	let mut config = client_config();
	config.quic_qlog = path.as_ptr() as *const c_char;
	config.quic_qlog_len = path.len();

	// Parsing stores the path either way; the dial is what rejects it.
	assert!(unsafe { crate::parse_client(Some(&config)) }.is_ok());

	let ret = dial(Some(&config));
	match moq_qlog_supported() {
		true => {
			assert!(ret > 0, "qlog is supported, so the dial must start: {ret}");
			moq_session_close(id(ret));
		}
		false => assert!(ret < 0, "qlog is unsupported, so the dial must be refused"),
	}

	std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn versions_lists_the_offered_set() {
	let count = unsafe { moq_versions(std::ptr::null_mut(), 0) };
	assert!(count > 0, "expected at least one offered version, got {count}");

	let mut names = vec![
		moq_string {
			data: std::ptr::null(),
			len: 0
		};
		count as usize
	];
	assert_eq!(unsafe { moq_versions(names.as_mut_ptr(), names.len()) }, count);

	for name in &names {
		let name = unsafe { ffi::parse_str(name.data, name.len) }.expect("version name is UTF-8");
		// Every listed name must be one a dial accepts, or the menu it builds is a lie.
		let one = [moq_str(name)];
		let mut config = client_config();
		config.versions = one.as_ptr();
		config.versions_len = one.len();
		assert!(
			unsafe { crate::parse_client(Some(&config)) }.is_ok(),
			"listed version {name} must be settable"
		);
	}
}

#[test]
fn dial_applies_the_config() {
	let versions = [moq_str("moq-lite-05")];
	let mut config = client_config();
	config.versions = versions.as_ptr();
	config.versions_len = versions.len();
	config.connect_timeout_ms = 100;
	config.has_connect_timeout = true;

	let cb = Callback::new();
	let url = b"moqt://localhost:1";
	let session = id(unsafe {
		moq_session_connect(
			url.as_ptr() as *const c_char,
			url.len(),
			&config,
			0,
			0,
			Some(channel_callback),
			cb.ptr,
		)
	});

	assert_eq!(moq_session_close(session), 0);
	assert!(cb.recv() <= 0, "session close delivers a terminal code");
}
