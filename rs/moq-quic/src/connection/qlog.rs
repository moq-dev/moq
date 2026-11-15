use std::sync::{Arc, Mutex};
use std::time::Duration;

use qlog::{
	events::{
		Event, EventData,
		quic::{PacketHeader, PacketLost, PacketLostTrigger, PacketReceived, PacketSent, PacketType},
	},
	streamer::QlogStreamer,
};
use tracing::warn;

use crate::{
	ConnectionId, Instant,
	connection::{PathData, SentPacket},
	packet::SpaceId,
};

/// Shareable handle to a single qlog output stream
#[derive(Clone)]
pub struct QlogStream {
	pub(crate) streamer: Arc<Mutex<QlogStreamer>>,
	/// The epoch event times count from, on MoQ's clock rather than the std one the
	/// streamer keeps.
	pub(crate) start: Instant,
}

impl QlogStream {
	fn emit_event(&self, orig_rem_cid: ConnectionId, event: EventData, now: Instant) {
		// Milliseconds since the epoch, at the nanosecond precision the streamer is built with.
		let time = now.saturating_duration_since(self.start).as_nanos() as f64 / 1e6;
		let mut event = Event::with_time(time, event);
		event.group_id = Some(Box::new(orig_rem_cid.to_string()));

		let mut qlog_streamer = self.streamer.lock().unwrap();
		if let Err(e) = qlog_streamer.add_event(event) {
			warn!("could not emit qlog event: {e}");
		}
	}
}

/// Where a connection sends qlog events, or nowhere when qlog is off
#[derive(Clone, Default)]
pub(crate) struct QlogSink {
	stream: Option<QlogStream>,
}

impl QlogSink {
	pub(super) fn emit_recovery_metrics(
		&self,
		pto_count: u32,
		path: &mut PathData,
		now: Instant,
		orig_rem_cid: ConnectionId,
	) {
		let Some(stream) = self.stream.as_ref() else {
			return;
		};

		let Some(metrics) = path.qlog_recovery_metrics(pto_count) else {
			return;
		};

		stream.emit_event(orig_rem_cid, EventData::QuicMetricsUpdated(metrics), now);
	}

	pub(super) fn emit_packet_lost(
		&self,
		pn: u64,
		info: &SentPacket,
		loss_delay: Duration,
		space: SpaceId,
		now: Instant,
		orig_rem_cid: ConnectionId,
	) {
		let Some(stream) = self.stream.as_ref() else {
			return;
		};

		let event = PacketLost {
			header: Some(PacketHeader {
				packet_number: Some(pn),
				packet_type: packet_type(space),
				length: Some(info.size),
				..Default::default()
			}),
			frames: None,
			is_mtu_probe_packet: None,
			trigger: Some(match now.saturating_duration_since(info.time_sent) >= loss_delay {
				true => PacketLostTrigger::TimeThreshold,
				false => PacketLostTrigger::ReorderingThreshold,
			}),
		};

		stream.emit_event(orig_rem_cid, EventData::QuicPacketLost(event), now);
	}

	pub(super) fn emit_packet_sent(
		&self,
		pn: u64,
		len: usize,
		space: SpaceId,
		now: Instant,
		orig_rem_cid: ConnectionId,
	) {
		let Some(stream) = self.stream.as_ref() else {
			return;
		};

		let event = PacketSent {
			header: PacketHeader {
				packet_number: Some(pn),
				packet_type: packet_type(space),
				length: Some(len as u16),
				..Default::default()
			},
			..Default::default()
		};

		stream.emit_event(orig_rem_cid, EventData::QuicPacketSent(event), now);
	}

	pub(super) fn emit_packet_received(&self, pn: u64, space: SpaceId, now: Instant, orig_rem_cid: ConnectionId) {
		let Some(stream) = self.stream.as_ref() else {
			return;
		};

		let event = PacketReceived {
			header: PacketHeader {
				packet_number: Some(pn),
				packet_type: packet_type(space),
				..Default::default()
			},
			..Default::default()
		};

		stream.emit_event(orig_rem_cid, EventData::QuicPacketReceived(event), now);
	}
}

impl From<Option<QlogStream>> for QlogSink {
	fn from(stream: Option<QlogStream>) -> Self {
		Self { stream }
	}
}

fn packet_type(space: SpaceId) -> PacketType {
	match space {
		SpaceId::Initial => PacketType::Initial,
		SpaceId::Handshake => PacketType::Handshake,
		SpaceId::Data => PacketType::OneRtt,
	}
}
