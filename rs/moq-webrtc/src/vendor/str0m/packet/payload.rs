use std::time::Instant;

use crate::format::{CodecSpec, Vp9PacketizerMode};
use crate::media::ToPayload;
use crate::rtp::vla::VideoLayersAllocation;
use crate::rtp_::{ExtensionValues, Frequency, Pt, SeqNo};
use crate::streams::{RtpWrite, StreamTx};

use super::PacketError;
use super::red::RedSender;
use super::{CodecPacketizer, Packetizer};

/// Turns frames into RTP packets.
///
/// The payloader is deliberately codec- and feature-agnostic: it packetizes via its
/// [`CodecPacketizer`] and emits [`OutgoingPacket`]s through a [`PacketSink`]. All codec-specific
/// behaviour (marker/nackable policy, DONL) lives on the packetizer, and RFC 2198 RED wrapping is
/// applied by the sink (see [`RedSink`]), owned a level up in `Media`.
#[derive(Debug)]
pub struct Payloader {
    pack: CodecPacketizer,
    clock_rate: Frequency,
}

/// A packet ready to hand to a [`PacketSink`]: the RTP fields plus the primary payload as an owned
/// `Vec`. The payload is only wrapped into an `Arc` once, when the sink builds the [`RtpWrite`], so
/// a RED sink can transform the bytes first without an extra allocation.
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

/// Where a [`Payloader`] sends its packetized output. Implemented by [`StreamTx`] (plain) and
/// [`RedSink`] (RFC 2198 wrapping), so the payloader stays unaware of both the concrete stream and
/// RED. Dispatch is static: [`Payloader::push_sample`] is monomorphised per sink, so the plain
/// path carries no RED branch at all.
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
            clock_rate: spec.rtp_clock_rate(),
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
                pkt_ext_vals.user_values.remove::<VideoLayersAllocation>();
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

/// Decorates a [`StreamTx`] so a [`Payloader`]'s output is wrapped in RFC 2198 RED and sent on the
/// RED payload type. Holds only borrows: the [`RedSender`] state (history, distances) is owned by
/// `Media`, and the same primary SSRC, sequence number, marker and extensions are preserved.
pub(crate) struct RedSink<'a> {
    stream: &'a mut StreamTx,
    red: &'a mut RedSender,
    /// Full aligned MTU; the RED payload (primary + redundancy) is shed to fit within this.
    budget: usize,
}

impl<'a> RedSink<'a> {
    pub(crate) fn new(stream: &'a mut StreamTx, red: &'a mut RedSender, budget: usize) -> Self {
        RedSink {
            stream,
            red,
            budget,
        }
    }
}

impl PacketSink for RedSink<'_> {
    fn next_seq_no(&mut self) -> SeqNo {
        StreamTx::next_seq_no(self.stream)
    }

    fn last_packet(&self) -> Option<&[u8]> {
        StreamTx::last_packet(self.stream)
    }

    fn send(&mut self, packet: OutgoingPacket) {
        // Wrap the primary Vec before it becomes an Arc, so RED adds no extra allocation. Swap to
        // the RED payload type; everything else on the packet is preserved.
        let payload = self.red.wrap(packet.payload, packet.rtp_time, self.budget);
        self.stream.write_rtp(
            RtpWrite::new(
                self.red.red_pt(),
                packet.seq_no,
                packet.rtp_time,
                packet.wallclock,
                payload,
            )
            .marker(packet.marker)
            .ext_vals(packet.ext_vals)
            .nackable(packet.nackable),
        );
    }
}
