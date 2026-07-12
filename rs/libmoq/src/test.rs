use super::*;

use std::ffi::{c_char, c_void};
use std::sync::mpsc;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

/// Convert a positive `i32` return value to `u32`, panicking on error.
fn id(raw: i32) -> u32 {
	assert!(raw > 0, "expected positive id, got {raw}");
	raw as u32
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
	let broadcast = id(moq_publish_create());
	let _guard = Guard(Some(|| {
		moq_publish_close(broadcast);
	}));

	let init = opus_head();
	let format = b"opus";
	let media = id(unsafe {
		moq_publish_media_ordered(
			broadcast,
			format.as_ptr() as *const c_char,
			format.len(),
			init.as_ptr(),
			init.len(),
		)
	});

	let payload = b"opus frame";
	let ret = unsafe { moq_publish_media_frame(media, payload.as_ptr(), payload.len(), 1000) };
	assert_eq!(ret, 0, "moq_publish_media_frame should succeed");

	assert_eq!(moq_publish_media_close(media), 0);
	assert_eq!(moq_publish_close(broadcast), 0);
}

#[test]
fn publish_catalog_config_invalid_broadcast() {
	let name = "video";
	let codec = "vp8";
	let video = moq_video_config {
		name: name.as_ptr() as *const c_char,
		name_len: name.len(),
		codec: codec.as_ptr() as *const c_char,
		codec_len: codec.len(),
		description: std::ptr::null(),
		description_len: 0,
		coded_width: std::ptr::null(),
		coded_height: std::ptr::null(),
	};
	assert!(unsafe { moq_publish_video_config(0, &video) } < 0);

	let audio_codec = "opus";
	let audio = moq_audio_config {
		name: name.as_ptr() as *const c_char,
		name_len: name.len(),
		codec: audio_codec.as_ptr() as *const c_char,
		codec_len: audio_codec.len(),
		description: std::ptr::null(),
		description_len: 0,
		sample_rate: 48000,
		channel_count: 2,
	};
	assert!(unsafe { moq_publish_audio_config(0, &audio) } < 0);

	assert!(unsafe { moq_publish_video_remove(0, name.as_ptr() as *const c_char, name.len()) } < 0);
	assert!(unsafe { moq_publish_audio_remove(0, name.as_ptr() as *const c_char, name.len()) } < 0);
}

#[test]
fn publish_catalog_config_null_pointer() {
	let broadcast = id(moq_publish_create());
	assert_eq!(
		unsafe { moq_publish_video_config(broadcast, std::ptr::null()) },
		-6,
		"null config should return InvalidPointer (-6)"
	);
	assert_eq!(
		unsafe { moq_publish_audio_config(broadcast, std::ptr::null()) },
		-6,
		"null config should return InvalidPointer (-6)"
	);
	assert_eq!(moq_publish_close(broadcast), 0);
}

#[test]
fn publish_catalog_roundtrip() {
	let origin = id(moq_origin_create());
	let broadcast = id(moq_publish_create());

	// Author the catalog directly instead of via moq_publish_media_ordered.
	let video_name = "video";
	let video_codec = "vp8";
	let width: u32 = 1920;
	let height: u32 = 1080;
	let description: &[u8] = &[0x01, 0x02, 0x03];
	let video = moq_video_config {
		name: video_name.as_ptr() as *const c_char,
		name_len: video_name.len(),
		codec: video_codec.as_ptr() as *const c_char,
		codec_len: video_codec.len(),
		description: description.as_ptr(),
		description_len: description.len(),
		coded_width: &width,
		coded_height: &height,
	};
	assert_eq!(unsafe { moq_publish_video_config(broadcast, &video) }, 0);

	let audio_name = "audio";
	let audio_codec = "opus";
	let audio = moq_audio_config {
		name: audio_name.as_ptr() as *const c_char,
		name_len: audio_name.len(),
		codec: audio_codec.as_ptr() as *const c_char,
		codec_len: audio_codec.len(),
		description: std::ptr::null(),
		description_len: 0,
		sample_rate: 48000,
		channel_count: 2,
	};
	assert_eq!(unsafe { moq_publish_audio_config(broadcast, &audio) }, 0);

	// Publish and consume the broadcast to verify the catalog round-trips.
	let path = b"catalog-producer";
	assert_eq!(
		unsafe { moq_origin_publish(origin, path.as_ptr() as *const c_char, path.len(), broadcast) },
		0
	);

	let consume = id(unsafe { moq_origin_consume(origin, path.as_ptr() as *const c_char, path.len()) });
	let catalog_cb = Callback::new();
	let catalog_task = id(unsafe { moq_consume_catalog(consume, Some(channel_callback), catalog_cb.ptr) });
	let catalog_id = id(catalog_cb.recv());

	// The video rendition we authored comes back through the consume API.
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
	assert_eq!(unsafe { moq_consume_video_config(catalog_id, 0, &mut video_cfg) }, 0);
	let codec = unsafe {
		std::str::from_utf8(std::slice::from_raw_parts(
			video_cfg.codec.cast::<u8>(),
			video_cfg.codec_len,
		))
	}
	.unwrap();
	assert_eq!(codec, "vp8");
	assert_eq!(unsafe { *video_cfg.coded_width }, 1920);
	assert_eq!(unsafe { *video_cfg.coded_height }, 1080);

	// And so does the audio rendition.
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
	assert_eq!(unsafe { moq_consume_audio_config(catalog_id, 0, &mut audio_cfg) }, 0);
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
	assert_eq!(moq_consume_catalog_free(catalog_id2), 0);
	assert_eq!(moq_consume_catalog_close(catalog_task), 0);
	assert_eq!(catalog_cb.recv_terminal(), 0, "catalog close delivers terminal 0");
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_close(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn publish_catalog_section_roundtrip() {
	let origin = id(moq_origin_create());
	let broadcast = id(moq_publish_create());

	// Set an untyped application section (e.g. advertising a side-channel transcript track).
	let name = "transcript";
	let value = r#"{"track":"transcript.json"}"#;
	assert_eq!(
		unsafe {
			moq_publish_catalog_section(
				broadcast,
				name.as_ptr() as *const c_char,
				name.len(),
				value.as_ptr() as *const c_char,
				value.len(),
			)
		},
		0
	);

	// Reserved media names and invalid JSON are rejected without republishing.
	let reserved = "video";
	assert!(
		unsafe {
			moq_publish_catalog_section(
				broadcast,
				reserved.as_ptr() as *const c_char,
				reserved.len(),
				value.as_ptr() as *const c_char,
				value.len(),
			)
		} < 0,
		"reserved section name should be rejected"
	);
	let bad = "not json";
	assert!(
		unsafe {
			moq_publish_catalog_section(
				broadcast,
				name.as_ptr() as *const c_char,
				name.len(),
				bad.as_ptr() as *const c_char,
				bad.len(),
			)
		} < 0,
		"invalid JSON should be rejected"
	);

	// Publish + consume to verify the section round-trips on the wire.
	let path = b"catalog-section-producer";
	assert_eq!(
		unsafe { moq_origin_publish(origin, path.as_ptr() as *const c_char, path.len(), broadcast) },
		0
	);
	let consume = id(unsafe { moq_origin_consume(origin, path.as_ptr() as *const c_char, path.len()) });
	let catalog_cb = Callback::new();
	let catalog_task = id(unsafe { moq_consume_catalog(consume, Some(channel_callback), catalog_cb.ptr) });
	let catalog_id = id(catalog_cb.recv());

	let mut section = moq_section {
		name: std::ptr::null(),
		name_len: 0,
		json: std::ptr::null(),
		json_len: 0,
	};
	assert_eq!(unsafe { moq_consume_catalog_section(catalog_id, 0, &mut section) }, 0);
	let got_name = unsafe {
		std::str::from_utf8(std::slice::from_raw_parts(section.name.cast::<u8>(), section.name_len)).unwrap()
	};
	let got_json = unsafe {
		std::str::from_utf8(std::slice::from_raw_parts(section.json.cast::<u8>(), section.json_len)).unwrap()
	};
	assert_eq!(got_name, "transcript");
	assert_eq!(
		serde_json::from_str::<serde_json::Value>(got_json).unwrap(),
		serde_json::json!({ "track": "transcript.json" })
	);
	// Only the one section exists.
	assert!(unsafe { moq_consume_catalog_section(catalog_id, 1, &mut section) } < 0);

	// Removing the section republishes a catalog without it.
	assert_eq!(
		unsafe { moq_publish_catalog_section_remove(broadcast, name.as_ptr() as *const c_char, name.len()) },
		0
	);
	let catalog_id2 = id(catalog_cb.recv());
	assert!(
		unsafe { moq_consume_catalog_section(catalog_id2, 0, &mut section) } < 0,
		"section should be gone after remove"
	);

	assert_eq!(moq_consume_catalog_free(catalog_id), 0);
	assert_eq!(moq_consume_catalog_free(catalog_id2), 0);
	assert_eq!(moq_consume_catalog_close(catalog_task), 0);
	assert_eq!(catalog_cb.recv_terminal(), 0, "catalog close delivers terminal 0");
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_close(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn publish_track_invalid_broadcast() {
	let name = b"data";
	assert!(unsafe { moq_publish_track(0, name.as_ptr() as *const c_char, name.len()) } < 0);
	assert!(moq_publish_track_group(9999) < 0);
	assert!(unsafe { moq_publish_track_frame(9999, name.as_ptr(), name.len()) } < 0);
	assert!(unsafe { moq_publish_group_frame(9999, name.as_ptr(), name.len()) } < 0);
	assert!(moq_publish_track_close(9999) < 0);
	assert!(moq_publish_group_close(9999) < 0);
}

#[test]
fn raw_track_publish_consume() {
	let origin = id(moq_origin_create());
	let broadcast = id(moq_publish_create());

	// A raw, non-media track: arbitrary bytes, no codec/container/catalog.
	let track_name = b"data";
	let track = id(unsafe { moq_publish_track(broadcast, track_name.as_ptr() as *const c_char, track_name.len()) });

	let path = b"raw-track";
	assert_eq!(
		unsafe { moq_origin_publish(origin, path.as_ptr() as *const c_char, path.len(), broadcast) },
		0
	);

	let consume = id(unsafe { moq_origin_consume(origin, path.as_ptr() as *const c_char, path.len()) });

	let frame_cb = Callback::new();
	let consumer = id(unsafe {
		moq_consume_track(
			consume,
			track_name.as_ptr() as *const c_char,
			track_name.len(),
			Some(channel_callback),
			frame_cb.ptr,
		)
	});

	// One-frame-per-group convenience write.
	let payload = b"hello raw track";
	assert_eq!(
		unsafe { moq_publish_track_frame(track, payload.as_ptr(), payload.len()) },
		0
	);

	let frame_id = id(frame_cb.recv());
	let mut frame = moq_frame {
		payload: std::ptr::null(),
		payload_size: 0,
		timestamp_us: 123, // should be overwritten with 0
		keyframe: true,    // should be overwritten with false
	};
	assert_eq!(unsafe { moq_consume_track_frame(frame_id, &mut frame) }, 0);
	let received = unsafe { std::slice::from_raw_parts(frame.payload, frame.payload_size) };
	assert_eq!(received, payload);
	assert_eq!(frame.timestamp_us, 0, "raw frames have no timestamp");
	assert!(!frame.keyframe, "raw frames have no keyframe flag");
	assert_eq!(moq_consume_track_frame_close(frame_id), 0);

	// Multi-frame group via the explicit group API.
	let group = id(moq_publish_track_group(track));
	let parts: [&[u8]; 2] = [b"part-0", b"part-1"];
	for part in parts {
		assert_eq!(unsafe { moq_publish_group_frame(group, part.as_ptr(), part.len()) }, 0);
	}
	assert_eq!(moq_publish_group_close(group), 0);

	for expected in parts {
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
		assert_eq!(moq_consume_track_frame_close(frame_id), 0);
	}

	assert_eq!(moq_consume_track_close(consumer), 0);
	// The task delivers one final terminal callback after close; drain it
	// before the Callback (user_data) drops.
	assert_eq!(frame_cb.recv_terminal(), 0, "clean close delivers terminal 0");
	assert!(moq_consume_track_close(consumer) < 0, "double-close should fail");
	assert_eq!(moq_publish_track_close(track), 0);
	assert!(moq_publish_track_close(track) < 0, "double-close should fail");
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_close(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn json_snapshot_publish_consume() {
	let origin = id(moq_origin_create());
	let broadcast = id(moq_publish_create());

	let track_name = b"meta";
	let config = moq_json_config {
		delta_ratio: 8,
		compression: true,
	};
	let producer = id(unsafe {
		moq_publish_json(
			broadcast,
			track_name.as_ptr() as *const c_char,
			track_name.len(),
			&config,
		)
	});

	let path = b"json-snapshot";
	assert_eq!(
		unsafe { moq_origin_publish(origin, path.as_ptr() as *const c_char, path.len(), broadcast) },
		0
	);
	let consume = id(unsafe { moq_origin_consume(origin, path.as_ptr() as *const c_char, path.len()) });

	let value_cb = Callback::new();
	let consumer = id(unsafe {
		moq_consume_json(
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
			unsafe { moq_publish_json_update(producer, expected.as_ptr() as *const c_char, expected.len()) },
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
		assert_eq!(moq_consume_json_value_close(value_id), 0);
	}

	assert_eq!(moq_consume_json_close(consumer), 0);
	assert_eq!(value_cb.recv_terminal(), 0, "clean close delivers terminal 0");
	assert!(moq_consume_json_close(consumer) < 0, "double-close should fail");
	assert_eq!(moq_publish_json_close(producer), 0);
	assert!(moq_publish_json_close(producer) < 0, "double-close should fail");
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_close(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn json_stream_publish_consume() {
	let origin = id(moq_origin_create());
	let broadcast = id(moq_publish_create());

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

	let path = b"json-stream";
	assert_eq!(
		unsafe { moq_origin_publish(origin, path.as_ptr() as *const c_char, path.len(), broadcast) },
		0
	);
	let consume = id(unsafe { moq_origin_consume(origin, path.as_ptr() as *const c_char, path.len()) });

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
		assert_eq!(moq_consume_json_value_close(value_id), 0);
	}

	assert_eq!(moq_consume_json_close(consumer), 0);
	assert_eq!(value_cb.recv_terminal(), 0, "clean close delivers terminal 0");
	assert!(moq_consume_json_close(consumer) < 0, "double-close should fail");
	assert_eq!(moq_publish_json_stream_close(producer), 0);
	assert!(moq_publish_json_stream_close(producer) < 0, "double-close should fail");
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_close(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn close_invalid_or_zero_ids() {
	assert!(moq_origin_close(9999) < 0);
	assert!(moq_session_close(9999) < 0);
	assert!(moq_publish_close(9999) < 0);
	assert!(moq_consume_close(9999) < 0);
	assert!(moq_consume_frame_close(9999) < 0);

	assert!(moq_origin_close(0) < 0);
	assert!(moq_session_close(0) < 0);
	assert!(moq_publish_close(0) < 0);
}

#[test]
fn double_close_all_resource_types() {
	let origin = id(moq_origin_create());
	assert_eq!(moq_origin_close(origin), 0);
	assert!(moq_origin_close(origin) < 0);

	let broadcast = id(moq_publish_create());
	let init = opus_head();
	let format = b"opus";
	let media = id(unsafe {
		moq_publish_media_ordered(
			broadcast,
			format.as_ptr() as *const c_char,
			format.len(),
			init.as_ptr(),
			init.len(),
		)
	});

	assert_eq!(moq_publish_media_close(media), 0);
	assert!(moq_publish_media_close(media) < 0);
	assert_eq!(moq_publish_close(broadcast), 0);

	let origin = id(moq_origin_create());
	let broadcast = id(moq_publish_create());
	let init = opus_head();
	let media = id(unsafe {
		moq_publish_media_ordered(
			broadcast,
			format.as_ptr() as *const c_char,
			format.len(),
			init.as_ptr(),
			init.len(),
		)
	});
	let path = b"double-close-test";
	assert_eq!(
		unsafe { moq_origin_publish(origin, path.as_ptr() as *const c_char, path.len(), broadcast) },
		0
	);

	let consume = id(unsafe { moq_origin_consume(origin, path.as_ptr() as *const c_char, path.len()) });
	let catalog_cb = Callback::new();
	let catalog_task = id(unsafe { moq_consume_catalog(consume, Some(channel_callback), catalog_cb.ptr) });

	let catalog_id = id(catalog_cb.recv());

	let frame_cb = Callback::new();
	let track = id(unsafe { moq_consume_audio_ordered(catalog_id, 0, 10_000, Some(channel_callback), frame_cb.ptr) });

	let payload = b"test";
	assert_eq!(
		unsafe { moq_publish_media_frame(media, payload.as_ptr(), payload.len(), 1_000_000) },
		0
	);
	let frame_id = id(frame_cb.recv());

	assert_eq!(moq_consume_frame_close(frame_id), 0);
	assert!(moq_consume_frame_close(frame_id) < 0);

	assert_eq!(moq_consume_audio_close(track), 0);
	assert_eq!(frame_cb.recv_terminal(), 0, "audio close delivers terminal 0");
	assert!(moq_consume_audio_close(track) < 0);

	assert_eq!(moq_consume_catalog_free(catalog_id), 0);
	assert!(moq_consume_catalog_free(catalog_id) < 0);

	assert_eq!(moq_consume_catalog_close(catalog_task), 0);
	assert_eq!(catalog_cb.recv_terminal(), 0, "catalog close delivers terminal 0");
	assert!(moq_consume_catalog_close(catalog_task) < 0);

	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_media_close(media), 0);
	assert_eq!(moq_publish_close(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn unknown_format() {
	let broadcast = id(moq_publish_create());
	let _guard = Guard(Some(|| {
		moq_publish_close(broadcast);
	}));

	let format = b"nope";
	let ret = unsafe {
		moq_publish_media_ordered(
			broadcast,
			format.as_ptr() as *const c_char,
			format.len(),
			std::ptr::null(),
			0,
		)
	};
	assert!(ret < 0, "unknown format should fail");
}

#[test]
fn local_announce() {
	let origin = id(moq_origin_create());

	let cb = Callback::new();
	let announced_task = id(unsafe { moq_origin_announced(origin, Some(channel_callback), cb.ptr) });

	let broadcast = id(moq_publish_create());
	let path = b"test/broadcast";
	assert_eq!(
		unsafe { moq_origin_publish(origin, path.as_ptr() as *const c_char, path.len(), broadcast) },
		0,
		"moq_origin_publish should succeed"
	);

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
	assert_eq!(moq_publish_close(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn announced_deactivation() {
	let origin = id(moq_origin_create());
	let cb = Callback::new();
	let announced_task = id(unsafe { moq_origin_announced(origin, Some(channel_callback), cb.ptr) });

	let broadcast = id(moq_publish_create());
	let path = b"deactivate/test";
	assert_eq!(
		unsafe { moq_origin_publish(origin, path.as_ptr() as *const c_char, path.len(), broadcast) },
		0
	);

	let announced_id = id(cb.recv());
	let mut info = moq_announced {
		path: std::ptr::null(),
		path_len: 0,
		active: false,
	};
	assert_eq!(unsafe { moq_origin_announced_info(announced_id, &mut info) }, 0);
	assert!(info.active);

	assert_eq!(moq_publish_close(broadcast), 0);

	let deactivated_id = id(cb.recv());
	assert_eq!(unsafe { moq_origin_announced_info(deactivated_id, &mut info) }, 0);
	assert!(!info.active, "broadcast should be inactive after publisher closes");

	assert_eq!(moq_origin_announced_close(announced_task), 0);
	assert_eq!(cb.recv_terminal(), 0, "announced close delivers terminal 0");
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn local_publish_consume() {
	let origin = id(moq_origin_create());
	let broadcast = id(moq_publish_create());

	let init = opus_head();
	let format = b"opus";
	let media = id(unsafe {
		moq_publish_media_ordered(
			broadcast,
			format.as_ptr() as *const c_char,
			format.len(),
			init.as_ptr(),
			init.len(),
		)
	});

	let path = b"live";
	assert_eq!(
		unsafe { moq_origin_publish(origin, path.as_ptr() as *const c_char, path.len(), broadcast) },
		0
	);

	let consume = id(unsafe { moq_origin_consume(origin, path.as_ptr() as *const c_char, path.len()) });
	let catalog_cb = Callback::new();
	let catalog_task = id(unsafe { moq_consume_catalog(consume, Some(channel_callback), catalog_cb.ptr) });

	let catalog_id = id(catalog_cb.recv());

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
		codec: std::ptr::null(),
		codec_len: 0,
		description: std::ptr::null(),
		description_len: 0,
		coded_width: std::ptr::null(),
		coded_height: std::ptr::null(),
	};
	assert!(
		unsafe { moq_consume_video_config(catalog_id, 0, &mut video_cfg) } < 0,
		"video config should fail (no video tracks)"
	);

	let frame_cb = Callback::new();
	let track = id(unsafe { moq_consume_audio_ordered(catalog_id, 0, 10_000, Some(channel_callback), frame_cb.ptr) });

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

	assert_eq!(moq_consume_frame_close(frame_id), 0);
	assert_eq!(moq_consume_audio_close(track), 0);
	assert_eq!(frame_cb.recv_terminal(), 0, "audio close delivers terminal 0");
	assert_eq!(moq_consume_catalog_free(catalog_id), 0);
	assert_eq!(moq_consume_catalog_close(catalog_task), 0);
	assert_eq!(catalog_cb.recv_terminal(), 0, "catalog close delivers terminal 0");
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_media_close(media), 0);
	assert_eq!(moq_publish_close(broadcast), 0);
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

	let broadcast = id(moq_publish_create());
	let init = opus_head();
	let format = b"opus";
	let media = id(unsafe {
		moq_publish_media_ordered(
			broadcast,
			format.as_ptr() as *const c_char,
			format.len(),
			init.as_ptr(),
			init.len(),
		)
	});
	assert_eq!(
		unsafe { moq_origin_publish(origin, path.as_ptr() as *const c_char, path.len(), broadcast) },
		0
	);

	// First the broadcast handle, then a terminal 0 once the wait finishes.
	let consume = id(cb.recv());
	assert_eq!(cb.recv_terminal(), 0, "wait delivers terminal 0 after the handle");

	// The delivered handle behaves like one from moq_origin_consume.
	let catalog_cb = Callback::new();
	let catalog_task = id(unsafe { moq_consume_catalog(consume, Some(channel_callback), catalog_cb.ptr) });
	let catalog_id = id(catalog_cb.recv());

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
	assert_eq!(unsafe { moq_consume_audio_config(catalog_id, 0, &mut audio_cfg) }, 0);
	assert_eq!(audio_cfg.sample_rate, 48000);
	assert_eq!(audio_cfg.channel_count, 2);

	assert_eq!(moq_consume_catalog_free(catalog_id), 0);
	assert_eq!(moq_consume_catalog_close(catalog_task), 0);
	assert_eq!(catalog_cb.recv_terminal(), 0, "catalog close delivers terminal 0");
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_media_close(media), 0);
	assert_eq!(moq_publish_close(broadcast), 0);
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
	let broadcast = id(moq_publish_create());

	let init = h264_init();
	let format = b"avc3";
	let media = id(unsafe {
		moq_publish_media_ordered(
			broadcast,
			format.as_ptr() as *const c_char,
			format.len(),
			init.as_ptr(),
			init.len(),
		)
	});

	let path = b"video-test";
	assert_eq!(
		unsafe { moq_origin_publish(origin, path.as_ptr() as *const c_char, path.len(), broadcast) },
		0
	);

	let consume = id(unsafe { moq_origin_consume(origin, path.as_ptr() as *const c_char, path.len()) });
	let catalog_cb = Callback::new();
	let catalog_task = id(unsafe { moq_consume_catalog(consume, Some(channel_callback), catalog_cb.ptr) });

	let catalog_id = id(catalog_cb.recv());

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

	assert!(!video_cfg.coded_width.is_null(), "coded_width should be set");
	assert!(!video_cfg.coded_height.is_null(), "coded_height should be set");
	let width = unsafe { *video_cfg.coded_width };
	let height = unsafe { *video_cfg.coded_height };
	assert_eq!(width, 1280);
	assert_eq!(height, 720);

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
	assert!(
		unsafe { moq_consume_audio_config(catalog_id, 0, &mut audio_cfg) } < 0,
		"audio config should fail (no audio tracks)"
	);

	let frame_cb = Callback::new();
	let track = id(unsafe { moq_consume_video_ordered(catalog_id, 0, 10_000, Some(channel_callback), frame_cb.ptr) });

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

	assert_eq!(moq_consume_frame_close(frame_id), 0);
	assert_eq!(moq_consume_video_close(track), 0);
	assert_eq!(frame_cb.recv_terminal(), 0, "video close delivers terminal 0");
	assert_eq!(moq_consume_catalog_free(catalog_id), 0);
	assert_eq!(moq_consume_catalog_close(catalog_task), 0);
	assert_eq!(catalog_cb.recv_terminal(), 0, "catalog close delivers terminal 0");
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_media_close(media), 0);
	assert_eq!(moq_publish_close(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn multiple_frames_ordering() {
	let origin = id(moq_origin_create());
	let broadcast = id(moq_publish_create());

	let init = opus_head();
	let format = b"opus";
	let media = id(unsafe {
		moq_publish_media_ordered(
			broadcast,
			format.as_ptr() as *const c_char,
			format.len(),
			init.as_ptr(),
			init.len(),
		)
	});

	let path = b"ordering-test";
	assert_eq!(
		unsafe { moq_origin_publish(origin, path.as_ptr() as *const c_char, path.len(), broadcast) },
		0
	);

	let consume = id(unsafe { moq_origin_consume(origin, path.as_ptr() as *const c_char, path.len()) });
	let catalog_cb = Callback::new();
	let catalog_task = id(unsafe { moq_consume_catalog(consume, Some(channel_callback), catalog_cb.ptr) });
	let catalog_id = id(catalog_cb.recv());

	let frame_cb = Callback::new();
	let track = id(unsafe { moq_consume_audio_ordered(catalog_id, 0, 10_000, Some(channel_callback), frame_cb.ptr) });

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

		assert_eq!(moq_consume_frame_close(frame_id), 0);
	}

	assert_eq!(moq_consume_audio_close(track), 0);
	assert_eq!(frame_cb.recv_terminal(), 0, "audio close delivers terminal 0");
	assert_eq!(moq_consume_catalog_free(catalog_id), 0);
	assert_eq!(moq_consume_catalog_close(catalog_task), 0);
	assert_eq!(catalog_cb.recv_terminal(), 0, "catalog close delivers terminal 0");
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_media_close(media), 0);
	assert_eq!(moq_publish_close(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn catalog_update_on_new_track() {
	let origin = id(moq_origin_create());
	let broadcast = id(moq_publish_create());

	let init = opus_head();
	let format = b"opus";
	let media1 = id(unsafe {
		moq_publish_media_ordered(
			broadcast,
			format.as_ptr() as *const c_char,
			format.len(),
			init.as_ptr(),
			init.len(),
		)
	});

	let path = b"catalog-update";
	assert_eq!(
		unsafe { moq_origin_publish(origin, path.as_ptr() as *const c_char, path.len(), broadcast) },
		0
	);

	let consume = id(unsafe { moq_origin_consume(origin, path.as_ptr() as *const c_char, path.len()) });
	let catalog_cb = Callback::new();
	let catalog_task = id(unsafe { moq_consume_catalog(consume, Some(channel_callback), catalog_cb.ptr) });

	let catalog_id1 = id(catalog_cb.recv());
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
	assert_eq!(unsafe { moq_consume_audio_config(catalog_id1, 0, &mut audio_cfg) }, 0);
	assert!(unsafe { moq_consume_audio_config(catalog_id1, 1, &mut audio_cfg) } < 0);

	let media2 = id(unsafe {
		moq_publish_media_ordered(
			broadcast,
			format.as_ptr() as *const c_char,
			format.len(),
			init.as_ptr(),
			init.len(),
		)
	});

	let catalog_id2 = id(catalog_cb.recv());

	assert_eq!(unsafe { moq_consume_audio_config(catalog_id2, 0, &mut audio_cfg) }, 0);
	assert_eq!(unsafe { moq_consume_audio_config(catalog_id2, 1, &mut audio_cfg) }, 0);

	assert_eq!(moq_consume_catalog_free(catalog_id1), 0);
	assert_eq!(moq_consume_catalog_free(catalog_id2), 0);
	assert_eq!(moq_consume_catalog_close(catalog_task), 0);
	assert_eq!(catalog_cb.recv_terminal(), 0, "catalog close delivers terminal 0");
	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_publish_media_close(media1), 0);
	assert_eq!(moq_publish_media_close(media2), 0);
	assert_eq!(moq_publish_close(broadcast), 0);
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
