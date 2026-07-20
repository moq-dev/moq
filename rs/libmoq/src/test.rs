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
	let format = b"opus";
	let media = id(unsafe {
		moq_publish_media(
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

	assert_eq!(moq_publish_media_finish(media), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
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
	let origin = id(moq_origin_create());
	let broadcast = publish_broadcast(origin, b"publish-catalog-config-null-pointer");
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
	assert_eq!(moq_publish_finish(broadcast), 0);
}

#[test]
fn publish_catalog_roundtrip() {
	let origin = id(moq_origin_create());
	let path = b"catalog-producer";
	let broadcast = publish_broadcast(origin, path);

	// Author the catalog directly instead of via moq_publish_media.
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

	// Consume the broadcast to verify the catalog round-trips.
	let consume = request_broadcast(origin, path);
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
		latency_max_ms: 0,
		latency_max_valid: false,
		timescale: 0,
		timescale_valid: false,
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
		latency_max_ms: 0,
		group_start: 0,
		group_start_valid: false,
		group_end: 0,
		group_end_valid: false,
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
		latency_max_ms: 0,
		latency_max_valid: false,
		timescale: 0,
		timescale_valid: true,
	};

	assert!(unsafe { moq_publish_track(broadcast, name.as_ptr() as *const c_char, name.len(), &info) } < 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
}

#[test]
fn raw_track_options_preserve_ordering_priority() {
	let mut info = moq_track_info {
		priority: 0,
		ordered: false,
		latency_max_ms: 0,
		latency_max_valid: false,
		timescale: 0,
		timescale_valid: false,
	};

	assert!(!moq_net::track::Info::try_from(&info).unwrap().ordered);
	info.ordered = true;
	assert!(moq_net::track::Info::try_from(&info).unwrap().ordered);

	let mut subscription = moq_subscription {
		priority: 0,
		ordered: false,
		latency_max_ms: 0,
		group_start: 0,
		group_start_valid: false,
		group_end: 0,
		group_end_valid: false,
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
	let consumer = id(unsafe {
		moq_consume_track(
			consume,
			track_name.as_ptr() as *const c_char,
			track_name.len(),
			std::ptr::null(),
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
		latency_max_ms: 1_000,
		latency_max_valid: true,
		timescale: 1_000_000,
		timescale_valid: true,
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
		latency_max_ms: 25,
		group_start: 1,
		group_start_valid: true,
		group_end: 1,
		group_end_valid: true,
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
	let format = b"opus";
	let media = id(unsafe {
		moq_publish_media(
			broadcast,
			format.as_ptr() as *const c_char,
			format.len(),
			init.as_ptr(),
			init.len(),
		)
	});

	assert_eq!(moq_publish_media_finish(media), 0);
	assert!(moq_publish_media_finish(media) < 0);
	assert_eq!(moq_publish_finish(broadcast), 0);

	let origin = id(moq_origin_create());
	let path = b"double-close-test";
	let broadcast = publish_broadcast(origin, path);
	let init = opus_head();
	let media = id(unsafe {
		moq_publish_media(
			broadcast,
			format.as_ptr() as *const c_char,
			format.len(),
			init.as_ptr(),
			init.len(),
		)
	});

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

#[test]
fn unknown_format() {
	let origin = id(moq_origin_create());
	let broadcast = publish_broadcast(origin, b"unknown-format");
	let _guard = Guard(Some(|| {
		moq_publish_finish(broadcast);
	}));

	let format = b"nope";
	let ret = unsafe {
		moq_publish_media(
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
	let format = b"opus";
	let media = id(unsafe {
		moq_publish_media(
			broadcast,
			format.as_ptr() as *const c_char,
			format.len(),
			init.as_ptr(),
			init.len(),
		)
	});

	let consume = request_broadcast(origin, path);
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
	let format = b"opus";
	let media = id(unsafe {
		moq_publish_media(
			broadcast,
			format.as_ptr() as *const c_char,
			format.len(),
			init.as_ptr(),
			init.len(),
		)
	});

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
	assert_eq!(moq_publish_media_finish(media), 0);
	assert_eq!(moq_publish_finish(broadcast), 0);
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
	let format = b"avc3";
	let media = id(unsafe {
		moq_publish_media(
			broadcast,
			format.as_ptr() as *const c_char,
			format.len(),
			init.as_ptr(),
			init.len(),
		)
	});

	let consume = request_broadcast(origin, path);
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

/// End-to-end native decode: publish real H.264 (encoded by moq-video) and
/// consume it through `moq_consume_video_raw`, asserting decoded I420 frames.
#[test]
fn video_raw_decode() {
	// Encode a few gray frames to Annex-B (avc3, SPS/PPS inline on the keyframe).
	let mut config = moq_video::encode::Config::new(320, 240, 30);
	config.kind = moq_video::encode::Kind::Software;
	let mut encoder = moq_video::encode::Encoder::new(&config).expect("openh264 encoder");
	let gray = vec![0x80u8; 320 * 240 * 4];
	let mut frames: Vec<bytes::Bytes> = Vec::new();
	for i in 0..5 {
		frames.extend(
			encoder
				.encode_rgba(&gray, moq_video::Size::new(320, 240), i == 0)
				.unwrap(),
		);
	}
	frames.extend(encoder.finish().unwrap());
	assert!(!frames.is_empty(), "encoder produced no frames");

	let origin = id(moq_origin_create());
	let path = b"video-raw-test";
	let broadcast = publish_broadcast(origin, path);

	// The init's SPS/PPS only seed catalog metadata; avc3 frames carry their own
	// inline parameter sets, so the decoder reads the true 320x240 from the wire.
	let init = h264_init();
	let format = b"avc3";
	let media = id(unsafe {
		moq_publish_media(
			broadcast,
			format.as_ptr() as *const c_char,
			format.len(),
			init.as_ptr(),
			init.len(),
		)
	});

	let consume = request_broadcast(origin, path);
	let catalog_cb = Callback::new();
	let catalog_task = id(unsafe { moq_consume_catalog(consume, Some(channel_callback), catalog_cb.ptr) });
	let catalog_id = id(catalog_cb.recv());

	// Subscribe + decode before publishing frames so the keyframe group is delivered.
	let output = moq_video_decoder_output { latency_max_ms: 10_000 };
	let frame_cb = Callback::new();
	let consumer = id(unsafe { moq_consume_video_raw(catalog_id, 0, &output, Some(channel_callback), frame_cb.ptr) });

	for (i, frame) in frames.iter().enumerate() {
		assert_eq!(
			unsafe { moq_publish_media_frame(media, frame.as_ptr(), frame.len(), (i as u64) * 33_000) },
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
	assert_eq!(unsafe { moq_consume_video_raw_frame(frame_id, &mut frame) }, 0);
	assert_eq!(frame.width, 320);
	assert_eq!(frame.height, 240);
	assert_eq!(frame.data_size, 320 * 240 * 3 / 2, "tightly-packed I420");
	assert!(!frame.data.is_null());

	assert_eq!(moq_consume_video_raw_frame_free(frame_id), 0);
	assert_eq!(moq_consume_video_raw_close(consumer), 0);

	// Drain any other decoded frames already queued, then expect the terminal 0.
	loop {
		let code = frame_cb.recv();
		if code > 0 {
			assert_eq!(moq_consume_video_raw_frame_free(id(code)), 0);
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
	let format = b"opus";
	let media = id(unsafe {
		moq_publish_media(
			broadcast,
			format.as_ptr() as *const c_char,
			format.len(),
			init.as_ptr(),
			init.len(),
		)
	});

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
	let format = b"opus";
	let media1 = id(unsafe {
		moq_publish_media(
			broadcast,
			format.as_ptr() as *const c_char,
			format.len(),
			init.as_ptr(),
			init.len(),
		)
	});

	let consume = request_broadcast(origin, path);
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
		moq_publish_media(
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
