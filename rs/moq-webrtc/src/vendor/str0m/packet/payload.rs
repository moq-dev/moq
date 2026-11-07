use std::time::Instant;

use crate::vendor::str0m::format::{CodecSpec, Vp9PacketizerMode};
use crate::vendor::str0m::media::ToPayload;
use crate::vendor::str0m::rtp_::{ExtensionValues, Frequency, Pt, SeqNo};
use crate::vendor::str0m::streams::{RtpWrite, StreamTx};

use super::PacketError;
use super::{CodecPacketizer, Packetizer};

/// Turns frames into RTP packets.
///
/// The payloader is deliberately codec- and feature-agnostic: it packetizes via its
/// [`CodecPacketizer`] and emits [`OutgoingPacket`]s through a [`PacketSink`]. All codec-specific
/// behaviour (marker/nackable policy, DONL) lives on the packetizer.
#[derive(Debug)]
pub struct Payloader {
	pack: CodecPacketizer,
	clock_rate: Frequency,
}

/// A packet ready to hand to a [`PacketSink`]: the RTP fields plus the payload as an owned
/// `Vec`. The payload is only wrapped into an `Arc` once, when the sink builds the [`RtpWrite`].
pub(crate) struct OutgoingPacket {
	pub pt: Pt,
	pub seq_no: SeqNo,
	pub rtp_time: u32,
	pub wallclock: Instant,
	pub marker: bool,
	pub nackable: bool,
	pub ext_vals: ExtensionValues,
	pub payload: Vec<u8>,
}

/// Where a [`Payloader`] sends its packetized output, implemented by [`StreamTx`].
pub(crate) trait PacketSink {
	/// The next sequence number to assign on the underlying stream.
	fn next_seq_no(&mut self) -> SeqNo;
	/// The most recently written packet payload on the underlying stream, for marker decisions.
	fn last_packet(&self) -> Option<&[u8]>;
	/// Emit a packet.
	fn send(&mut self, packet: OutgoingPacket);
}

impl Payloader {
	pub(crate) fn new(spec: CodecSpec, vp9_mode: Vp9PacketizerMode) -> Self {
		let mut pack = CodecPacketizer::new(spec.codec, vp9_mode);
		// The packetizer applies its own codec-specific configuration (e.g. DONL); the payloader
		// never names concrete packetizer variants.
		pack.configure_for(&spec);

		Payloader {
			pack,
			clock_rate: spec.clock_rate,
		}
	}

	pub(crate) fn push_sample<S: PacketSink>(
		&mut self,
		to_payload: ToPayload,
		mtu: usize,
		sink: &mut S,
	) -> Result<(), PacketError> {
		let ToPayload {
			pt,
			wallclock,
			rtp_time,
			data,
			start_of_talk_spurt,
			ext_vals,
			..
		} = to_payload;

		let chunks = self.pack.packetize(mtu, data.as_ref())?;
		let len = chunks.len();

		for (idx, data) in chunks.into_iter().enumerate() {
			let last = idx == len - 1;
			let first = idx == 0;

			let previous_data = sink.last_packet();
			let marker = self.pack.is_marker(data.as_slice(), previous_data, last)
				|| (self.pack.marks_talkspurt() && start_of_talk_spurt);

			let seq_no = sink.next_seq_no();

			let nackable = self.pack.nackable();

			let mut pkt_ext_vals = ext_vals.clone();

			if !first {
				pkt_ext_vals.abs_capture_time = None;
			}

			if !last {
				pkt_ext_vals.video_orientation = None;
				pkt_ext_vals.video_content_type = None;
				pkt_ext_vals.video_timing = None;
			}

			let rtp_time_u32 = rtp_time.rebase(self.clock_rate).numer() as u32;

			sink.send(OutgoingPacket {
				pt,
				seq_no,
				rtp_time: rtp_time_u32,
				wallclock,
				marker,
				nackable,
				ext_vals: pkt_ext_vals,
				payload: data,
			});
		}

		Ok(())
	}
}

/// The plain sink: writes each packet straight to the stream on its own payload type.
impl PacketSink for StreamTx {
	fn next_seq_no(&mut self) -> SeqNo {
		StreamTx::next_seq_no(self)
	}

	fn last_packet(&self) -> Option<&[u8]> {
		StreamTx::last_packet(self)
	}

	fn send(&mut self, packet: OutgoingPacket) {
		self.write_rtp(
			RtpWrite::new(
				packet.pt,
				packet.seq_no,
				packet.rtp_time,
				packet.wallclock,
				packet.payload,
			)
			.marker(packet.marker)
			.ext_vals(packet.ext_vals)
			.nackable(packet.nackable),
		);
	}
}
