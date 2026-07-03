use super::*;
use crate::rml::chunk_io::{ChunkDeserializer, ChunkSerializer, Packet};
use crate::rml::messages::{MessagePayload, RtmpMessage, UserControlEventType};
use bytes::Bytes;
use bytes::BytesMut;
use rand;
use rml_amf0::Amf0Value;
use std::collections::HashMap;

#[test]
fn new_session_and_successful_connect_creates_set_chunk_size_message() {
	let app_name = "test".to_string();
	let mut config = ClientSessionConfig::new();
	config.chunk_size = 1111;

	let mut deserializer = ChunkDeserializer::new();
	let mut serializer = ChunkSerializer::new();
	let (mut session, initial_results) = ClientSession::new(config.clone()).unwrap();
	consume_results(&mut deserializer, initial_results);

	perform_successful_connect(app_name.clone(), &mut session, &mut serializer, &mut deserializer);

	assert_eq!(
		deserializer.get_max_chunk_size(),
		1111,
		"Incorrect deserializer default chunk size"
	);
}

#[test]
fn can_send_connect_request() {
	let app_name = "test".to_string();
	let config = ClientSessionConfig::new();
	let mut deserializer = ChunkDeserializer::new();
	let (mut session, initial_results) = ClientSession::new(config.clone()).unwrap();
	consume_results(&mut deserializer, initial_results);

	let results = session.request_connection(app_name.clone()).unwrap();
	let (mut responses, _) = split_results(&mut deserializer, vec![results]);

	assert_eq!(responses.len(), 1, "Expected 1 response");
	match responses.remove(0) {
		(
			payload,
			RtmpMessage::Amf0Command {
				command_name,
				transaction_id,
				command_object,
				additional_arguments,
			},
		) => {
			assert_eq!(payload.message_stream_id, 0, "Unexpected message stream id");
			assert_eq!(command_name, "connect".to_string(), "Unexpected command name");
			assert_ne!(transaction_id, 0.0, "Transaction id should not be zero");
			assert_eq!(additional_arguments.len(), 0, "Expected no additional arguments");

			match command_object {
				Amf0Value::Object(properties) => {
					assert_eq!(
						properties.get("app"),
						Some(&Amf0Value::Utf8String(app_name.clone())),
						"Unexpected app name"
					);
					assert_eq!(
						properties.get("objectEncoding"),
						Some(&Amf0Value::Number(0.0)),
						"Unexpected object encoding"
					);
					assert_eq!(
						properties.get("flashVer"),
						Some(&Amf0Value::Utf8String(config.flash_version.clone())),
						"Unexpected flash version"
					);
				}

				x => panic!(
					"Expected Amf0Value::Object for command object, instead received: {:?}",
					x
				),
			}
		}

		x => panic!("Expected Amf0Command, instead received: {:?}", x),
	}
}

#[test]
fn can_send_connect_request_with_tc_url() {
	let app_name = "test".to_string();
	let mut config = ClientSessionConfig::new();
	let tc_url = "rtmp://1.2.3.4:1935/app".to_string();
	config.tc_url = Some(tc_url.clone());

	let mut deserializer = ChunkDeserializer::new();
	let (mut session, initial_results) = ClientSession::new(config.clone()).unwrap();
	consume_results(&mut deserializer, initial_results);

	let results = session.request_connection(app_name.clone()).unwrap();
	let (mut responses, _) = split_results(&mut deserializer, vec![results]);

	assert_eq!(responses.len(), 1, "Expected 1 response");
	match responses.remove(0) {
		(
			payload,
			RtmpMessage::Amf0Command {
				command_name,
				transaction_id,
				command_object,
				additional_arguments,
			},
		) => {
			assert_eq!(payload.message_stream_id, 0, "Unexpected message stream id");
			assert_eq!(command_name, "connect".to_string(), "Unexpected command name");
			assert_ne!(transaction_id, 0.0, "Transaction id should not be zero");
			assert_eq!(additional_arguments.len(), 0, "Expected no additional arguments");

			match command_object {
				Amf0Value::Object(properties) => {
					assert_eq!(
						properties.get("app"),
						Some(&Amf0Value::Utf8String(app_name.clone())),
						"Unexpected app name"
					);
					assert_eq!(
						properties.get("objectEncoding"),
						Some(&Amf0Value::Number(0.0)),
						"Unexpected object encoding"
					);
					assert_eq!(
						properties.get("flashVer"),
						Some(&Amf0Value::Utf8String(config.flash_version.clone())),
						"Unexpected flash version"
					);
					assert_eq!(
						properties.get("tcUrl"),
						Some(&Amf0Value::Utf8String(tc_url.clone())),
						"Unexpected tcUrl"
					);
				}

				x => panic!(
					"Expected Amf0Value::Object for command object, instead received: {:?}",
					x
				),
			}
		}

		x => panic!("Expected Amf0Command, instead received: {:?}", x),
	}
}

#[test]
fn can_process_connect_success_response() {
	let app_name = "test".to_string();
	let config = ClientSessionConfig::new();
	let mut deserializer = ChunkDeserializer::new();
	let mut serializer = ChunkSerializer::new();
	let (mut session, initial_results) = ClientSession::new(config.clone()).unwrap();
	consume_results(&mut deserializer, initial_results);

	let results = session.request_connection(app_name.clone()).unwrap();
	consume_results(&mut deserializer, vec![results]);

	let response = get_connect_success_response(&mut serializer);
	let results = session.handle_input(&response.bytes[..]).unwrap();
	let (_, mut events) = split_results(&mut deserializer, results);

	assert_eq!(events.len(), 1, "Expected one event returned");
	match events.remove(0) {
		ClientSessionEvent::ConnectionRequestAccepted => (),
		x => panic!("Expected connection accepted event, instead received: {:?}", x),
	}
}

#[test]
fn event_raised_when_connect_request_rejected() {
	let app_name = "test".to_string();
	let config = ClientSessionConfig::new();
	let mut deserializer = ChunkDeserializer::new();
	let mut serializer = ChunkSerializer::new();
	let (mut session, initial_results) = ClientSession::new(config.clone()).unwrap();
	consume_results(&mut deserializer, initial_results);

	let results = session.request_connection(app_name.clone()).unwrap();
	consume_results(&mut deserializer, vec![results]);

	let response = get_connect_error_response(&mut serializer);
	let results = session.handle_input(&response.bytes[..]).unwrap();
	let (_, mut events) = split_results(&mut deserializer, results);

	assert_eq!(events.len(), 1, "Expected one event returned");
	match events.remove(0) {
		ClientSessionEvent::ConnectionRequestRejected { description } => {
			assert!(description.len() > 0, "Expected a non-empty description");
		}

		x => panic!("Expected connection accepted event, instead received: {:?}", x),
	}
}

#[test]
fn error_thrown_when_connect_request_made_after_successful_connection() {
	let app_name = "test".to_string();
	let config = ClientSessionConfig::new();
	let mut deserializer = ChunkDeserializer::new();
	let mut serializer = ChunkSerializer::new();
	let (mut session, initial_results) = ClientSession::new(config.clone()).unwrap();
	consume_results(&mut deserializer, initial_results);

	let results = session.request_connection(app_name.clone()).unwrap();
	consume_results(&mut deserializer, vec![results]);

	let response = get_connect_success_response(&mut serializer);
	let results = session.handle_input(&response.bytes[..]).unwrap();
	consume_results(&mut deserializer, results);

	let error = session.request_connection(app_name.clone()).unwrap_err();
	match error {
		ClientSessionError::CantConnectWhileAlreadyConnected => (),
		x => panic!("Expected CantConnectWhileAlreadyConnected, instead found {:?}", x),
	}
}

#[test]
fn successful_connect_request_sends_window_ack_size() {
	let app_name = "test".to_string();
	let config = ClientSessionConfig::new();
	let mut deserializer = ChunkDeserializer::new();
	let mut serializer = ChunkSerializer::new();
	let (mut session, initial_results) = ClientSession::new(config.clone()).unwrap();
	consume_results(&mut deserializer, initial_results);

	let results = session.request_connection(app_name.clone()).unwrap();
	consume_results(&mut deserializer, vec![results]);

	let response = get_connect_success_response(&mut serializer);
	let results = session.handle_input(&response.bytes[..]).unwrap();
	let (mut responses, _) = split_results(&mut deserializer, results);

	assert_eq!(responses.len(), 2, "Unexpected number of responses received");
	match responses.remove(0) {
		(payload, RtmpMessage::WindowAcknowledgement { size }) => {
			assert_eq!(payload.message_stream_id, 0, "Unexpected message stream id");
			assert_eq!(size, config.window_ack_size, "Unexpected message ack size");
		}

		(_, x) => panic!("Expected window ack message, instead found {:?}", x),
	}
	match responses.remove(0) {
		(_, RtmpMessage::SetChunkSize { size }) => {
			assert_eq!(size, config.chunk_size, "Unexpected chunk size");
		}

		(_, x) => panic!("Expected set chunk size message, instead found {:?}", x),
	}
}

#[test]
fn successful_play_request_workflow() {
	let app_name = "test".to_string();
	let stream_key = "test-key".to_string();
	let config = ClientSessionConfig::new();
	let mut deserializer = ChunkDeserializer::new();
	let mut serializer = ChunkSerializer::new();
	let (mut session, initial_results) = ClientSession::new(config.clone()).unwrap();
	consume_results(&mut deserializer, initial_results);

	perform_successful_connect(app_name.clone(), &mut session, &mut serializer, &mut deserializer);

	let result = session.request_playback(stream_key.clone()).unwrap();
	let (mut responses, _) = split_results(&mut deserializer, vec![result]);

	assert_eq!(responses.len(), 1, "Unexpected number of responses");
	let transaction_id = match responses.remove(0) {
		(
			payload,
			RtmpMessage::Amf0Command {
				command_name,
				transaction_id,
				command_object,
				additional_arguments,
			},
		) => {
			assert_eq!(payload.message_stream_id, 0, "Unexpected stream id");
			assert_eq!(command_name, "createStream", "Unexpected command name");
			assert_eq!(command_object, Amf0Value::Null, "Unexpected command object");
			assert_eq!(
				additional_arguments.len(),
				0,
				"Uenxpected number of additional arguments"
			);
			transaction_id
		}

		x => panic!("Unexpected response seen: {:?}", x),
	};

	let (created_stream_id, create_stream_response) =
		get_create_stream_success_response(transaction_id, &mut serializer);
	let results = session.handle_input(&create_stream_response.bytes[..]).unwrap();
	let (mut responses, _) = split_results(&mut deserializer, results);

	assert_eq!(responses.len(), 2, "Expected one response returned");
	match responses.remove(0) {
		(
			payload,
			RtmpMessage::UserControl {
				event_type,
				stream_id,
				buffer_length,
				timestamp,
			},
		) => {
			assert_eq!(payload.message_stream_id, 0, "Unexpected message stream id");
			assert_eq!(stream_id, Some(created_stream_id), "Unexpected user control stream id");
			assert_eq!(
				event_type,
				UserControlEventType::SetBufferLength,
				"Unexpected user control event type"
			);
			assert_eq!(
				buffer_length,
				Some(config.playback_buffer_length_ms),
				"Unexpected playback buffer lenght"
			);
			assert_eq!(timestamp, None, "Unexpected timestamp");
		}

		x => panic!("Expected set buffer length message, instead received: {:?}", x),
	}

	match responses.remove(0) {
		(
			payload,
			RtmpMessage::Amf0Command {
				command_name,
				transaction_id,
				command_object,
				additional_arguments,
			},
		) => {
			assert_eq!(
				payload.message_stream_id, created_stream_id,
				"Unexpected message stream id"
			);
			assert_eq!(command_name, "play".to_string(), "Unexpected command name");
			assert_eq!(transaction_id, 0.0, "Unexpected transaction id");
			assert_eq!(command_object, Amf0Value::Null, "Unexpected command object");
			assert_eq!(
				additional_arguments.len(),
				1,
				"Unexpected number of additional arguments"
			);
			assert_eq!(
				additional_arguments[0],
				Amf0Value::Utf8String(stream_key.clone()),
				"Unexpected stream key"
			);
		}

		x => panic!("Expected play message, instead received: {:?}", x),
	};

	let play_response = get_play_success_response(&mut serializer, created_stream_id);
	let results = session.handle_input(&play_response.bytes[..]).unwrap();
	let (_, mut events) = split_results(&mut deserializer, results);

	assert_eq!(events.len(), 1, "Expected one event returned");
	match events.remove(0) {
		ClientSessionEvent::PlaybackRequestAccepted => (),
		x => panic!("Expected playback accepted event, instead received: {:?}", x),
	}
}

#[test]
fn active_play_session_raises_events_when_stream_metadata_changes() {
	let config = ClientSessionConfig::new();
	let mut deserializer = ChunkDeserializer::new();
	let mut serializer = ChunkSerializer::new();
	let (mut session, initial_results) = ClientSession::new(config.clone()).unwrap();
	consume_results(&mut deserializer, initial_results);

	perform_successful_connect("test".to_string(), &mut session, &mut serializer, &mut deserializer);
	let stream_id = perform_successful_play_request(config, &mut session, &mut serializer, &mut deserializer);

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
			Amf0Value::Utf8String("onMetaData".to_string()),
			Amf0Value::Object(properties),
		],
	};

	let payload = message.into_message_payload(RtmpTimestamp::new(0), stream_id).unwrap();
	let packet = serializer.serialize(&payload, false, false).unwrap();
	let results = session.handle_input(&packet.bytes[..]).unwrap();
	let (_, mut events) = split_results(&mut deserializer, results);

	assert_eq!(events.len(), 1, "Unexpected number of events received");
	match events.remove(0) {
		ClientSessionEvent::StreamMetadataReceived { metadata } => {
			assert_eq!(metadata.video_width, Some(1920), "Unexpected video width");
			assert_eq!(metadata.video_height, Some(1080), "Unexpected video height");
			assert_eq!(metadata.video_codec_id, Some(10), "Unexpected video codec");
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

		x => panic!("Expected stream metadata received event, instead received: {:?}", x),
	}
}

#[test]
fn active_play_session_raises_events_when_video_data_received() {
	let config = ClientSessionConfig::new();
	let mut deserializer = ChunkDeserializer::new();
	let mut serializer = ChunkSerializer::new();
	let (mut session, initial_results) = ClientSession::new(config.clone()).unwrap();
	consume_results(&mut deserializer, initial_results);

	perform_successful_connect("test".to_string(), &mut session, &mut serializer, &mut deserializer);
	let stream_id = perform_successful_play_request(config, &mut session, &mut serializer, &mut deserializer);

	let video_data = Bytes::from(vec![1, 2, 3, 4, 5]);
	let message = RtmpMessage::VideoData {
		data: video_data.clone(),
	};
	let payload = message
		.into_message_payload(RtmpTimestamp::new(1234), stream_id)
		.unwrap();
	let packet = serializer.serialize(&payload, false, false).unwrap();
	let results = session.handle_input(&packet.bytes[..]).unwrap();
	let (_, mut events) = split_results(&mut deserializer, results);

	assert_eq!(events.len(), 1, "Unexpected number of events received");
	match events.remove(0) {
		ClientSessionEvent::VideoDataReceived { data, timestamp } => {
			assert_eq!(timestamp, RtmpTimestamp::new(1234), "Unexpected timestamp");
			assert_eq!(&data[..], &video_data[..], "Unexpected video data");
		}

		x => panic!("Expected video data received event, instead received: {:?}", x),
	}
}

#[test]
fn active_play_session_raises_events_when_audio_data_received() {
	let config = ClientSessionConfig::new();
	let mut deserializer = ChunkDeserializer::new();
	let mut serializer = ChunkSerializer::new();
	let (mut session, initial_results) = ClientSession::new(config.clone()).unwrap();
	consume_results(&mut deserializer, initial_results);

	perform_successful_connect("test".to_string(), &mut session, &mut serializer, &mut deserializer);
	let stream_id = perform_successful_play_request(config, &mut session, &mut serializer, &mut deserializer);

	let audio_data = Bytes::from(vec![1, 2, 3, 4, 5]);
	let message = RtmpMessage::AudioData {
		data: audio_data.clone(),
	};
	let payload = message
		.into_message_payload(RtmpTimestamp::new(1234), stream_id)
		.unwrap();
	let packet = serializer.serialize(&payload, false, false).unwrap();
	let results = session.handle_input(&packet.bytes[..]).unwrap();
	let (_, mut events) = split_results(&mut deserializer, results);

	assert_eq!(events.len(), 1, "Unexpected number of events received");
	match events.remove(0) {
		ClientSessionEvent::AudioDataReceived { data, timestamp } => {
			assert_eq!(timestamp, RtmpTimestamp::new(1234), "Unexpected timestamp");
			assert_eq!(&data[..], &audio_data[..], "Unexpected audio data");
		}

		x => panic!("Expected video data received event, instead received: {:?}", x),
	}
}

#[test]
fn can_receive_audio_data_prior_to_play_request_being_accepted() {
	let app_name = "test".to_string();
	let stream_key = "test-key".to_string();
	let config = ClientSessionConfig::new();
	let mut deserializer = ChunkDeserializer::new();
	let mut serializer = ChunkSerializer::new();
	let (mut session, initial_results) = ClientSession::new(config.clone()).unwrap();
	consume_results(&mut deserializer, initial_results);

	perform_successful_connect(app_name.clone(), &mut session, &mut serializer, &mut deserializer);

	let result = session.request_playback(stream_key.clone()).unwrap();
	let (mut responses, _) = split_results(&mut deserializer, vec![result]);

	assert_eq!(responses.len(), 1, "Unexpected number of responses");
	let transaction_id = match responses.remove(0) {
		(
			payload,
			RtmpMessage::Amf0Command {
				command_name,
				transaction_id,
				command_object,
				additional_arguments,
			},
		) => {
			assert_eq!(payload.message_stream_id, 0, "Unexpected stream id");
			assert_eq!(command_name, "createStream", "Unexpected command name");
			assert_eq!(command_object, Amf0Value::Null, "Unexpected command object");
			assert_eq!(
				additional_arguments.len(),
				0,
				"Uenxpected number of additional arguments"
			);
			transaction_id
		}

		x => panic!("Unexpected response seen: {:?}", x),
	};

	let (created_stream_id, create_stream_response) =
		get_create_stream_success_response(transaction_id, &mut serializer);
	let results = session.handle_input(&create_stream_response.bytes[..]).unwrap();
	let (mut responses, _) = split_results(&mut deserializer, results);

	assert_eq!(responses.len(), 2, "Expected one response returned");
	match responses.remove(0) {
		(
			payload,
			RtmpMessage::UserControl {
				event_type,
				stream_id,
				buffer_length,
				timestamp,
			},
		) => {
			assert_eq!(payload.message_stream_id, 0, "Unexpected message stream id");
			assert_eq!(stream_id, Some(created_stream_id), "Unexpected user control stream id");
			assert_eq!(
				event_type,
				UserControlEventType::SetBufferLength,
				"Unexpected user control event type"
			);
			assert_eq!(
				buffer_length,
				Some(config.playback_buffer_length_ms),
				"Unexpected playback buffer lenght"
			);
			assert_eq!(timestamp, None, "Unexpected timestamp");
		}

		x => panic!("Expected set buffer length message, instead received: {:?}", x),
	}

	match responses.remove(0) {
		(
			payload,
			RtmpMessage::Amf0Command {
				command_name,
				transaction_id,
				command_object,
				additional_arguments,
			},
		) => {
			assert_eq!(
				payload.message_stream_id, created_stream_id,
				"Unexpected message stream id"
			);
			assert_eq!(command_name, "play".to_string(), "Unexpected command name");
			assert_eq!(transaction_id, 0.0, "Unexpected transaction id");
			assert_eq!(command_object, Amf0Value::Null, "Unexpected command object");
			assert_eq!(
				additional_arguments.len(),
				1,
				"Unexpected number of additional arguments"
			);
			assert_eq!(
				additional_arguments[0],
				Amf0Value::Utf8String(stream_key.clone()),
				"Unexpected stream key"
			);
		}

		x => panic!("Expected play message, instead received: {:?}", x),
	};

	let audio_data = Bytes::from(vec![1, 2, 3, 4, 5]);
	let message = RtmpMessage::AudioData {
		data: audio_data.clone(),
	};
	let payload = message
		.into_message_payload(RtmpTimestamp::new(1234), created_stream_id)
		.unwrap();
	let packet = serializer.serialize(&payload, false, false).unwrap();
	let results = session.handle_input(&packet.bytes[..]).unwrap();
	let (_, mut events) = split_results(&mut deserializer, results);

	assert_eq!(events.len(), 1, "Unexpected number of events received");
	match events.remove(0) {
		ClientSessionEvent::AudioDataReceived { data, timestamp } => {
			assert_eq!(timestamp, RtmpTimestamp::new(1234), "Unexpected timestamp");
			assert_eq!(&data[..], &audio_data[..], "Unexpected audio data");
		}

		x => panic!("Expected audio data received event, instead received: {:?}", x),
	}
}

#[test]
fn can_receive_video_data_prior_to_play_request_being_accepted() {
	let app_name = "test".to_string();
	let stream_key = "test-key".to_string();
	let config = ClientSessionConfig::new();
	let mut deserializer = ChunkDeserializer::new();
	let mut serializer = ChunkSerializer::new();
	let (mut session, initial_results) = ClientSession::new(config.clone()).unwrap();
	consume_results(&mut deserializer, initial_results);

	perform_successful_connect(app_name.clone(), &mut session, &mut serializer, &mut deserializer);

	let result = session.request_playback(stream_key.clone()).unwrap();
	let (mut responses, _) = split_results(&mut deserializer, vec![result]);

	assert_eq!(responses.len(), 1, "Unexpected number of responses");
	let transaction_id = match responses.remove(0) {
		(
			payload,
			RtmpMessage::Amf0Command {
				command_name,
				transaction_id,
				command_object,
				additional_arguments,
			},
		) => {
			assert_eq!(payload.message_stream_id, 0, "Unexpected stream id");
			assert_eq!(command_name, "createStream", "Unexpected command name");
			assert_eq!(command_object, Amf0Value::Null, "Unexpected command object");
			assert_eq!(
				additional_arguments.len(),
				0,
				"Uenxpected number of additional arguments"
			);
			transaction_id
		}

		x => panic!("Unexpected response seen: {:?}", x),
	};

	let (created_stream_id, create_stream_response) =
		get_create_stream_success_response(transaction_id, &mut serializer);
	let results = session.handle_input(&create_stream_response.bytes[..]).unwrap();
	let (mut responses, _) = split_results(&mut deserializer, results);

	assert_eq!(responses.len(), 2, "Expected one response returned");
	match responses.remove(0) {
		(
			payload,
			RtmpMessage::UserControl {
				event_type,
				stream_id,
				buffer_length,
				timestamp,
			},
		) => {
			assert_eq!(payload.message_stream_id, 0, "Unexpected message stream id");
			assert_eq!(stream_id, Some(created_stream_id), "Unexpected user control stream id");
			assert_eq!(
				event_type,
				UserControlEventType::SetBufferLength,
				"Unexpected user control event type"
			);
			assert_eq!(
				buffer_length,
				Some(config.playback_buffer_length_ms),
				"Unexpected playback buffer lenght"
			);
			assert_eq!(timestamp, None, "Unexpected timestamp");
		}

		x => panic!("Expected set buffer length message, instead received: {:?}", x),
	}

	match responses.remove(0) {
		(
			payload,
			RtmpMessage::Amf0Command {
				command_name,
				transaction_id,
				command_object,
				additional_arguments,
			},
		) => {
			assert_eq!(
				payload.message_stream_id, created_stream_id,
				"Unexpected message stream id"
			);
			assert_eq!(command_name, "play".to_string(), "Unexpected command name");
			assert_eq!(transaction_id, 0.0, "Unexpected transaction id");
			assert_eq!(command_object, Amf0Value::Null, "Unexpected command object");
			assert_eq!(
				additional_arguments.len(),
				1,
				"Unexpected number of additional arguments"
			);
			assert_eq!(
				additional_arguments[0],
				Amf0Value::Utf8String(stream_key.clone()),
				"Unexpected stream key"
			);
		}

		x => panic!("Expected play message, instead received: {:?}", x),
	};

	let video_data = Bytes::from(vec![1, 2, 3, 4, 5]);
	let message = RtmpMessage::VideoData {
		data: video_data.clone(),
	};
	let payload = message
		.into_message_payload(RtmpTimestamp::new(1234), created_stream_id)
		.unwrap();
	let packet = serializer.serialize(&payload, false, false).unwrap();
	let results = session.handle_input(&packet.bytes[..]).unwrap();
	let (_, mut events) = split_results(&mut deserializer, results);

	assert_eq!(events.len(), 1, "Unexpected number of events received");
	match events.remove(0) {
		ClientSessionEvent::VideoDataReceived { data, timestamp } => {
			assert_eq!(timestamp, RtmpTimestamp::new(1234), "Unexpected timestamp");
			assert_eq!(&data[..], &video_data[..], "Unexpected video data");
		}

		x => panic!("Expected video data received event, instead received: {:?}", x),
	}
}

#[test]
fn can_stop_playback() {
	let config = ClientSessionConfig::new();
	let mut deserializer = ChunkDeserializer::new();
	let mut serializer = ChunkSerializer::new();
	let (mut session, initial_results) = ClientSession::new(config.clone()).unwrap();
	consume_results(&mut deserializer, initial_results);

	perform_successful_connect("test".to_string(), &mut session, &mut serializer, &mut deserializer);
	let stream_id = perform_successful_play_request(config, &mut session, &mut serializer, &mut deserializer);

	let results = session.stop_playback().unwrap();
	let (mut responses, _) = split_results(&mut deserializer, results);

	assert_eq!(responses.len(), 1, "Unexpected number of responses");
	match responses.remove(0) {
		(
			payload,
			RtmpMessage::Amf0Command {
				command_name,
				transaction_id,
				command_object,
				additional_arguments,
			},
		) => {
			assert_eq!(payload.message_stream_id, stream_id, "Unexpected message stream id");
			assert_eq!(command_name, "deleteStream", "Unexpected command name");
			assert_eq!(command_object, Amf0Value::Null, "Unexpected command object");
			assert_eq!(
				additional_arguments.len(),
				1,
				"Unexpected number of additional arguments"
			);
			assert_eq!(
				additional_arguments[0],
				Amf0Value::Number(stream_id as f64),
				"Unexpected argument stream id"
			);
			assert_eq!(transaction_id, 0.0, "Unexpected transaction id");
		}

		x => panic!("Expected Amf0 command, instead received: {:?}", x),
	}
}

#[test]
fn automatically_responds_to_ping_requests() {
	let config = ClientSessionConfig::new();
	let mut deserializer = ChunkDeserializer::new();
	let mut serializer = ChunkSerializer::new();
	let (mut session, initial_results) = ClientSession::new(config.clone()).unwrap();
	consume_results(&mut deserializer, initial_results);

	perform_successful_connect("test".to_string(), &mut session, &mut serializer, &mut deserializer);

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
	let config = ClientSessionConfig::new();
	let mut deserializer = ChunkDeserializer::new();
	let mut serializer = ChunkSerializer::new();
	let (mut session, initial_results) = ClientSession::new(config.clone()).unwrap();
	consume_results(&mut deserializer, initial_results);

	perform_successful_connect("test".to_string(), &mut session, &mut serializer, &mut deserializer);

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
		ClientSessionEvent::PingResponseReceived { timestamp } => {
			assert_eq!(timestamp, RtmpTimestamp::new(5230), "Unexpected timestamp received");
		}

		x => panic!("Expected PingResponse event, instead received {:?}", x),
	}
}

#[test]
fn can_send_ping_request() {
	let config = ClientSessionConfig::new();
	let mut deserializer = ChunkDeserializer::new();
	let mut serializer = ChunkSerializer::new();
	let (mut session, initial_results) = ClientSession::new(config.clone()).unwrap();
	consume_results(&mut deserializer, initial_results);

	perform_successful_connect("test".to_string(), &mut session, &mut serializer, &mut deserializer);

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
fn sends_ack_after_receiving_window_ack_bytes() {
	let config = ClientSessionConfig::new();
	let mut deserializer = ChunkDeserializer::new();
	let mut serializer = ChunkSerializer::new();
	let (mut session, initial_results) = ClientSession::new(config.clone()).unwrap();
	consume_results(&mut deserializer, initial_results);

	perform_successful_connect("test".to_string(), &mut session, &mut serializer, &mut deserializer);
	let _ = perform_successful_play_request(config, &mut session, &mut serializer, &mut deserializer);

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
fn event_raised_when_server_sends_an_acknowledgement() {
	let config = ClientSessionConfig::new();
	let mut deserializer = ChunkDeserializer::new();
	let mut serializer = ChunkSerializer::new();
	let (mut session, initial_results) = ClientSession::new(config.clone()).unwrap();
	consume_results(&mut deserializer, initial_results);

	perform_successful_connect("test".to_string(), &mut session, &mut serializer, &mut deserializer);

	let message = RtmpMessage::Acknowledgement { sequence_number: 1234 };
	let payload = message.into_message_payload(RtmpTimestamp::new(0), 0).unwrap();
	let packet = serializer.serialize(&payload, false, false).unwrap();
	let results = session.handle_input(&packet.bytes[..]).unwrap();
	let (_, mut events) = split_results(&mut deserializer, results);

	assert_eq!(events.len(), 1, "Unexpected number of events");
	match events.remove(0) {
		ClientSessionEvent::AcknowledgementReceived { bytes_received } => {
			assert_eq!(bytes_received, 1234, "Incorrect number of bytes received in event");
		}

		x => panic!("Expected acknowledgement received event, instead got: {:?}", x),
	}
}

#[test]
fn successful_publish_request_workflow() {
	let stream_key = "test-key".to_string();
	let config = ClientSessionConfig::new();
	let mut deserializer = ChunkDeserializer::new();
	let mut serializer = ChunkSerializer::new();
	let (mut session, initial_results) = ClientSession::new(config.clone()).unwrap();
	consume_results(&mut deserializer, initial_results);

	perform_successful_connect("test".to_string(), &mut session, &mut serializer, &mut deserializer);

	let result = session
		.request_publishing(stream_key.clone(), PublishRequestType::Live)
		.unwrap();
	let (mut responses, _) = split_results(&mut deserializer, vec![result]);

	assert_eq!(responses.len(), 1, "Unexpected number of responses");
	let transaction_id = match responses.remove(0) {
		(
			payload,
			RtmpMessage::Amf0Command {
				command_name,
				transaction_id,
				command_object,
				additional_arguments,
			},
		) => {
			assert_eq!(payload.message_stream_id, 0, "Unexpected stream id");
			assert_eq!(command_name, "createStream", "Unexpected command name");
			assert_eq!(command_object, Amf0Value::Null, "Unexpected command object");
			assert_eq!(
				additional_arguments.len(),
				0,
				"Uenxpected number of additional arguments"
			);
			transaction_id
		}

		x => panic!("Unexpected response seen: {:?}", x),
	};

	let (created_stream_id, create_stream_response) =
		get_create_stream_success_response(transaction_id, &mut serializer);
	let results = session.handle_input(&create_stream_response.bytes[..]).unwrap();
	let (mut responses, _) = split_results(&mut deserializer, results);

	assert_eq!(responses.len(), 1, "Unexpected number of responses");
	match responses.remove(0) {
		(
			_,
			RtmpMessage::Amf0Command {
				command_name,
				transaction_id,
				command_object,
				additional_arguments,
			},
		) => {
			assert_eq!(command_name, "publish".to_string(), "Unexpected command name");
			assert_eq!(command_object, Amf0Value::Null, "Unexpected command object");
			assert_eq!(transaction_id, 0.0, "Unexpected transaction id");
			assert_eq!(
				additional_arguments.len(),
				2,
				"Unexpected number of additional arguments"
			);
			assert_eq!(
				additional_arguments[0],
				Amf0Value::Utf8String(stream_key.clone()),
				"Unexpected stream key"
			);
			assert_eq!(
				additional_arguments[1],
				Amf0Value::Utf8String("live".to_string()),
				"Unexpected publish type"
			);
			transaction_id
		}

		x => panic!("Expected amf0 command, received: {:?}", x),
	};

	let publish_response = get_publish_success_response(&mut serializer, created_stream_id);
	let results = session.handle_input(&publish_response.bytes[..]).unwrap();
	let (_, mut events) = split_results(&mut deserializer, results);

	assert_eq!(events.len(), 1, "Unexpected number of events");
	match events.remove(0) {
		ClientSessionEvent::PublishRequestAccepted => (),
		x => panic!("Expected publish request accepted event, instead received: {:?}", x),
	}
}

#[test]
fn publisher_can_send_metadata() {
	let config = ClientSessionConfig::new();
	let mut deserializer = ChunkDeserializer::new();
	let mut serializer = ChunkSerializer::new();
	let (mut session, initial_results) = ClientSession::new(config.clone()).unwrap();
	consume_results(&mut deserializer, initial_results);

	perform_successful_connect("test".to_string(), &mut session, &mut serializer, &mut deserializer);
	let stream_id = perform_successful_publish_request(&mut session, &mut serializer, &mut deserializer);

	let mut metadata = StreamMetadata::new();
	metadata.video_width = Some(100);
	metadata.video_height = Some(101);
	metadata.video_codec_id = Some(10);
	metadata.video_frame_rate = Some(102.0);
	metadata.video_bitrate_kbps = Some(103);
	metadata.audio_codec_id = Some(7);
	metadata.audio_bitrate_kbps = Some(104);
	metadata.audio_sample_rate = Some(105);
	metadata.audio_channels = Some(106);
	metadata.audio_is_stereo = Some(true);
	metadata.encoder = Some("encoder".to_string());

	let result = session.publish_metadata(&metadata).unwrap();
	let (mut responses, _) = split_results(&mut deserializer, vec![result]);

	assert_eq!(responses.len(), 1, "Unexpected number of responses");
	match responses.remove(0) {
		(payload, RtmpMessage::Amf0Data { mut values }) => {
			assert_eq!(payload.message_stream_id, stream_id, "Unexpected stream id");
			assert_eq!(values.len(), 3, "Unexpected number of arguments");

			match values.remove(0) {
				Amf0Value::Utf8String(string) => {
					assert_eq!(string, "@setDataFrame".to_string(), "Unexpected first parameter")
				}
				x => panic!("Expected Amf0 string, instead got {:?}", x),
			}

			match values.remove(0) {
				Amf0Value::Utf8String(string) => {
					assert_eq!(string, "onMetaData".to_string(), "Unexpected second parameter")
				}
				x => panic!("Expected Amf0 string, instead got {:?}", x),
			}

			match values.remove(0) {
				Amf0Value::Object(properties) => {
					assert_eq!(
						properties.get("width"),
						Some(&Amf0Value::Number(100.0)),
						"Unexpected video width"
					);
					assert_eq!(
						properties.get("height"),
						Some(&Amf0Value::Number(101.0)),
						"Unexpected video height"
					);
					assert_eq!(
						properties.get("videocodecid"),
						Some(&Amf0Value::Number(10.0)),
						"Unexpected video codec"
					);
					assert_eq!(
						properties.get("framerate"),
						Some(&Amf0Value::Number(102.0)),
						"Unexpected framerate"
					);
					assert_eq!(
						properties.get("videodatarate"),
						Some(&Amf0Value::Number(103.0)),
						"Unexpected video bitrate"
					);
					assert_eq!(
						properties.get("audiocodecid"),
						Some(&Amf0Value::Number(7.0)),
						"Unexpected audio codec"
					);
					assert_eq!(
						properties.get("audiodatarate"),
						Some(&Amf0Value::Number(104.0)),
						"Unexpected audio bitrate"
					);
					assert_eq!(
						properties.get("audiosamplerate"),
						Some(&Amf0Value::Number(105.0)),
						"Unexpected audio sample rate"
					);
					assert_eq!(
						properties.get("audiochannels"),
						Some(&Amf0Value::Number(106.0)),
						"Unexpected audio channels"
					);
					assert_eq!(
						properties.get("stereo"),
						Some(&Amf0Value::Boolean(true)),
						"Unexpected stereo value"
					);
					assert_eq!(
						properties.get("encoder"),
						Some(&Amf0Value::Utf8String("encoder".to_string())),
						"Unexpected encoder value"
					);
				}

				x => panic!("Expected Amf0 object, instead got {:?}", x),
			}
		}

		x => panic!("Expected amf0 data, instead received {:?}", x),
	}
}

#[test]
fn publisher_can_send_video_data() {
	let config = ClientSessionConfig::new();
	let mut deserializer = ChunkDeserializer::new();
	let mut serializer = ChunkSerializer::new();
	let (mut session, initial_results) = ClientSession::new(config.clone()).unwrap();
	consume_results(&mut deserializer, initial_results);

	perform_successful_connect("test".to_string(), &mut session, &mut serializer, &mut deserializer);
	let stream_id = perform_successful_publish_request(&mut session, &mut serializer, &mut deserializer);

	let data = Bytes::from(vec![1, 2, 3, 4, 5]);
	let result = session
		.publish_video_data(data.clone(), RtmpTimestamp::new(1234), false)
		.unwrap();
	let (mut responses, _) = split_results(&mut deserializer, vec![result]);

	assert_eq!(responses.len(), 1, "Unexpected number of responses");
	match responses.remove(0) {
		(payload, RtmpMessage::VideoData { data: message_data }) => {
			assert_eq!(
				payload.timestamp,
				RtmpTimestamp::new(1234),
				"Unexpected message time stamp"
			);
			assert_eq!(payload.message_stream_id, stream_id, "Unexpected message stream id");
			assert_eq!(&message_data[..], &data[..], "Unexpected video data")
		}

		x => panic!("Expected video data, instead got {:?}", x),
	}
}

#[test]
fn publisher_can_send_audio_data() {
	let config = ClientSessionConfig::new();
	let mut deserializer = ChunkDeserializer::new();
	let mut serializer = ChunkSerializer::new();
	let (mut session, initial_results) = ClientSession::new(config.clone()).unwrap();
	consume_results(&mut deserializer, initial_results);

	perform_successful_connect("test".to_string(), &mut session, &mut serializer, &mut deserializer);
	let stream_id = perform_successful_publish_request(&mut session, &mut serializer, &mut deserializer);

	let data = Bytes::from(vec![1, 2, 3, 4, 5]);
	let result = session
		.publish_audio_data(data.clone(), RtmpTimestamp::new(1234), false)
		.unwrap();
	let (mut responses, _) = split_results(&mut deserializer, vec![result]);

	assert_eq!(responses.len(), 1, "Unexpected number of responses");
	match responses.remove(0) {
		(payload, RtmpMessage::AudioData { data: message_data }) => {
			assert_eq!(payload.message_stream_id, stream_id, "Unexpected message stream id");
			assert_eq!(
				payload.timestamp,
				RtmpTimestamp::new(1234),
				"Unexpected message time stamp"
			);
			assert_eq!(&message_data[..], &data[..], "Unexpected audio data")
		}

		x => panic!("Expected audio data, instead got {:?}", x),
	}
}

#[test]
fn can_stop_publishing() {
	let config = ClientSessionConfig::new();
	let mut deserializer = ChunkDeserializer::new();
	let mut serializer = ChunkSerializer::new();
	let (mut session, initial_results) = ClientSession::new(config.clone()).unwrap();
	consume_results(&mut deserializer, initial_results);

	perform_successful_connect("test".to_string(), &mut session, &mut serializer, &mut deserializer);
	let stream_id = perform_successful_publish_request(&mut session, &mut serializer, &mut deserializer);

	let results = session.stop_publishing().unwrap();
	let (mut responses, _) = split_results(&mut deserializer, results);

	assert_eq!(responses.len(), 1, "Unexpected number of responses");
	match responses.remove(0) {
		(
			payload,
			RtmpMessage::Amf0Command {
				command_name,
				transaction_id,
				command_object,
				additional_arguments,
			},
		) => {
			assert_eq!(payload.message_stream_id, stream_id, "Unexpected message stream id");
			assert_eq!(command_name, "deleteStream", "Unexpected command name");
			assert_eq!(command_object, Amf0Value::Null, "Unexpected command object");
			assert_eq!(
				additional_arguments.len(),
				1,
				"Unexpected number of additional arguments"
			);
			assert_eq!(
				additional_arguments[0],
				Amf0Value::Number(stream_id as f64),
				"Unexpected argument stream id"
			);
			assert_eq!(transaction_id, 0.0, "Unexpected transaction id");
		}

		x => panic!("Expected Amf0 command, instead received: {:?}", x),
	}
}

fn split_results(
	deserializer: &mut ChunkDeserializer,
	mut results: Vec<ClientSessionResult>,
) -> (Vec<(MessagePayload, RtmpMessage)>, Vec<ClientSessionEvent>) {
	let mut responses = Vec::new();
	let mut events = Vec::new();

	for result in results.drain(..) {
		match result {
			ClientSessionResult::OutboundResponse(packet) => {
				let payload = deserializer.get_next_message(&packet.bytes[..]).unwrap().unwrap();
				let message = payload.to_rtmp_message().unwrap();
				match message {
					RtmpMessage::SetChunkSize { size } => deserializer.set_max_chunk_size(size as usize).unwrap(),
					_ => (),
				}

				println!("response received: {:?}", message);
				responses.push((payload, message));
			}

			ClientSessionResult::RaisedEvent(event) => {
				println!("event received: {:?}", event);
				events.push(event);
			}

			ClientSessionResult::UnhandleableMessageReceived(payload) => {
				println!("unhandleable message: {:?}", payload);
			}
		}
	}

	(responses, events)
}

fn consume_results(deserializer: &mut ChunkDeserializer, results: Vec<ClientSessionResult>) {
	// Needed to keep the deserializer up to date
	split_results(deserializer, results);
}

fn get_connect_success_response(serializer: &mut ChunkSerializer) -> Packet {
	let mut command_properties = HashMap::new();
	command_properties.insert("fmsVer".to_string(), Amf0Value::Utf8String("fms".to_string()));
	command_properties.insert("capabilities".to_string(), Amf0Value::Number(31.0));

	let mut additional_properties = HashMap::new();
	additional_properties.insert("level".to_string(), Amf0Value::Utf8String("status".to_string()));
	additional_properties.insert(
		"code".to_string(),
		Amf0Value::Utf8String("NetConnection.Connect.Success".to_string()),
	);
	additional_properties.insert("description".to_string(), Amf0Value::Utf8String("hi".to_string()));
	additional_properties.insert("objectEncoding".to_string(), Amf0Value::Number(0.0));

	let message = RtmpMessage::Amf0Command {
		command_name: "_result".to_string(),
		transaction_id: 1.0,
		command_object: Amf0Value::Object(command_properties),
		additional_arguments: vec![Amf0Value::Object(additional_properties)],
	};

	let payload = message.into_message_payload(RtmpTimestamp::new(0), 0).unwrap();
	serializer.serialize(&payload, false, false).unwrap()
}

fn get_connect_error_response(serializer: &mut ChunkSerializer) -> Packet {
	let mut command_properties = HashMap::new();
	command_properties.insert("fmsVer".to_string(), Amf0Value::Utf8String("fms".to_string()));
	command_properties.insert("capabilities".to_string(), Amf0Value::Number(31.0));

	let mut additional_properties = HashMap::new();
	additional_properties.insert("level".to_string(), Amf0Value::Utf8String("error".to_string()));
	additional_properties.insert(
		"code".to_string(),
		Amf0Value::Utf8String("NetConnection.Connect.Failed".to_string()),
	);
	additional_properties.insert("description".to_string(), Amf0Value::Utf8String("hi".to_string()));
	additional_properties.insert("objectEncoding".to_string(), Amf0Value::Number(0.0));

	let message = RtmpMessage::Amf0Command {
		command_name: "_error".to_string(),
		transaction_id: 1.0,
		command_object: Amf0Value::Object(command_properties),
		additional_arguments: vec![Amf0Value::Object(additional_properties)],
	};

	let payload = message.into_message_payload(RtmpTimestamp::new(0), 0).unwrap();
	serializer.serialize(&payload, false, false).unwrap()
}

fn get_create_stream_success_response(transaction_id: f64, serializer: &mut ChunkSerializer) -> (u32, Packet) {
	let stream_id = rand::random::<u32>();
	let message = RtmpMessage::Amf0Command {
		command_name: "_result".to_string(),
		command_object: Amf0Value::Null,
		additional_arguments: vec![Amf0Value::Number(stream_id as f64)],
		transaction_id,
	};

	let payload = message.into_message_payload(RtmpTimestamp::new(0), 0).unwrap();
	let packet = serializer.serialize(&payload, false, false).unwrap();
	(stream_id, packet)
}

fn get_play_success_response(serializer: &mut ChunkSerializer, stream_id: u32) -> Packet {
	let mut additional_properties = HashMap::new();
	additional_properties.insert("level".to_string(), Amf0Value::Utf8String("status".to_string()));
	additional_properties.insert(
		"code".to_string(),
		Amf0Value::Utf8String("NetStream.Play.Start".to_string()),
	);
	additional_properties.insert("description".to_string(), Amf0Value::Utf8String("hi".to_string()));

	let message = RtmpMessage::Amf0Command {
		command_name: "onStatus".to_string(),
		transaction_id: 0.0,
		command_object: Amf0Value::Null,
		additional_arguments: vec![Amf0Value::Object(additional_properties)],
	};

	let payload = message.into_message_payload(RtmpTimestamp::new(0), stream_id).unwrap();
	serializer.serialize(&payload, false, false).unwrap()
}

fn get_publish_success_response(serializer: &mut ChunkSerializer, stream_id: u32) -> Packet {
	let mut additional_properties = HashMap::new();
	additional_properties.insert("level".to_string(), Amf0Value::Utf8String("status".to_string()));
	additional_properties.insert(
		"code".to_string(),
		Amf0Value::Utf8String("NetStream.Publish.Start".to_string()),
	);
	additional_properties.insert("description".to_string(), Amf0Value::Utf8String("hi".to_string()));

	let message = RtmpMessage::Amf0Command {
		command_name: "onStatus".to_string(),
		transaction_id: 0.0,
		command_object: Amf0Value::Null,
		additional_arguments: vec![Amf0Value::Object(additional_properties)],
	};

	let payload = message.into_message_payload(RtmpTimestamp::new(0), stream_id).unwrap();
	serializer.serialize(&payload, false, false).unwrap()
}

fn perform_successful_connect(
	app_name: String,
	session: &mut ClientSession,
	serializer: &mut ChunkSerializer,
	deserializer: &mut ChunkDeserializer,
) {
	let results = session.request_connection(app_name).unwrap();
	consume_results(deserializer, vec![results]);

	let response = get_connect_success_response(serializer);
	let results = session.handle_input(&response.bytes[..]).unwrap();
	let (_, mut events) = split_results(deserializer, results);

	assert_eq!(events.len(), 1, "Expected one event returned");
	match events.remove(0) {
		ClientSessionEvent::ConnectionRequestAccepted => (),
		x => panic!("Expected connection accepted event, instead received: {:?}", x),
	}
}

fn perform_successful_play_request(
	config: ClientSessionConfig,
	session: &mut ClientSession,
	serializer: &mut ChunkSerializer,
	deserializer: &mut ChunkDeserializer,
) -> u32 {
	let stream_key = "abcd".to_string();
	let result = session.request_playback(stream_key.clone()).unwrap();
	let (mut responses, _) = split_results(deserializer, vec![result]);

	assert_eq!(responses.len(), 1, "Unexpected number of responses");
	let transaction_id = match responses.remove(0) {
		(
			payload,
			RtmpMessage::Amf0Command {
				command_name,
				transaction_id,
				command_object,
				additional_arguments,
			},
		) => {
			assert_eq!(payload.message_stream_id, 0, "Unexpected stream id");
			assert_eq!(command_name, "createStream", "Unexpected command name");
			assert_eq!(command_object, Amf0Value::Null, "Unexpected command object");
			assert_eq!(
				additional_arguments.len(),
				0,
				"Uenxpected number of additional arguments"
			);
			transaction_id
		}

		x => panic!("Unexpected response seen: {:?}", x),
	};

	let (created_stream_id, create_stream_response) = get_create_stream_success_response(transaction_id, serializer);
	let results = session.handle_input(&create_stream_response.bytes[..]).unwrap();
	let (mut responses, _) = split_results(deserializer, results);

	assert_eq!(responses.len(), 2, "Expected one response returned");
	match responses.remove(0) {
		(
			payload,
			RtmpMessage::UserControl {
				event_type,
				stream_id,
				buffer_length,
				timestamp,
			},
		) => {
			assert_eq!(payload.message_stream_id, 0, "Unexpected message stream id");
			assert_eq!(stream_id, Some(created_stream_id), "Unexpected user control stream id");
			assert_eq!(
				event_type,
				UserControlEventType::SetBufferLength,
				"Unexpected user control event type"
			);
			assert_eq!(
				buffer_length,
				Some(config.playback_buffer_length_ms),
				"Unexpected playback buffer lenght"
			);
			assert_eq!(timestamp, None, "Unexpected timestamp");
		}

		x => panic!("Expected set buffer length message, instead received: {:?}", x),
	}

	match responses.remove(0) {
		(
			payload,
			RtmpMessage::Amf0Command {
				command_name,
				transaction_id,
				command_object,
				additional_arguments,
			},
		) => {
			assert_eq!(
				payload.message_stream_id, created_stream_id,
				"Unexpected message stream id"
			);
			assert_eq!(command_name, "play".to_string(), "Unexpected command name");
			assert_eq!(transaction_id, 0.0, "Unexpected transaction id");
			assert_eq!(command_object, Amf0Value::Null, "Unexpected command object");
			assert_eq!(
				additional_arguments.len(),
				1,
				"Unexpected number of additional arguments"
			);
			assert_eq!(
				additional_arguments[0],
				Amf0Value::Utf8String(stream_key.clone()),
				"Unexpected stream key"
			);
		}

		x => panic!("Expected play message, instead received: {:?}", x),
	};

	let play_response = get_play_success_response(serializer, created_stream_id);
	let results = session.handle_input(&play_response.bytes[..]).unwrap();
	let (_, mut events) = split_results(deserializer, results);

	assert_eq!(events.len(), 1, "Expected one event returned");
	match events.remove(0) {
		ClientSessionEvent::PlaybackRequestAccepted => (),
		x => panic!("Expected playback accepted event, instead received: {:?}", x),
	}

	created_stream_id
}

fn perform_successful_publish_request(
	session: &mut ClientSession,
	serializer: &mut ChunkSerializer,
	deserializer: &mut ChunkDeserializer,
) -> u32 {
	let stream_key = "abcd".to_string();
	let result = session
		.request_publishing(stream_key.clone(), PublishRequestType::Live)
		.unwrap();
	let (mut responses, _) = split_results(deserializer, vec![result]);

	assert_eq!(responses.len(), 1, "Unexpected number of responses");
	let transaction_id = match responses.remove(0) {
		(
			payload,
			RtmpMessage::Amf0Command {
				command_name,
				transaction_id,
				command_object,
				additional_arguments,
			},
		) => {
			assert_eq!(payload.message_stream_id, 0, "Unexpected stream id");
			assert_eq!(command_name, "createStream", "Unexpected command name");
			assert_eq!(command_object, Amf0Value::Null, "Unexpected command object");
			assert_eq!(
				additional_arguments.len(),
				0,
				"Uenxpected number of additional arguments"
			);
			transaction_id
		}

		x => panic!("Unexpected response seen: {:?}", x),
	};

	let (created_stream_id, create_stream_response) = get_create_stream_success_response(transaction_id, serializer);
	let results = session.handle_input(&create_stream_response.bytes[..]).unwrap();
	let (mut responses, _) = split_results(deserializer, results);

	assert_eq!(responses.len(), 1, "Unexpected number of responses");
	match responses.remove(0) {
		(
			_,
			RtmpMessage::Amf0Command {
				command_name,
				transaction_id,
				command_object,
				additional_arguments,
			},
		) => {
			assert_eq!(command_name, "publish".to_string(), "Unexpected command name");
			assert_eq!(command_object, Amf0Value::Null, "Unexpected command object");
			assert_eq!(transaction_id, 0.0, "Unexpected transaction id");
			assert_eq!(
				additional_arguments.len(),
				2,
				"Unexpected number of additional arguments"
			);
			assert_eq!(
				additional_arguments[0],
				Amf0Value::Utf8String(stream_key.clone()),
				"Unexpected stream key"
			);
			assert_eq!(
				additional_arguments[1],
				Amf0Value::Utf8String("live".to_string()),
				"Unexpected publish type"
			);
			transaction_id
		}

		x => panic!("Expected amf0 command, received: {:?}", x),
	};

	let publish_response = get_publish_success_response(serializer, created_stream_id);
	let results = session.handle_input(&publish_response.bytes[..]).unwrap();
	let (_, mut events) = split_results(deserializer, results);

	assert_eq!(events.len(), 1, "Unexpected number of events");
	match events.remove(0) {
		ClientSessionEvent::PublishRequestAccepted => (),
		x => panic!("Expected publish request accepted event, instead received: {:?}", x),
	}

	created_stream_id
}
