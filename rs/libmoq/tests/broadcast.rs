//! Integration tests for the libmoq C API.
//!
//! Tests the full publish/consume pipeline using the FFI functions,
//! exercising local origin-based pub/sub without requiring a network connection.

use std::ffi::{c_char, c_void};
use std::sync::mpsc;
use std::time::Duration;

use moq::*;

const TIMEOUT: Duration = Duration::from_secs(10);

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

/// Allocate a [`mpsc::Sender`] on the heap and return the receiver plus a
/// raw pointer suitable for passing as `user_data` to FFI callbacks.
fn make_callback() -> (mpsc::Receiver<i32>, *mut c_void) {
	let (tx, rx) = mpsc::channel();
	let ptr = Box::into_raw(Box::new(tx));
	(rx, ptr as *mut c_void)
}

/// Free a heap-allocated sender created by [`make_callback`].
///
/// # Safety
/// Must only be called once per pointer returned by `make_callback`,
/// and only after the callback will no longer fire.
unsafe fn free_callback(ptr: *mut c_void) {
	drop(unsafe { Box::from_raw(ptr as *mut mpsc::Sender<i32>) });
}

/// FFI callback that forwards the status code through an `mpsc::Sender`.
extern "C" fn channel_callback(user_data: *mut c_void, code: i32) {
	let tx = unsafe { &*(user_data as *const mpsc::Sender<i32>) };
	let _ = tx.send(code);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn origin_lifecycle() {
	let origin = moq_origin_create();
	assert!(origin > 0, "moq_origin_create should return a positive id");

	let ret = moq_origin_close(origin as u32);
	assert_eq!(ret, 0, "moq_origin_close should succeed");

	// Closing again should fail.
	let ret = moq_origin_close(origin as u32);
	assert!(ret < 0, "double-close should fail");
}

#[test]
fn publish_lifecycle() {
	let broadcast = moq_publish_create();
	assert!(broadcast > 0, "moq_publish_create should return a positive id");

	// Create an opus media track.
	let init = opus_head();
	let format = b"opus";
	let media = unsafe {
		moq_publish_media_ordered(
			broadcast as u32,
			format.as_ptr() as *const c_char,
			format.len(),
			init.as_ptr(),
			init.len(),
		)
	};
	assert!(media > 0, "moq_publish_media_ordered should return a positive id");

	// Write a frame.
	let payload = b"opus frame";
	let ret = unsafe { moq_publish_media_frame(media as u32, payload.as_ptr(), payload.len(), 1000) };
	assert_eq!(ret, 0, "moq_publish_media_frame should succeed");

	// Close media, then broadcast.
	assert_eq!(moq_publish_media_close(media as u32), 0);
	assert_eq!(moq_publish_close(broadcast as u32), 0);
}

#[test]
fn invalid_ids() {
	// Non-existent resources.
	assert!(moq_origin_close(9999) < 0);
	assert!(moq_session_close(9999) < 0);
	assert!(moq_publish_close(9999) < 0);
	assert!(moq_consume_close(9999) < 0);
	assert!(moq_consume_frame_close(9999) < 0);

	// ID zero is always invalid.
	assert!(moq_origin_close(0) < 0);
	assert!(moq_session_close(0) < 0);
	assert!(moq_publish_close(0) < 0);
}

#[test]
fn unknown_format() {
	let broadcast = moq_publish_create();
	assert!(broadcast > 0);

	let format = b"nope";
	let ret = unsafe {
		moq_publish_media_ordered(
			broadcast as u32,
			format.as_ptr() as *const c_char,
			format.len(),
			std::ptr::null(),
			0,
		)
	};
	assert!(ret < 0, "unknown format should fail");

	assert_eq!(moq_publish_close(broadcast as u32), 0);
}

#[test]
fn local_announce() {
	let origin = moq_origin_create();
	assert!(origin > 0);

	// Listen for announcements.
	let (rx, cb_ptr) = make_callback();
	let announced_task = unsafe { moq_origin_announced(origin as u32, Some(channel_callback), cb_ptr) };
	assert!(announced_task > 0, "moq_origin_announced should return a positive id");

	// Create and publish a broadcast.
	let broadcast = moq_publish_create();
	assert!(broadcast > 0);

	let path = b"test/broadcast";
	let ret = unsafe {
		moq_origin_publish(origin as u32, path.as_ptr() as *const c_char, path.len(), broadcast as u32)
	};
	assert_eq!(ret, 0, "moq_origin_publish should succeed");

	// Wait for the announcement callback.
	let announced_id = rx.recv_timeout(TIMEOUT).expect("announcement timed out");
	assert!(announced_id > 0, "announced callback should deliver a positive id");

	// Query announcement info.
	let mut info = moq_announced {
		path: std::ptr::null(),
		path_len: 0,
		active: false,
	};
	let ret = unsafe { moq_origin_announced_info(announced_id as u32, &mut info) };
	assert_eq!(ret, 0, "moq_origin_announced_info should succeed");
	assert!(info.active, "broadcast should be active");

	let announced_path =
		unsafe { std::str::from_utf8(std::slice::from_raw_parts(info.path as *const u8, info.path_len)).unwrap() };
	assert_eq!(announced_path, "test/broadcast");

	// Cleanup.
	assert_eq!(moq_origin_announced_close(announced_task as u32), 0);
	assert_eq!(moq_publish_close(broadcast as u32), 0);
	assert_eq!(moq_origin_close(origin as u32), 0);
	unsafe { free_callback(cb_ptr) };
}

#[test]
fn local_publish_consume() {
	// ── publisher ──────────────────────────────────────────────────
	let origin = moq_origin_create();
	assert!(origin > 0);

	let broadcast = moq_publish_create();
	assert!(broadcast > 0);

	let init = opus_head();
	let format = b"opus";
	let media = unsafe {
		moq_publish_media_ordered(
			broadcast as u32,
			format.as_ptr() as *const c_char,
			format.len(),
			init.as_ptr(),
			init.len(),
		)
	};
	assert!(media > 0, "media track creation should succeed");

	// Publish broadcast to the origin.
	let path = b"live";
	let ret = unsafe {
		moq_origin_publish(origin as u32, path.as_ptr() as *const c_char, path.len(), broadcast as u32)
	};
	assert_eq!(ret, 0);

	// ── consumer ───────────────────────────────────────────────────
	let consume = unsafe { moq_origin_consume(origin as u32, path.as_ptr() as *const c_char, path.len()) };
	assert!(consume > 0, "moq_origin_consume should succeed");

	// Subscribe to the catalog.
	let (catalog_rx, catalog_cb) = make_callback();
	let catalog_task = unsafe { moq_consume_catalog(consume as u32, Some(channel_callback), catalog_cb) };
	assert!(catalog_task > 0);

	// The catalog should arrive promptly (opus track was already created).
	let catalog_id = catalog_rx.recv_timeout(TIMEOUT).expect("catalog timed out");
	assert!(catalog_id > 0, "catalog callback should deliver a positive id");

	// Query audio config.
	let mut audio_cfg = moq_audio_config {
		name: std::ptr::null(),
		name_len: 0,
		codec: std::ptr::null(),
		codec_len: 0,
		description: std::ptr::null(),
		description_len: 0,
		sample_rate: 0,
		channel_count: 0,
	};
	let ret = unsafe { moq_consume_audio_config(catalog_id as u32, 0, &mut audio_cfg) };
	assert_eq!(ret, 0, "moq_consume_audio_config should succeed");
	assert_eq!(audio_cfg.sample_rate, 48000);
	assert_eq!(audio_cfg.channel_count, 2);

	let codec =
		unsafe { std::str::from_utf8(std::slice::from_raw_parts(audio_cfg.codec as *const u8, audio_cfg.codec_len)) }
			.unwrap();
	assert_eq!(codec, "opus");

	// No video tracks in this broadcast.
	let mut video_cfg = moq_video_config {
		name: std::ptr::null(),
		name_len: 0,
		codec: std::ptr::null(),
		codec_len: 0,
		description: std::ptr::null(),
		description_len: 0,
		coded_width: std::ptr::null(),
		coded_height: std::ptr::null(),
	};
	let ret = unsafe { moq_consume_video_config(catalog_id as u32, 0, &mut video_cfg) };
	assert!(ret < 0, "video config should fail (no video tracks)");

	// Subscribe to the audio track.
	let (frame_rx, frame_cb) = make_callback();
	let track = unsafe { moq_consume_audio_ordered(catalog_id as u32, 0, 10_000, Some(channel_callback), frame_cb) };
	assert!(track > 0);

	// Write a frame after subscribing so the consumer definitely sees it.
	let payload = b"opus audio payload data";
	let timestamp_us: u64 = 1_000_000; // 1 second
	let ret = unsafe { moq_publish_media_frame(media as u32, payload.as_ptr(), payload.len(), timestamp_us) };
	assert_eq!(ret, 0);

	// Wait for the frame callback.
	let frame_id = frame_rx.recv_timeout(TIMEOUT).expect("frame callback timed out");
	assert!(frame_id > 0, "frame callback should deliver a positive id");

	// Read frame chunk and verify payload.
	let mut frame = moq_frame {
		payload: std::ptr::null(),
		payload_size: 0,
		timestamp_us: 0,
		keyframe: false,
	};
	let ret = unsafe { moq_consume_frame_chunk(frame_id as u32, 0, &mut frame) };
	assert_eq!(ret, 0, "moq_consume_frame_chunk should succeed");
	assert_eq!(frame.payload_size, payload.len());
	assert_eq!(frame.timestamp_us, timestamp_us);

	let received = unsafe { std::slice::from_raw_parts(frame.payload, frame.payload_size) };
	assert_eq!(received, payload, "frame payload should match");

	// Out-of-bounds chunk index should fail.
	let ret = unsafe { moq_consume_frame_chunk(frame_id as u32, 999, &mut frame) };
	assert!(ret < 0, "out-of-bounds chunk index should fail");

	// ── cleanup ────────────────────────────────────────────────────
	assert_eq!(moq_consume_frame_close(frame_id as u32), 0);
	assert_eq!(moq_consume_audio_close(track as u32), 0);
	assert_eq!(moq_consume_catalog_free(catalog_id as u32), 0);
	assert_eq!(moq_consume_catalog_close(catalog_task as u32), 0);
	assert_eq!(moq_consume_close(consume as u32), 0);
	assert_eq!(moq_publish_media_close(media as u32), 0);
	assert_eq!(moq_publish_close(broadcast as u32), 0);
	assert_eq!(moq_origin_close(origin as u32), 0);
	unsafe {
		free_callback(catalog_cb);
		free_callback(frame_cb);
	}
}

#[test]
fn session_connect_invalid_url() {
	let url = b"not a valid url!!!";
	let ret = unsafe {
		moq_session_connect(
			url.as_ptr() as *const c_char,
			url.len(),
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
	// Connect to a URL that will fail asynchronously (nothing listening).
	let (rx, cb_ptr) = make_callback();
	let url = b"moqt://localhost:1";
	let session = unsafe {
		moq_session_connect(
			url.as_ptr() as *const c_char,
			url.len(),
			0,
			0,
			Some(channel_callback),
			cb_ptr,
		)
	};
	assert!(session > 0, "moq_session_connect should return a session id");

	// Close the session immediately — the callback must NOT fire after close.
	assert_eq!(moq_session_close(session as u32), 0);

	// Give the runtime a moment, then verify no callback arrived.
	assert!(
		rx.recv_timeout(Duration::from_millis(200)).is_err(),
		"callback should not fire after session_close"
	);

	unsafe { free_callback(cb_ptr) };
}
