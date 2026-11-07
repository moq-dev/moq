use crate::association::Association;
use crate::association::state::AssociationState;
use crate::chunk::chunk_payload_data::{ChunkPayloadData, PayloadProtocolIdentifier};
use crate::error::{Error, Result};
use crate::queue::reassembly_queue::{Chunks, ReassemblyQueue};
use crate::{ErrorCauseCode, Side};

use crate::util::{ByteSlice, BytesArray, BytesSource};
use alloc::vec;
use alloc::vec::Vec;
use bytes::Bytes;
use core::fmt;
use log::{debug, error, trace};

/// Identifier for a stream within a particular association
pub type StreamId = u16;

/// Cumulative WebRTC application payload totals at the SCTP API boundary.
///
/// Writes count once after queue acceptance; reads count once when a complete
/// message is returned. Retries, acknowledgements and abandonment do not change
/// these totals. Text uses UTF-8 bytes. Empty-message PPIDs count one message
/// and zero bytes; DCEP and unknown PPIDs are excluded. An empty write that
/// queues no SCTP message is excluded (use the empty PPID with its dummy byte).
/// All counters saturate at `u64::MAX`.
///
/// This is not a complete RTCDataChannelStats report: the embedding layer owns
/// label, protocol, WebRTC state, timestamp and report ID. Sends buffered above
/// SCTP and reads discarded above SCTP require accounting at that layer's API
/// boundary. Use checked conversion for WebIDL's 32-bit message counters.
/// Pair `generation` with an association identity for a unique report identity;
/// the numeric stream ID alone can be reused. Retain final snapshots from
/// [`crate::Event::DataChannelStatsFinal`] for closed-channel reporting.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
pub struct DataChannelStats {
    /// SCTP stream identifier.
    pub stream_identifier: StreamId,
    /// Unique stream generation within this association.
    pub generation: u64,
    /// Successfully accepted application messages.
    pub messages_sent: u64,
    /// Application payload bytes accepted for sending.
    pub bytes_sent: u64,
    /// Complete application messages returned to the caller.
    pub messages_received: u64,
    /// Application payload bytes returned to the caller.
    pub bytes_received: u64,
}

impl DataChannelStats {
    fn payload_bytes(ppi: PayloadProtocolIdentifier, len: usize) -> Option<u64> {
        match ppi {
            PayloadProtocolIdentifier::String | PayloadProtocolIdentifier::Binary => {
                Some(u64::try_from(len).unwrap_or(u64::MAX))
            }
            PayloadProtocolIdentifier::StringEmpty | PayloadProtocolIdentifier::BinaryEmpty => {
                Some(0)
            }
            _ => None,
        }
    }

    fn sent(&mut self, ppi: PayloadProtocolIdentifier, len: usize) {
        if let Some(bytes) = Self::payload_bytes(ppi, len) {
            self.messages_sent = self.messages_sent.saturating_add(1);
            self.bytes_sent = self.bytes_sent.saturating_add(bytes);
        }
    }

    fn received(&mut self, ppi: PayloadProtocolIdentifier, len: usize) {
        if let Some(bytes) = Self::payload_bytes(ppi, len) {
            self.messages_received = self.messages_received.saturating_add(1);
            self.bytes_received = self.bytes_received.saturating_add(bytes);
        }
    }
}

/// Why a stream reset did not complete successfully.
#[non_exhaustive]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum StreamResetError {
    /// The peer explicitly denied the reset request.
    Denied,
    /// The reset failed because of a protocol error or exhausted retransmissions.
    Failed,
}

/// Application events about streams
#[non_exhaustive]
#[derive(Debug, PartialEq, Eq)]
pub enum StreamEvent {
    /// One or more new streams has been opened
    Opened {
        /// Which stream was opened
        id: StreamId,
    },
    /// A currently open stream has data or errors waiting to be read
    Readable {
        /// Which stream is now readable
        id: StreamId,
    },
    /// A formerly write-blocked stream might be ready for a write or have been stopped
    ///
    /// Only generated for streams that are currently open.
    Writable {
        /// Which stream is now writable
        id: StreamId,
    },
    /// The stream was torn down by a reset, an inbound reset
    /// request from the peer naming the id was processed or
    /// its reciprocal to a locally-initiated one.
    ///
    /// No more data can be read from or written to it.
    ///
    /// Once this fires, either [`StreamEvent::ResetComplete`] or
    /// [`StreamEvent::ResetFailed`] for the same id is guaranteed to eventually
    /// follow, unless the association closes first.
    Finished {
        /// Which stream has been finished
        id: StreamId,
    },
    /// A reset handshake involving this stream id completed successfully.
    ///
    /// The id can be reused unless a newer reset for the same id has since
    /// started or failed.
    ResetComplete {
        /// Which stream id completed its reset.
        id: StreamId,
    },
    /// A reset handshake involving this stream id did not complete.
    ///
    /// The id remains unavailable for reuse until a later reset succeeds or
    /// the association closes.
    ResetFailed {
        /// Which stream id failed to reset.
        id: StreamId,
        /// Why the reset did not complete.
        reason: StreamResetError,
    },
    /// The peer asked us to stop sending on an outgoing stream
    Stopped {
        /// Which stream has been stopped
        id: StreamId,
        /// Error code supplied by the peer
        error_code: ErrorCauseCode,
    },
    /// At least one new stream of a certain directionality may be opened
    Available,
    /// The number of bytes of outgoing data buffered is lower than the threshold.
    BufferedAmountLow {
        /// Which stream is now readable
        id: StreamId,
    },
    /// The number of bytes of outgoing data buffered is higher than the threshold.
    BufferedAmountHigh {
        /// Which stream has crossed the high threshold
        id: StreamId,
    },
}

/// Reliability type for stream
#[derive(Debug, Copy, Clone, PartialEq, Default)]
pub enum ReliabilityType {
    /// ReliabilityTypeReliable is used for reliable transmission
    #[default]
    Reliable = 0,
    /// Limit retries per DATA chunk, excluding its original transmission.
    /// Originals and the last permitted retry remain buffered until acknowledged
    /// or a subsequent recovery event requires abandonment.
    Rexmit = 1,
    /// ReliabilityTypeTimed is used for partial reliability by retransmission duration
    Timed = 2,
}

impl fmt::Display for ReliabilityType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match *self {
            ReliabilityType::Reliable => "Reliable",
            ReliabilityType::Rexmit => "Rexmit",
            ReliabilityType::Timed => "Timed",
        };
        write!(f, "{}", s)
    }
}

impl From<u8> for ReliabilityType {
    fn from(v: u8) -> ReliabilityType {
        match v {
            1 => ReliabilityType::Rexmit,
            2 => ReliabilityType::Timed,
            _ => ReliabilityType::Reliable,
        }
    }
}

/// Stream represents an SCTP stream
pub struct Stream<'a> {
    pub(crate) stream_identifier: StreamId,
    pub(crate) association: &'a mut Association,
}

impl<'a> Stream<'a> {
    /// Return a read-only snapshot for the current stream generation.
    pub fn stats(&self) -> Result<DataChannelStats> {
        self.association
            .streams
            .get(&self.stream_identifier)
            .map(|s| s.stats)
            .ok_or(Error::ErrStreamClosed)
    }

    /// read reads a packet of len(p) bytes, dropping the Payload Protocol Identifier.
    /// Returns EOF when the stream is reset or an error if the stream is closed
    /// otherwise.
    pub fn read(&mut self) -> Result<Option<Chunks>> {
        self.read_sctp()
    }

    /// read_sctp reads a packet of len(p) bytes and returns the associated Payload
    /// Protocol Identifier.
    /// Returns EOF when the stream is reset or an error if the stream is closed
    /// otherwise.
    pub fn read_sctp(&mut self) -> Result<Option<Chunks>> {
        let (message, drained, generation) =
            if let Some(s) = self.association.streams.get_mut(&self.stream_identifier) {
                if s.state != RecvSendState::ReadWritable && s.state != RecvSendState::Readable {
                    return Err(Error::ErrStreamClosed);
                }

                let message = s.reassembly_queue.read();
                let drained = message.is_some() && s.reassembly_queue.get_num_bytes() == 0;
                (message, drained, s.stats.generation)
            } else {
                return Err(Error::ErrStreamClosed);
            };

        if drained {
            self.association
                .finish_retiring_stream(self.stream_identifier)?;
        }

        // A DATA chunk may exceed the last few bytes of receive credit. Reading
        // the final complete message can therefore leave a full partial message.
        if message.is_some() && self.association.receive_capacity_exhausted() {
            self.association.abort_receive_capacity();
        }

        if let Some(message) = &message {
            let stats = self
                .association
                .streams
                .get_mut(&self.stream_identifier)
                .filter(|s| s.stats.generation == generation)
                .map(|s| &mut s.stats)
                .or_else(|| {
                    self.association
                        .events
                        .iter_mut()
                        .find_map(|event| match event {
                            crate::Event::DataChannelStatsFinal(stats)
                                if stats.generation == generation =>
                            {
                                Some(stats)
                            }
                            _ => None,
                        })
                });
            if let Some(stats) = stats {
                stats.received(message.ppi, message.len());
            }
        }
        Ok(message)
    }

    /// write_sctp writes len(p) bytes from p to the DTLS connection
    pub fn write_sctp(&mut self, p: &Bytes, ppi: PayloadProtocolIdentifier) -> Result<usize> {
        self.write_source(&mut ByteSlice::from_slice(p), ppi)
    }

    /// Send data on the given stream.
    ///
    /// Uses the deafult payload protocol (PPI).
    ///
    /// Returns the number of bytes successfully written.
    pub fn write(&mut self, data: &[u8]) -> Result<usize> {
        self.write_with_ppi(data, self.get_default_payload_type()?)
    }

    /// Send data on the given stream, with a specific payload protocol.
    ///
    /// Returns the number of bytes successfully written.
    pub fn write_with_ppi(&mut self, data: &[u8], ppi: PayloadProtocolIdentifier) -> Result<usize> {
        self.write_source(&mut ByteSlice::from_slice(data), ppi)
    }

    /// write writes len(p) bytes from p with the default Payload Protocol Identifier
    pub fn write_chunk(&mut self, p: &Bytes) -> Result<usize> {
        self.write_source(
            &mut ByteSlice::from_slice(p),
            self.get_default_payload_type()?,
        )
    }

    /// Send data on the given stream
    ///
    /// Returns the number of bytes and chunks successfully written.
    /// Note that this method might also write a partial chunk. In this case
    /// it will not count this chunk as fully written. However
    /// the chunk will be advanced and contain only non-written data after the call.
    pub fn write_chunks(&mut self, data: &mut [Bytes]) -> Result<usize> {
        self.write_source(
            &mut BytesArray::from_chunks(data),
            self.get_default_payload_type()?,
        )
    }

    /// write_source writes BytesSource to the DTLS connection
    fn write_source<B: BytesSource>(
        &mut self,
        source: &mut B,
        ppi: PayloadProtocolIdentifier,
    ) -> Result<usize> {
        if !self.is_writable() {
            return Err(Error::ErrStreamClosed);
        }

        if source.remaining() > self.association.max_send_message_size() as usize {
            return Err(Error::ErrOutboundPacketTooLarge);
        }

        let state: AssociationState = self.association.state();
        match state {
            AssociationState::ShutdownSent
            | AssociationState::ShutdownAckSent
            | AssociationState::ShutdownPending
            | AssociationState::ShutdownReceived => return Err(Error::ErrStreamClosed),
            _ => {}
        };

        let (p, _) = source.pop_chunk(self.association.max_send_message_size() as usize);

        if let Some(s) = self.association.streams.get_mut(&self.stream_identifier) {
            let (is_buffered_amount_high, mut chunks) = s.packetize(&p, ppi);
            for chunk in &mut chunks {
                chunk.reliability = Some((s.reliability_type, s.reliability_value));
            }
            let queued = !chunks.is_empty();
            self.association.send_payload_data(chunks)?;
            if queued {
                self.association
                    .streams
                    .get_mut(&self.stream_identifier)
                    .expect("accepted stream remains registered")
                    .stats
                    .sent(ppi, p.len());
            }

            if is_buffered_amount_high {
                trace!("StreamEvent::BufferedAmountHigh");
                self.association
                    .events
                    .push_back(crate::association::Event::Stream(
                        StreamEvent::BufferedAmountHigh {
                            id: self.stream_identifier,
                        },
                    ));
            }

            Ok(p.len())
        } else {
            Err(Error::ErrStreamClosed)
        }
    }

    pub fn is_readable(&self) -> bool {
        if let Some(s) = self.association.streams.get(&self.stream_identifier) {
            s.state == RecvSendState::Readable || s.state == RecvSendState::ReadWritable
        } else {
            false
        }
    }

    pub fn is_writable(&self) -> bool {
        // RFC 6525 section 5.1.2 A1 forbids assigning new SSNs while an
        // Outgoing SSN Reset Request for this stream is pending. A failed
        // reset also quarantines the outgoing direction because the peer may
        // already have reset its corresponding incoming stream.
        if self
            .association
            .stream_reset_blocked(self.stream_identifier)
        {
            return false;
        }
        if let Some(s) = self.association.streams.get(&self.stream_identifier) {
            s.state == RecvSendState::Writable || s.state == RecvSendState::ReadWritable
        } else {
            false
        }
    }

    /// stop closes the read-direction of the stream.
    /// Future calls to read are not permitted after calling stop.
    /// If stopping removes the last way to drain an exhausted receive buffer,
    /// the association reports [`super::AssociationError::ReceiveBufferExhausted`].
    ///    
    /// NOTE: a stream closed without a queued reset never produces
    /// `StreamEvent::Finished` followed by `StreamEvent::ResetComplete` or
    /// `StreamEvent::ResetFailed`.
    pub fn stop(&mut self) -> Result<()> {
        let retiring = self
            .association
            .retiring_streams
            .contains_key(&self.stream_identifier);
        let reset = self
            .association
            .streams
            .get(&self.stream_identifier)
            .is_some_and(|s| {
                s.state == RecvSendState::Readable || s.state == RecvSendState::ReadWritable
            });

        if reset
            && !retiring
            && !self
                .association
                .stream_reset_in_progress(self.stream_identifier)
        {
            // Reset the outgoing stream
            // https://tools.ietf.org/html/rfc6525
            //
            // Queued before clearing the read bit below, send_reset_request
            // is the only fallible step, and failing after the state
            // mutation would leave the stream half-closed with the reset
            // silently dropped and unretryable.
            self.association
                .send_reset_request(self.stream_identifier)?;
        }

        if let Some(s) = self.association.streams.get_mut(&self.stream_identifier) {
            s.state = ((s.state as u8) & 0x2).into();
        }

        // stop() promises that future reads are not permitted. If a reset was
        // preserving unread boundary DATA, discard that old generation now so
        // its Finished event and unrelated association events cannot stall.
        if retiring {
            self.association
                .discard_retiring_streams(self.stream_identifier);
        }

        if self.association.receive_capacity_exhausted() {
            self.association.abort_receive_capacity();
        }

        Ok(())
    }

    /// finish closes the write-direction of the stream.
    /// Future calls to write are not permitted after calling Close.
    pub fn finish(&mut self) -> Result<()> {
        if let Some(s) = self.association.streams.get_mut(&self.stream_identifier) {
            s.state = ((s.state as u8) & 0x1).into();
        }
        Ok(())
    }

    /// close shuts down both the read and write halves of this stream.
    ///
    /// This is a convenience method that calls `finish()` followed by `stop()`.
    /// Resets the stream when both halves are shutdown.
    ///
    /// The single failure mode is `stop()`'s, if the association is not
    /// established, no reset was queued, retrying `close()` will
    /// attempt the reset again.
    pub fn close(&mut self) -> Result<()> {
        self.finish()?;
        self.stop()
    }

    /// stream_identifier returns the Stream identifier associated to the stream.
    pub fn stream_identifier(&self) -> StreamId {
        self.stream_identifier
    }

    /// set_default_payload_type sets the default payload type used by write.
    pub fn set_default_payload_type(
        &mut self,
        default_payload_type: PayloadProtocolIdentifier,
    ) -> Result<()> {
        if let Some(s) = self.association.streams.get_mut(&self.stream_identifier) {
            s.default_payload_type = default_payload_type;
            Ok(())
        } else {
            Err(Error::ErrStreamClosed)
        }
    }

    /// get_default_payload_type returns the payload type associated to the stream.
    pub fn get_default_payload_type(&self) -> Result<PayloadProtocolIdentifier> {
        if let Some(s) = self.association.streams.get(&self.stream_identifier) {
            Ok(s.default_payload_type)
        } else {
            Err(Error::ErrStreamClosed)
        }
    }

    /// set_reliability_params sets reliability parameters for this stream.
    ///
    /// Timed-message lifetimes start at the next [`Association::poll_transmit`], using its
    /// caller-supplied time, and include waiting for send credit after that poll. Each
    /// accepted message retains its reliability policy even if the stream settings change.
    pub fn set_reliability_params(
        &mut self,
        unordered: bool,
        rel_type: ReliabilityType,
        rel_val: u32,
    ) -> Result<()> {
        if let Some(s) = self.association.streams.get_mut(&self.stream_identifier) {
            debug!(
                "[{}] reliability params: ordered={} type={} value={}",
                s.side, !unordered, rel_type, rel_val
            );
            s.unordered = unordered;
            s.reliability_type = rel_type;
            s.reliability_value = rel_val;
            Ok(())
        } else {
            Err(Error::ErrStreamClosed)
        }
    }

    /// buffered_amount returns the number of bytes of data currently queued to be sent over this stream.
    pub fn buffered_amount(&self) -> Result<usize> {
        if let Some(s) = self.association.streams.get(&self.stream_identifier) {
            Ok(s.buffered_amount)
        } else {
            Err(Error::ErrStreamClosed)
        }
    }

    /// buffered_amount_low_threshold returns the number of bytes of buffered outgoing data that is
    /// considered "low." Defaults to 0.
    pub fn buffered_amount_low_threshold(&self) -> Result<usize> {
        if let Some(s) = self.association.streams.get(&self.stream_identifier) {
            Ok(s.buffered_amount_low)
        } else {
            Err(Error::ErrStreamClosed)
        }
    }

    /// set_buffered_amount_low_threshold is used to update the threshold.
    /// See buffered_amount_low_threshold().
    pub fn set_buffered_amount_low_threshold(&mut self, th: usize) -> Result<()> {
        if let Some(s) = self.association.streams.get_mut(&self.stream_identifier) {
            s.buffered_amount_low = th;
            Ok(())
        } else {
            Err(Error::ErrStreamClosed)
        }
    }

    /// buffered_amount_high_threshold returns the number of bytes of buffered outgoing data that is
    /// considered "high." Defaults to usize::MAX (effectively disabled).
    pub fn buffered_amount_high_threshold(&self) -> Result<usize> {
        if let Some(s) = self.association.streams.get(&self.stream_identifier) {
            Ok(s.buffered_amount_high)
        } else {
            Err(Error::ErrStreamClosed)
        }
    }

    /// set_buffered_amount_high_threshold is used to update the threshold.
    /// See buffered_amount_high_threshold().
    pub fn set_buffered_amount_high_threshold(&mut self, th: usize) -> Result<()> {
        if let Some(s) = self.association.streams.get_mut(&self.stream_identifier) {
            s.buffered_amount_high = th;
            Ok(())
        } else {
            Err(Error::ErrStreamClosed)
        }
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Default)]
pub enum RecvSendState {
    #[default]
    Closed = 0,
    Readable = 1,
    Writable = 2,
    ReadWritable = 3,
}

impl From<u8> for RecvSendState {
    fn from(v: u8) -> Self {
        match v {
            1 => RecvSendState::Readable,
            2 => RecvSendState::Writable,
            3 => RecvSendState::ReadWritable,
            _ => RecvSendState::Closed,
        }
    }
}

/// StreamState represents the state of an SCTP stream
#[derive(Default, Debug)]
pub struct StreamState {
    pub(crate) stats: DataChannelStats,
    pub(crate) first_message_id: u64,
    pub(crate) side: Side,
    pub(crate) max_payload_size: u32,
    pub(crate) stream_identifier: StreamId,
    pub(crate) default_payload_type: PayloadProtocolIdentifier,
    pub(crate) reassembly_queue: ReassemblyQueue,
    pub(crate) sequence_number: u16,
    pub(crate) state: RecvSendState,
    pub(crate) unordered: bool,
    pub(crate) reliability_type: ReliabilityType,
    pub(crate) reliability_value: u32,
    pub(crate) buffered_amount: usize,
    pub(crate) buffered_amount_low: usize,
    pub(crate) buffered_amount_high: usize,
}
impl StreamState {
    pub(crate) fn new(
        side: Side,
        stream_identifier: StreamId,
        max_payload_size: u32,
        max_receive_message_size: u32,
        default_payload_type: PayloadProtocolIdentifier,
    ) -> Self {
        StreamState {
            stats: DataChannelStats {
                stream_identifier,
                ..Default::default()
            },
            first_message_id: 0,
            side,
            stream_identifier,
            max_payload_size,
            default_payload_type,
            reassembly_queue: ReassemblyQueue::new(stream_identifier, max_receive_message_size),
            sequence_number: 0,
            state: RecvSendState::ReadWritable,
            unordered: false,
            reliability_type: ReliabilityType::Reliable,
            reliability_value: 0,
            buffered_amount: 0,
            buffered_amount_low: 0,
            buffered_amount_high: usize::MAX,
        }
    }

    pub(crate) fn handle_data(&mut self, pd: &ChunkPayloadData) -> Result<bool> {
        self.reassembly_queue.push(pd.clone())
    }

    fn packetize(
        &mut self,
        raw: &Bytes,
        ppi: PayloadProtocolIdentifier,
    ) -> (bool, Vec<ChunkPayloadData>) {
        let mut i = 0;
        let mut remaining = raw.len();

        // From draft-ietf-rtcweb-data-protocol-09, section 6:
        //   All Data Channel Establishment Protocol messages MUST be sent using
        //   ordered delivery and reliable transmission.
        let unordered = ppi != PayloadProtocolIdentifier::Dcep && self.unordered;

        let mut chunks = vec![];

        while remaining != 0 {
            // self.association.max_payload_size
            let fragment_size = core::cmp::min(self.max_payload_size as usize, remaining);

            // Copy the userdata since we'll have to store it until acked
            // and the caller may re-use the buffer in the mean time
            let user_data = raw.slice(i..i + fragment_size);

            let chunk = ChunkPayloadData {
                stream_identifier: self.stream_identifier,
                user_data,
                unordered,
                beginning_fragment: i == 0,
                ending_fragment: remaining - fragment_size == 0,
                immediate_sack: false,
                payload_type: ppi,
                stream_sequence_number: self.sequence_number,
                ..Default::default()
            };

            chunks.push(chunk);

            remaining -= fragment_size;
            i += fragment_size;
        }

        // RFC 4960 Sec 6.6
        // Note: When transmitting ordered and unordered data, an endpoint does
        // not increment its Stream Sequence Number when transmitting a DATA
        // chunk with U flag set to 1.
        if !unordered {
            self.sequence_number = self.sequence_number.wrapping_add(1);
        }

        let old_amount = self.buffered_amount;
        self.buffered_amount += raw.len();
        let new_amount = self.buffered_amount;

        // Check if we crossed the high threshold
        let is_buffered_amount_high =
            old_amount < self.buffered_amount_high && new_amount >= self.buffered_amount_high;

        (is_buffered_amount_high, chunks)
    }

    /// This method is called by association's read_loop (go-)routine to notify this stream
    /// of the specified amount of outgoing data has been delivered to the peer.
    pub(crate) fn on_buffer_released(&mut self, n_bytes_released: i64) -> bool {
        if n_bytes_released <= 0 {
            return false;
        }

        let from_amount = self.buffered_amount;
        let new_amount = if from_amount < n_bytes_released as usize {
            self.buffered_amount = 0;
            error!(
                "[{}] released buffer size {} should be <= {}",
                self.side, n_bytes_released, 0,
            );
            0
        } else {
            self.buffered_amount -= n_bytes_released as usize;

            from_amount - n_bytes_released as usize
        };

        let buffered_amount_low = self.buffered_amount_low;

        trace!(
            "[{}] bufferedAmount = {}, from_amount = {}, buffered_amount_low = {}",
            self.side, new_amount, from_amount, buffered_amount_low,
        );

        from_amount > buffered_amount_low && new_amount <= buffered_amount_low
    }

    pub(crate) fn get_num_bytes_in_reassembly_queue(&self) -> usize {
        // No lock is required as it reads the size with atomic load function.
        self.reassembly_queue.get_num_bytes()
    }
}

#[cfg(test)]
mod stats_tests {
    use super::*;

    #[test]
    fn counters_saturate_and_share_payload_projection() {
        let mut stats = DataChannelStats {
            messages_sent: u64::MAX - 1,
            bytes_sent: u64::MAX - 1,
            messages_received: u64::MAX - 1,
            bytes_received: u64::MAX - 1,
            ..Default::default()
        };
        for ppi in [
            PayloadProtocolIdentifier::Binary,
            PayloadProtocolIdentifier::String,
        ] {
            stats.sent(ppi, 2);
            stats.received(ppi, 2);
        }
        assert_eq!(stats.messages_sent, u64::MAX);
        assert_eq!(stats.bytes_sent, u64::MAX);
        assert_eq!(stats.messages_received, u64::MAX);
        assert_eq!(stats.bytes_received, u64::MAX);
        for ppi in [
            PayloadProtocolIdentifier::Dcep,
            PayloadProtocolIdentifier::Unknown,
        ] {
            let mut stats = DataChannelStats::default();
            stats.sent(ppi, 10);
            stats.received(ppi, 10);
            assert_eq!(stats, DataChannelStats::default());
        }
    }
}
