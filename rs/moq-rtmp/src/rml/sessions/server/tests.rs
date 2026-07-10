use super::*;
use crate::rml::chunk_io::ChunkDeserializer;
use crate::rml::messages::{MessagePayload, PeerBandwidthLimitType, RtmpMessage, UserControlEventType};
use bytes::BytesMut;
use rml_amf0::Amf0Value;
use std::collections::HashMap;

const DEFAULT_CHUNK_SIZE: u32 = 1111;
const DEFAULT_PEER_BANDWIDTH: u32 = 2222;
const DEFAULT_WINDOW_ACK_SIZE: u32 = 3333;

const TEST_APP_NAME: &str = "some_app";
const TEST_STREAM_KEY: &str = "stream_key";

#[test]
fn new_config_creates_initial_responses() {
	let config = get_basic_config();
	let mut deserializer = ChunkDeserializer::new();
	let (_, results) = ServerSession::new(config).unwrap();

	let (responses, _) = split_results(&mut deserializer, results);

	assert_vec_contains!(
		responses,
		&(
			_,
			RtmpMessage::WindowAcknowledgement {
				size: DEFAULT_WINDOW_ACK_SIZE
			}
		)
	);
	assert_vec_contains!(
		responses,
		&(
			_,
			RtmpMessage::SetPeerBandwidth {
				size: DEFAULT_PEER_BANDWIDTH,
				limit_type: PeerBandwidthLimitType::Dynamic
			}
		)
	);
	assert_vec_contains!(
		responses,
		&(
			_,
			RtmpMessage::UserControl {
				event_type: UserControlEventType::StreamBegin,
				stream_id: Some(0),
				buffer_length: None,
				timestamp: None
			}
		)
	);

	// Based on packet capture, not 100% sure if needed
	let mut additional_values: &Vec<Amf0Value> = &Vec::new();
	assert_vec_contains!(responses, &(_, RtmpMessage::Amf0Command {
            command_name: ref command_name_value,
            transaction_id: transaction_id_value,
            command_object: Amf0Value::Null,
            additional_arguments: ref x,
        }) if command_name_value == "onBWDone" && transaction_id_value == 0_f64 => additional_values = x);
	assert_eq!(
		&additional_values[..],
		&[Amf0Value::Number(8192_f64)],
		"onBWDone additional values were unexpected"
	);
}

#[test]
fn on_bw_done_not_sent_when_config_disables_it() {
	let mut config = get_basic_config();
	config.send_on_bw_done_message_on_start = false;

	let mut deserializer = ChunkDeserializer::new();
	let (_, results) = ServerSession::new(config).unwrap();

	let (responses, _) = split_results(&mut deserializer, results);

	for (_, message) in responses {
		if let RtmpMessage::Amf0Command { command_name, .. } = message {
			if command_name == "onBWDone" {
				assert!(false, "onBWDone message received, but not expected");
			}
		}
	}
}

#[test]
fn can_accept_connection_request() {
	let config = get_basic_config();
	let (mut deserializer, mut serializer, mut session) = common_setup(&config);

	let connect_payload = create_connect_message("some_app".to_string(), 15, 0, 0.0);
	let connect_packet = serializer.serialize(&connect_payload, true, false).unwrap();
	let connect_results = session.handle_input(&connect_packet.bytes[..]).unwrap();
	assert_eq!(
		connect_results.len(),
		1,
		"Unexpected number of responses when handling connect request message"
	);

	let (_, events) = split_results(&mut deserializer, connect_results);
	assert_eq!(events.len(), 1, "Unexpected number of events returned");
	let request_id = match events[0] {
		ServerSessionEvent::ConnectionRequested {
			ref app_name,
			request_id,
			..
		} if app_name == "some_app" => request_id,
		_ => panic!("First event was not as expected: {:?}", events[0]),
	};

	let accept_results = session.accept_request(request_id).unwrap();
	assert_eq!(accept_results.len(), 1, "Unexpected number of results returned");

	let (responses, _) = split_results(&mut deserializer, accept_results);
	match responses[0] {
		(
			_,
			RtmpMessage::Amf0Command {
				ref command_name,
				transaction_id: _,
				command_object: Amf0Value::Object(ref properties),
				ref additional_arguments,
			},
		) if command_name == "_result" => {
			assert_eq!(
				properties.get("fmsVer"),
				Some(&Amf0Value::Utf8String(config.fms_version)),
				"Unexpected fms version"
			);
			assert_eq!(
				properties.get("capabilities"),
				Some(&Amf0Value::Number(31.0)),
				"Unexpected capabilities value"
			);
			assert_eq!(
				additional_arguments.len(),
				1,
				"Unexpected number of additional arguments"
			);
			match additional_arguments[0] {
				Amf0Value::Object(ref properties) => {
					assert_eq!(
						properties.get("level"),
						Some(&Amf0Value::Utf8String("status".to_string())),
						"Unexpected level value"
					);
					assert_eq!(
						properties.get("code"),
						Some(&Amf0Value::Utf8String("NetConnection.Connect.Success".to_string())),
						"Unexpected code value"
					);
					assert_eq!(
						properties.get("objectEncoding"),
						Some(&Amf0Value::Number(0.0)),
						"Unexpected object encoding value"
					);
					assert!(properties.contains_key("description"), "No description provided");
				}

				_ => panic!(
					"Additional arguments was not an Amf0 object: {:?}",
					additional_arguments[0]
				),
			}
		}

		_ => panic!("Unexpected first response message: {:?}", responses[0]),
	}
}

#[test]
fn connect_request_strips_trailing_slash() {
	let (mut deserializer, mut serializer, mut session) = common_basic_setup();

	let connect_payload = create_connect_message("some_app/".to_string(), 15, 0, 0.0);
	let connect_packet = serializer.serialize(&connect_payload, true, false).unwrap();
	let connect_results = session.handle_input(&connect_packet.bytes[..]).unwrap();
	assert_eq!(
		connect_results.len(),
		1,
		"Unexpected number of responses when handling connect request message"
	);

	let (_, events) = split_results(&mut deserializer, connect_results);
	assert_eq!(events.len(), 1, "Unexpected number of events returned");
	match events[0] {
		ServerSessionEvent::ConnectionRequested {
			ref app_name,
			request_id: _,
			..
		} => assert_eq!(app_name, "some_app", "Unexpected app name"),
		_ => panic!("First event was not as expected: {:?}", events[0]),
	};
}

#[test]
fn accepted_connection_responds_with_same_object_encoding_value_as_connection_request() {
	let config = get_basic_config();
	let (mut deserializer, mut serializer, mut session) = common_setup(&config);

	let connect_payload = create_connect_message("some_app".to_string(), 15, 0, 3.0);
	let connect_packet = serializer.serialize(&connect_payload, true, false).unwrap();
	let connect_results = session.handle_input(&connect_packet.bytes[..]).unwrap();
	assert_eq!(
		connect_results.len(),
		1,
		"Unexpected number of responses when handling connect request message"
	);

	let (_, events) = split_results(&mut deserializer, connect_results);
	assert_eq!(events.len(), 1, "Unexpected number of events returned");
	let request_id = match events[0] {
		ServerSessionEvent::ConnectionRequested {
			ref app_name,
			request_id,
			..
		} if app_name == "some_app" => request_id,
		_ => panic!("First event was not as expected: {:?}", events[0]),
	};

	let accept_results = session.accept_request(request_id).unwrap();
	assert_eq!(accept_results.len(), 1, "Unexpected number of results returned");

	let (responses, _) = split_results(&mut deserializer, accept_results);
	match responses[0] {
		(
			_,
			RtmpMessage::Amf0Command {
				ref command_name,
				transaction_id: _,
				command_object: Amf0Value::Object(ref properties),
				ref additional_arguments,
			},
		) if command_name == "_result" => {
			assert_eq!(
				properties.get("fmsVer"),
				Some(&Amf0Value::Utf8String(config.fms_version)),
				"Unexpected fms version"
			);
			assert_eq!(
				properties.get("capabilities"),
				Some(&Amf0Value::Number(31.0)),
				"Unexpected capabilities value"
			);
			assert_eq!(
				additional_arguments.len(),
				1,
				"Unexpected number of additional arguments"
			);
			match additional_arguments[0] {
				Amf0Value::Object(ref properties) => {
					assert_eq!(
						properties.get("level"),
						Some(&Amf0Value::Utf8String("status".to_string())),
						"Unexpected level value"
					);
					assert_eq!(
						properties.get("code"),
						Some(&Amf0Value::Utf8String("NetConnection.Connect.Success".to_string())),
						"Unexpected code value"
					);
					assert_eq!(
						properties.get("objectEncoding"),
						Some(&Amf0Value::Number(3.0)),
						"Unexpected object encoding value"
					);
					assert!(properties.contains_key("description"), "No description provided");
				}

				_ => panic!(
					"Additional arguments was not an Amf0 object: {:?}",
					additional_arguments[0]
				),
			}
		}

		_ => panic!("Unexpected first response message: {:?}", responses[0]),
	}
}

#[test]
fn connect_request_reports_fourcc_list() {
	let (mut deserializer, mut serializer, mut session) = common_basic_setup();
	let payload = RtmpMessage::Amf0Command {
		command_name: "connect".to_string(),
		transaction_id: 1.0,
		command_object: Amf0Value::Object(HashMap::from([
			("app".to_string(), Amf0Value::Utf8String("some_app".to_string())),
			("capsEx".to_string(), Amf0Value::Number(2.0)),
			(
				"fourCcList".to_string(),
				Amf0Value::StrictArray(vec![
					Amf0Value::Utf8String("hvc1".to_string()),
					Amf0Value::Utf8String("Opus".to_string()),
				]),
			),
		])),
		additional_arguments: vec![],
	}
	.into_message_payload(RtmpTimestamp::new(15), 0)
	.unwrap();
	let packet = serializer.serialize(&payload, true, false).unwrap();
	let results = session.handle_input(&packet.bytes[..]).unwrap();

	let (_, events) = split_results(&mut deserializer, results);
	assert_eq!(events.len(), 1, "Unexpected number of events returned");
	match &events[0] {
		ServerSessionEvent::ConnectionRequested {
			caps_ex,
			video_fourccs,
			audio_fourccs,
			..
		} => {
			assert_eq!(*caps_ex, 2);
			assert_eq!(video_fourccs.fourccs, vec![*b"hvc1", *b"Opus"]);
			assert_eq!(audio_fourccs.fourccs, vec![*b"hvc1", *b"Opus"]);
		}
		_ => panic!("First event was not as expected: {:?}", events[0]),
	}
}

#[test]
fn connect_request_reports_decodable_fourcc_maps() {
	let (mut deserializer, mut serializer, mut session) = common_basic_setup();
	let payload = RtmpMessage::Amf0Command {
		command_name: "connect".to_string(),
		transaction_id: 1.0,
		command_object: Amf0Value::Object(HashMap::from([
			("app".to_string(), Amf0Value::Utf8String("some_app".to_string())),
			(
				"videoFourCcInfoMap".to_string(),
				Amf0Value::Object(HashMap::from([
					("vp09".to_string(), Amf0Value::Number(1.0)),
					("hvc1".to_string(), Amf0Value::Number(4.0)),
				])),
			),
			(
				"audioFourCcInfoMap".to_string(),
				Amf0Value::Object(HashMap::from([("*".to_string(), Amf0Value::Number(1.0))])),
			),
		])),
		additional_arguments: vec![],
	}
	.into_message_payload(RtmpTimestamp::new(15), 0)
	.unwrap();
	let packet = serializer.serialize(&payload, true, false).unwrap();
	let results = session.handle_input(&packet.bytes[..]).unwrap();

	let (_, events) = split_results(&mut deserializer, results);
	assert_eq!(events.len(), 1, "Unexpected number of events returned");
	match &events[0] {
		ServerSessionEvent::ConnectionRequested {
			video_fourccs,
			audio_fourccs,
			..
		} => {
			assert_eq!(video_fourccs.fourccs, vec![*b"vp09"]);
			assert!(!video_fourccs.fourccs.contains(b"hvc1"));
			assert!(audio_fourccs.any);
		}
		_ => panic!("First event was not as expected: {:?}", events[0]),
	}
}

#[test]
fn can_create_stream_on_connected_session() {
	let (mut deserializer, mut serializer, mut session) = common_basic_setup();
	perform_connection("some_app", &mut session, &mut serializer, &mut deserializer);

	let message = RtmpMessage::Amf0Command {
		command_name: "createStream".to_string(),
		transaction_id: 4.0,
		command_object: Amf0Value::Null,
		additional_arguments: Vec::new(),
	};

	let payload = message.into_message_payload(RtmpTimestamp::new(0), 0).unwrap();
	let packet = serializer.serialize(&payload, true, false).unwrap();
	let results = session.handle_input(&packet.bytes[..]).unwrap();
	let (responses, _) = split_results(&mut deserializer, results);

	assert_eq!(responses.len(), 1, "Unexpected number of responses returned");
	match responses[0] {
		(
			ref payload,
			RtmpMessage::Amf0Command {
				ref command_name,
				transaction_id,
				command_object: Amf0Value::Null,
				ref additional_arguments,
			},
		) if command_name == "_result" && transaction_id == 4.0 => {
			assert_eq!(
				additional_arguments.len(),
				1,
				"Unexpected number of additional arguments in response"
			);
			assert_vec_match!(additional_arguments, Amf0Value::Number(x) if x > 0.0);
			assert_eq!(payload.message_stream_id, 0, "Unexpected message stream id");
		}

		_ => panic!("First response was not the expected value: {:?}", responses[0]),
	}
}

#[test]
fn can_accept_live_publishing_to_requested_stream_key() {
	let (mut deserializer, mut serializer, mut session) = common_basic_setup();
	perform_connection("some_app", &mut session, &mut serializer, &mut deserializer);

	let stream_id = create_active_stream(&mut session, &mut serializer, &mut deserializer);
	let message = RtmpMessage::Amf0Command {
		command_name: "publish".to_string(),
		transaction_id: 5.0,
		command_object: Amf0Value::Null,
		additional_arguments: vec![
			Amf0Value::Utf8String("stream_key".to_string()),
			Amf0Value::Utf8String("live".to_string()),
		],
	};

	let publish_payload = message.into_message_payload(RtmpTimestamp::new(0), stream_id).unwrap();
	let publish_packet = serializer.serialize(&publish_payload, false, false).unwrap();
	let publish_results = session.handle_input(&publish_packet.bytes[..]).unwrap();
	let (_, events) = split_results(&mut deserializer, publish_results);

	assert_eq!(events.len(), 1, "Unexpected number of events returned");
	let request_id = match events[0] {
		ServerSessionEvent::PublishStreamRequested {
			ref app_name,
			ref stream_key,
			request_id: returned_request_id,
			mode: PublishMode::Live,
		} if app_name == "some_app" && stream_key == "stream_key" => returned_request_id,

		_ => panic!("Unexpected first event found: {:?}", events[0]),
	};

	let accept_results = session.accept_request(request_id).unwrap();
	let (mut responses, _) = split_results(&mut deserializer, accept_results);
	assert_eq!(responses.len(), 2, "Unexpected number of responses received");

	match responses.remove(0) {
		(
			_,
			RtmpMessage::UserControl {
				event_type: UserControlEventType::StreamBegin,
				stream_id: Some(received_stream_id),
				buffer_length: None,
				timestamp: None,
			},
		) => {
			assert_eq!(
				received_stream_id, stream_id,
				"Stream begin did not contain the expected stream id"
			);
		}

		x => panic!(
			"Expected stream begin for stream id {:?} but instead received: {:?}",
			stream_id, x
		),
	}

	verify_is_onstatus(&responses.remove(0).1, "status", "NetStream.Publish.Start");
}

#[test]
fn can_receive_and_raise_event_for_metadata_from_obs() {
	let (mut deserializer, mut serializer, mut session) = common_basic_setup();
	perform_connection(TEST_APP_NAME, &mut session, &mut serializer, &mut deserializer);
	let stream_id = create_active_stream(&mut session, &mut serializer, &mut deserializer);
	start_publishing(
		TEST_STREAM_KEY,
		stream_id,
		&mut session,
		&mut serializer,
		&mut deserializer,
	);

	let mut properties = HashMap::new();
	properties.insert("width".to_string(), Amf0Value::Number(1920_f64));
	properties.insert("height".to_string(), Amf0Value::Number(1080_f64));
	properties.insert("videocodecid".to_string(), Amf0Value::Number(10.0));
	properties.insert("videodatarate".to_string(), Amf0Value::Number(1200_f64));
	properties.insert("framerate".to_string(), Amf0Value::Number(30_f64));
	properties.insert("audiocodecid".to_string(), Amf0Value::Number(7.0));
	properties.insert("audiodatarate".to_string(), Amf0Value::Number(96_f64));
	properties.insert("audiosamplerate".to_string(), Amf0Value::Number(48000_f64));
	properties.insert("audiosamplesize".to_string(), Amf0Value::Number(16_f64));
	properties.insert("audiochannels".to_string(), Amf0Value::Number(2_f64));
	properties.insert("stereo".to_string(), Amf0Value::Boolean(true));
	properties.insert("encoder".to_string(), Amf0Value::Utf8String("Test Encoder".to_string()));

	let message = RtmpMessage::Amf0Data {
		values: vec![
			Amf0Value::Utf8String("@setDataFrame".to_string()),
			Amf0Value::Utf8String("onMetaData".to_string()),
			Amf0Value::Object(properties),
		],
	};

	let metadata_payload = message.into_message_payload(RtmpTimestamp::new(0), stream_id).unwrap();
	let metadata_packet = serializer.serialize(&metadata_payload, false, false).unwrap();
	let metadata_results = session.handle_input(&metadata_packet.bytes[..]).unwrap();
	let (_, mut events) = split_results(&mut deserializer, metadata_results);

	assert_eq!(events.len(), 1, "Unexpected number of metadata events");

	match events.remove(0) {
		ServerSessionEvent::StreamMetadataChanged {
			app_name,
			stream_key,
			metadata,
		} => {
			assert_eq!(app_name, TEST_APP_NAME, "Unexpected metadata app name");
			assert_eq!(stream_key, TEST_STREAM_KEY, "Unexpected metadata stream key");
			assert_eq!(metadata.video_width, Some(1920), "Unexpected video width");
			assert_eq!(metadata.video_height, Some(1080), "Unexepcted video height");
			assert_eq!(metadata.video_codec_id, Some(10), "Unexepcted video codec");
			assert_eq!(metadata.video_frame_rate, Some(30_f32), "Unexpected framerate");
			assert_eq!(metadata.video_bitrate_kbps, Some(1200), "Unexpected video bitrate");
			assert_eq!(metadata.audio_codec_id, Some(7), "Unexpected audio codec");
			assert_eq!(metadata.audio_bitrate_kbps, Some(96), "Unexpected audio bitrate");
			assert_eq!(metadata.audio_sample_rate, Some(48000), "Unexpected audio sample rate");
			assert_eq!(metadata.audio_channels, Some(2), "Unexpected audio channels");
			assert_eq!(metadata.audio_is_stereo, Some(true), "Unexpected audio is stereo value");
			assert_eq!(
				metadata.encoder,
				Some("Test Encoder".to_string()),
				"Unexpected encoder value"
			);
		}

		_ => panic!("Unexpected event received: {:?}", events[0]),
	}
}

#[test]
fn can_receive_audio_data_on_published_stream() {
	let (mut deserializer, mut serializer, mut session) = common_basic_setup();
	perform_connection(TEST_APP_NAME, &mut session, &mut serializer, &mut deserializer);
	let stream_id = create_active_stream(&mut session, &mut serializer, &mut deserializer);
	start_publishing(
		TEST_STREAM_KEY,
		stream_id,
		&mut session,
		&mut serializer,
		&mut deserializer,
	);

	let message = RtmpMessage::AudioData {
		data: Bytes::from(vec![1_u8, 2_u8, 3_u8]),
	};
	let payload = message
		.into_message_payload(RtmpTimestamp::new(1234), stream_id)
		.unwrap();
	let packet = serializer.serialize(&payload, false, false).unwrap();
	let results = session.handle_input(&packet.bytes[..]).unwrap();
	let (_, mut events) = split_results(&mut deserializer, results);

	assert_eq!(events.len(), 1, "Unexpected number of events returned");

	match events.remove(0) {
		ServerSessionEvent::AudioDataReceived {
			app_name,
			stream_key,
			data,
			timestamp,
		} => {
			assert_eq!(app_name, TEST_APP_NAME, "Unexpected app name");
			assert_eq!(stream_key, TEST_STREAM_KEY, "Unexpected stream key");
			assert_eq!(timestamp, RtmpTimestamp::new(1234), "Unexepcted timestamp");
			assert_eq!(&data[..], &[1_u8, 2_u8, 3_u8], "Unexpected data");
		}

		event => panic!("Expected AudioDataReceived event, instead got: {:?}", event),
	}
}

#[test]
fn can_receive_video_data_on_published_stream() {
	let (mut deserializer, mut serializer, mut session) = common_basic_setup();
	perform_connection(TEST_APP_NAME, &mut session, &mut serializer, &mut deserializer);
	let stream_id = create_active_stream(&mut session, &mut serializer, &mut deserializer);
	start_publishing(
		TEST_STREAM_KEY,
		stream_id,
		&mut session,
		&mut serializer,
		&mut deserializer,
	);

	let message = RtmpMessage::VideoData {
		data: Bytes::from(vec![1_u8, 2_u8, 3_u8]),
	};
	let payload = message
		.into_message_payload(RtmpTimestamp::new(1234), stream_id)
		.unwrap();
	let packet = serializer.serialize(&payload, false, false).unwrap();
	let results = session.handle_input(&packet.bytes[..]).unwrap();
	let (_, mut events) = split_results(&mut deserializer, results);

	assert_eq!(events.len(), 1, "Unexpected number of events returned");

	match events.remove(0) {
		ServerSessionEvent::VideoDataReceived {
			app_name,
			stream_key,
			data,
			timestamp,
		} => {
			assert_eq!(app_name, TEST_APP_NAME, "Unexpected app name");
			assert_eq!(stream_key, TEST_STREAM_KEY, "Unexpected stream key");
			assert_eq!(timestamp, RtmpTimestamp::new(1234), "Unexpected timestamp");
			assert_eq!(&data[..], &[1_u8, 2_u8, 3_u8], "Unexpected data");
		}

		event => panic!("Expected AudioDataReceived event, instead got: {:?}", event),
	}
}

#[test]
fn publish_finished_event_raised_when_delete_stream_invoked_on_publishing_stream() {
	let (mut deserializer, mut serializer, mut session) = common_basic_setup();
	perform_connection(TEST_APP_NAME, &mut session, &mut serializer, &mut deserializer);
	let stream_id = create_active_stream(&mut session, &mut serializer, &mut deserializer);
	start_publishing(
		TEST_STREAM_KEY,
		stream_id,
		&mut session,
		&mut serializer,
		&mut deserializer,
	);

	let message = RtmpMessage::Amf0Command {
		command_name: "deleteStream".to_string(),
		transaction_id: 4_f64,
		command_object: Amf0Value::Null,
		additional_arguments: vec![Amf0Value::Number(stream_id as f64)],
	};

	let payload = message
		.into_message_payload(RtmpTimestamp::new(1234), stream_id)
		.unwrap();
	let packet = serializer.serialize(&payload, false, false).unwrap();
	let results = session.handle_input(&packet.bytes[..]).unwrap();
	let (_, mut events) = split_results(&mut deserializer, results);

	assert_eq!(events.len(), 1, "Unexpected number of events returned");

	match events.remove(0) {
		ServerSessionEvent::PublishStreamFinished { app_name, stream_key } => {
			assert_eq!(app_name, TEST_APP_NAME, "Unexpected app name");
			assert_eq!(stream_key, TEST_STREAM_KEY, "Unexpected stream key");
		}

		event => panic!("Expected PublishStreamFinished event, instead got: {:?}", event),
	}
}

#[test]
fn publish_finished_event_raised_when_close_stream_invoked_on_publishing_stream() {
	let (mut deserializer, mut serializer, mut session) = common_basic_setup();
	perform_connection(TEST_APP_NAME, &mut session, &mut serializer, &mut deserializer);
	let stream_id = create_active_stream(&mut session, &mut serializer, &mut deserializer);
	start_publishing(
		TEST_STREAM_KEY,
		stream_id,
		&mut session,
		&mut serializer,
		&mut deserializer,
	);

	let message = RtmpMessage::Amf0Command {
		command_name: "closeStream".to_string(),
		transaction_id: 4_f64,
		command_object: Amf0Value::Null,
		additional_arguments: vec![Amf0Value::Number(stream_id as f64)],
	};

	let payload = message
		.into_message_payload(RtmpTimestamp::new(1234), stream_id)
		.unwrap();
	let packet = serializer.serialize(&payload, false, false).unwrap();
	let results = session.handle_input(&packet.bytes[..]).unwrap();
	let (_, mut events) = split_results(&mut deserializer, results);

	assert_eq!(events.len(), 1, "Unexpected number of events returned");

	match events.remove(0) {
		ServerSessionEvent::PublishStreamFinished { app_name, stream_key } => {
			assert_eq!(app_name, TEST_APP_NAME, "Unexpected app name");
			assert_eq!(stream_key, TEST_STREAM_KEY, "Unexpected stream key");
		}

		event => panic!("Expected PublishStreamFinished event, instead got: {:?}", event),
	}
}

#[test]
fn can_request_publishing_on_closed_stream() {
	let (mut deserializer, mut serializer, mut session) = common_basic_setup();
	perform_connection(TEST_APP_NAME, &mut session, &mut serializer, &mut deserializer);
	let stream_id = create_active_stream(&mut session, &mut serializer, &mut deserializer);
	start_publishing(
		TEST_STREAM_KEY,
		stream_id,
		&mut session,
		&mut serializer,
		&mut deserializer,
	);
	close_stream(stream_id, &mut session, &mut serializer, &mut deserializer);

	let message = RtmpMessage::Amf0Command {
		command_name: "publish".to_string(),
		transaction_id: 5.0,
		command_object: Amf0Value::Null,
		additional_arguments: vec![
			Amf0Value::Utf8String(TEST_STREAM_KEY.to_string()),
			Amf0Value::Utf8String("live".to_string()),
		],
	};

	let publish_payload = message
		.into_message_payload(RtmpTimestamp::new(2000), stream_id)
		.unwrap();
	let publish_packet = serializer.serialize(&publish_payload, false, false).unwrap();
	let publish_results = session.handle_input(&publish_packet.bytes[..]).unwrap();
	let (_, events) = split_results(&mut deserializer, publish_results);

	assert_eq!(events.len(), 1, "Unexpected number of events returned");
	match events[0] {
		ServerSessionEvent::PublishStreamRequested {
			ref app_name,
			ref stream_key,
			request_id: _,
			mode: PublishMode::Live,
		} => {
			assert_eq!(app_name, &TEST_APP_NAME, "Unexpected app name");
			assert_eq!(stream_key, &TEST_STREAM_KEY, "Unexpected stream key");
		}

		_ => panic!("Unexpected first event found: {:?}", events[0]),
	}
}

#[test]
fn can_accept_play_command_with_no_optional_parameters_to_requested_stream_key() {
	let (mut deserializer, mut serializer, mut session) = common_basic_setup();
	perform_connection(TEST_APP_NAME, &mut session, &mut serializer, &mut deserializer);
	let stream_id = create_active_stream(&mut session, &mut serializer, &mut deserializer);

	let message = RtmpMessage::Amf0Command {
		command_name: "play".to_string(),
		transaction_id: 4.0,
		command_object: Amf0Value::Null,
		additional_arguments: vec![Amf0Value::Utf8String(TEST_STREAM_KEY.to_string())],
	};

	let play_payload = message.into_message_payload(RtmpTimestamp::new(0), stream_id).unwrap();
	let play_packet = serializer.serialize(&play_payload, false, false).unwrap();
	let play_results = session.handle_input(&play_packet.bytes[..]).unwrap();
	let (_, mut events) = split_results(&mut deserializer, play_results);

	assert_eq!(events.len(), 1, "Unexpected number of events returned");
	let request_id = match events.remove(0) {
		ServerSessionEvent::PlayStreamRequested {
			app_name,
			stream_key,
			start_at,
			duration,
			reset,
			request_id,
			stream_id: sid,
		} => {
			assert_eq!(app_name, TEST_APP_NAME, "Unexpected app name");
			assert_eq!(stream_key, TEST_STREAM_KEY, "Unexpected stream key");
			assert_eq!(start_at, PlayStartValue::LiveOrRecorded, "Unexpected start at");
			assert_eq!(duration, None, "Unexpected duration");
			assert_eq!(reset, false, "Unexpected reset value");
			assert_eq!(sid, stream_id, "Unexpected stream id");
			request_id
		}

		x => panic!("Expected play event but instead received: {:?}", x),
	};

	let accept_results = session.accept_request(request_id).unwrap();
	let (mut responses, _) = split_results(&mut deserializer, accept_results);
	assert_eq!(responses.len(), 5, "Unexpected number of messages received");

	verify_is_onstatus(&responses.remove(0).1, "status", "NetStream.Play.Reset");

	match responses.remove(0) {
		(
			_,
			RtmpMessage::UserControl {
				event_type,
				stream_id: sid,
				buffer_length,
				timestamp,
			},
		) => {
			assert_eq!(
				event_type,
				UserControlEventType::StreamBegin,
				"Unexpected user control event type received"
			);
			assert_eq!(sid, Some(stream_id), "Unexpected user control stream id");
			assert_eq!(buffer_length, None, "Unexpected user control buffer length");
			assert_eq!(timestamp, None, "Unexpected user control timestamp");
		}

		x => println!("Expected stream begin message, instead received: {:?}", x),
	}

	verify_is_onstatus(&responses.remove(0).1, "status", "NetStream.Play.Start");

	match responses.remove(0) {
		(_, RtmpMessage::Amf0Data { values }) => {
			assert_eq!(values.len(), 3, "Unexpected number of values received");
			assert_eq!(
				values[0],
				Amf0Value::Utf8String("|RtmpSampleAccess".to_string()),
				"Incorrect first data argument"
			);
			assert_eq!(values[1], Amf0Value::Boolean(false), "Incorrect second data argument");
			assert_eq!(values[2], Amf0Value::Boolean(false), "Incorrect third data argument");
		}

		x => println!("Expected RtmpSampleAccess data argument, instead received: {:?}", x),
	}

	match responses.remove(0) {
		(_, RtmpMessage::Amf0Data { mut values }) => {
			assert_eq!(values.len(), 2, "Unexpected number of values received");
			assert_eq!(
				values[0],
				Amf0Value::Utf8String("onStatus".to_string()),
				"Unexpected first data argument"
			);
			match values.remove(1) {
				Amf0Value::Object(ref properties) => {
					assert_eq!(
						properties.get("code"),
						Some(&Amf0Value::Utf8String("NetStream.Data.Start".to_string())),
						"Unexpected code value"
					);
				}

				x => panic!("Expected object for 2nd data argument, instead received: {:?}", x),
			}
		}

		x => panic!("Expected onStatus data argument, instead received: {:?}", x),
	}
}

#[test]
fn can_accept_play_command_with_all_optional_parameters_to_requested_stream_key() {
	let (mut deserializer, mut serializer, mut session) = common_basic_setup();
	perform_connection(TEST_APP_NAME, &mut session, &mut serializer, &mut deserializer);
	let stream_id = create_active_stream(&mut session, &mut serializer, &mut deserializer);

	let message = RtmpMessage::Amf0Command {
		command_name: "play".to_string(),
		transaction_id: 4.0,
		command_object: Amf0Value::Null,
		additional_arguments: vec![
			Amf0Value::Utf8String(TEST_STREAM_KEY.to_string()),
			Amf0Value::Number(5.0),   // Start argument
			Amf0Value::Number(25.0),  // Duration,
			Amf0Value::Boolean(true), // reset
		],
	};

	let play_payload = message.into_message_payload(RtmpTimestamp::new(0), stream_id).unwrap();
	let play_packet = serializer.serialize(&play_payload, false, false).unwrap();
	let play_results = session.handle_input(&play_packet.bytes[..]).unwrap();
	let (_, mut events) = split_results(&mut deserializer, play_results);

	assert_eq!(events.len(), 1, "Unexpected number of events returned");
	let request_id = match events.remove(0) {
		ServerSessionEvent::PlayStreamRequested {
			app_name,
			stream_key,
			start_at,
			duration,
			reset,
			request_id,
			stream_id: sid,
		} => {
			assert_eq!(app_name, TEST_APP_NAME, "Unexpected app name");
			assert_eq!(stream_key, TEST_STREAM_KEY, "Unexpected stream key");
			assert_eq!(start_at, PlayStartValue::StartTimeInSeconds(5), "Unexpected start at");
			assert_eq!(duration, Some(25), "Unexpected duration");
			assert_eq!(reset, true, "Unexpected reset value");
			assert_eq!(sid, stream_id, "Unexpected stream id");
			request_id
		}

		x => panic!("Expected play event but instead received: {:?}", x),
	};

	let accept_results = session.accept_request(request_id).unwrap();
	consume_results(&mut deserializer, accept_results);
}

#[test]
fn play_finished_event_when_close_stream_invoked() {
	let (mut deserializer, mut serializer, mut session) = common_basic_setup();
	perform_connection(TEST_APP_NAME, &mut session, &mut serializer, &mut deserializer);
	let stream_id = create_active_stream(&mut session, &mut serializer, &mut deserializer);

	start_playing(
		TEST_STREAM_KEY,
		stream_id,
		&mut session,
		&mut serializer,
		&mut deserializer,
	);

	let message = RtmpMessage::Amf0Command {
		command_name: "closeStream".to_string(),
		transaction_id: 4_f64,
		command_object: Amf0Value::Null,
		additional_arguments: vec![Amf0Value::Number(stream_id as f64)],
	};

	let payload = message
		.into_message_payload(RtmpTimestamp::new(1234), stream_id)
		.unwrap();
	let packet = serializer.serialize(&payload, false, false).unwrap();
	let results = session.handle_input(&packet.bytes[..]).unwrap();
	let (_, mut events) = split_results(&mut deserializer, results);

	assert_eq!(events.len(), 1, "Unexpected number of events returned");

	match events.remove(0) {
		ServerSessionEvent::PlayStreamFinished { app_name, stream_key } => {
			assert_eq!(app_name, TEST_APP_NAME, "Unexpected app name");
			assert_eq!(stream_key, TEST_STREAM_KEY, "Unexpected stream key");
		}

		event => panic!("Expected PublishStreamFinished event, instead got: {:?}", event),
	}
}

#[test]
fn play_finished_event_when_delete_stream_invoked_on_playing_stream() {
	let (mut deserializer, mut serializer, mut session) = common_basic_setup();
	perform_connection(TEST_APP_NAME, &mut session, &mut serializer, &mut deserializer);
	let stream_id = create_active_stream(&mut session, &mut serializer, &mut deserializer);

	start_playing(
		TEST_STREAM_KEY,
		stream_id,
		&mut session,
		&mut serializer,
		&mut deserializer,
	);

	let message = RtmpMessage::Amf0Command {
		command_name: "deleteStream".to_string(),
		transaction_id: 4_f64,
		command_object: Amf0Value::Null,
		additional_arguments: vec![Amf0Value::Number(stream_id as f64)],
	};

	let payload = message
		.into_message_payload(RtmpTimestamp::new(1234), stream_id)
		.unwrap();
	let packet = serializer.serialize(&payload, false, false).unwrap();
	let results = session.handle_input(&packet.bytes[..]).unwrap();
	let (_, mut events) = split_results(&mut deserializer, results);

	assert_eq!(events.len(), 1, "Unexpected number of events returned");

	match events.remove(0) {
		ServerSessionEvent::PlayStreamFinished { app_name, stream_key } => {
			assert_eq!(app_name, TEST_APP_NAME, "Unexpected app name");
			assert_eq!(stream_key, TEST_STREAM_KEY, "Unexpected stream key");
		}

		event => panic!("Expected PublishStreamFinished event, instead got: {:?}", event),
	}
}

#[test]
fn can_send_metadata_to_playing_stream() {
	let (mut deserializer, mut serializer, mut session) = common_basic_setup();
	perform_connection(TEST_APP_NAME, &mut session, &mut serializer, &mut deserializer);
	let stream_id = create_active_stream(&mut session, &mut serializer, &mut deserializer);
	start_playing(
		TEST_STREAM_KEY,
		stream_id,
		&mut session,
		&mut serializer,
		&mut deserializer,
	);

	let metadata = StreamMetadata {
		audio_bitrate_kbps: Some(100),
		audio_channels: Some(101),
		audio_codec_id: Some(7),
		audio_is_stereo: Some(true),
		audio_sample_rate: Some(103),
		encoder: Some("104".to_string()),
		video_bitrate_kbps: Some(105),
		video_codec_id: Some(10),
		video_frame_rate: Some(107.0),
		video_height: Some(108),
		video_width: Some(109),
	};

	let packet = session.send_metadata(stream_id, &metadata).unwrap();
	let payload = deserializer.get_next_message(&packet.bytes[..]).unwrap().unwrap();
	let message = payload.to_rtmp_message().unwrap();

	match message {
		RtmpMessage::Amf0Data { mut values } => {
			assert_eq!(values.len(), 2, "2 amf0 data values expected");

			match values.remove(0) {
				Amf0Value::Utf8String(string) => {
					assert_eq!(string, "onMetaData");
				}
				x => panic!("Expected 'onMetaData' received: {:?}", x),
			}

			match values.remove(0) {
				Amf0Value::Object(properties) => {
					assert_eq!(
						properties.get("width"),
						Some(&Amf0Value::Number(109.0)),
						"Unexpected width"
					);
					assert_eq!(
						properties.get("height"),
						Some(&Amf0Value::Number(108.0)),
						"Unexpected height"
					);
					assert_eq!(
						properties.get("videocodecid"),
						Some(&Amf0Value::Number(10.0)),
						"Unexpected videocodecid"
					);
					assert_eq!(
						properties.get("videodatarate"),
						Some(&Amf0Value::Number(105.0)),
						"Unexpected videodatarate"
					);
					assert_eq!(
						properties.get("framerate"),
						Some(&Amf0Value::Number(107.0)),
						"Unexpected framerate"
					);
					assert_eq!(
						properties.get("audiocodecid"),
						Some(&Amf0Value::Number(7.0)),
						"Unexpected audiocodecid"
					);
					assert_eq!(
						properties.get("audiodatarate"),
						Some(&Amf0Value::Number(100.0)),
						"Unexpected audiodatarate"
					);
					assert_eq!(
						properties.get("audiosamplerate"),
						Some(&Amf0Value::Number(103.0)),
						"Unexpected audiosamplerate"
					);
					assert_eq!(
						properties.get("audiochannels"),
						Some(&Amf0Value::Number(101.0)),
						"Unexpected audiochannels"
					);
					assert_eq!(
						properties.get("stereo"),
						Some(&Amf0Value::Boolean(true)),
						"Unexpected stereo"
					);
					assert_eq!(
						properties.get("encoder"),
						Some(&Amf0Value::Utf8String("104".to_string())),
						"Unexpected encoder"
					);
				}

				x => panic!("Expected Amf0 object with metadata, instead received: {:?}", x),
			}
		}

		x => panic!("Expected Amf0 data, instead received: {:?}", x),
	}
}

#[test]
fn can_send_video_data_to_playing_stream() {
	let (mut deserializer, mut serializer, mut session) = common_basic_setup();
	perform_connection(TEST_APP_NAME, &mut session, &mut serializer, &mut deserializer);
	let stream_id = create_active_stream(&mut session, &mut serializer, &mut deserializer);
	start_playing(
		TEST_STREAM_KEY,
		stream_id,
		&mut session,
		&mut serializer,
		&mut deserializer,
	);

	let original_data = Bytes::from(vec![1_u8, 2_u8, 3_u8]);
	let timestamp = RtmpTimestamp::new(500);
	let packet = session
		.send_video_data(stream_id, original_data.clone(), timestamp.clone(), false)
		.unwrap();
	let payload = deserializer.get_next_message(&packet.bytes[..]).unwrap().unwrap();
	let message = payload.to_rtmp_message().unwrap();

	match message {
		RtmpMessage::VideoData { data: message_data } => {
			assert_eq!(
				payload.timestamp, timestamp,
				"Serialized timestamp did not match original timestamp"
			);
			assert_eq!(
				&message_data[..],
				&original_data[..],
				"Packetized data did not match original data"
			);
		}

		x => panic!("Expected video data message, received: {:?}", x),
	}
}

#[test]
fn can_send_audio_data_to_playing_stream() {
	let (mut deserializer, mut serializer, mut session) = common_basic_setup();
	perform_connection(TEST_APP_NAME, &mut session, &mut serializer, &mut deserializer);
	let stream_id = create_active_stream(&mut session, &mut serializer, &mut deserializer);
	start_playing(
		TEST_STREAM_KEY,
		stream_id,
		&mut session,
		&mut serializer,
		&mut deserializer,
	);

	let original_data = Bytes::from(vec![1_u8, 2_u8, 3_u8]);
	let timestamp = RtmpTimestamp::new(500);
	let packet = session
		.send_audio_data(stream_id, original_data.clone(), timestamp.clone(), false)
		.unwrap();
	let payload = deserializer.get_next_message(&packet.bytes[..]).unwrap().unwrap();
	let message = payload.to_rtmp_message().unwrap();

	match message {
		RtmpMessage::AudioData { data: message_data } => {
			assert_eq!(
				payload.timestamp, timestamp,
				"Serialized timestamp did not match original timestamp"
			);
			assert_eq!(
				&message_data[..],
				&original_data[..],
				"Packetized data did not match original data"
			);
		}

		x => panic!("Expected video data message, received: {:?}", x),
	}
}

#[test]
fn automatically_responds_to_ping_requests() {
	let (mut deserializer, mut serializer, mut session) = common_basic_setup();
	perform_connection(TEST_APP_NAME, &mut session, &mut serializer, &mut deserializer);

	let message = RtmpMessage::UserControl {
		event_type: UserControlEventType::PingRequest,
		timestamp: Some(RtmpTimestamp::new(5230)),
		stream_id: None,
		buffer_length: None,
	};

	let payload = message.into_message_payload(RtmpTimestamp::new(6000), 0).unwrap();
	let packet = serializer.serialize(&payload, false, false).unwrap();
	let results = session.handle_input(&packet.bytes[..]).unwrap();
	let (mut responses, _) = split_results(&mut deserializer, results);

	assert_eq!(responses.len(), 1, "Expected one response for handling ping request");
	match responses.remove(0) {
		(
			_,
			RtmpMessage::UserControl {
				event_type,
				timestamp: Some(timestamp),
				stream_id: None,
				buffer_length: None,
			},
		) => {
			assert_eq!(event_type, UserControlEventType::PingResponse, "Unexpected event type");
			assert_eq!(timestamp, RtmpTimestamp::new(5230), "Unexpected timestamp");
		}

		x => panic!("Expected PingResponse, found {:?}", x),
	}
}

#[test]
fn event_raised_when_ping_response_received() {
	let (mut deserializer, mut serializer, mut session) = common_basic_setup();
	perform_connection(TEST_APP_NAME, &mut session, &mut serializer, &mut deserializer);

	let message = RtmpMessage::UserControl {
		event_type: UserControlEventType::PingResponse,
		timestamp: Some(RtmpTimestamp::new(5230)),
		stream_id: None,
		buffer_length: None,
	};

	let payload = message.into_message_payload(RtmpTimestamp::new(6000), 0).unwrap();
	let packet = serializer.serialize(&payload, false, false).unwrap();
	let results = session.handle_input(&packet.bytes[..]).unwrap();
	let (_, mut events) = split_results(&mut deserializer, results);

	assert_eq!(events.len(), 1, "One event expected");
	match events.remove(0) {
		ServerSessionEvent::PingResponseReceived { timestamp } => {
			assert_eq!(timestamp, RtmpTimestamp::new(5230), "Unexpected timestamp received");
		}

		x => panic!("Expected PingResponse event, instead received {:?}", x),
	}
}

#[test]
fn can_send_ping_request() {
	let (mut deserializer, mut serializer, mut session) = common_basic_setup();
	perform_connection(TEST_APP_NAME, &mut session, &mut serializer, &mut deserializer);

	let (packet, sent_timestamp) = session.send_ping_request().unwrap();
	let payload = deserializer.get_next_message(&packet.bytes[..]).unwrap().unwrap();
	let message = payload.to_rtmp_message().unwrap();

	match message {
		RtmpMessage::UserControl {
			event_type,
			timestamp: Some(timestamp),
			buffer_length: None,
			stream_id: None,
		} => {
			assert_eq!(
				event_type,
				UserControlEventType::PingRequest,
				"Unexpected user control event type"
			);
			assert_eq!(timestamp, sent_timestamp, "Unexpected timestamp in outbound message");
		}

		x => panic!("Expected PingRequest being sent, instead found {:?}", x),
	}
}

#[test]
fn can_finish_playing_stream() {
	let (mut deserializer, mut serializer, mut session) = common_basic_setup();
	perform_connection(TEST_APP_NAME, &mut session, &mut serializer, &mut deserializer);
	let stream_id = create_active_stream(&mut session, &mut serializer, &mut deserializer);
	start_playing(
		TEST_STREAM_KEY,
		stream_id,
		&mut session,
		&mut serializer,
		&mut deserializer,
	);

	let packet = session.finish_playing(stream_id).unwrap();
	let payload = deserializer.get_next_message(&packet.bytes[..]).unwrap().unwrap();
	let message = payload.to_rtmp_message().unwrap();

	verify_is_onstatus(&message, "status", "NetStream.Play.Complete");
}

#[test]
fn sends_ack_after_receiving_window_ack_bytes() {
	let (mut deserializer, mut serializer, mut session) = common_basic_setup();
	perform_connection(TEST_APP_NAME, &mut session, &mut serializer, &mut deserializer);

	let window_ack_message = RtmpMessage::WindowAcknowledgement { size: 100 };
	let window_ack_payload = window_ack_message
		.into_message_payload(RtmpTimestamp::new(0), 0)
		.unwrap();
	let window_ack_packet = serializer.serialize(&window_ack_payload, false, false).unwrap();
	let results = session.handle_input(&window_ack_packet.bytes[..]).unwrap();
	consume_results(&mut deserializer, results);

	let mut bytes = BytesMut::new();
	bytes.extend_from_slice(&[1; 101]);
	let video_message = RtmpMessage::VideoData { data: bytes.freeze() };
	let video_payload = video_message.into_message_payload(RtmpTimestamp::new(0), 0).unwrap();
	let video_packet = serializer.serialize(&video_payload, false, false).unwrap();
	let results = session.handle_input(&video_packet.bytes[..]).unwrap();
	let (mut responses, _) = split_results(&mut deserializer, results);

	assert_eq!(responses.len(), 1, "Unexpected number of responses");
	match responses.remove(0) {
		(_, RtmpMessage::Acknowledgement { sequence_number: _ }) => (), // No good way to predict sequence number
		x => panic!("Expected Acknowledgement, instead received: {:?}", x),
	}

	let mut bytes = BytesMut::new();
	bytes.extend_from_slice(&[1; 1]);
	let video_message = RtmpMessage::VideoData { data: bytes.freeze() };
	let video_payload = video_message.into_message_payload(RtmpTimestamp::new(0), 0).unwrap();
	let video_packet = serializer.serialize(&video_payload, false, false).unwrap();
	let results = session.handle_input(&video_packet.bytes[..]).unwrap();
	let (responses, _) = split_results(&mut deserializer, results);
	assert_eq!(responses.len(), 0, "Expected no responses");

	let mut bytes = BytesMut::new();
	bytes.extend_from_slice(&[1; 100]);
	let video_message = RtmpMessage::VideoData { data: bytes.freeze() };
	let video_payload = video_message.into_message_payload(RtmpTimestamp::new(0), 0).unwrap();
	let video_packet = serializer.serialize(&video_payload, false, false).unwrap();
	let results = session.handle_input(&video_packet.bytes[..]).unwrap();
	let (mut responses, _) = split_results(&mut deserializer, results);
	assert_eq!(responses.len(), 1, "Unexpected number of responses");
	match responses.remove(0) {
		(_, RtmpMessage::Acknowledgement { sequence_number: _ }) => (), // No good way to predict sequence number
		x => panic!("Expected Acknowledgement, instead received: {:?}", x),
	}
}

#[test]
fn event_raised_when_client_sends_an_acknowledgement() {
	let (mut deserializer, mut serializer, mut session) = common_basic_setup();
	perform_connection(TEST_APP_NAME, &mut session, &mut serializer, &mut deserializer);

	let message = RtmpMessage::Acknowledgement { sequence_number: 1234 };
	let payload = message.into_message_payload(RtmpTimestamp::new(0), 0).unwrap();
	let packet = serializer.serialize(&payload, false, false).unwrap();
	let results = session.handle_input(&packet.bytes[..]).unwrap();
	let (_, mut events) = split_results(&mut deserializer, results);

	assert_eq!(events.len(), 1, "Unexpected number of events");
	match events.remove(0) {
		ServerSessionEvent::AcknowledgementReceived { bytes_received } => {
			assert_eq!(bytes_received, 1234, "Incorrect number of bytes received in event");
		}

		x => panic!("Expected acknowledgement received event, instead got: {:?}", x),
	}
}

fn get_basic_config() -> ServerSessionConfig {
	ServerSessionConfig {
		chunk_size: DEFAULT_CHUNK_SIZE,
		fms_version: "fms_version".to_string(),
		peer_bandwidth: DEFAULT_PEER_BANDWIDTH,
		window_ack_size: DEFAULT_WINDOW_ACK_SIZE,
		send_on_bw_done_message_on_start: true,
	}
}

fn common_setup(config: &ServerSessionConfig) -> (ChunkDeserializer, ChunkSerializer, ServerSession) {
	let mut deserializer = ChunkDeserializer::new();
	let serializer = ChunkSerializer::new();
	let (session, initial_results) = ServerSession::new(config.clone()).unwrap();
	consume_results(&mut deserializer, initial_results);
	(deserializer, serializer, session)
}

fn common_basic_setup() -> (ChunkDeserializer, ChunkSerializer, ServerSession) {
	common_setup(&get_basic_config())
}

fn split_results(
	deserializer: &mut ChunkDeserializer,
	mut results: Vec<ServerSessionResult>,
) -> (Vec<(MessagePayload, RtmpMessage)>, Vec<ServerSessionEvent>) {
	let mut responses = Vec::new();
	let mut events = Vec::new();

	for result in results.drain(..) {
		match result {
			ServerSessionResult::OutboundResponse(packet) => {
				let payload = deserializer.get_next_message(&packet.bytes[..]).unwrap().unwrap();
				let message = payload.to_rtmp_message().unwrap();
				match message {
					RtmpMessage::SetChunkSize { size } => deserializer.set_max_chunk_size(size as usize).unwrap(),
					_ => (),
				}

				println!("response received from server: {:?}", message);
				responses.push((payload, message));
			}

			ServerSessionResult::RaisedEvent(event) => {
				println!("event received from server: {:?}", event);
				events.push(event);
			}

			_ => (),
		}
	}

	(responses, events)
}

fn consume_results(deserializer: &mut ChunkDeserializer, results: Vec<ServerSessionResult>) {
	// Needed to keep the deserializer up to date
	split_results(deserializer, results);
}

fn create_connect_message(app_name: String, timestamp: u32, stream_id: u32, object_encoding: f64) -> MessagePayload {
	let mut properties = HashMap::new();
	properties.insert("app".to_string(), Amf0Value::Utf8String(app_name));
	properties.insert("objectEncoding".to_string(), Amf0Value::Number(object_encoding));

	let message = RtmpMessage::Amf0Command {
		command_name: "connect".to_string(),
		transaction_id: 1.0,
		command_object: Amf0Value::Object(properties),
		additional_arguments: vec![],
	};

	let timestamp = RtmpTimestamp::new(timestamp);
	let payload = message.into_message_payload(timestamp, stream_id).unwrap();
	payload
}

fn perform_connection(
	app_name: &str,
	session: &mut ServerSession,
	serializer: &mut ChunkSerializer,
	deserializer: &mut ChunkDeserializer,
) {
	let connect_payload = create_connect_message(app_name.to_string(), 15, 0, 0.0);
	let connect_packet = serializer.serialize(&connect_payload, true, false).unwrap();
	let connect_results = session.handle_input(&connect_packet.bytes[..]).unwrap();
	assert_eq!(
		connect_results.len(),
		1,
		"Unexpected number of responses when handling connect request message"
	);

	let (_, events) = split_results(deserializer, connect_results);
	assert_eq!(events.len(), 1, "Unexpected number of events returned");
	let request_id = match events[0] {
		ServerSessionEvent::ConnectionRequested {
			ref app_name,
			request_id,
			..
		} if app_name == "some_app" => request_id,
		_ => panic!("First event was not as expected: {:?}", events[0]),
	};

	let results = session.accept_request(request_id).unwrap();
	consume_results(deserializer, results);

	// Assume it was successful
}

fn create_active_stream(
	session: &mut ServerSession,
	serializer: &mut ChunkSerializer,
	deserializer: &mut ChunkDeserializer,
) -> u32 {
	let message = RtmpMessage::Amf0Command {
		command_name: "createStream".to_string(),
		transaction_id: 4.0,
		command_object: Amf0Value::Null,
		additional_arguments: Vec::new(),
	};

	let payload = message.into_message_payload(RtmpTimestamp::new(0), 0).unwrap();
	let packet = serializer.serialize(&payload, true, false).unwrap();
	let results = session.handle_input(&packet.bytes[..]).unwrap();
	let (responses, _) = split_results(deserializer, results);

	assert_eq!(responses.len(), 1, "Unexpected number of responses returned");
	match responses[0] {
		(
			_,
			RtmpMessage::Amf0Command {
				ref command_name,
				transaction_id,
				command_object: Amf0Value::Null,
				ref additional_arguments,
			},
		) if command_name == "_result" && transaction_id == 4.0 => {
			assert_eq!(
				additional_arguments.len(),
				1,
				"Unexpected number of additional arguments in response"
			);
			match additional_arguments[0] {
				Amf0Value::Number(x) => return x as u32,
				_ => panic!("First additional argument was not an Amf0Value::Number"),
			}
		}

		_ => panic!("First response was not the expected value: {:?}", responses[0]),
	}
}

fn close_stream(
	stream_id: u32,
	session: &mut ServerSession,
	serializer: &mut ChunkSerializer,
	deserializer: &mut ChunkDeserializer,
) {
	let message = RtmpMessage::Amf0Command {
		command_name: "closeStream".to_string(),
		transaction_id: 4_f64,
		command_object: Amf0Value::Null,
		additional_arguments: vec![Amf0Value::Number(stream_id as f64)],
	};

	let payload = message.into_message_payload(RtmpTimestamp::new(0), stream_id).unwrap();
	let packet = serializer.serialize(&payload, false, false).unwrap();
	let results = session.handle_input(&packet.bytes[..]).unwrap();
	consume_results(deserializer, results);
}

fn start_publishing(
	stream_key: &str,
	stream_id: u32,
	session: &mut ServerSession,
	serializer: &mut ChunkSerializer,
	deserializer: &mut ChunkDeserializer,
) {
	let message = RtmpMessage::Amf0Command {
		command_name: "publish".to_string(),
		transaction_id: 5.0,
		command_object: Amf0Value::Null,
		additional_arguments: vec![
			Amf0Value::Utf8String(stream_key.to_string()),
			Amf0Value::Utf8String("live".to_string()),
		],
	};

	let publish_payload = message.into_message_payload(RtmpTimestamp::new(0), stream_id).unwrap();
	let publish_packet = serializer.serialize(&publish_payload, false, false).unwrap();
	let publish_results = session.handle_input(&publish_packet.bytes[..]).unwrap();
	let (_, events) = split_results(deserializer, publish_results);

	assert_eq!(events.len(), 1, "Unexpected number of events returned");
	let request_id = match events[0] {
		ServerSessionEvent::PublishStreamRequested {
			ref app_name,
			ref stream_key,
			request_id: returned_request_id,
			mode: PublishMode::Live,
		} if app_name == "some_app" && stream_key == "stream_key" => returned_request_id,

		_ => panic!("Unexpected first event found: {:?}", events[0]),
	};

	let accept_results = session.accept_request(request_id).unwrap();
	consume_results(deserializer, accept_results);
}

fn start_playing(
	stream_key: &str,
	stream_id: u32,
	session: &mut ServerSession,
	serializer: &mut ChunkSerializer,
	deserializer: &mut ChunkDeserializer,
) {
	let message = RtmpMessage::Amf0Command {
		command_name: "play".to_string(),
		transaction_id: 4.0,
		command_object: Amf0Value::Null,
		additional_arguments: vec![
			Amf0Value::Utf8String(stream_key.to_string()),
			Amf0Value::Number(5.0),   // Start argument
			Amf0Value::Number(25.0),  // Duration,
			Amf0Value::Boolean(true), // reset
		],
	};

	let play_payload = message.into_message_payload(RtmpTimestamp::new(0), stream_id).unwrap();
	let play_packet = serializer.serialize(&play_payload, false, false).unwrap();
	let play_results = session.handle_input(&play_packet.bytes[..]).unwrap();
	let (_, mut events) = split_results(deserializer, play_results);

	assert_eq!(events.len(), 1, "Unexpected number of events returned");
	let request_id = match events.remove(0) {
		ServerSessionEvent::PlayStreamRequested {
			app_name: _,
			stream_key: _,
			start_at: _,
			duration: _,
			reset: _,
			request_id,
			stream_id: _,
		} => request_id,

		x => panic!("Expected play event but instead received: {:?}", x),
	};

	let accept_results = session.accept_request(request_id).unwrap();
	consume_results(deserializer, accept_results);
}

fn verify_is_onstatus(subject: &RtmpMessage, expected_status: &str, expected_code: &str) {
	match subject {
		RtmpMessage::Amf0Command {
			command_name,
			transaction_id,
			command_object,
			additional_arguments,
		} => {
			assert_eq!(command_name.as_str(), "onStatus", "Unexpected command name");
			assert_eq!(transaction_id, &0.0, "Unexpected transaction id");
			assert_eq!(command_object, &Amf0Value::Null, "Unexpected command object");
			assert_eq!(
				additional_arguments.len(),
				1,
				"Unexpected number of additional arguments"
			);

			match additional_arguments.first().unwrap() {
				Amf0Value::Object(properties) => {
					assert_eq!(
						properties.get("level"),
						Some(&Amf0Value::Utf8String(expected_status.to_string())),
						"Unexpected level value"
					);
					assert_eq!(
						properties.get("code"),
						Some(&Amf0Value::Utf8String(expected_code.to_string())),
						"Unexpected code value"
					);
					assert!(properties.contains_key("description"), "Expected description");
				}

				x => panic!("Expected amf0 object, but instead argument was: {:?}", x),
			}
		}

		x => panic!("Expected Amf0Command command, instead received: {:?}", x),
	}
}
