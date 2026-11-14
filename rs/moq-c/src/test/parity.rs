//! The binding features moq-c gained to match moq-ffi for the cut.

use std::sync::Arc;

use super::*;

/// Open the socket endpoint `name` under `room` on `origin`.
fn endpoint(origin: u32, name: &str) -> u32 {
	let room = "room";
	id(unsafe {
		moq_socket_create(
			origin,
			room.as_ptr().cast(),
			room.len(),
			name.as_ptr().cast(),
			name.len(),
		)
	})
}

/// Read a `moq_string` a handle lends.
fn lent(s: moq_string) -> String {
	borrowed_string(s.data, s.len).unwrap_or_default()
}

/// Read the next raw frame delivered to `cb`, releasing it.
fn next_frame(cb: &Callback) -> Vec<u8> {
	let frame_id = id(cb.recv());
	let mut frame: moq_frame = unsafe { std::mem::zeroed() };
	assert_eq!(unsafe { moq_consume_track_frame(frame_id, &mut frame) }, 0);
	let payload = unsafe { std::slice::from_raw_parts(frame.payload, frame.payload_size) }.to_vec();
	assert_eq!(moq_consume_track_frame_free(frame_id), 0);
	payload
}

/// Write `payload` into a raw group of a scale-less track, stamped with the group's key.
fn write_unscaled(group: u32, payload: &[u8]) -> i32 {
	let mut key = 0;
	assert_eq!(unsafe { moq_publish_group_sequence(group, &mut key) }, 0);
	unsafe { moq_publish_group_frame(group, payload.as_ptr(), payload.len(), &moq_timestamp::unscaled(key)) }
}

/// Dial `b` from `a` and accept it, returning (dialed, accepted) stream handles plus the
/// callbacks that must outlive them.
fn connect(a: u32, b: u32, track: bool) -> (u32, u32, [Callback; 3]) {
	let accepts = Callback::new();
	let _accept = id(unsafe {
		match track {
			false => moq_socket_accept_tcp(b, Some(channel_callback), accepts.ptr),
			true => moq_socket_accept_track(b, Some(channel_callback), accepts.ptr),
		}
	});
	let dialed = Callback::new();
	let peer = "b";
	let _dial = id(unsafe {
		match track {
			false => moq_socket_dial_tcp(a, peer.as_ptr().cast(), peer.len(), Some(channel_callback), dialed.ptr),
			true => moq_socket_dial_track(
				a,
				peer.as_ptr().cast(),
				peer.len(),
				std::ptr::null(),
				Some(channel_callback),
				dialed.ptr,
			),
		}
	});

	let incoming = id(accepts.recv());
	let (mut claimed, mut session) = (moq_str(""), moq_str(""));
	assert_eq!(
		unsafe { moq_socket_incoming_info(incoming, &mut claimed, &mut session) },
		0
	);
	assert_eq!(lent(claimed), "a");
	let accepted = Callback::new();
	let _accepting =
		id(unsafe { moq_socket_incoming_accept(incoming, std::ptr::null(), Some(channel_callback), accepted.ptr) });
	assert!(
		unsafe { moq_socket_incoming_info(incoming, &mut claimed, &mut session) } < 0,
		"accepting releases the incoming handle"
	);

	let client = id(dialed.recv());
	assert_eq!(dialed.recv_terminal(), 0);
	let server = id(accepted.recv());
	assert_eq!(accepted.recv_terminal(), 0);
	(client, server, [accepts, dialed, accepted])
}

#[test]
fn socket_tcp_round_trip() {
	let origin = id(moq_origin_create());
	let (a, b) = (endpoint(origin, "a"), endpoint(origin, "b"));
	let (client, server, [accepts, ..]) = connect(a, b, false);

	let (mut peer, mut session) = (moq_str(""), moq_str(""));
	assert_eq!(unsafe { moq_socket_stream_info(client, &mut peer, &mut session) }, 0);
	assert_eq!(lent(peer), "b");
	let client_session = lent(session);
	assert_eq!(
		unsafe { moq_socket_stream_info(server, &mut peer, std::ptr::null_mut()) },
		0
	);
	assert_eq!(lent(peer), "a");
	assert_eq!(
		unsafe { moq_socket_stream_info(server, std::ptr::null_mut(), &mut session) },
		0
	);
	assert_eq!(
		lent(session),
		client_session,
		"both sides share the dialer's session id"
	);

	let writer = id(moq_socket_stream_writer(client));
	assert!(moq_socket_stream_writer(client) < 0, "the writer is taken once");
	let frames = Callback::new();
	let reader = id(unsafe { moq_socket_stream_reader(server, Some(channel_callback), frames.ptr) });
	assert!(
		unsafe { moq_socket_stream_reader(server, Some(ignore_callback), std::ptr::null_mut()) } < 0,
		"the reader is taken once"
	);
	assert_eq!(
		unsafe { moq_consume_track_update(reader, std::ptr::null()) },
		MOQ_ERROR_UNSUPPORTED,
		"a tcp half has no subscription"
	);
	// The halves outlive the stream handles. Take the reverse writer first: dropping it
	// unfinished would reset the session.
	let reverse = id(moq_socket_stream_writer(server));
	assert_eq!(moq_socket_stream_free(server), 0);

	for part in [b"one".as_slice(), b"two"] {
		assert_eq!(write_unscaled(writer, part), 0);
	}
	assert_eq!(moq_publish_group_finish(writer), 0);
	assert_eq!(next_frame(&frames), b"one");
	assert_eq!(next_frame(&frames), b"two");
	assert_eq!(frames.recv_terminal(), 0, "a finished writer ends the reader cleanly");
	assert_eq!(moq_publish_group_finish(reverse), 0);

	assert_eq!(moq_socket_stream_free(client), 0);
	assert_eq!(moq_socket_close(b), 0);
	assert_eq!(accepts.recv_terminal(), 0, "closing the endpoint ends its accept loop");
	assert!(moq_socket_close(b) < 0);
	assert_eq!(moq_socket_close(a), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn socket_track_round_trip_and_endpoint_close() {
	let origin = id(moq_origin_create());
	let (a, b) = (endpoint(origin, "a"), endpoint(origin, "b"));
	let (client, server, [accepts, ..]) = connect(a, b, true);

	let track = id(moq_socket_stream_writer(client));
	let frames = Callback::new();
	let _reader = id(unsafe { moq_socket_stream_reader(server, Some(channel_callback), frames.ptr) });
	for part in [b"first".as_slice(), b"later"] {
		let group = id(moq_publish_track_group(track));
		assert_eq!(write_unscaled(group, part), 0);
		assert_eq!(moq_publish_group_finish(group), 0);
	}
	assert_eq!(next_frame(&frames), b"first");
	assert_eq!(next_frame(&frames), b"later");

	// Closing the endpoint ends its open track halves.
	assert_eq!(moq_socket_close(a), 0);
	assert!(
		moq_publish_track_finish(track) < 0,
		"the endpoint's close released the track"
	);
	assert!(frames.recv_terminal() < 0, "the peer's half reset");

	assert_eq!(moq_socket_stream_free(client), 0);
	assert_eq!(moq_socket_stream_free(server), 0);
	assert_eq!(moq_socket_close(b), 0);
	assert_eq!(accepts.recv_terminal(), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn socket_finished_tracks_are_not_kept_by_the_endpoint() {
	let origin = id(moq_origin_create());
	let (a, b) = (endpoint(origin, "a"), endpoint(origin, "b"));
	let accepts = Callback::new();
	let _accept = id(unsafe { moq_socket_accept_track(b, Some(channel_callback), accepts.ptr) });
	let peer = "b";
	for round in 0..3u8 {
		let dialed = Callback::new();
		let _dial = id(unsafe {
			moq_socket_dial_track(
				a,
				peer.as_ptr().cast(),
				peer.len(),
				std::ptr::null(),
				Some(channel_callback),
				dialed.ptr,
			)
		});
		let incoming = id(accepts.recv());
		let accepted = Callback::new();
		let _accepting =
			id(unsafe { moq_socket_incoming_accept(incoming, std::ptr::null(), Some(channel_callback), accepted.ptr) });
		let client = id(dialed.recv());
		assert_eq!(dialed.recv_terminal(), 0);
		let server = id(accepted.recv());
		assert_eq!(accepted.recv_terminal(), 0);

		let track = id(moq_socket_stream_writer(client));
		let frames = Callback::new();
		let _reader = id(unsafe { moq_socket_stream_reader(server, Some(channel_callback), frames.ptr) });
		let group = id(moq_publish_track_group(track));
		assert_eq!(write_unscaled(group, &[round]), 0);
		assert_eq!(moq_publish_group_finish(group), 0);
		assert_eq!(moq_publish_track_finish(track), 0);
		assert_eq!(next_frame(&frames), [round]);
		assert_eq!(frames.recv_terminal(), 0, "a finished track ends its reader cleanly");
		assert_eq!(moq_socket_stream_free(client), 0);
		assert_eq!(moq_socket_stream_free(server), 0);
		assert_eq!(
			State::lock().socket.endpoint_children(Id::try_from(a).unwrap()),
			(0, 0),
			"round {round}: the endpoint neither tracks nor keeps a finished track"
		);
	}

	assert_eq!(moq_socket_close(a), 0);
	assert_eq!(moq_socket_close(b), 0);
	assert_eq!(accepts.recv_terminal(), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn socket_reject_and_cancel() {
	let origin = id(moq_origin_create());
	let (a, b) = (endpoint(origin, "a"), endpoint(origin, "b"));
	let accepts = Callback::new();
	let accept = id(unsafe { moq_socket_accept_tcp(b, Some(channel_callback), accepts.ptr) });

	let dialed = Callback::new();
	let peer = "b";
	let _dial =
		id(unsafe { moq_socket_dial_tcp(a, peer.as_ptr().cast(), peer.len(), Some(channel_callback), dialed.ptr) });
	let incoming = id(accepts.recv());
	assert_eq!(moq_socket_incoming_reject(incoming), 0);
	assert!(
		moq_socket_incoming_reject(incoming) < 0,
		"rejecting releases the handle"
	);
	assert!(dialed.recv_terminal() < 0, "a rejected dial fails");

	// Dialing oneself is refused at once.
	let me = "a";
	assert_eq!(
		unsafe {
			moq_socket_dial_tcp(
				a,
				me.as_ptr().cast(),
				me.len(),
				Some(ignore_callback),
				std::ptr::null_mut(),
			)
		},
		MOQ_ERROR_UNSUPPORTED
	);

	// A pending dial gives up when cancelled.
	let waiting = Callback::new();
	let absent = "nobody";
	let dial = id(unsafe {
		moq_socket_dial_tcp(
			a,
			absent.as_ptr().cast(),
			absent.len(),
			Some(channel_callback),
			waiting.ptr,
		)
	});
	assert_eq!(moq_socket_cancel(dial), 0);
	assert_eq!(waiting.recv_terminal(), 0);

	assert_eq!(moq_socket_cancel(accept), 0);
	assert_eq!(accepts.recv_terminal(), 0);
	assert!(moq_socket_cancel(accept) < 0);
	assert_eq!(moq_socket_close(a), 0);
	assert_eq!(moq_socket_close(b), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn consume_closed_fires_when_the_broadcast_ends() {
	let origin = id(moq_origin_create());
	let path = b"closed-watch";
	let broadcast = publish_broadcast(origin, path);
	let consume = request_broadcast(origin, path);

	let cancelled = Callback::new();
	let watcher = id(unsafe { moq_consume_closed(consume, Some(channel_callback), cancelled.ptr) });
	assert_eq!(moq_consume_closed_cancel(watcher), 0);
	assert_eq!(cancelled.recv_terminal(), 0);
	assert!(moq_consume_closed_cancel(watcher) < 0);

	let closed = Callback::new();
	let _watcher = id(unsafe { moq_consume_closed(consume, Some(channel_callback), closed.ptr) });
	assert_eq!(moq_publish_close(broadcast), 0);
	assert_eq!(closed.recv_terminal(), 0);

	assert_eq!(moq_consume_close(consume), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn track_largest_and_group_ready() {
	let origin = id(moq_origin_create());
	let broadcast = publish_broadcast(origin, b"largest");
	let track = publish_named_track(broadcast, b"data");

	let (mut group, mut frame, mut present) = (9, 9, true);
	assert_eq!(
		unsafe { moq_publish_track_largest(track, &mut group, &mut frame, &mut present) },
		0
	);
	assert!(!present, "an empty track has no largest position");

	let first = id(moq_publish_track_group_at(track, 3));
	for part in [b"a".as_slice(), b"b"] {
		assert_eq!(
			unsafe { moq_publish_group_frame(first, part.as_ptr(), part.len(), &micros(0)) },
			0
		);
	}
	assert_eq!(
		unsafe { moq_publish_track_largest(track, &mut group, &mut frame, &mut present) },
		0
	);
	assert!(present);
	assert_eq!((group, frame), (3, 1));

	// Nobody reads the group, so the wait resolves once the group is finished. A parked wait
	// must still wake, though moq-net does not wake `poll_ready` on a finish.
	let mut parked = ReadyWait::park(first);
	let ready = Callback::new();
	assert_eq!(
		unsafe { moq_publish_group_ready(first, 1, Some(channel_callback), ready.ptr) },
		0
	);
	assert_eq!(moq_publish_group_finish(first), 0);
	assert!(parked.woken() && parked.poll(), "finishing wakes the parked wait");
	assert_eq!(ready.recv(), 0);
	assert!(
		unsafe { moq_publish_group_ready(first, 1, Some(ignore_callback), std::ptr::null_mut()) } < 0,
		"a finished group's handle is gone"
	);

	assert_eq!(moq_publish_track_finish(track), 0);
	assert_eq!(moq_publish_close(broadcast), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

/// A [moq_publish_group_ready] wait, polled by hand so a test knows it parked.
struct ReadyWait {
	wait: std::pin::Pin<Box<dyn Future<Output = ()> + Send>>,
	woken: Arc<Woken>,
}

#[derive(Default)]
struct Woken(std::sync::atomic::AtomicBool);

impl std::task::Wake for Woken {
	fn wake(self: Arc<Self>) {
		self.0.store(true, std::sync::atomic::Ordering::SeqCst);
	}
}

impl ReadyWait {
	/// Start waiting on raw `group`, asserting the wait parked.
	fn park(group: u32) -> Self {
		let mut wait = Self {
			wait: Box::pin(crate::api::group_ready(Id::try_from(group).unwrap(), 1).unwrap()),
			woken: Arc::default(),
		};
		assert!(!wait.poll(), "the wait parks");
		wait
	}

	/// Whether something woke the parked wait since it last polled.
	fn woken(&self) -> bool {
		self.woken.0.load(std::sync::atomic::Ordering::SeqCst)
	}

	/// Poll once, returning whether the wait resolved.
	fn poll(&mut self) -> bool {
		self.woken.0.store(false, std::sync::atomic::Ordering::SeqCst);
		let waker = std::task::Waker::from(self.woken.clone());
		self.wait
			.as_mut()
			.poll(&mut std::task::Context::from_waker(&waker))
			.is_ready()
	}
}

/// Closing a broadcast ends its open tracks' groups and so their ready waits, but a track
/// that declared its end keeps taking groups below it, so its group's wait lasts until the
/// group itself ends.
#[test]
fn group_ready_across_a_broadcast_close() {
	let origin = id(moq_origin_create());
	let broadcast = publish_broadcast(origin, b"ready-close");
	let open = publish_named_track(broadcast, b"open");
	let declared = publish_named_track(broadcast, b"declared");
	assert_eq!(moq_publish_track_finish_at(declared, 1), 0);

	let (open_group, declared_group) = (
		id(moq_publish_track_group(open)),
		id(moq_publish_track_group_at(declared, 0)),
	);
	// Unread, so both waits park.
	let (mut open_wait, mut declared_wait) = (ReadyWait::park(open_group), ReadyWait::park(declared_group));

	assert_eq!(moq_publish_close(broadcast), 0);
	assert!(open_wait.woken() && open_wait.poll(), "the close ends the group");
	let payload = b"late";
	assert!(
		unsafe { moq_publish_group_frame(open_group, payload.as_ptr(), payload.len(), &micros(0)) } < 0,
		"the next write reports why"
	);
	assert!(!declared_wait.poll(), "a declared-end track's group stays open");
	assert_eq!(
		unsafe { moq_publish_group_frame(declared_group, payload.as_ptr(), payload.len(), &micros(0)) },
		0
	);
	assert_eq!(moq_publish_group_finish(declared_group), 0);
	assert!(declared_wait.woken() && declared_wait.poll());

	assert!(
		moq_publish_group_abort(open_group, 0) < 0,
		"the close already ended the group; this only releases its handle"
	);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn timebase_handles_are_owned_by_their_broadcast() {
	let origin = id(moq_origin_create());
	let broadcast = publish_broadcast(origin, b"timebase");
	let timebase = id(moq_publish_timebase(broadcast));
	assert_eq!(moq_publish_timebase_free(timebase), 0);
	assert!(moq_publish_timebase_free(timebase) < 0);
	assert!(moq_publish_timebase(0) < 0);

	let kept = id(moq_publish_timebase(broadcast));
	assert_eq!(moq_publish_close(broadcast), 0);
	// The handle outlives the broadcast, which already let go of the timebase itself.
	assert_eq!(moq_publish_timebase_free(kept), 0);
	assert_eq!(moq_origin_close(origin), 0);
}

#[test]
fn client_once_and_server_quic_options_parse() {
	let mut config = client_config();
	assert!(!parsed(&config).once);
	config.once = true;
	assert!(parsed(&config).once);

	let bind = "127.0.0.1:0";
	let generate = [moq_str("localhost")];
	let versions = [moq_str("moq-lite-03")];
	let mut server: moq_server_config = unsafe { std::mem::zeroed() };
	server.bind = bind.as_ptr().cast();
	server.bind_len = bind.len();
	server.tls_generate = generate.as_ptr();
	server.tls_generate_len = generate.len();
	server.versions = versions.as_ptr();
	server.versions_len = versions.len();
	server.quic_max_streams = 64;
	server.has_quic_max_streams = true;
	server.quic_idle_timeout_us = 5_000_000;
	server.has_quic_idle_timeout = true;
	let cb = Callback::new();
	let listening = id(unsafe { moq_server_listen(&server, Some(channel_callback), cb.ptr) });
	assert_eq!(moq_server_close(listening), 0);
	assert_eq!(cb.recv_terminal(), 0);

	let bogus = [moq_str("moq-nope")];
	server.versions = bogus.as_ptr();
	assert_eq!(
		unsafe { moq_server_listen(&server, Some(ignore_callback), std::ptr::null_mut()) },
		MOQ_ERROR_INVALID_CONFIG,
		"an unknown version is refused"
	);
}

#[test]
fn session_tokens_and_grants() {
	let served = id(moq_origin_create());
	let server_cb = Callback::new();
	let (server, addr, fingerprint) = listen(&server_cb);
	let received = id(moq_origin_create());
	let client_cb = States::new();
	let client = dial_pinned(&format!("https://{addr}/"), &fingerprint, 0, received, client_cb.ptr);

	let request = id(server_cb.recv());
	let session_cb = States::new();
	let session = id(unsafe { moq_session_request_accept(request, served, 0, Some(state_callback), session_cb.ptr) });
	for cb in [&session_cb, &client_cb] {
		assert_eq!(cb.recv().phase, moq_connection_phase::MOQ_CONNECTION_PHASE_CONNECTED);
	}

	// An accepted session presents no tokens.
	let token = [moq_str("token")];
	assert_eq!(
		unsafe { moq_session_tokens_set(session, token.as_ptr(), token.len()) },
		MOQ_ERROR_UNSUPPORTED
	);
	assert_eq!(
		unsafe { moq_session_tokens_answered(session, Some(ignore_callback), std::ptr::null_mut()) },
		MOQ_ERROR_UNSUPPORTED
	);
	// A dialed session refuses an empty token, and takes real ones.
	let empty = [moq_str("")];
	assert_eq!(
		unsafe { moq_session_tokens_set(client, empty.as_ptr(), empty.len()) },
		MOQ_ERROR_INVALID_CONFIG
	);
	assert_eq!(
		unsafe { moq_session_tokens_set(client, token.as_ptr(), token.len()) },
		0
	);

	// The grant is delivered at once; the default protocol carries no AUTH, so it stays absent.
	let grants = Callback::new();
	let watcher = id(unsafe { moq_session_grants(client, Some(channel_callback), grants.ptr) });
	let grant = id(grants.recv());
	let mut info: moq_grant = unsafe { std::mem::zeroed() };
	info.present = true;
	assert_eq!(unsafe { moq_grant_info(grant, &mut info) }, 0);
	assert!(!info.present);
	assert_eq!((info.publish_len, info.subscribe_len), (0, 0));
	assert_eq!(moq_grant_free(grant), 0);
	assert!(moq_grant_free(grant) < 0);
	assert_eq!(moq_session_grants_cancel(watcher), 0);
	loop {
		let code = grants.recv();
		if code <= 0 {
			assert_eq!(code, 0);
			break;
		}
		assert_eq!(moq_grant_free(id(code)), 0);
	}

	// A watcher still running when the session closes ends cleanly.
	let ending = Callback::new();
	let _watcher = id(unsafe { moq_session_grants(client, Some(channel_callback), ending.ptr) });
	assert_eq!(moq_grant_free(id(ending.recv())), 0);

	assert_eq!(moq_session_close(session), 0);
	session_cb.recv_terminal();
	assert_eq!(moq_session_close(client), 0);
	client_cb.recv_terminal();
	loop {
		let code = ending.recv();
		if code <= 0 {
			assert_eq!(code, 0, "a closed session ends the watch cleanly");
			break;
		}
		assert_eq!(moq_grant_free(id(code)), 0);
	}
	assert_eq!(moq_server_close(server), 0);
	assert_eq!(server_cb.recv_terminal(), 0);
	assert_eq!(moq_origin_close(served), 0);
	assert_eq!(moq_origin_close(received), 0);
}

#[test]
fn audio_encoder_without_a_broadcast() {
	let codec = b"opus";
	let output = moq_audio_encoder_output {
		codec: codec.as_ptr().cast(),
		codec_len: codec.len(),
		sample_rate: 0,
		channels: 0,
		bitrate: 0,
		frame_duration_us: 20_000,
	};
	let encoder = id(unsafe { moq_audio_encoder_create(&output) });
	let frame_size = moq_audio_encoder_frame_size(encoder);
	assert_eq!(frame_size, 960, "20 ms at the default 48 kHz");
	let mut init: moq_audio_init = unsafe { std::mem::zeroed() };
	assert_eq!(unsafe { moq_audio_encoder_init(encoder, &mut init) }, 0);
	assert_eq!(moq_audio_encoder_bitrate(encoder, 64_000), 0);

	// Stereo by default, so a frame is twice the frame size in interleaved samples.
	let pcm = vec![0.0f32; frame_size as usize * 2];
	let mut packet: moq_audio_packet = unsafe { std::mem::zeroed() };
	let mut packets = 0;
	for _ in 0..3 {
		assert_eq!(unsafe { moq_audio_encoder_encode(encoder, pcm.as_ptr(), pcm.len()) }, 0);
		assert_eq!(unsafe { moq_audio_encoder_receive(encoder, &mut packet) }, 1);
		assert!(packet.payload_size > 0);
		assert_eq!(unsafe { moq_audio_encoder_receive(encoder, &mut packet) }, 0);
		packets += 1;
	}
	assert_eq!(moq_audio_encoder_flush(encoder), 0);
	assert_eq!(
		unsafe { moq_audio_encoder_receive(encoder, &mut packet) },
		0,
		"libopus holds no packet"
	);
	let mut padding = 0;
	let drained = unsafe { moq_audio_encoder_finish(encoder, pcm.as_ptr(), pcm.len() / 2, &mut padding) };
	assert!(drained >= 0, "finish failed: {drained}");
	assert!(packets + drained > 0, "the encoder produced packets");
	if drained > 0 {
		assert_eq!(unsafe { moq_audio_encoder_packet(encoder, 0, &mut packet) }, 0);
		assert!(packet.payload_size > 0);
		assert!(unsafe { moq_audio_encoder_packet(encoder, drained as u32, &mut packet) } < 0);
	}
	assert!(
		unsafe { moq_audio_encoder_encode(encoder, pcm.as_ptr(), pcm.len()) } < 0,
		"a finished encoder takes no more"
	);
	assert_eq!(moq_audio_encoder_free(encoder), 0);
	assert!(moq_audio_encoder_free(encoder) < 0);
}

#[test]
fn video_encoder_without_a_broadcast() {
	let input = moq_video_encoder_input {
		format: moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_RGBA as u32,
		width: 320,
		height: 240,
		framerate: moq_video_frame_rate {
			numerator: 30,
			denominator: 1,
		},
	};
	let output = moq_video_encoder_output {
		codec: moq_video_codec::MOQ_VIDEO_CODEC_H264 as u32,
		bitrate: 0,
		join_us: 0,
		kind: moq_video_encoder_kind::MOQ_VIDEO_ENCODER_KIND_SOFTWARE as u32,
		encoder: std::ptr::null(),
		encoder_len: 0,
	};
	let encoder = id(unsafe { moq_video_encoder_create(&input, &output) });
	let rgba = gray_rgba(320, 240);
	let mut packet: moq_frame = unsafe { std::mem::zeroed() };
	let mut keyframes = Vec::new();
	let mut receive = |keyframes: &mut Vec<bool>| loop {
		match unsafe { moq_video_encoder_receive(encoder, &mut packet) } {
			0 => break,
			1 => {
				assert!(packet.payload_size > 0);
				assert!(!packet.timestamp.is_null(), "an access unit carries its timestamp");
				keyframes.push(packet.keyframe);
			}
			code => panic!("receive failed: {code}"),
		}
	};
	for index in 0..5 {
		if index == 3 {
			assert_eq!(moq_video_encoder_cut(encoder), 0);
		}
		let frame = moq_video_encoder_frame {
			timestamp: micros(index * 33_333),
			data: rgba.as_ptr(),
			data_size: rgba.len(),
		};
		assert_eq!(unsafe { moq_video_encoder_encode(encoder, &frame) }, 0);
		receive(&mut keyframes);
	}
	assert_eq!(moq_video_encoder_flush(encoder), 0);
	receive(&mut keyframes);
	assert_eq!(
		keyframes,
		[true, false, false, true, false],
		"keyframes at the start and the cut"
	);
	assert_eq!(moq_video_encoder_free(encoder), 0);
	assert!(moq_video_encoder_free(encoder) < 0);
}

/// Only Apple decoders hand out a native surface; elsewhere asking for one is refused up front.
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
#[test]
fn native_surfaces_are_unsupported_off_apple() {
	let output = moq_video_decoder_output {
		delay: moq_delay::default(),
		format: moq_video_pixel_format::MOQ_VIDEO_PIXEL_FORMAT_I420 as u32,
		width: 0,
		height: 0,
		start: 0,
		surface: true,
	};
	assert_eq!(
		unsafe { moq_decode_video(1, 0, &output, Some(ignore_callback), std::ptr::null_mut()) },
		MOQ_ERROR_UNSUPPORTED
	);
	// A surface ignores the CPU layout and size, so an odd size is not the refusal.
	let odd = moq_video_decoder_output {
		width: 641,
		height: 481,
		format: 999,
		..output
	};
	assert_eq!(
		unsafe { moq_decode_video(1, 0, &odd, Some(ignore_callback), std::ptr::null_mut()) },
		MOQ_ERROR_UNSUPPORTED
	);
}
