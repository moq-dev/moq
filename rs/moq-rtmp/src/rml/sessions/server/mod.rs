mod active_stream;
mod config;
mod errors;
mod events;
mod outstanding_requests;
mod publish_mode;
mod result;
mod session_state;

#[cfg(test)]
mod tests;

use self::active_stream::{ActiveStream, StreamState};
use self::outstanding_requests::OutstandingRequest;
use self::session_state::SessionState;
use crate::rml::chunk_io::{ChunkDeserializer, ChunkSerializer, Packet};
use crate::rml::messages::{PeerBandwidthLimitType, RtmpMessage, UserControlEventType};
use crate::rml::sessions::StreamMetadata;
use crate::rml::time::RtmpTimestamp;
use bytes::Bytes;
use rml_amf0::Amf0Value;
use std::collections::HashMap;
use std::time::SystemTime;

pub use self::config::ServerSessionConfig;
pub use self::errors::ServerSessionError;
pub use self::events::{PlayStartValue, ServerSessionEvent};
pub use self::publish_mode::PublishMode;
pub use self::result::ServerSessionResult;

/// A session that represents the server side of a single RTMP connection.
///
/// The `ServerSession` encapsulates the process of parsing RTMP chunks coming in from a client
/// into RTMP messages and performs common server side workflows to handle those messages.  It can
/// either provide pre-serialized messages to be sent back to the client or events that
/// parent applications can perform custom logic against (like verifying if a connection request
/// should be accepted or not).
///
/// The `ServerSession` does not care how RTMP chunks (encoded as bytes) come in or get sent out,
/// but leaves that up to the application utilizing the `ServerSession`.
///
/// Due to the header compression properties of the RTMP chunking protocol it is required that
/// all bytes **after** the handshake has been completed are passed into the `ServerSession`, that
/// all responses returned by the `ServerSession` are sent to the client **in order**, and that
/// no additional bytes are sent to the client.  Any violation of these rules have a high
/// high probability of causing RTMP chunk parsing errors by the peer or by the `ServerSession`
/// instance itself.
pub struct ServerSession {
	start_time: SystemTime,
	serializer: ChunkSerializer,
	deserializer: ChunkDeserializer,
	connected_app_name: Option<String>,
	outstanding_requests: HashMap<u32, OutstandingRequest>,
	next_request_number: u32,
	current_state: SessionState,
	fms_version: String,
	object_encoding: f64,
	active_streams: HashMap<u32, ActiveStream>,
	next_stream_id: u32,
	peer_window_ack_size: Option<u32>,
	bytes_received: u64,
	bytes_received_since_last_ack: u32,
	connect_response_properties: HashMap<String, Amf0Value>,
}

impl ServerSession {
	/// Creates a new server session.
	///
	/// As part of the initial creation it automatically creates the initial outbound RTMp messages
	/// that the RTMP message protocol requires to confirm to the client that it can stream on
	/// stream id 0 (as well as other important initial information
	pub fn new(config: ServerSessionConfig) -> Result<(ServerSession, Vec<ServerSessionResult>), ServerSessionError> {
		let mut session = ServerSession {
			start_time: SystemTime::now(),
			serializer: ChunkSerializer::new(),
			deserializer: ChunkDeserializer::new(),
			connected_app_name: None,
			outstanding_requests: HashMap::new(),
			next_request_number: 0,
			current_state: SessionState::Started,
			fms_version: config.fms_version,
			object_encoding: 0.0,
			active_streams: HashMap::new(),
			next_stream_id: 1,
			peer_window_ack_size: None,
			bytes_received: 0,
			bytes_received_since_last_ack: 0,
			connect_response_properties: HashMap::new(),
		};

		let mut results = Vec::with_capacity(4);

		let chunk_size_packet = session
			.serializer
			.set_max_chunk_size(config.chunk_size, RtmpTimestamp::new(0))?;
		results.push(ServerSessionResult::OutboundResponse(chunk_size_packet));

		let window_ack_message = RtmpMessage::WindowAcknowledgement {
			size: config.window_ack_size,
		};
		let window_ack_payload = window_ack_message.into_message_payload(session.get_epoch(), 0)?;
		let window_ack_packet = session.serializer.serialize(&window_ack_payload, true, false)?;
		results.push(ServerSessionResult::OutboundResponse(window_ack_packet));

		let begin_message = RtmpMessage::UserControl {
			event_type: UserControlEventType::StreamBegin,
			stream_id: Some(0),
			timestamp: None,
			buffer_length: None,
		};

		let begin_payload = begin_message.into_message_payload(session.get_epoch(), 0)?;
		let begin_packet = session.serializer.serialize(&begin_payload, true, false)?;
		results.push(ServerSessionResult::OutboundResponse(begin_packet));

		let peer_message = RtmpMessage::SetPeerBandwidth {
			size: config.peer_bandwidth,
			limit_type: PeerBandwidthLimitType::Dynamic,
		};
		let peer_payload = peer_message.into_message_payload(session.get_epoch(), 0)?;
		let peer_packet = session.serializer.serialize(&peer_payload, true, false)?;
		results.push(ServerSessionResult::OutboundResponse(peer_packet));

		if config.send_on_bw_done_message_on_start {
			let bw_done_message = RtmpMessage::Amf0Command {
				command_name: "onBWDone".to_string(),
				transaction_id: 0.0,
				command_object: Amf0Value::Null,
				additional_arguments: vec![Amf0Value::Number(8192_f64)],
			};

			let bw_done_payload = bw_done_message.into_message_payload(session.get_epoch(), 0)?;
			let bw_done_packet = session.serializer.serialize(&bw_done_payload, true, false)?;
			results.push(ServerSessionResult::OutboundResponse(bw_done_packet));
		}

		Ok((session, results))
	}

	/// Sets extra name-value pairs to include in the command (properties) object of the
	/// `_result` sent when a connection request is accepted, alongside `fmsVer` and
	/// `capabilities`. This lets a server advertise enhanced-RTMP capabilities (e.g.
	/// `videoFourCcInfoMap`, `audioFourCcInfoMap`, `capsEx`) in its connect response.
	///
	/// Must be called before `accept_request` for the connection request. When empty
	/// (the default) the `_result` is unchanged. Keys here override the built-in
	/// `fmsVer`/`capabilities` if they collide.
	pub fn set_connect_response_properties(&mut self, properties: HashMap<String, Amf0Value>) {
		self.connect_response_properties = properties;
	}

	/// Takes in bytes that are encoding RTMP chunks and returns any responses or events that can
	/// be reacted to.
	pub fn handle_input(&mut self, bytes: &[u8]) -> Result<Vec<ServerSessionResult>, ServerSessionError> {
		let mut results = Vec::new();
		self.bytes_received += bytes.len() as u64;

		if let Some(peer_ack_size) = self.peer_window_ack_size {
			self.bytes_received_since_last_ack += bytes.len() as u32;
			if self.bytes_received_since_last_ack >= peer_ack_size {
				let ack_message = RtmpMessage::Acknowledgement {
					sequence_number: self.bytes_received_since_last_ack,
				};
				let ack_payload = ack_message.into_message_payload(self.get_epoch(), 0)?;
				let ack_packet = self.serializer.serialize(&ack_payload, false, false)?;

				self.bytes_received_since_last_ack = 0;
				results.push(ServerSessionResult::OutboundResponse(ack_packet));
			}
		}

		let mut bytes_to_process = bytes;

		loop {
			match self.deserializer.get_next_message(bytes_to_process)? {
				None => break,
				Some(payload) => {
					let message = payload.to_rtmp_message()?;

					let mut message_results = match message {
						RtmpMessage::Abort { stream_id } => self.handle_abort_message(stream_id)?,

						RtmpMessage::Acknowledgement { sequence_number } => {
							self.handle_acknowledgement_message(sequence_number)?
						}

						RtmpMessage::Amf0Command {
							command_name,
							transaction_id,
							command_object,
							additional_arguments,
						} => self.handle_amf0_command(
							payload.message_stream_id,
							command_name,
							transaction_id,
							command_object,
							additional_arguments,
						)?,

						RtmpMessage::Amf0Data { values } => self.handle_amf0_data(values, payload.message_stream_id)?,

						RtmpMessage::AudioData { data } => {
							self.handle_audio_data(data, payload.message_stream_id, payload.timestamp)?
						}

						RtmpMessage::SetChunkSize { size } => self.handle_set_chunk_size(size)?,

						RtmpMessage::SetPeerBandwidth { size, limit_type } => {
							self.handle_set_peer_bandwidth(size, limit_type)?
						}

						RtmpMessage::UserControl {
							event_type,
							stream_id,
							buffer_length,
							timestamp,
						} => self.handle_user_control(event_type, stream_id, buffer_length, timestamp)?,

						RtmpMessage::VideoData { data } => {
							self.handle_video_data(data, payload.message_stream_id, payload.timestamp)?
						}

						RtmpMessage::WindowAcknowledgement { size } => self.handle_window_acknowledgement(size)?,

						_ => vec![ServerSessionResult::UnhandleableMessageReceived(payload)],
					};

					results.append(&mut message_results);
					bytes_to_process = &[];
				}
			}
		}

		Ok(results)
	}

	/// Tells the server session that it should accept an outstanding request
	pub fn accept_request(&mut self, request_id: u32) -> Result<Vec<ServerSessionResult>, ServerSessionError> {
		let request = match self.outstanding_requests.remove(&request_id) {
			Some(x) => x,
			None => return Err(ServerSessionError::InvalidRequestId),
		};

		match request {
			OutstandingRequest::ConnectionRequest {
				app_name,
				transaction_id,
			} => self.accept_connection_request(app_name, transaction_id),

			OutstandingRequest::PublishRequested {
				stream_key,
				mode,
				stream_id,
			} => self.accept_publish_request(stream_id, stream_key, mode),

			OutstandingRequest::PlayRequested { stream_key, stream_id } => {
				self.accept_play_request(stream_id, stream_key)
			}
		}
	}

	/// Tells the server session that it should reject an outstanding request
	pub fn reject_request(
		&mut self,
		request_id: u32,
		code: &str,
		description: &str,
	) -> Result<Vec<ServerSessionResult>, ServerSessionError> {
		let request = match self.outstanding_requests.remove(&request_id) {
			Some(x) => x,
			None => return Err(ServerSessionError::InvalidRequestId),
		};

		let (transaction_id, stream_id) = match request {
			OutstandingRequest::ConnectionRequest { transaction_id, .. } => (transaction_id, 0),
			OutstandingRequest::PublishRequested { stream_id, .. } => (0.0, stream_id),
			OutstandingRequest::PlayRequested { stream_id, .. } => (0.0, stream_id),
		};

		let packet = self.create_error_packet(code, description, transaction_id, stream_id)?;

		Ok(vec![ServerSessionResult::OutboundResponse(packet)])
	}

	/// Prepares metadata information to be sent to the client
	pub fn send_metadata(&mut self, stream_id: u32, metadata: &StreamMetadata) -> Result<Packet, ServerSessionError> {
		let mut properties = HashMap::with_capacity(11);

		metadata
			.video_width
			.map(|x| properties.insert("width".to_string(), Amf0Value::Number(x as f64)));

		metadata
			.video_height
			.map(|x| properties.insert("height".to_string(), Amf0Value::Number(x as f64)));

		metadata
			.video_codec_id
			.map(|x| properties.insert("videocodecid".to_string(), Amf0Value::Number(x as f64)));

		metadata
			.video_bitrate_kbps
			.map(|x| properties.insert("videodatarate".to_string(), Amf0Value::Number(x as f64)));

		metadata
			.video_frame_rate
			.map(|x| properties.insert("framerate".to_string(), Amf0Value::Number(x as f64)));

		metadata
			.audio_codec_id
			.map(|x| properties.insert("audiocodecid".to_string(), Amf0Value::Number(x as f64)));

		metadata
			.audio_bitrate_kbps
			.map(|x| properties.insert("audiodatarate".to_string(), Amf0Value::Number(x as f64)));

		metadata
			.audio_sample_rate
			.map(|x| properties.insert("audiosamplerate".to_string(), Amf0Value::Number(x as f64)));

		metadata
			.audio_channels
			.map(|x| properties.insert("audiochannels".to_string(), Amf0Value::Number(x as f64)));

		metadata
			.audio_is_stereo
			.map(|x| properties.insert("stereo".to_string(), Amf0Value::Boolean(x)));

		metadata
			.encoder
			.as_ref()
			.map(|x| properties.insert("encoder".to_string(), Amf0Value::Utf8String(x.clone())));

		let message = RtmpMessage::Amf0Data {
			values: vec![
				Amf0Value::Utf8String("onMetaData".to_string()),
				Amf0Value::Object(properties),
			],
		};

		let payload = message.into_message_payload(self.get_epoch(), stream_id)?;
		let packet = self.serializer.serialize(&payload, false, false)?;
		Ok(packet)
	}

	/// Prepare video data to be sent to the client
	pub fn send_video_data(
		&mut self,
		stream_id: u32,
		data: Bytes,
		timestamp: RtmpTimestamp,
		can_be_dropped: bool,
	) -> Result<Packet, ServerSessionError> {
		let message = RtmpMessage::VideoData { data };
		let payload = message.into_message_payload(timestamp, stream_id)?;
		let packet = self.serializer.serialize(&payload, false, can_be_dropped)?;
		Ok(packet)
	}

	/// Prepare audio data to be sent to the client
	pub fn send_audio_data(
		&mut self,
		stream_id: u32,
		data: Bytes,
		timestamp: RtmpTimestamp,
		can_be_dropped: bool,
	) -> Result<Packet, ServerSessionError> {
		let message = RtmpMessage::AudioData { data };
		let payload = message.into_message_payload(timestamp, stream_id)?;
		let packet = self.serializer.serialize(&payload, false, can_be_dropped)?;
		Ok(packet)
	}

	/// Sends a ping request to the client
	pub fn send_ping_request(&mut self) -> Result<(Packet, RtmpTimestamp), ServerSessionError> {
		let epoch = self.get_epoch();
		let message = RtmpMessage::UserControl {
			event_type: UserControlEventType::PingRequest,
			timestamp: Some(epoch.clone()),
			buffer_length: None,
			stream_id: None,
		};

		let payload = message.into_message_payload(epoch.clone(), 0)?;
		let packet = self.serializer.serialize(&payload, false, false)?;
		Ok((packet, epoch))
	}

	/// Changes stream to Completed, and sends out an
	/// `onStatus(code: NetStream.Play.Complete)`
	pub fn finish_playing(&mut self, stream_id: u32) -> Result<Packet, ServerSessionError> {
		let stream_key = match self.active_streams.get_mut(&stream_id) {
			Some(ActiveStream { current_state: state }) => {
				let k = match state {
					StreamState::Playing { stream_key: k } => k.clone(),
					_ => {
						return Err(ServerSessionError::ActionAttemptedOnInactiveStream {
							action: "complete".to_string(),
							stream_id,
						});
					}
				};
				*state = StreamState::Completed;
				k
			}
			_ => {
				return Err(ServerSessionError::ActionAttemptedOnInactiveStream {
					action: "complete".to_string(),
					stream_id,
				});
			}
		};

		let description = format!("Stream playback is completed for {}", stream_key);
		let status_message = RtmpMessage::Amf0Command {
			command_name: "onStatus".to_string(),
			transaction_id: 0.0,
			command_object: Amf0Value::Null,
			additional_arguments: vec![Amf0Value::Object(create_status_object(
				"status",
				"NetStream.Play.Complete",
				description.as_ref(),
			))],
		};

		let payload = status_message.into_message_payload(self.get_epoch(), stream_id)?;

		Ok(self.serializer.serialize(&payload, false, false)?)
	}

	fn handle_abort_message(&self, _stream_id: u32) -> Result<Vec<ServerSessionResult>, ServerSessionError> {
		Ok(Vec::new())
	}

	fn handle_acknowledgement_message(
		&self,
		sequence_number: u32,
	) -> Result<Vec<ServerSessionResult>, ServerSessionError> {
		let event = ServerSessionEvent::AcknowledgementReceived {
			bytes_received: sequence_number,
		};
		Ok(vec![ServerSessionResult::RaisedEvent(event)])
	}

	fn handle_amf0_command(
		&mut self,
		stream_id: u32,
		name: String,
		transaction_id: f64,
		command_object: Amf0Value,
		additional_args: Vec<Amf0Value>,
	) -> Result<Vec<ServerSessionResult>, ServerSessionError> {
		let results = match name.as_str() {
			"connect" => self.handle_command_connect(transaction_id, command_object)?,
			"closeStream" => self.handle_command_close_stream(additional_args)?,
			"createStream" => self.handle_command_create_stream(transaction_id)?,
			"deleteStream" => self.handle_command_delete_stream(additional_args)?,
			"play" => self.handle_command_play(stream_id, transaction_id, additional_args)?,
			"publish" => self.handle_command_publish(stream_id, transaction_id, additional_args)?,

			_ => vec![ServerSessionResult::RaisedEvent(
				ServerSessionEvent::UnhandleableAmf0Command {
					command_name: name,
					additional_values: additional_args,
					transaction_id,
					command_object,
				},
			)],
		};

		Ok(results)
	}

	fn handle_command_connect(
		&mut self,
		transaction_id: f64,
		command_object: Amf0Value,
	) -> Result<Vec<ServerSessionResult>, ServerSessionError> {
		let mut properties = match command_object {
			Amf0Value::Object(properties) => properties,
			_ => return Err(ServerSessionError::NoAppNameForConnectionRequest),
		};

		let app_name = match properties.remove("app") {
			Some(value) => match value {
				Amf0Value::Utf8String(mut app) => {
					if app.ends_with("/") {
						app.pop();
					}

					app
				}
				_ => return Err(ServerSessionError::NoAppNameForConnectionRequest),
			},
			None => return Err(ServerSessionError::NoAppNameForConnectionRequest),
		};

		self.object_encoding = match properties.remove("objectEncoding") {
			Some(value) => match value {
				Amf0Value::Number(number) => number,
				_ => 0.0,
			},
			None => 0.0,
		};

		let request = OutstandingRequest::ConnectionRequest {
			app_name: app_name.clone(),
			transaction_id,
		};

		let request_number = self.next_request_number;
		self.next_request_number = self.next_request_number + 1;
		self.outstanding_requests.insert(request_number, request);

		let event = ServerSessionEvent::ConnectionRequested {
			app_name: app_name,
			request_id: request_number,
		};

		Ok(vec![ServerSessionResult::RaisedEvent(event)])
	}

	fn handle_command_close_stream(
		&mut self,
		mut arguments: Vec<Amf0Value>,
	) -> Result<Vec<ServerSessionResult>, ServerSessionError> {
		if self.current_state != SessionState::Connected {
			return Ok(Vec::new());
		}

		let app_name = match self.connected_app_name {
			Some(ref name) => name.clone(),
			None => return Ok(Vec::new()),
		};

		// First argument should be the stream id to close
		if arguments.len() == 0 {
			return Ok(Vec::new());
		}

		let stream_id = match arguments.remove(0) {
			Amf0Value::Number(x) => x as u32,
			_ => return Ok(Vec::new()),
		};

		let stream = match self.active_streams.get_mut(&stream_id) {
			Some(x) => x,
			None => return Ok(Vec::new()),
		};

		// Before we change the stream state we need to grab the info from it for any
		// events that need to be raised
		let results = match stream.current_state {
			StreamState::Publishing {
				ref stream_key,
				mode: _,
			} => {
				let event = ServerSessionEvent::PublishStreamFinished {
					app_name,
					stream_key: stream_key.clone(),
				};

				vec![ServerSessionResult::RaisedEvent(event)]
			}

			StreamState::Playing { ref stream_key } => {
				let event = ServerSessionEvent::PlayStreamFinished {
					app_name,
					stream_key: stream_key.clone(),
				};

				vec![ServerSessionResult::RaisedEvent(event)]
			}

			_ => Vec::new(),
		};

		// As afar as we are concerned, a created and closed stream are equivalent.  Both allow
		// reusing the stream
		stream.current_state = StreamState::Created;

		Ok(results)
	}

	fn handle_command_create_stream(
		&mut self,
		transaction_id: f64,
	) -> Result<Vec<ServerSessionResult>, ServerSessionError> {
		let new_stream_id = self.next_stream_id;
		self.next_stream_id = self.next_stream_id + 1;

		let new_stream = ActiveStream {
			current_state: StreamState::Created,
		};

		self.active_streams.insert(new_stream_id, new_stream);

		let packet = self.create_success_response(
			transaction_id,
			Amf0Value::Null,
			vec![Amf0Value::Number(new_stream_id as f64)],
			0,
		)?; // Stream create result must always be on stream 0 for flash clients

		Ok(vec![ServerSessionResult::OutboundResponse(packet)])
	}

	fn handle_command_delete_stream(
		&mut self,
		mut arguments: Vec<Amf0Value>,
	) -> Result<Vec<ServerSessionResult>, ServerSessionError> {
		// Not sure if I need to send a response
		if self.current_state != SessionState::Connected {
			return Ok(Vec::new());
		}

		let app_name = match self.connected_app_name {
			Some(ref name) => name.clone(),
			None => return Ok(Vec::new()),
		};

		if arguments.len() == 0 {
			return Ok(Vec::new());
		}

		// First argument is expected to be the stream id
		let stream_id = match arguments.remove(0) {
			Amf0Value::Number(x) => x as u32,
			_ => return Ok(Vec::new()),
		};

		let stream = match self.active_streams.remove(&stream_id) {
			Some(stream) => stream,
			None => return Ok(Vec::new()),
		};

		let result = match stream.current_state {
			StreamState::Publishing {
				ref stream_key,
				mode: _,
			} => {
				let event = ServerSessionEvent::PublishStreamFinished {
					stream_key: stream_key.clone(),
					app_name,
				};

				vec![ServerSessionResult::RaisedEvent(event)]
			}

			StreamState::Playing { ref stream_key } => {
				let event = ServerSessionEvent::PlayStreamFinished {
					app_name,
					stream_key: stream_key.clone(),
				};

				vec![ServerSessionResult::RaisedEvent(event)]
			}
			_ => Vec::new(),
		};

		Ok(result)
	}

	fn handle_command_publish(
		&mut self,
		stream_id: u32,
		transaction_id: f64,
		mut arguments: Vec<Amf0Value>,
	) -> Result<Vec<ServerSessionResult>, ServerSessionError> {
		if arguments.len() < 2 {
			let packet = self.create_error_packet(
				"NetStream.Publish.Start",
				"Invalid publish arguments",
				transaction_id,
				stream_id,
			)?;
			return Ok(vec![ServerSessionResult::OutboundResponse(packet)]);
		}

		if self.current_state != SessionState::Connected {
			let packet = self.create_error_packet(
				"NetStream.Publish.Start",
				"Can't publish before connecting",
				transaction_id,
				stream_id,
			)?;
			return Ok(vec![ServerSessionResult::OutboundResponse(packet)]);
		}

		let app_name = match self.connected_app_name {
			Some(ref name) => name.clone(),
			None => {
				let packet = self.create_error_packet(
					"NetStream.Publish.Start",
					"Can't publish before connecting",
					transaction_id,
					stream_id,
				)?;
				return Ok(vec![ServerSessionResult::OutboundResponse(packet)]);
			}
		};

		let stream_key = match arguments.remove(0) {
			Amf0Value::Utf8String(stream_key) => stream_key,
			_ => {
				let packet = self.create_error_packet(
					"NetStream.Publish.Start",
					"Invalid publish arguments",
					transaction_id,
					stream_id,
				)?;
				return Ok(vec![ServerSessionResult::OutboundResponse(packet)]);
			}
		};

		let mode = match arguments.remove(0) {
			Amf0Value::Utf8String(raw_mode) => match raw_mode.to_lowercase().as_ref() {
				"live" => PublishMode::Live,
				"append" => PublishMode::Append,
				"record" => PublishMode::Record,
				_ => {
					let error_properties =
						create_status_object("error", "NetStream.Publish.Start", "Invalid publish mode given");
					let packet = self.create_error_response(
						transaction_id,
						Amf0Value::Null,
						vec![Amf0Value::Object(error_properties)],
						stream_id,
					)?;

					return Ok(vec![ServerSessionResult::OutboundResponse(packet)]);
				}
			},

			_ => {
				let packet = self.create_error_packet(
					"NetStream.Publish.Start",
					"Invalid publish arguments",
					transaction_id,
					stream_id,
				)?;
				return Ok(vec![ServerSessionResult::OutboundResponse(packet)]);
			}
		};

		let request = OutstandingRequest::PublishRequested {
			stream_key: stream_key.clone(),
			mode: mode.clone(),
			stream_id,
		};

		let request_number = self.next_request_number;
		self.next_request_number = self.next_request_number + 1;
		self.outstanding_requests.insert(request_number, request);

		let event = ServerSessionEvent::PublishStreamRequested {
			request_id: request_number,
			app_name,
			stream_key,
			mode,
		};

		Ok(vec![ServerSessionResult::RaisedEvent(event)])
	}

	fn handle_command_play(
		&mut self,
		stream_id: u32,
		transaction_id: f64,
		mut arguments: Vec<Amf0Value>,
	) -> Result<Vec<ServerSessionResult>, ServerSessionError> {
		if arguments.len() < 1 {
			let packet = self.create_error_packet(
				"NetStream.Play.Start",
				"Invalid play arguments",
				transaction_id,
				stream_id,
			)?;
			return Ok(vec![ServerSessionResult::OutboundResponse(packet)]);
		}

		if self.current_state != SessionState::Connected {
			let packet = self.create_error_packet(
				"NetStream.Play.Start",
				"Can't play before connecting",
				transaction_id,
				stream_id,
			)?;
			return Ok(vec![ServerSessionResult::OutboundResponse(packet)]);
		}

		let app_name = match self.connected_app_name {
			Some(ref name) => name.clone(),
			None => {
				let packet = self.create_error_packet(
					"NetStream.Play.Start",
					"Can't play before connecting",
					transaction_id,
					stream_id,
				)?;
				return Ok(vec![ServerSessionResult::OutboundResponse(packet)]);
			}
		};

		let stream_key = match arguments.remove(0) {
			Amf0Value::Utf8String(stream_key) => stream_key,
			_ => {
				let packet = self.create_error_packet(
					"NetStream.Play.Start",
					"Invalid play arguments",
					transaction_id,
					stream_id,
				)?;
				return Ok(vec![ServerSessionResult::OutboundResponse(packet)]);
			}
		};

		let start_at = if arguments.len() >= 1 {
			match arguments.remove(0) {
				Amf0Value::Number(x) => {
					if x == -2.0 {
						PlayStartValue::LiveOrRecorded
					} else if x == -1.0 {
						PlayStartValue::LiveOnly
					} else if x >= 0.0 {
						PlayStartValue::StartTimeInSeconds(x as u32)
					} else {
						PlayStartValue::LiveOrRecorded // Invalid value so return default
					}
				}

				_ => PlayStartValue::LiveOrRecorded,
			}
		} else {
			PlayStartValue::LiveOrRecorded
		};

		let duration = if arguments.len() >= 1 {
			match arguments.remove(0) {
				Amf0Value::Number(x) => {
					if x >= 0.0 {
						Some(x as u32)
					} else {
						None
					}
				}

				_ => None,
			}
		} else {
			None
		};

		let reset = if arguments.len() >= 1 {
			match arguments.remove(0) {
				Amf0Value::Boolean(x) => x,
				_ => false,
			}
		} else {
			false
		};

		let request = OutstandingRequest::PlayRequested {
			stream_key: stream_key.clone(),
			stream_id,
		};

		let request_number = self.next_request_number;
		self.next_request_number = self.next_request_number + 1;
		self.outstanding_requests.insert(request_number, request);

		let event = ServerSessionEvent::PlayStreamRequested {
			request_id: request_number,
			app_name,
			stream_key,
			start_at,
			duration,
			reset,
			stream_id,
		};

		Ok(vec![ServerSessionResult::RaisedEvent(event)])
	}

	fn handle_amf0_data(
		&mut self,
		mut data: Vec<Amf0Value>,
		stream_id: u32,
	) -> Result<Vec<ServerSessionResult>, ServerSessionError> {
		if data.len() == 0 {
			// No data so just do nothing
			return Ok(Vec::new());
		}

		let first_element = data.remove(0);
		match first_element {
			Amf0Value::Utf8String(ref value) if value == "@setDataFrame" => {
				self.handle_amf0_data_set_data_frame(data, stream_id)
			}
			_ => Ok(Vec::new()),
		}
	}

	fn handle_amf0_data_set_data_frame(
		&mut self,
		mut data: Vec<Amf0Value>,
		stream_id: u32,
	) -> Result<Vec<ServerSessionResult>, ServerSessionError> {
		if data.len() < 2 {
			// We are expecting a "onMetaData" value and then a property with the actual metadata.  Since
			// this wasn't provided we don't know how to deal with this message. Returning also avoids the
			// `data[0]` index panic below on empty untrusted input.
			return Ok(Vec::new());
		}

		match data[0] {
			Amf0Value::Utf8String(ref value) if value == "onMetaData" => (),
			_ => return Ok(Vec::new()),
		}

		if self.connected_app_name.is_none() {
			return Ok(Vec::new()); // setDataFrame has no meaning until they are conneted and publishing
		}

		let app_name = match self.connected_app_name {
			Some(ref name) => name.clone(),
			None => return Ok(Vec::new()), // Not connected on a known app name.  Shouldn't really happen.
		};

		let publish_stream_key = match self.active_streams.get(&stream_id) {
			Some(ref stream) => {
				match stream.current_state {
					StreamState::Publishing {
						ref stream_key,
						mode: _,
					} => stream_key,
					_ => return Ok(Vec::new()), // Return nothing since we aren't publishing
				}
			}

			None => return Ok(Vec::new()), // Return nothing since this was not sent on an active stream
		};

		let mut metadata = StreamMetadata::new();
		let object = data.remove(1);
		let properties_option = object.get_object_properties();
		match properties_option {
			Some(properties) => metadata.apply_metadata_values(properties),
			_ => (),
		}

		let event = ServerSessionEvent::StreamMetadataChanged {
			stream_key: publish_stream_key.clone(),
			app_name,
			metadata,
		};

		Ok(vec![ServerSessionResult::RaisedEvent(event)])
	}

	fn handle_audio_data(
		&self,
		data: Bytes,
		stream_id: u32,
		timestamp: RtmpTimestamp,
	) -> Result<Vec<ServerSessionResult>, ServerSessionError> {
		if self.current_state != SessionState::Connected {
			// Audio data sent before connected, just ignore it.
			return Ok(Vec::new());
		}

		let app_name = match self.connected_app_name {
			Some(ref x) => x.clone(),
			None => return Ok(Vec::new()), // No app name so we aren't in a valid connection state.
		};

		let publish_stream_key = match self.active_streams.get(&stream_id) {
			Some(ref stream) => {
				match stream.current_state {
					StreamState::Publishing {
						ref stream_key,
						mode: _,
					} => stream_key.clone(),
					_ => return Ok(Vec::new()), // Not a publishing stream so ignore it
				}
			}

			None => return Ok(Vec::new()), // Audio sent over an invalid stream, ignore it
		};

		let event = ServerSessionEvent::AudioDataReceived {
			stream_key: publish_stream_key,
			app_name,
			timestamp,
			data,
		};

		Ok(vec![ServerSessionResult::RaisedEvent(event)])
	}

	fn handle_set_chunk_size(&mut self, size: u32) -> Result<Vec<ServerSessionResult>, ServerSessionError> {
		self.deserializer.set_max_chunk_size(size as usize)?;
		Ok(Vec::new())
	}

	fn handle_set_peer_bandwidth(
		&self,
		_size: u32,
		_limit_type: PeerBandwidthLimitType,
	) -> Result<Vec<ServerSessionResult>, ServerSessionError> {
		Ok(Vec::new())
	}

	fn handle_user_control(
		&mut self,
		event_type: UserControlEventType,
		_stream_id: Option<u32>,
		_buffer_length: Option<u32>,
		timestamp: Option<RtmpTimestamp>,
	) -> Result<Vec<ServerSessionResult>, ServerSessionError> {
		match event_type {
			UserControlEventType::PingRequest => {
				let message = RtmpMessage::UserControl {
					event_type: UserControlEventType::PingResponse,
					stream_id: None,
					buffer_length: None,
					timestamp,
				};

				let payload = message.into_message_payload(self.get_epoch(), 0)?;
				let response = self.serializer.serialize(&payload, false, false)?;
				Ok(vec![ServerSessionResult::OutboundResponse(response)])
			}

			UserControlEventType::PingResponse => {
				let timestamp = timestamp.unwrap_or(RtmpTimestamp::new(0));
				let event = ServerSessionEvent::PingResponseReceived { timestamp };
				Ok(vec![ServerSessionResult::RaisedEvent(event)])
			}

			_ => Ok(Vec::new()),
		}
	}

	fn handle_video_data(
		&self,
		data: Bytes,
		stream_id: u32,
		timestamp: RtmpTimestamp,
	) -> Result<Vec<ServerSessionResult>, ServerSessionError> {
		if self.current_state != SessionState::Connected {
			// Video data sent before connected, just ignore it.
			return Ok(Vec::new());
		}

		let app_name = match self.connected_app_name {
			Some(ref x) => x.clone(),
			None => return Ok(Vec::new()), // No app name so we aren't in a valid connection state.
		};

		let publish_stream_key = match self.active_streams.get(&stream_id) {
			Some(ref stream) => {
				match stream.current_state {
					StreamState::Publishing {
						ref stream_key,
						mode: _,
					} => stream_key.clone(),
					_ => return Ok(Vec::new()), // Not a publishing stream so ignore it
				}
			}

			None => return Ok(Vec::new()), // Video sent over an invalid stream, ignore it
		};

		let event = ServerSessionEvent::VideoDataReceived {
			stream_key: publish_stream_key,
			app_name,
			timestamp,
			data,
		};

		Ok(vec![ServerSessionResult::RaisedEvent(event)])
	}

	fn handle_window_acknowledgement(&mut self, size: u32) -> Result<Vec<ServerSessionResult>, ServerSessionError> {
		self.peer_window_ack_size = Some(size);
		Ok(Vec::new())
	}

	fn accept_connection_request(
		&mut self,
		app_name: String,
		transaction_id: f64,
	) -> Result<Vec<ServerSessionResult>, ServerSessionError> {
		self.connected_app_name = Some(app_name.clone());
		self.current_state = SessionState::Connected;

		let mut command_object_properties = HashMap::new();
		command_object_properties.insert("fmsVer".to_string(), Amf0Value::Utf8String(self.fms_version.clone()));
		command_object_properties.insert("capabilities".to_string(), Amf0Value::Number(31.0));
		command_object_properties.extend(self.connect_response_properties.clone());

		let description = "Successfully connected on app: ".to_string() + &app_name;
		let mut additional_properties =
			create_status_object("status", "NetConnection.Connect.Success", description.as_ref());
		additional_properties.insert("objectEncoding".to_string(), Amf0Value::Number(self.object_encoding));

		let message = RtmpMessage::Amf0Command {
			command_name: "_result".to_string(),
			transaction_id: transaction_id,
			command_object: Amf0Value::Object(command_object_properties),
			additional_arguments: vec![Amf0Value::Object(additional_properties)],
		};

		let payload = message.into_message_payload(self.get_epoch(), 0)?;
		let packet = self.serializer.serialize(&payload, false, false)?;

		Ok(vec![ServerSessionResult::OutboundResponse(packet)])
	}

	fn accept_publish_request(
		&mut self,
		stream_id: u32,
		stream_key: String,
		mode: PublishMode,
	) -> Result<Vec<ServerSessionResult>, ServerSessionError> {
		match self.active_streams.get_mut(&stream_id) {
			Some(active_stream) => {
				active_stream.current_state = StreamState::Publishing {
					stream_key: stream_key.clone(),
					mode,
				};
			}

			None => {
				return Err(ServerSessionError::ActionAttemptedOnInactiveStream {
					action: "publish".to_string(),
					stream_id,
				});
			}
		};

		let description = format!("Successfully started publishing on stream key {}", stream_key);

		let stream_begin_message = RtmpMessage::UserControl {
			event_type: UserControlEventType::StreamBegin,
			stream_id: Some(stream_id),
			buffer_length: None,
			timestamp: None,
		};

		let stream_begin_payload = stream_begin_message.into_message_payload(self.get_epoch(), stream_id)?;
		let stream_begin_packet = self.serializer.serialize(&stream_begin_payload, false, false)?;

		let status_object = create_status_object("status", "NetStream.Publish.Start", description.as_ref());
		let publish_start_message = RtmpMessage::Amf0Command {
			command_name: "onStatus".to_string(),
			transaction_id: 0.0,
			command_object: Amf0Value::Null,
			additional_arguments: vec![Amf0Value::Object(status_object)],
		};

		let publish_start_payload = publish_start_message.into_message_payload(self.get_epoch(), stream_id)?;
		let publish_packet = self.serializer.serialize(&publish_start_payload, false, false)?;

		Ok(vec![
			ServerSessionResult::OutboundResponse(stream_begin_packet),
			ServerSessionResult::OutboundResponse(publish_packet),
		])
	}

	fn accept_play_request(
		&mut self,
		stream_id: u32,
		stream_key: String,
	) -> Result<Vec<ServerSessionResult>, ServerSessionError> {
		match self.active_streams.get_mut(&stream_id) {
			Some(active_stream) => {
				active_stream.current_state = StreamState::Playing {
					stream_key: stream_key.clone(),
				};
			}

			None => {
				return Err(ServerSessionError::ActionAttemptedOnInactiveStream {
					action: "play".to_string(),
					stream_id,
				});
			}
		}

		let reset_status_object = create_status_object("status", "NetStream.Play.Reset", "Reset stream");
		let reset_message = RtmpMessage::Amf0Command {
			command_name: "onStatus".to_string(),
			transaction_id: 0.0,
			command_object: Amf0Value::Null,
			additional_arguments: vec![Amf0Value::Object(reset_status_object)],
		};

		let stream_begin_message = RtmpMessage::UserControl {
			event_type: UserControlEventType::StreamBegin,
			stream_id: Some(stream_id),
			buffer_length: None,
			timestamp: None,
		};

		let description = format!("Successfully started playback on stream key {}", stream_key);
		let start_status_object = create_status_object("status", "NetStream.Play.Start", description.as_ref());
		let start_message = RtmpMessage::Amf0Command {
			command_name: "onStatus".to_string(),
			transaction_id: 0.0,
			command_object: Amf0Value::Null,
			additional_arguments: vec![Amf0Value::Object(start_status_object)],
		};

		let data1_message = RtmpMessage::Amf0Data {
			values: vec![
				Amf0Value::Utf8String("|RtmpSampleAccess".to_string()),
				Amf0Value::Boolean(false),
				Amf0Value::Boolean(false),
			],
		};

		let mut data_start_properties = HashMap::new();
		data_start_properties.insert(
			"code".to_string(),
			Amf0Value::Utf8String("NetStream.Data.Start".to_string()),
		);

		let data2_message = RtmpMessage::Amf0Data {
			values: vec![
				Amf0Value::Utf8String("onStatus".to_string()),
				Amf0Value::Object(data_start_properties),
			],
		};

		let stream_begin_payload = stream_begin_message.into_message_payload(self.get_epoch(), stream_id)?;
		let stream_begin_packet = self.serializer.serialize(&stream_begin_payload, false, false)?;

		let start_payload = start_message.into_message_payload(self.get_epoch(), stream_id)?;
		let start_packet = self.serializer.serialize(&start_payload, false, false)?;

		let data1_payload = data1_message.into_message_payload(self.get_epoch(), stream_id)?;
		let data1_packet = self.serializer.serialize(&data1_payload, false, false)?;

		let data2_payload = data2_message.into_message_payload(self.get_epoch(), stream_id)?;
		let data2_packet = self.serializer.serialize(&data2_payload, false, false)?;

		let reset_payload = reset_message.into_message_payload(self.get_epoch(), stream_id)?;
		let reset_packet = self.serializer.serialize(&reset_payload, false, false)?;

		Ok(vec![
			ServerSessionResult::OutboundResponse(reset_packet),
			ServerSessionResult::OutboundResponse(stream_begin_packet),
			ServerSessionResult::OutboundResponse(start_packet),
			ServerSessionResult::OutboundResponse(data1_packet),
			ServerSessionResult::OutboundResponse(data2_packet),
		])
	}

	fn create_success_response(
		&mut self,
		transaction_id: f64,
		command_object: Amf0Value,
		additional_arguments: Vec<Amf0Value>,
		stream_id: u32,
	) -> Result<Packet, ServerSessionError> {
		let message = RtmpMessage::Amf0Command {
			command_name: "_result".to_string(),
			transaction_id,
			command_object,
			additional_arguments,
		};

		let payload = message.into_message_payload(self.get_epoch(), stream_id)?;
		let packet = self.serializer.serialize(&payload, false, false)?;
		Ok(packet)
	}

	fn create_error_response(
		&mut self,
		transaction_id: f64,
		command_object: Amf0Value,
		additional_arguments: Vec<Amf0Value>,
		stream_id: u32,
	) -> Result<Packet, ServerSessionError> {
		let message = RtmpMessage::Amf0Command {
			command_name: "_error".to_string(),
			transaction_id,
			command_object,
			additional_arguments,
		};

		let payload = message.into_message_payload(self.get_epoch(), stream_id)?;
		let packet = self.serializer.serialize(&payload, false, false)?;
		Ok(packet)
	}

	fn get_epoch(&self) -> RtmpTimestamp {
		match self.start_time.elapsed() {
			Ok(duration) => {
				let milliseconds = (duration.as_secs() * 1000) + (duration.subsec_nanos() as u64 / 1_000_000);

				// Casting to u32 should auto-wrap the value as expected.  If not a stream will probably
				// break after 49 days but testing shows it should wrap
				RtmpTimestamp::new(milliseconds as u32)
			}

			Err(_) => RtmpTimestamp::new(0), // Time went backwards, so just consider time as at epoch
		}
	}

	fn create_error_packet(
		&mut self,
		code: &str,
		description: &str,
		transaction_id: f64,
		stream_id: u32,
	) -> Result<Packet, ServerSessionError> {
		let status_object = create_status_object("_error", code, description);
		let packet = self.create_error_response(
			transaction_id,
			Amf0Value::Null,
			vec![Amf0Value::Object(status_object)],
			stream_id,
		)?;
		Ok(packet)
	}
}

fn create_status_object(level: &str, code: &str, description: &str) -> HashMap<String, Amf0Value> {
	let mut properties = HashMap::new();
	properties.insert("level".to_string(), Amf0Value::Utf8String(level.to_string()));
	properties.insert("code".to_string(), Amf0Value::Utf8String(code.to_string()));
	properties.insert(
		"description".to_string(),
		Amf0Value::Utf8String(description.to_string()),
	);
	properties
}
