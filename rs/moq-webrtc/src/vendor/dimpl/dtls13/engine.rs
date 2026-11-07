use std::mem;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use arrayvec::ArrayVec;

use super::queue::{QueueRx, QueueTx};
use crate::buffer::{Buf, BufferPool, TmpBuf};
use crate::crypto::Aad;
use crate::crypto::Cipher;
use crate::crypto::HmacProvider;
use crate::crypto::Nonce;
use crate::crypto::SigningKey;
use crate::crypto::SupportedDtls13CipherSuite;
use crate::crypto::SupportedKxGroup;
use crate::crypto::prf_hkdf;
use crate::dtls13::incoming::{Incoming, Record, RecordHandler};
use crate::dtls13::message::Body;
use crate::dtls13::message::ContentType;
use crate::dtls13::message::Dtls13CipherSuite;
use crate::dtls13::message::Dtls13Record;
use crate::dtls13::message::Handshake;
use crate::dtls13::message::Header;
use crate::dtls13::message::KeyUpdateRequest;
use crate::dtls13::message::MessageType;
use crate::dtls13::message::Sequence;
use crate::timer::{HandshakeTimers, Timeout, deadline};
use crate::types::{HashAlgorithm, Random};
use crate::window::ReplayWindow;
use crate::{Config, DtlsCertificate, Error, InternalError, Output, SeededRng};

const MAX_DEFRAGMENT_PACKETS: usize = 50;

/// Bound record-number history retained in each saved record's pooled buffer.
const MAX_PREVIOUS_TRANSMISSIONS: usize = 32;

/// Maximum DTLS sequence number (2^48 - 1). Per RFC 9147 §4.2,
/// implementations MUST NOT allow the sequence number to wrap.
const MAX_SEQUENCE_NUMBER: u64 = (1u64 << 48) - 1;

pub struct Engine {
    /// Configuration options.
    config: Arc<Config>,

    /// Saved certificate
    certificate: DtlsCertificate,

    /// Seedable random number generator for deterministic testing
    rng: SeededRng,

    /// Pool of buffers
    buffers_free: BufferPool,

    /// Counters for sending DTLSPlaintext during epoch 0.
    sequence_epoch_0: Sequence,

    /// Queue of incoming packets.
    queue_rx: QueueRx,

    /// Queue of outgoing packets.
    queue_tx: QueueTx,

    /// The cipher suite in use. Set by ServerHello.
    cipher_suite: Option<Dtls13CipherSuite>,

    /// Handshake send keys (epoch 2)
    hs_send_keys: Option<EpochKeys>,

    /// Handshake receive keys (epoch 2)
    hs_recv_keys: Option<EpochKeys>,

    /// Expected next receive sequence number for epoch 2 (handshake)
    hs_expected_recv_seq: u64,

    /// Application send epoch (3 initially, increments on KeyUpdate)
    app_send_epoch: u16,

    /// Sequence number for handshake epoch (epoch 2) sending
    hs_send_seq: u64,

    /// Sequence number within current send epoch
    app_send_seq: u64,

    /// Application send keys (only latest epoch; replaced on KeyUpdate)
    app_send_keys: Option<EpochKeys>,

    /// Previous application send keys, retained for KeyUpdate retransmission.
    prev_app_send_keys: Option<EpochKeys>,

    /// Epoch of the previous send keys.
    prev_app_send_epoch: u16,

    /// Next sequence number on the previous send epoch (for retransmissions).
    prev_app_send_seq: u64,

    /// Application receive keys. Multiple epochs may coexist due to KeyUpdate.
    app_recv_keys: ArrayVec<RecvEpochEntry, 4>,

    /// Whether the remote peer has enabled encryption
    peer_encryption_enabled: bool,

    /// Signing key for CertificateVerify
    signing_key: Box<dyn SigningKey>,

    /// Whether this engine is for a client (true) or server (false)
    is_client: bool,

    /// Expected peer handshake sequence number
    peer_handshake_seq_no: u16,

    /// Next handshake message sequence number for sending
    next_handshake_seq_no: u16,

    /// Handshakes collected for hash computation.
    /// TLS 1.3 transcript: msg_type(1) + length(3), no DTLS framing.
    pub(crate) transcript: Buf,

    /// Anti-replay window for handshake epoch (epoch 2)
    hs_replay: ReplayWindow,

    /// Record numbers of received handshake records, for ACK generation.
    /// Each entry is (epoch, sequence_number).
    received_record_numbers: ArrayVec<(u64, u64), 32>,

    /// Deadline for sending a handshake ACK (epoch 2).
    /// Set when we detect gaps or partial flights during handshake.
    handshake_ack_deadline: Option<Instant>,

    /// When true, the next record must start a fresh datagram instead of
    /// appending to the current last buffer in queue_tx.
    datagram_sealed: bool,

    /// The records that have been sent in the current flight.
    flight_saved_records: ArrayVec<Entry, 12>,

    timers: HandshakeTimers,

    /// Whether we are ready to release application data from poll_output.
    release_app_data: bool,

    /// Exporter master secret for TLS 1.3 exporters (RFC 8446 Section 7.5).
    exporter_master_secret: Option<Buf>,

    /// Number of AEAD encryptions on the current application send keys.
    app_send_record_count: u64,

    /// Jittered threshold for the current key epoch. When `app_send_record_count`
    /// reaches this value, a KeyUpdate is triggered. Randomized to
    /// `[3/4 * configured_limit, configured_limit]` to prevent both sides from
    /// initiating KeyUpdate simultaneously when using the same config.
    aead_encryption_threshold: u64,

    /// Set when app_send_record_count reaches aead_encryption_threshold.
    needs_key_update: bool,

    /// A locally sent KeyUpdate is awaiting its ACK. Send keys are rotated only
    /// after that ACK arrives, so application data continues on the old epoch.
    key_update_in_flight: bool,

    /// Sequence number of the received close_notify alert, if any.
    /// Per RFC 9147 §5.10, any data with an epoch/sequence number pair
    /// after this must be discarded; earlier records are still valid.
    close_notify_sequence: Option<Sequence>,

    /// Whether [`Output::CloseNotify`] has already been emitted.
    close_notify_reported: bool,
}

struct EpochKeys {
    cipher: Box<dyn Cipher>,
    iv: [u8; 12],
    traffic_secret: Buf,
    sn_key: Buf,
}

struct RecvEpochEntry {
    epoch: u16,
    keys: EpochKeys,
    expected_recv_seq: u64,
    replay: ReplayWindow,
}

#[derive(Debug)]
struct Entry {
    content_type: ContentType,
    epoch: u16,
    send_seq: u64,
    fragment_len: usize,
    /// Payload followed by prior transmission sequence numbers (8 bytes each).
    fragment: Buf,
    acked: bool,
}

enum PollOutput<'a> {
    Data(&'a [u8]),
    BufferTooSmall { needed: usize },
    None(&'a mut [u8]),
}

impl Engine {
    pub fn new(config: Arc<Config>, certificate: DtlsCertificate) -> Self {
        let mut rng = SeededRng::new(config.rng_seed());

        let timers = HandshakeTimers::new(&config, &mut rng);

        let signing_key = config
            .crypto_provider()
            .key_provider
            .load_private_key(&certificate.private_key)
            .expect("Failed to load private key");

        let aead_encryption_threshold =
            jittered_aead_threshold(config.aead_encryption_limit(), &mut rng);
        let mut buffers_free = BufferPool::default();
        let transcript = buffers_free.pop();

        Self {
            config,
            certificate,
            rng,
            buffers_free,
            sequence_epoch_0: Sequence::new(0),
            queue_rx: QueueRx::new(),
            queue_tx: QueueTx::new(),
            cipher_suite: None,
            hs_send_keys: None,
            hs_recv_keys: None,
            hs_expected_recv_seq: 0,
            app_send_epoch: 3,
            hs_send_seq: 0,
            app_send_seq: 0,
            app_send_keys: None,
            prev_app_send_keys: None,
            prev_app_send_epoch: 0,
            prev_app_send_seq: 0,
            app_recv_keys: ArrayVec::new(),
            peer_encryption_enabled: false,
            signing_key,
            is_client: false,
            peer_handshake_seq_no: 0,
            next_handshake_seq_no: 0,
            transcript,
            hs_replay: ReplayWindow::new(),
            received_record_numbers: ArrayVec::new(),
            handshake_ack_deadline: None,
            datagram_sealed: false,
            flight_saved_records: ArrayVec::new(),
            timers,
            release_app_data: false,
            exporter_master_secret: None,
            app_send_record_count: 0,
            aead_encryption_threshold,
            needs_key_update: false,
            key_update_in_flight: false,
            close_notify_sequence: None,
            close_notify_reported: false,
        }
    }

    pub fn into_fallback(self) -> (Arc<Config>, DtlsCertificate, Timeout) {
        (
            self.config,
            self.certificate,
            self.timers.handshake_deadline(),
        )
    }

    pub fn set_client(&mut self, is_client: bool) {
        self.is_client = is_client;
    }

    pub fn start_handshake(&mut self, now: Instant) {
        self.timers.start_handshake(now);
    }

    /// Inject a pre-built hybrid ClientHello into this engine.
    ///
    /// Inject the transcript and state from a hybrid ClientHello that was
    /// already sent on the wire by [`ClientPending`].
    ///
    /// Sets the transcript, advances the handshake sequence number to 1,
    /// and bumps the epoch-0 record sequence so subsequent records don't
    /// collide.  Does **not** enqueue the record for output — the hybrid
    /// CH was already transmitted. Retains its outstanding retry state.
    pub fn inject_hybrid_client_hello(
        &mut self,
        transcript_bytes: &[u8],
        fragment: &[u8],
        timers: HandshakeTimers,
    ) {
        self.timers = timers;
        self.transcript.extend_from_slice(transcript_bytes);
        let mut saved_fragment = self.pop_buffer();
        saved_fragment.extend_from_slice(fragment);
        self.flight_saved_records.push(Entry {
            content_type: ContentType::Handshake,
            epoch: 0,
            send_seq: 0,
            fragment_len: fragment.len(),
            fragment: saved_fragment,
            acked: false,
        });
        self.next_handshake_seq_no = 1;
        // Advance past the record sequence used by the hybrid CH.
        // Defense-in-depth: guard against epoch-0 sequence overflow.
        if self.sequence_epoch_0.sequence_number < MAX_SEQUENCE_NUMBER {
            self.sequence_epoch_0.sequence_number += 1;
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn cipher_suite(&self) -> Option<Dtls13CipherSuite> {
        self.cipher_suite
    }

    pub fn set_cipher_suite(&mut self, cipher_suite: Dtls13CipherSuite) {
        self.cipher_suite = Some(cipher_suite);
    }

    pub fn app_send_epoch(&self) -> u16 {
        self.app_send_epoch
    }

    pub fn is_key_update_in_flight(&self) -> bool {
        self.key_update_in_flight
    }

    /// Returns true if the AEAD encryption limit has been reached and a
    /// KeyUpdate should be initiated. Clears the flag after returning true.
    pub fn needs_key_update(&mut self) -> bool {
        if self.needs_key_update {
            self.needs_key_update = false;
            true
        } else {
            false
        }
    }

    pub fn is_cipher_suite_allowed(&self, suite: Dtls13CipherSuite) -> bool {
        self.config
            .dtls13_cipher_suites()
            .any(|cs| cs.suite() == suite)
    }

    pub fn certificate_der(&self) -> &[u8] {
        &self.certificate.certificate
    }

    pub fn signing_key(&mut self) -> &mut dyn SigningKey {
        &mut *self.signing_key
    }

    /// Unconsumed records for protocol-state validation.
    pub fn pending_records(&self) -> impl Iterator<Item = &Record> {
        self.queue_rx
            .iter()
            .flat_map(|incoming| incoming.records().iter())
            .filter(|record| !record.is_handled())
    }

    /// Queue a datagram and report whether it contains the first ClientHello fragment.
    pub fn parse_packet(&mut self, packet: &[u8]) -> Result<bool, InternalError> {
        let cs = self.cipher_suite;
        let incoming = Incoming::parse_packet(packet, self, cs)?;
        if let Some(incoming) = incoming {
            // A reordered protected flight can be retransmitted once keys are ready.
            // A coalesced flight starts with its plaintext key-establishing records.
            if !self.peer_encryption_enabled
                && Dtls13Record::is_ciphertext_header(incoming.first().buffer()[0])
            {
                self.recycle_incoming(incoming);
                return Ok(false);
            }
            let is_client_hello = self.insert_incoming(incoming)?;
            return Ok(is_client_hello);
        }

        Ok(false)
    }

    /// Discard ciphertext still awaiting keys after processing the datagram.
    pub fn discard_undecryptable(&mut self) {
        if self.peer_encryption_enabled {
            return;
        }
        for incoming in self.queue_rx.iter() {
            for record in incoming.records().iter() {
                if Dtls13Record::is_ciphertext_header(record.buffer()[0]) {
                    record.set_handled();
                }
            }
        }
        self.purge_handled_queue_rx();
    }

    fn insert_incoming(&mut self, incoming: Incoming) -> Result<bool, Error> {
        if self.queue_rx.len() >= self.config.max_queue_rx() {
            warn!(
                "Receive queue full (max {}): {:?}",
                self.config.max_queue_rx(),
                self.queue_rx
            );
            self.recycle_incoming(incoming);
            return Err(Error::ReceiveQueueFull);
        }

        if incoming.first().first_handshake().is_some() {
            self.insert_incoming_handshake(incoming)
        } else {
            self.insert_incoming_non_handshake(incoming).map(|_| false)
        }
    }

    fn insert_incoming_handshake(&mut self, incoming: Incoming) -> Result<bool, Error> {
        let first_record = incoming.first();
        let handshake = first_record
            .first_handshake()
            .expect("caller ensures handshake");

        let key_current = (
            handshake.header.message_seq,
            handshake.header.fragment_offset,
        );

        let maybe_dupe_seq = incoming
            .records()
            .iter()
            .filter_map(|r| r.first_handshake())
            .filter_map(|h| h.dupe_triggers_resend())
            .next();

        if let Some(dupe_seq) = maybe_dupe_seq {
            // Only answer an already processed flight when we have a saved response.
            if dupe_seq < self.peer_handshake_seq_no && !self.flight_saved_records.is_empty() {
                // Reserve a resend and advance backoff using the same budget as
                // timed retries, so duplicates cannot bypass flight_retries.
                let resend_requested = self.timers.request_resend(&mut self.rng);
                if resend_requested && let Err(error) = self.flight_resend("dupe triggers resend") {
                    self.recycle_incoming(incoming);
                    return Err(error);
                }
            }
        }

        let is_session_ticket = self.is_client
            && handshake.header.msg_type == MessageType::NewSessionTicket
            && first_record.record().sequence.epoch >= 3;

        // Drop old duplicates we've already processed
        if handshake.header.message_seq < self.peer_handshake_seq_no {
            let result = self.ack_processed_post_handshake_records(&incoming);
            self.recycle_incoming(incoming);
            result?;
            return Ok(false);
        }

        // Reject new handshakes after initial handshake is complete,
        // but allow KeyUpdate and server-issued session tickets.
        if self.release_app_data
            && handshake.header.message_seq >= self.peer_handshake_seq_no
            && handshake.header.msg_type != MessageType::KeyUpdate
            && !is_session_ticket
        {
            self.recycle_incoming(incoming);
            return Err(Error::RenegotiationAttempt);
        }

        let is_client_hello = first_record.record().sequence.epoch == 0
            && handshake.header.msg_type == MessageType::ClientHello
            && handshake.header.message_seq == self.peer_handshake_seq_no;

        let search_result = self.queue_rx.binary_search_by(|item| {
            let key_other = item
                .first()
                .first_handshake()
                .as_ref()
                .map(|h| (h.header.message_seq, h.header.fragment_offset))
                .unwrap_or((u16::MAX, u32::MAX));
            key_other.cmp(&key_current)
        });

        match search_result {
            Err(index) => {
                // Track received record numbers for ACK generation
                for record in incoming.records().iter() {
                    let seq = record.record().sequence;
                    if seq.epoch >= 2 && record.record().content_type == ContentType::Handshake {
                        let _ = self
                            .received_record_numbers
                            .try_push((seq.epoch as u64, seq.sequence_number));
                    }
                }
                self.queue_rx.insert(index, incoming);
            }
            Ok(index) => {
                // Duplicate message_seq + fragment_offset. Replace if either:
                // (a) the existing entry was already consumed (handled), so a
                //     fresh retransmission (e.g., CH2 after HRR) can be processed.
                // (b) the existing entry looks corrupted: its total `length`
                //     differs from `fragment_length` while the new entry's match,
                //     indicating the retransmission corrected a bit-flip.
                let existing = &self.queue_rx[index];
                let should_replace = existing.first().is_handled() || {
                    let existing_corrupt = existing
                        .first()
                        .first_handshake()
                        .map(|h| h.header.length != h.header.fragment_length)
                        .unwrap_or(false);
                    let incoming_ok = incoming
                        .first()
                        .first_handshake()
                        .map(|h| h.header.length == h.header.fragment_length)
                        .unwrap_or(false);
                    existing_corrupt && incoming_ok
                };
                if should_replace {
                    for record in incoming.records().iter() {
                        let seq = record.record().sequence;
                        if seq.epoch >= 2 && record.record().content_type == ContentType::Handshake
                        {
                            let _ = self
                                .received_record_numbers
                                .try_push((seq.epoch as u64, seq.sequence_number));
                        }
                    }
                    let replaced = std::mem::replace(&mut self.queue_rx[index], incoming);
                    self.recycle_incoming(replaced);
                } else {
                    self.recycle_incoming(incoming);
                }
            }
        }

        Ok(is_client_hello)
    }

    fn insert_incoming_non_handshake(&mut self, incoming: Incoming) -> Result<(), Error> {
        let first = incoming.first();
        let seq_current = first.record().sequence;

        let search_result = self
            .queue_rx
            .binary_search_by_key(&seq_current, |item| item.first().record().sequence);

        match search_result {
            Err(index) => self.queue_rx.insert(index, incoming),
            Ok(_) => {
                // Duplicate - silently drop. For encrypted records (epoch >= 2) the replay
                // window filters most duplicates, but undecrypted ciphertext records can
                // reach here before enable_peer_encryption is called.
                self.recycle_incoming(incoming);
            }
        }

        Ok(())
    }

    fn recycle_incoming(&mut self, incoming: Incoming) {
        for record in incoming.into_records() {
            self.push_buffer(record.into_buffer());
        }
    }

    pub fn handle_timeout(&mut self, now: Instant) -> Result<(), Error> {
        if self
            .timers
            .handle_timeout(now, &mut self.rng)
            .map_err(Error::Timeout)?
        {
            self.flight_resend("flight timeout")?;
        }

        // During handshake, schedule/flush ACKs to help peer with selective retransmission
        self.maybe_schedule_handshake_ack(now);
        self.maybe_flush_handshake_ack(now)?;

        Ok(())
    }

    pub fn poll_output<'a>(&mut self, buf: &'a mut [u8], now: Instant) -> Output<'a> {
        self.purge_handled_queue_rx();

        let buf = match self.poll_app_data(buf) {
            PollOutput::Data(p) => return Output::ApplicationData(p),
            PollOutput::BufferTooSmall { needed } => return Output::BufferTooSmall { needed },
            PollOutput::None(b) => b,
        };

        self.maybe_schedule_handshake_ack(now);

        match self.poll_packet_tx(buf) {
            PollOutput::Data(p) => {
                if self.is_client {
                    self.timers.start_handshake(now);
                }
                self.timers.flight_sent(now);
                return Output::Packet(p);
            }
            PollOutput::BufferTooSmall { needed } => return Output::BufferTooSmall { needed },
            PollOutput::None(_) => {}
        }

        if self.close_notify_sequence.is_some() && !self.close_notify_reported {
            self.close_notify_reported = true;
            return Output::CloseNotify;
        }

        let next_timeout = self.poll_timeout(now);

        Output::Timeout(next_timeout)
    }

    fn poll_app_data<'a>(&mut self, buf: &'a mut [u8]) -> PollOutput<'a> {
        if !self.release_app_data {
            return PollOutput::None(buf);
        }

        let mut unhandled = self
            .queue_rx
            .iter()
            .flat_map(|i| i.records().iter())
            .filter(|r| r.record().content_type == ContentType::ApplicationData)
            .skip_while(|r| r.is_handled());

        let Some(next) = unhandled.next() else {
            return PollOutput::None(buf);
        };

        let record_buffer = next.buffer();
        let fragment = next.record().fragment(record_buffer);
        let len = fragment.len();

        if len > buf.len() {
            return PollOutput::BufferTooSmall { needed: len };
        }

        buf[..len].copy_from_slice(fragment);
        next.set_handled();

        PollOutput::Data(&buf[..len])
    }

    fn purge_handled_queue_rx(&mut self) {
        while let Some(peek) = self.queue_rx.front() {
            let fully_handled = peek.records().iter().all(|r| r.is_handled());

            if fully_handled {
                let incoming = self.queue_rx.pop_front().unwrap();
                incoming
                    .into_records()
                    .for_each(|r| self.buffers_free.push(r.into_buffer()));
            } else {
                break;
            }
        }
    }

    fn poll_packet_tx<'a>(&mut self, buf: &'a mut [u8]) -> PollOutput<'a> {
        let Some(p) = self.queue_tx.front() else {
            return PollOutput::None(buf);
        };

        if p.len() > buf.len() {
            return PollOutput::BufferTooSmall { needed: p.len() };
        }

        let p = self
            .queue_tx
            .pop_front()
            .expect("queue front checked above");
        let len = p.len();
        buf[..len].copy_from_slice(&p);

        PollOutput::Data(&buf[..len])
    }

    /// Prevent subsequent records from being appended to the current last
    /// datagram in queue_tx. The next record will start a fresh datagram.
    fn seal_current_datagram(&mut self) {
        self.datagram_sealed = true;
    }

    fn poll_timeout(&self, now: Instant) -> Instant {
        let mut timeout = self.timers.poll_timeout(now);

        if let Some(deadline) = self.handshake_ack_deadline
            && deadline < timeout
        {
            timeout = deadline;
        }

        timeout
    }

    pub fn flight_begin(&mut self, flight_no: u8) {
        debug!("Begin flight {}", flight_no);
        self.timers.begin_flight(&mut self.rng);
        self.flight_clear_resends();
    }

    pub fn flight_stop_resend_timers(&mut self) {
        debug!("Stop flight timeout");
        self.timers.stop_flight();
    }

    fn flight_clear_resends(&mut self) {
        for entry in self.flight_saved_records.drain(..) {
            self.buffers_free.push(entry.fragment);
        }
    }

    pub fn flight_resend(&mut self, reason: &str) -> Result<(), Error> {
        debug!("Resending flight due to {}", reason);
        // Check before moving or sending the flight so exhaustion retains every
        // saved record and its history, including records already ACKed.
        if self.flight_saved_records.iter().any(|entry| {
            !entry.acked
                && entry.epoch >= 2
                && entry.content_type == ContentType::Handshake
                && entry.fragment.len() - entry.fragment_len >= MAX_PREVIOUS_TRANSMISSIONS * 8
        }) {
            return Err(Error::TransmitHistoryFull);
        }
        let mut records = mem::take(&mut self.flight_saved_records);

        // Mark the current last datagram as "sealed" so resent records
        // go into fresh datagrams. Without this, can_append would pack
        // resent records into the same datagram as the original flight,
        // which causes duplicate handshake fragments at the receiver.
        self.seal_current_datagram();

        for entry in &mut records {
            // Selective retransmission: skip records that have been ACKed
            if entry.acked {
                continue;
            }

            if entry.epoch == 0 {
                // Capture the sequence number the retransmitted record will use
                let new_seq = self.sequence_epoch_0.sequence_number;
                self.create_plaintext_record(entry.content_type, false, |fragment| {
                    fragment.extend_from_slice(entry.fragment());
                })?;
                entry.send_seq = new_seq;
            } else {
                // Capture the sequence number the retransmitted record will use
                let new_seq = if entry.epoch == 2 {
                    self.hs_send_seq
                } else if self.prev_app_send_keys.is_some()
                    && entry.epoch == self.prev_app_send_epoch
                {
                    self.prev_app_send_seq
                } else {
                    self.app_send_seq
                };
                self.create_ciphertext_record(
                    entry.content_type,
                    entry.epoch,
                    false,
                    |fragment| {
                        fragment.extend_from_slice(entry.fragment());
                    },
                )?;
                // ACKs may name any transmission of a protected handshake
                // record (RFC 9147 section 7.2). ACK records themselves are
                // never ACKed and can be resent without retaining history.
                if entry.content_type == ContentType::Handshake {
                    entry
                        .fragment
                        .extend_from_slice(&entry.send_seq.to_be_bytes());
                }
                entry.send_seq = new_seq;
            }
        }

        self.flight_saved_records = records;

        Ok(())
    }

    pub fn has_complete_handshake(&mut self, wanted: MessageType) -> bool {
        self.has_complete_handshake_with_seq(wanted, self.peer_handshake_seq_no)
    }

    fn has_complete_handshake_with_seq(&mut self, wanted: MessageType, expected_seq: u16) -> bool {
        let mut skip_handled = self
            .queue_rx
            .iter()
            .flat_map(|i| i.records().iter())
            .skip_while(|r| r.is_handled())
            .take(MAX_DEFRAGMENT_PACKETS)
            .flat_map(|r| r.handshakes().iter())
            .skip_while(|h| h.is_handled())
            .peekable();

        let maybe_first_handshake = skip_handled.peek();

        let Some(first) = maybe_first_handshake else {
            return false;
        };

        if first.header.message_seq != expected_seq {
            return false;
        }

        if first.header.msg_type != wanted {
            return false;
        }

        let wanted_seq = first.header.message_seq;
        let wanted_length = first.header.length;
        let mut last_fragment_end = 0;

        for h in skip_handled {
            if wanted_seq != h.header.message_seq {
                continue;
            }

            // Overlap-tolerant: only reject if there's an actual gap
            if h.header.fragment_offset > last_fragment_end {
                return false;
            }
            let end = h.header.fragment_offset + h.header.fragment_length;
            if end > last_fragment_end {
                last_fragment_end = end;
            }

            if last_fragment_end == wanted_length {
                return true;
            }
        }

        false
    }

    pub fn next_handshake(
        &mut self,
        wanted: MessageType,
        defragment_buffer: &mut Buf,
    ) -> Result<Option<Handshake>, InternalError> {
        self.next_handshake_with_options(wanted, defragment_buffer, false)
    }

    pub(crate) fn next_client_hello_for_auto_sense(
        &mut self,
        defragment_buffer: &mut Buf,
    ) -> Result<Option<Handshake>, InternalError> {
        self.next_handshake_with_options(MessageType::ClientHello, defragment_buffer, true)
    }

    fn next_handshake_with_options(
        &mut self,
        wanted: MessageType,
        defragment_buffer: &mut Buf,
        allow_unknown_client_hello_suites: bool,
    ) -> Result<Option<Handshake>, InternalError> {
        if !self.has_complete_handshake(wanted) {
            return Ok(None);
        }

        let iter = self
            .queue_rx
            .iter()
            .flat_map(|i| i.records().iter())
            .skip_while(|r| r.is_handled())
            .flat_map(|r| r.handshakes().iter().map(move |h| (h, r.buffer())))
            .skip_while(|(h, _)| h.is_handled());

        let handshake = if allow_unknown_client_hello_suites {
            Handshake::defragment_allow_unknown_client_hello_suites(
                iter,
                defragment_buffer,
                self.cipher_suite,
                Some(&mut self.transcript),
            )
        } else {
            Handshake::defragment(
                iter,
                defragment_buffer,
                self.cipher_suite,
                Some(&mut self.transcript),
            )
        }?;

        Ok(Some(handshake))
    }

    /// Like `next_handshake` but does NOT update the transcript hash.
    /// Used for post-handshake messages like KeyUpdate.
    pub fn next_handshake_no_transcript(
        &mut self,
        wanted: MessageType,
        defragment_buffer: &mut Buf,
    ) -> Result<Option<Handshake>, InternalError> {
        if !self.has_complete_handshake(wanted) {
            return Ok(None);
        }

        let iter = self
            .queue_rx
            .iter()
            .flat_map(|i| i.records().iter())
            .skip_while(|r| r.is_handled())
            .flat_map(|r| r.handshakes().iter().map(move |h| (h, r.buffer())))
            .skip_while(|(h, _)| h.is_handled());

        let handshake = Handshake::defragment(
            iter,
            defragment_buffer,
            self.cipher_suite,
            None, // no transcript update
        )?;

        Ok(Some(handshake))
    }

    /// Advance the expected peer handshake sequence number.
    ///
    /// Must be called by the caller of `next_handshake` / `next_handshake_no_transcript`
    /// AFTER all validation of the handshake body succeeds. This prevents stale
    /// retransmissions from advancing the counter before validation can reject them.
    pub fn advance_peer_handshake_seq(&mut self) {
        self.peer_handshake_seq_no += 1;
    }

    /// Create a DTLSPlaintext record (epoch 0, unencrypted).
    pub fn create_plaintext_record<F>(
        &mut self,
        content_type: ContentType,
        save_fragment: bool,
        f: F,
    ) -> Result<(), Error>
    where
        F: FnOnce(&mut Buf),
    {
        let mut fragment = self.buffers_free.pop();
        f(&mut fragment);

        let current_seq = self.sequence_epoch_0.sequence_number;

        if save_fragment {
            let mut clone = self.buffers_free.pop();
            clone.extend_from_slice(&fragment);
            self.flight_saved_records.push(Entry {
                content_type,
                epoch: 0,
                send_seq: current_seq,
                fragment_len: fragment.len(),
                fragment: clone,
                acked: false,
            });
        }

        let record_wire_len = Dtls13Record::PLAINTEXT_HEADER_LEN + fragment.len();

        let can_append = !self.datagram_sealed
            && self
                .queue_tx
                .back()
                .map(|b| b.len() + record_wire_len <= self.config.mtu())
                .unwrap_or(false);

        if !can_append && self.queue_tx.len() >= self.config.max_queue_tx() {
            warn!(
                "Transmit queue full (max {}): {:?}",
                self.config.max_queue_tx(),
                self.queue_tx
            );
            return Err(Error::TransmitQueueFull);
        }

        let sequence = self.sequence_epoch_0;

        let record = Dtls13Record {
            content_type,
            sequence,
            length: fragment.len() as u16,
            fragment_range: 0..fragment.len(),
        };

        if self.sequence_epoch_0.sequence_number >= MAX_SEQUENCE_NUMBER {
            return Err(Error::CryptoError(
                crate::CryptoError::Epoch0SequenceNumberExhausted,
            ));
        }
        self.sequence_epoch_0.sequence_number += 1;

        if can_append {
            let last = self.queue_tx.back_mut().unwrap();
            record.serialize(&fragment, last);
        } else {
            self.datagram_sealed = false;
            let mut buffer = self.buffers_free.pop();
            buffer.clear();
            record.serialize(&fragment, &mut buffer);
            self.queue_tx.push_back(buffer);
        }

        self.buffers_free.push(fragment);

        Ok(())
    }

    /// Create a DTLSCiphertext record (epoch >= 2, encrypted).
    ///
    /// The plaintext fragment is wrapped as DTLSInnerPlaintext:
    /// `content || content_type(1) || zeros*` before AEAD encryption.
    pub fn create_ciphertext_record<F>(
        &mut self,
        content_type: ContentType,
        epoch: u16,
        save_fragment: bool,
        f: F,
    ) -> Result<(), Error>
    where
        F: FnOnce(&mut Buf),
    {
        let mut fragment = self.buffers_free.pop();
        f(&mut fragment);

        // Determine sequence number for this record
        let seq = if epoch == 2 {
            self.hs_send_seq
        } else if self.prev_app_send_keys.is_some() && epoch == self.prev_app_send_epoch {
            self.prev_app_send_seq
        } else {
            self.app_send_seq
        };

        if save_fragment {
            let mut clone = self.buffers_free.pop();
            clone.extend_from_slice(&fragment);
            self.flight_saved_records.push(Entry {
                content_type,
                epoch,
                send_seq: seq,
                fragment_len: fragment.len(),
                fragment: clone,
                acked: false,
            });
        }

        // Build DTLSInnerPlaintext: content || content_type(1)
        // (no zero padding for now)
        fragment.push(content_type.as_u8());

        let suite = self.suite_provider();
        let tag_len = suite.tag_len();

        // Get the send keys for this epoch
        let keys = if epoch == 2 {
            self.hs_send_keys.as_mut()
        } else if self.prev_app_send_keys.is_some() && epoch == self.prev_app_send_epoch {
            self.prev_app_send_keys.as_mut()
        } else {
            self.app_send_keys.as_mut()
        };

        let Some(keys) = keys else {
            return Err(Error::CryptoError(
                crate::CryptoError::SendKeysNotAvailable { epoch },
            ));
        };

        // Construct the nonce: iv XOR padded_seq
        let nonce = Nonce::xor(&keys.iv, seq);

        // Build the unified header for AAD
        // Always use S=1 (2-byte seq) and L=1 (length present)
        let epoch_bits = (epoch & 0x03) as u8;
        let flags: u8 = 0b0010_0000
            | 0b0000_1000 // S=1
            | 0b0000_0100 // L=1
            | epoch_bits;

        let ciphertext_len = fragment.len() + tag_len;

        let mut header_buf = [0u8; 5];
        header_buf[0] = flags;
        header_buf[1..3].copy_from_slice(&(seq as u16).to_be_bytes());
        header_buf[3..5].copy_from_slice(&(ciphertext_len as u16).to_be_bytes());

        let aad = Aad::new_dtls13(&header_buf);

        // Save sn_key before losing mutable borrow of keys
        let mut sn_key = [0u8; 32];
        let sn_key_len = keys.sn_key.len();
        sn_key[..sn_key_len].copy_from_slice(&keys.sn_key);

        // Encrypt in place (appends tag)
        keys.cipher
            .encrypt(&mut fragment, aad, nonce)
            .map_err(Error::CryptoError)?;

        // Record number encryption (RFC 9147 Section 4.2.3):
        // mask = AES-ECB(sn_key, ciphertext_sample)
        // XOR mask[0..2] over the sequence number bytes in the header
        let sn_mask = if fragment.len() >= 16 {
            // unwrap: we checked length >= 16
            let ciphertext_sample: [u8; 16] = fragment[..16].try_into().unwrap();
            suite.encrypt_sn(&sn_key[..sn_key_len], &ciphertext_sample)
        } else {
            [0u8; 16] // degenerate case: no masking if ciphertext too short
        };

        // Unified header: flags(1) + seq(2) + length(2) = 5 bytes
        let record_wire_len = 5 + fragment.len();

        let can_append = !self.datagram_sealed
            && self
                .queue_tx
                .back()
                .map(|b| b.len() + record_wire_len <= self.config.mtu())
                .unwrap_or(false);

        if !can_append && self.queue_tx.len() >= self.config.max_queue_tx() {
            warn!(
                "Transmit queue full (max {}): {:?}",
                self.config.max_queue_tx(),
                self.queue_tx
            );
            return Err(Error::TransmitQueueFull);
        }

        // Build the record for serialization
        let record = Dtls13Record {
            content_type: ContentType::ApplicationData,
            sequence: Sequence {
                epoch,
                sequence_number: seq,
            },
            length: fragment.len() as u16,
            fragment_range: 0..fragment.len(),
        };

        // Increment send sequence, guarding against 48-bit overflow (RFC 9147 §4.2)
        if epoch == 2 {
            if self.hs_send_seq >= MAX_SEQUENCE_NUMBER {
                return Err(Error::CryptoError(
                    crate::CryptoError::SendSequenceNumberExhausted { epoch },
                ));
            }
            self.hs_send_seq += 1;
        } else if self.prev_app_send_keys.is_some() && epoch == self.prev_app_send_epoch {
            if self.prev_app_send_seq >= MAX_SEQUENCE_NUMBER {
                return Err(Error::CryptoError(
                    crate::CryptoError::SendSequenceNumberExhausted { epoch },
                ));
            }
            self.prev_app_send_seq += 1;
        } else {
            if self.app_send_seq >= MAX_SEQUENCE_NUMBER {
                return Err(Error::CryptoError(
                    crate::CryptoError::SendSequenceNumberExhausted { epoch },
                ));
            }
            self.app_send_seq += 1;

            // Track AEAD encryptions on current application keys
            if epoch >= 3 {
                self.app_send_record_count += 1;
                if self.app_send_record_count >= self.aead_encryption_threshold {
                    self.needs_key_update = true;
                }
            }
        }

        if can_append {
            let last = self.queue_tx.back_mut().unwrap();
            let header_start = last.len();
            record.serialize(&fragment, last);
            // Apply record number encryption: XOR mask over seq bytes
            // Seq bytes are at offset header_start+1..header_start+3
            last[header_start + 1] ^= sn_mask[0];
            last[header_start + 2] ^= sn_mask[1];
        } else {
            self.datagram_sealed = false;
            let mut buffer = self.buffers_free.pop();
            buffer.clear();
            record.serialize(&fragment, &mut buffer);
            // Apply record number encryption: XOR mask over seq bytes
            buffer[1] ^= sn_mask[0];
            buffer[2] ^= sn_mask[1];
            self.queue_tx.push_back(buffer);
        }

        self.buffers_free.push(fragment);

        Ok(())
    }

    /// Create a handshake message and wrap it in a DTLS record.
    pub fn create_handshake<F>(&mut self, msg_type: MessageType, f: F) -> Result<(), Error>
    where
        F: FnOnce(&mut Buf, &mut Self) -> Result<(), Error>,
    {
        let mut body_buffer = self.buffers_free.pop();

        f(&mut body_buffer, self)?;

        let handshake_header = Header {
            msg_type,
            length: body_buffer.len() as u32,
            message_seq: self.next_handshake_seq_no,
            fragment_offset: 0,
            fragment_length: body_buffer.len() as u32,
        };

        // Write TLS 1.3 transcript: msg_type(1) + length(3) + body (no DTLS framing)
        self.transcript.push(msg_type.as_u8());
        self.transcript
            .extend_from_slice(&handshake_header.length.to_be_bytes()[1..]);
        self.transcript
            .extend_from_slice(&body_buffer[..handshake_header.length as usize]);

        self.next_handshake_seq_no += 1;

        let epoch = epoch_for_message(msg_type);
        let total_len = body_buffer.len();
        let mut offset: usize = 0;

        let handshake_header_len = 12usize;
        let tag_len = if epoch >= 2 {
            self.suite_provider().tag_len()
        } else {
            0
        };
        // Per-record protection overhead on the wire: tag + 1 byte inner
        // content type (the DTLS 1.3 record protection layer's expansion).
        let protection_overhead = if epoch >= 2 { tag_len + 1 } else { 0 };

        while offset < total_len || (total_len == 0 && offset == 0) {
            let already_used_in_current = self.queue_tx.back().map(|b| b.len()).unwrap_or(0);
            let available_in_current = self.config.mtu().saturating_sub(already_used_in_current);

            let record_header_len = if epoch == 0 {
                Dtls13Record::PLAINTEXT_HEADER_LEN
            } else {
                5 // unified header: flags(1) + seq(2) + length(2)
            };

            let fixed_overhead = record_header_len + handshake_header_len + protection_overhead;

            let available_for_body = if available_in_current > fixed_overhead {
                available_in_current - fixed_overhead
            } else {
                self.config.mtu().saturating_sub(fixed_overhead)
            };

            let remaining_body_bytes = total_len.saturating_sub(offset);

            let chunk_len = if total_len == 0 {
                0
            } else {
                remaining_body_bytes.min(available_for_body)
            };

            let frag_range = if chunk_len == 0 {
                0..0
            } else {
                offset..offset + chunk_len
            };

            let frag_handshake = Handshake {
                header: Header {
                    msg_type,
                    length: handshake_header.length,
                    message_seq: handshake_header.message_seq,
                    fragment_offset: offset as u32,
                    fragment_length: chunk_len as u32,
                },
                body: Body::Fragment(frag_range),
                handled: AtomicBool::new(false),
            };

            if epoch == 0 {
                self.create_plaintext_record(ContentType::Handshake, true, |fragment| {
                    frag_handshake.serialize(&body_buffer, fragment);
                })?;
            } else {
                self.create_ciphertext_record(ContentType::Handshake, epoch, true, |fragment| {
                    frag_handshake.serialize(&body_buffer, fragment);
                })?;
            }

            if total_len == 0 {
                break;
            }

            offset += chunk_len;
        }

        self.buffers_free.push(body_buffer);

        Ok(())
    }

    /// Release application data from the incoming queue.
    ///
    /// Also clears handshake receive keys to prevent epoch collision:
    /// after 3+ KeyUpdates the app recv epoch's low 2 bits can match
    /// the handshake epoch (2), causing records to be routed to stale
    /// handshake keys instead of the correct application keys.
    pub fn release_application_data(&mut self) {
        self.release_app_data = true;
        self.hs_recv_keys = None;
        self.timers.finish_handshake();
    }

    pub fn release_application_data_retaining_handshake_keys(&mut self) {
        self.release_app_data = true;
        self.timers.finish_handshake();
    }

    /// Whether a close_notify alert has been received from the peer.
    pub fn close_notify_received(&self) -> bool {
        self.close_notify_sequence.is_some()
    }

    /// Whether a received close_notify still needs to be surfaced.
    pub fn close_notify_pending(&self) -> bool {
        self.close_notify_sequence.is_some() && !self.close_notify_reported
    }

    /// Whether close-related output remains to be polled.
    pub fn has_pending_close_output(&self) -> bool {
        !self.queue_tx.is_empty() || self.close_notify_pending()
    }

    /// Cancel in-flight retransmissions without clearing the transmit queue.
    /// Used by close() to stop retransmitting control records while still
    /// allowing the queued close_notify alert to be sent.
    pub fn cancel_flights(&mut self) {
        self.flight_saved_records.clear();
        self.timers.stop();
        self.handshake_ack_deadline = None;
    }

    /// Abort the connection: flush all queued output, retransmission state, and
    /// disable timers so that no further packets are emitted.
    pub fn abort(&mut self) {
        self.queue_tx.clear();
        self.flight_saved_records.clear();
        self.timers.stop();
        self.handshake_ack_deadline = None;
    }

    fn ack_processed_post_handshake_records(&mut self, incoming: &Incoming) -> Result<(), Error> {
        // RFC 9147 section 7: ACK post-handshake retransmissions too, since
        // the original ACK may have been lost. Do not process the update twice.
        let mut needs_ack = false;
        for record in incoming.records().iter() {
            let seq = record.record().sequence;
            let is_post_handshake_record =
                seq.epoch >= 3 && record.record().content_type == ContentType::Handshake;
            // The datagram may also contain fresh messages. Do not ACK
            // those when discarding it, so the peer can retransmit them.
            let already_processed = record
                .handshakes()
                .iter()
                .all(|h| h.header.message_seq < self.peer_handshake_seq_no);
            if is_post_handshake_record && already_processed {
                let recorded = self
                    .received_record_numbers
                    .try_push((seq.epoch as u64, seq.sequence_number))
                    .is_ok();
                needs_ack |= recorded;
            }
        }
        if needs_ack {
            self.send_ack()?;
        }
        Ok(())
    }

    /// Send an ACK record listing received handshake record numbers.
    ///
    /// ACK format: record_numbers_length(2) + N * (epoch(8) + sequence(8))
    pub fn send_ack(&mut self) -> Result<(), Error> {
        self.send_ack_inner(false)
    }

    pub fn send_ack_retransmittable(&mut self) -> Result<(), Error> {
        if !self.received_record_numbers.is_empty() {
            self.flight_clear_resends();
        }
        self.send_ack_inner(true)
    }

    fn send_ack_inner(&mut self, save_fragment: bool) -> Result<(), Error> {
        if self.received_record_numbers.is_empty() {
            return Ok(());
        }

        let entries = mem::take(&mut self.received_record_numbers);
        let epoch = if self.app_send_keys.is_some() {
            self.app_send_epoch
        } else {
            2
        };

        self.create_ciphertext_record(ContentType::Ack, epoch, save_fragment, |fragment| {
            // record_numbers_length: 2 bytes, value = entries.len() * 16
            let len = (entries.len() * 16) as u16;
            fragment.extend_from_slice(&len.to_be_bytes());
            for &(ep, seq) in &entries {
                fragment.extend_from_slice(&ep.to_be_bytes());
                fragment.extend_from_slice(&seq.to_be_bytes());
            }
        })?;

        Ok(())
    }

    /// Process an incoming ACK record, marking acknowledged flight entries.
    fn process_ack(&mut self, ack_data: &[u8]) -> Result<(), Error> {
        if ack_data.len() < 2 {
            return Ok(());
        }

        let record_numbers_len = u16::from_be_bytes([ack_data[0], ack_data[1]]) as usize;
        let entries_data = &ack_data[2..];

        if entries_data.len() != record_numbers_len || !record_numbers_len.is_multiple_of(16) {
            return Ok(());
        }

        let num_entries = record_numbers_len / 16;
        for i in 0..num_entries {
            let offset = i * 16;
            if offset + 16 > entries_data.len() {
                break;
            }
            let ack_epoch = u64::from_be_bytes(
                // unwrap: bounds checked above
                entries_data[offset..offset + 8].try_into().unwrap(),
            );
            let ack_seq = u64::from_be_bytes(
                // unwrap: bounds checked above
                entries_data[offset + 8..offset + 16].try_into().unwrap(),
            );

            // Mark matching flight entries as acknowledged
            for entry in &mut self.flight_saved_records {
                if entry.epoch as u64 == ack_epoch && entry.was_sent_as(ack_seq) {
                    entry.acked = true;
                }
            }
        }

        // If all epoch-2 records in the flight are ACKed, stop retransmitting.
        // This happens when the peer ACKs our handshake flight.
        let has_epoch2 = self.flight_saved_records.iter().any(|e| e.epoch == 2);
        let all_epoch2_acked = self
            .flight_saved_records
            .iter()
            .filter(|e| e.epoch == 2)
            .all(|e| e.acked);
        if has_epoch2 && all_epoch2_acked {
            debug!("Handshake flight ACKed; stopping retransmission");
            self.timers.stop_flight();
            self.flight_clear_resends();
        }

        // Rotate send keys only after the peer ACKs our KeyUpdate. Until then,
        // application data continues on the old/current application epoch.
        if self.key_update_in_flight
            && !self.flight_saved_records.is_empty()
            && self.flight_saved_records.iter().all(|e| e.acked)
        {
            debug!("KeyUpdate ACKed; rotating send keys");
            self.update_send_keys()?;
            self.prev_app_send_keys = None;
            self.key_update_in_flight = false;
            self.flight_clear_resends();
            self.timers.stop_flight();
        }

        Ok(())
    }

    // =========================================================================
    // Handshake ACK Scheduling (RFC 9147 Section 7)
    // =========================================================================

    /// Whether we're in the DTLS 1.3 handshake phase where ACK scheduling applies.
    ///
    /// True when handshake send keys are installed but application keys are not yet.
    /// This corresponds to the client receiving the server's encrypted flight.
    fn handshake_in_progress(&self) -> bool {
        self.hs_send_keys.is_some() && self.app_send_keys.is_none() && !self.release_app_data
    }

    /// Check if the incoming handshake queue needs help (missing fragments/messages).
    fn handshake_ack_help_needed(&self) -> bool {
        let mut skip_handled = self
            .queue_rx
            .iter()
            .flat_map(|i| i.records().iter())
            .skip_while(|r| r.is_handled())
            .take(MAX_DEFRAGMENT_PACKETS)
            .flat_map(|r| r.handshakes().iter())
            .skip_while(|h| h.is_handled())
            .peekable();

        let Some(first) = skip_handled.peek() else {
            return false;
        };

        // If we're not seeing the expected message_seq, we're missing earlier fragments.
        if first.header.message_seq != self.peer_handshake_seq_no {
            return true;
        }

        // Check if the current message is complete
        let wanted_seq = first.header.message_seq;
        let wanted_length = first.header.length;
        let mut last_fragment_end = 0;

        for h in skip_handled {
            if wanted_seq != h.header.message_seq {
                continue;
            }
            // Overlap-tolerant: only flag help needed if there's an actual gap
            if h.header.fragment_offset > last_fragment_end {
                return true;
            }
            let end = h.header.fragment_offset + h.header.fragment_length;
            if end > last_fragment_end {
                last_fragment_end = end;
            }
            if last_fragment_end == wanted_length {
                return false;
            }
        }

        true
    }

    /// Check if there's a gap in the incoming handshake data that requires
    /// an immediate ACK to help the peer retransmit.
    fn has_gap_in_incoming_handshake(&self) -> bool {
        let mut skip_handled = self
            .queue_rx
            .iter()
            .flat_map(|i| i.records().iter())
            .skip_while(|r| r.is_handled())
            .take(MAX_DEFRAGMENT_PACKETS)
            .flat_map(|r| r.handshakes().iter())
            .skip_while(|h| h.is_handled())
            .peekable();

        let Some(first) = skip_handled.peek() else {
            return false;
        };

        // Gap: not seeing the expected message_seq
        if first.header.message_seq != self.peer_handshake_seq_no {
            return true;
        }

        // Check for fragment gaps within the expected message
        let wanted_seq = first.header.message_seq;
        let wanted_length = first.header.length;
        let mut last_fragment_end = 0;

        for h in skip_handled {
            if wanted_seq != h.header.message_seq {
                continue;
            }
            // Overlap-tolerant: only flag a gap if there's an actual gap
            if h.header.fragment_offset > last_fragment_end {
                return true;
            }
            let end = h.header.fragment_offset + h.header.fragment_length;
            if end > last_fragment_end {
                last_fragment_end = end;
            }
            if last_fragment_end == wanted_length {
                return false;
            }
        }

        false
    }

    /// Schedule a handshake ACK if we detect gaps or partial flights.
    fn maybe_schedule_handshake_ack(&mut self, now: Instant) {
        if !self.handshake_in_progress() {
            self.handshake_ack_deadline = None;
            return;
        }

        if self.handshake_ack_deadline.is_some() {
            return;
        }

        if !self.handshake_ack_help_needed() {
            return;
        }

        // If we detect a gap (missing fragments/messages), send ACK immediately.
        // Otherwise, use RTO/4 delay to allow piggybacking on the next flight.
        let delay = if self.has_gap_in_incoming_handshake() {
            Duration::from_millis(0)
        } else {
            self.timers.rto() / 4
        };

        self.handshake_ack_deadline = Some(deadline(now, delay));
    }

    /// Flush a scheduled handshake ACK if the deadline has passed.
    fn maybe_flush_handshake_ack(&mut self, now: Instant) -> Result<(), Error> {
        let Some(deadline) = self.handshake_ack_deadline else {
            return Ok(());
        };

        if now < deadline {
            return Ok(());
        }

        // Collect record numbers for epoch-2 handshake records we have received.
        let mut record_numbers = ArrayVec::<(u64, u64), 32>::new();
        for incoming in self.queue_rx.iter() {
            for r in incoming.records().iter() {
                if r.record().sequence.epoch == 2
                    && r.record().content_type == ContentType::Handshake
                {
                    let seq = r.record().sequence;
                    let _ = record_numbers.try_push((seq.epoch as u64, seq.sequence_number));
                }
            }
        }

        self.handshake_ack_deadline = None;

        if record_numbers.is_empty() {
            return Ok(());
        }

        self.send_handshake_ack_epoch2(&record_numbers)
    }

    /// Send a handshake ACK on epoch 2 listing received epoch-2 record numbers.
    fn send_handshake_ack_epoch2(&mut self, record_numbers: &[(u64, u64)]) -> Result<(), Error> {
        if !self.handshake_in_progress() {
            return Ok(());
        }

        self.create_ciphertext_record(ContentType::Ack, 2, false, |fragment| {
            let len = (record_numbers.len() * 16) as u16;
            fragment.extend_from_slice(&len.to_be_bytes());
            for &(epoch, seq) in record_numbers {
                fragment.extend_from_slice(&epoch.to_be_bytes());
                fragment.extend_from_slice(&seq.to_be_bytes());
            }
        })?;

        Ok(())
    }

    /// Pop a buffer from the buffer pool for temporary use
    pub(crate) fn pop_buffer(&mut self) -> Buf {
        self.buffers_free.pop()
    }

    /// Return a buffer to the buffer pool
    pub(crate) fn push_buffer(&mut self, buf: Buf) {
        self.buffers_free.push(buf);
    }

    // =========================================================================
    // Key Schedule
    // =========================================================================

    fn hmac(&self) -> &dyn HmacProvider {
        self.config.crypto_provider().hmac_provider
    }

    fn hash_algorithm(&self) -> HashAlgorithm {
        // unwrap: cipher_suite must be set before key schedule operations
        self.cipher_suite.unwrap().hash_algorithm()
    }

    fn suite_provider(&self) -> &'static dyn SupportedDtls13CipherSuite {
        let suite = self.cipher_suite.unwrap();
        *self
            .config
            .crypto_provider()
            .dtls13_cipher_suites
            .iter()
            .find(|cs| cs.suite() == suite)
            .expect("cipher suite not found in provider")
    }

    /// Derive the early secret: HKDF-Extract(0, 0) [no PSK support]
    pub fn derive_early_secret(&mut self) -> Result<Buf, Error> {
        let hash = self.hash_algorithm();
        let hash_len = hash.output_len();
        let zeros = [0u8; 48];
        let zeros = &zeros[..hash_len];
        let mut early_secret = self.buffers_free.pop();
        prf_hkdf::hkdf_extract(self.hmac(), hash, zeros, zeros, &mut early_secret)
            .map_err(Error::CryptoError)?;
        Ok(early_secret)
    }

    /// Derive handshake secrets from ECDHE shared secret + transcript hash through ServerHello.
    ///
    /// Returns (client_handshake_traffic_secret, server_handshake_traffic_secret, handshake_secret)
    pub fn derive_handshake_secrets(
        &mut self,
        shared_secret: &[u8],
    ) -> Result<(Buf, Buf, Buf), Error> {
        // Call derive_early_secret first (needs &mut self) before borrowing hmac
        let early_secret = self.derive_early_secret()?;
        let mut handshake_secret = self.buffers_free.pop();
        let mut c_hs_traffic = self.buffers_free.pop();
        let mut s_hs_traffic = self.buffers_free.pop();

        let hash = self.hash_algorithm();
        let hash_len = hash.output_len();
        let hmac = self.hmac();

        // Derive-Secret(early_secret, "derived", "")
        let empty_hash = self.transcript_hash_of(b"");
        let mut derived = Buf::new();
        prf_hkdf::hkdf_expand_label_dtls13(
            hmac,
            hash,
            &early_secret,
            b"derived",
            &empty_hash,
            &mut derived,
            hash_len,
        )
        .map_err(Error::CryptoError)?;

        // handshake_secret = HKDF-Extract(derived, shared_secret)
        prf_hkdf::hkdf_extract(hmac, hash, &derived, shared_secret, &mut handshake_secret)
            .map_err(Error::CryptoError)?;

        // Get transcript hash up to and including ServerHello
        let mut transcript_hash = Buf::new();
        self.transcript_hash(&mut transcript_hash);

        // client_handshake_traffic_secret
        prf_hkdf::hkdf_expand_label_dtls13(
            hmac,
            hash,
            &handshake_secret,
            b"c hs traffic",
            &transcript_hash,
            &mut c_hs_traffic,
            hash_len,
        )
        .map_err(Error::CryptoError)?;

        // server_handshake_traffic_secret
        prf_hkdf::hkdf_expand_label_dtls13(
            hmac,
            hash,
            &handshake_secret,
            b"s hs traffic",
            &transcript_hash,
            &mut s_hs_traffic,
            hash_len,
        )
        .map_err(Error::CryptoError)?;

        self.buffers_free.push(early_secret);
        Ok((c_hs_traffic, s_hs_traffic, handshake_secret))
    }

    /// Install handshake keys (epoch 2) from the traffic secrets.
    pub fn install_handshake_keys(
        &mut self,
        client_traffic_secret: &Buf,
        server_traffic_secret: &Buf,
    ) -> Result<(), Error> {
        let (send_secret, recv_secret) = if self.is_client {
            (client_traffic_secret, server_traffic_secret)
        } else {
            (server_traffic_secret, client_traffic_secret)
        };

        self.hs_send_keys = Some(self.derive_epoch_keys(send_secret)?);
        self.hs_recv_keys = Some(self.derive_epoch_keys(recv_secret)?);

        // Reset send sequence for epoch 2
        self.hs_send_seq = 0;

        Ok(())
    }

    /// Derive application secrets from transcript hash through server Finished.
    ///
    /// Returns (client_app_traffic_secret, server_app_traffic_secret)
    pub fn derive_application_secrets(
        &mut self,
        handshake_secret: &[u8],
    ) -> Result<(Buf, Buf), Error> {
        let mut exp_master = self.buffers_free.pop();
        let mut c_ap_traffic = self.buffers_free.pop();
        let mut s_ap_traffic = self.buffers_free.pop();
        let hash = self.hash_algorithm();
        let hash_len = hash.output_len();
        let hmac = self.hmac();

        // Derive-Secret(handshake_secret, "derived", "")
        let empty_hash = self.transcript_hash_of(b"");
        let mut derived = Buf::new();
        prf_hkdf::hkdf_expand_label_dtls13(
            hmac,
            hash,
            handshake_secret,
            b"derived",
            &empty_hash,
            &mut derived,
            hash_len,
        )
        .map_err(Error::CryptoError)?;

        // master_secret = HKDF-Extract(derived, 0)
        let zeros = [0u8; 48];
        let zeros = &zeros[..hash_len];
        let mut master_secret = Buf::new();
        prf_hkdf::hkdf_extract(hmac, hash, &derived, zeros, &mut master_secret)
            .map_err(Error::CryptoError)?;

        // Get transcript hash up to and including server Finished
        let mut transcript_hash = Buf::new();
        self.transcript_hash(&mut transcript_hash);

        // exporter_master_secret = Derive-Secret(master_secret, "exp master", transcript_hash)
        prf_hkdf::hkdf_expand_label_dtls13(
            hmac,
            hash,
            &master_secret,
            b"exp master",
            &transcript_hash,
            &mut exp_master,
            hash_len,
        )
        .map_err(Error::CryptoError)?;

        // client_application_traffic_secret_0
        prf_hkdf::hkdf_expand_label_dtls13(
            hmac,
            hash,
            &master_secret,
            b"c ap traffic",
            &transcript_hash,
            &mut c_ap_traffic,
            hash_len,
        )
        .map_err(Error::CryptoError)?;

        // server_application_traffic_secret_0
        prf_hkdf::hkdf_expand_label_dtls13(
            hmac,
            hash,
            &master_secret,
            b"s ap traffic",
            &transcript_hash,
            &mut s_ap_traffic,
            hash_len,
        )
        .map_err(Error::CryptoError)?;

        // Store exporter master secret (deferred to avoid borrow conflict with hmac)
        self.exporter_master_secret = Some(exp_master);

        Ok((c_ap_traffic, s_ap_traffic))
    }

    /// Install application keys (epoch 3) from the traffic secrets.
    pub fn install_application_keys(
        &mut self,
        client_traffic_secret: &Buf,
        server_traffic_secret: &Buf,
    ) -> Result<(), Error> {
        let (send_secret, recv_secret) = if self.is_client {
            (client_traffic_secret, server_traffic_secret)
        } else {
            (server_traffic_secret, client_traffic_secret)
        };

        self.app_send_keys = Some(self.derive_epoch_keys(send_secret)?);

        let recv_keys = self.derive_epoch_keys(recv_secret)?;
        self.app_recv_keys.push(RecvEpochEntry {
            epoch: 3,
            keys: recv_keys,
            expected_recv_seq: 0,
            replay: ReplayWindow::new(),
        });

        self.app_send_epoch = 3;
        self.app_send_seq = 0;

        Ok(())
    }

    /// Derive the next application traffic secret from the current one.
    /// Per RFC 8446 Section 7.2: application_traffic_secret_N+1 =
    ///   HKDF-Expand-Label(application_traffic_secret_N, "traffic upd", "", Hash.length)
    fn derive_next_traffic_secret(&self, current: &Buf) -> Result<Buf, Error> {
        let hash = self.hash_algorithm();
        let hash_len = hash.output_len();
        let hmac = self.hmac();

        let mut next = Buf::new();
        prf_hkdf::hkdf_expand_label_dtls13(
            hmac,
            hash,
            current,
            b"traffic upd",
            &[],
            &mut next,
            hash_len,
        )
        .map_err(Error::CryptoError)?;

        Ok(next)
    }

    /// Rotate send keys: move current app send keys → prev, derive new ones.
    fn update_send_keys(&mut self) -> Result<(), Error> {
        let current_keys = self.app_send_keys.take().ok_or(Error::InvalidState(
            crate::InvalidStateError::NoCurrentAppSendKeysForKeyUpdate,
        ))?;

        let next_secret = self.derive_next_traffic_secret(&current_keys.traffic_secret)?;
        let new_keys = self.derive_epoch_keys(&next_secret)?;

        // Save old keys for retransmission
        self.prev_app_send_keys = Some(current_keys);
        self.prev_app_send_epoch = self.app_send_epoch;
        self.prev_app_send_seq = self.app_send_seq;

        // Install new keys
        self.app_send_keys = Some(new_keys);
        self.app_send_epoch += 1;
        self.app_send_seq = 0;
        self.app_send_record_count = 0;
        self.aead_encryption_threshold =
            jittered_aead_threshold(self.config.aead_encryption_limit(), &mut self.rng);
        self.needs_key_update = false;

        debug!("Send keys updated to epoch {}", self.app_send_epoch);

        Ok(())
    }

    /// Install new receive keys from the current recv epoch's traffic secret.
    /// Derives the next secret, creates keys, and adds a new RecvEpochEntry.
    /// Returns the new epoch number.
    pub fn update_recv_keys(&mut self) -> Result<u16, Error> {
        // Find the latest recv epoch entry
        let latest = self.app_recv_keys.last().ok_or(Error::InvalidState(
            crate::InvalidStateError::NoCurrentAppRecvKeysForKeyUpdate,
        ))?;

        let next_secret = self.derive_next_traffic_secret(&latest.keys.traffic_secret)?;
        let new_epoch = latest.epoch + 1;
        let new_keys = self.derive_epoch_keys(&next_secret)?;

        // Evict oldest if full
        if self.app_recv_keys.is_full() {
            self.app_recv_keys.remove(0);
        }

        self.app_recv_keys.push(RecvEpochEntry {
            epoch: new_epoch,
            keys: new_keys,
            expected_recv_seq: 0,
            replay: ReplayWindow::new(),
        });

        debug!("Recv keys updated to epoch {}", new_epoch);

        Ok(new_epoch)
    }

    /// Create and send a KeyUpdate handshake message.
    ///
    /// Arms the flight timer for retransmission. The KeyUpdate is sent on
    /// the current app epoch. Send keys rotate only after its ACK arrives.
    pub fn create_key_update(&mut self, request: KeyUpdateRequest) -> Result<(), Error> {
        // Set up retransmission
        self.timers.begin_flight(&mut self.rng);
        self.flight_clear_resends();

        let msg_seq = self.next_handshake_seq_no;
        self.next_handshake_seq_no += 1;

        let epoch = self.app_send_epoch;

        // Build the handshake message manually (12-byte DTLS header + 1-byte body)
        self.create_ciphertext_record(ContentType::Handshake, epoch, true, |fragment| {
            // DTLS handshake header (12 bytes):
            // msg_type(1) + length(3) + message_seq(2) + fragment_offset(3) + fragment_length(3)
            fragment.push(MessageType::KeyUpdate.as_u8());
            fragment.extend_from_slice(&1u32.to_be_bytes()[1..]); // length = 1
            fragment.extend_from_slice(&msg_seq.to_be_bytes()); // message_seq
            fragment.extend_from_slice(&0u32.to_be_bytes()[1..]); // fragment_offset = 0
            fragment.extend_from_slice(&1u32.to_be_bytes()[1..]); // fragment_length = 1
            // Body: 1 byte
            fragment.push(request.as_u8());
        })?;

        self.key_update_in_flight = true;

        debug!(
            "KeyUpdate sent (request={:?}) on epoch {}, awaiting ACK before rotating send keys",
            request, epoch
        );

        Ok(())
    }

    /// Install send handshake keys for client flight (after receiving server Finished).
    /// Reset handshake state for a new ClientHello after HelloRetryRequest.
    pub fn reset_for_hello_retry(&mut self) {
        // Per RFC 9147, message_seq increments across HRR (CH1=0, HRR=0,
        // CH2=1, SH=1), so neither peer nor own handshake sequence numbers
        // are reset. Only reset epoch 2 record sequence (unused at HRR time).
        self.hs_send_seq = 0;
        self.handshake_ack_deadline = None;
        // Drain handled items so stale retransmissions with the same
        // message_seq can be accepted after the sequence number reset.
        self.queue_rx.retain(|item| !item.first().is_handled());
    }

    /// Derive epoch keys (cipher + IV + sn_key) from a traffic secret.
    fn derive_epoch_keys(&mut self, traffic_secret: &Buf) -> Result<EpochKeys, Error> {
        let mut sn_key = self.buffers_free.pop();
        let mut secret = self.buffers_free.pop();
        let hash = self.hash_algorithm();
        let suite = self.suite_provider();
        let hmac = self.hmac();

        // key = HKDF-Expand-Label(secret, "key", "", key_length)
        let mut key = Buf::new();
        prf_hkdf::hkdf_expand_label_dtls13(
            hmac,
            hash,
            traffic_secret,
            b"key",
            &[],
            &mut key,
            suite.key_len(),
        )
        .map_err(Error::CryptoError)?;

        // iv = HKDF-Expand-Label(secret, "iv", "", iv_length)
        let mut iv_buf = Buf::new();
        prf_hkdf::hkdf_expand_label_dtls13(
            hmac,
            hash,
            traffic_secret,
            b"iv",
            &[],
            &mut iv_buf,
            suite.iv_len(),
        )
        .map_err(Error::CryptoError)?;

        // sn_key = HKDF-Expand-Label(secret, "sn", "", key_length)
        prf_hkdf::hkdf_expand_label_dtls13(
            hmac,
            hash,
            traffic_secret,
            b"sn",
            &[],
            &mut sn_key,
            suite.key_len(),
        )
        .map_err(Error::CryptoError)?;

        let cipher = suite.create_cipher(&key).map_err(Error::CryptoError)?;

        let mut iv = [0u8; 12];
        iv.copy_from_slice(&iv_buf);

        secret.extend_from_slice(traffic_secret);

        Ok(EpochKeys {
            cipher,
            iv,
            traffic_secret: secret,
            sn_key,
        })
    }

    /// Compute verify_data for Finished messages.
    ///
    /// finished_key = HKDF-Expand-Label(traffic_secret, "finished", "", Hash.len)
    /// verify_data = HMAC(finished_key, transcript_hash)
    ///
    /// We use `hkdf_extract(salt=finished_key, IKM=transcript_hash)` to compute
    /// the HMAC because HKDF-Extract is defined as HMAC-Hash(salt, IKM) (RFC 5869
    /// §2.2). This gives us a generic HMAC that works for any hash algorithm
    /// (SHA-256, SHA-384) without needing a separate per-algorithm HMAC API.
    pub fn compute_verify_data(&self, traffic_secret: &[u8]) -> Result<Buf, Error> {
        let hash = self.hash_algorithm();
        let hash_len = hash.output_len();
        let hmac = self.hmac();

        // finished_key = HKDF-Expand-Label(secret, "finished", "", Hash.len)
        let mut finished_key = Buf::new();
        prf_hkdf::hkdf_expand_label_dtls13(
            hmac,
            hash,
            traffic_secret,
            b"finished",
            &[],
            &mut finished_key,
            hash_len,
        )
        .map_err(Error::CryptoError)?;

        let mut transcript_hash = Buf::new();
        self.transcript_hash(&mut transcript_hash);

        // HMAC(finished_key, transcript_hash) via HKDF-Extract(salt=key, IKM=data)
        let mut verify_data = Buf::new();
        prf_hkdf::hkdf_extract(
            hmac,
            hash,
            &finished_key,
            &transcript_hash,
            &mut verify_data,
        )
        .map_err(Error::CryptoError)?;

        Ok(verify_data)
    }

    // =========================================================================
    // Transcript Management
    // =========================================================================

    pub fn transcript_hash(&self, out: &mut Buf) {
        let hash = self.hash_algorithm();
        let mut ctx = self
            .config
            .crypto_provider()
            .hash_provider
            .create_hash(hash);
        ctx.update(&self.transcript);
        ctx.clone_and_finalize(out);
    }

    /// Compute transcript hash of arbitrary data (e.g. empty for "derived" label).
    fn transcript_hash_of(&self, data: &[u8]) -> Buf {
        let hash = self.hash_algorithm();
        let mut ctx = self
            .config
            .crypto_provider()
            .hash_provider
            .create_hash(hash);
        ctx.update(data);
        let mut out = Buf::new();
        ctx.clone_and_finalize(&mut out);
        out
    }

    /// Replace transcript with message_hash for HelloRetryRequest.
    ///
    /// Per RFC 8446 Section 4.4.1: Hash replaces transcript with
    /// message_hash = 0xFE || 00 00 Hash.length || Hash(CH1)
    ///
    /// `split_at` is the byte offset that separates the portion to hash (CH1)
    /// from the tail to preserve (HRR bytes appended by next_handshake).
    /// The transcript becomes: message_hash(CH1) || transcript[split_at..].
    pub fn replace_transcript_with_message_hash(&mut self, split_at: usize) {
        let hash = self.hash_algorithm();
        let mut hash_ctx = self
            .config
            .crypto_provider()
            .hash_provider
            .create_hash(hash);
        hash_ctx.update(&self.transcript[..split_at]);
        let mut hash_value = Buf::new();
        hash_ctx.clone_and_finalize(&mut hash_value);

        // Build new transcript: message_hash || tail
        let mut new_transcript = self.buffers_free.pop();
        // message_hash construct: msg_type=0xFE, length(3)=hash_len
        new_transcript.push(0xFE);
        let hash_len = hash_value.len() as u32;
        new_transcript.extend_from_slice(&hash_len.to_be_bytes()[1..]);
        new_transcript.extend_from_slice(&hash_value);
        // Append the preserved tail (HRR bytes)
        new_transcript.extend_from_slice(&self.transcript[split_at..]);

        let old = mem::replace(&mut self.transcript, new_transcript);
        self.buffers_free.push(old);
    }

    // =========================================================================
    // Peer Encryption Management
    // =========================================================================

    pub fn enable_peer_encryption(&mut self) -> Result<(), InternalError> {
        debug!("Peer encryption enabled");
        self.peer_encryption_enabled = true;

        // Re-parse any buffered epoch 2+ records
        let maybe_index = self
            .queue_rx
            .iter()
            .position(|i| i.records().iter().any(|r| r.record().sequence.epoch >= 2));

        let Some(index) = maybe_index else {
            return Ok(());
        };

        let all = self.queue_rx.split_off(index);

        for incoming in all {
            let unhandled = incoming.into_records().filter(|r| !r.is_handled());

            for record in unhandled {
                let buf = record.into_buffer();
                self.parse_packet(&buf)?;
                self.buffers_free.push(buf);
            }
        }

        Ok(())
    }

    // =========================================================================
    // Key Exchange Helpers
    // =========================================================================

    pub fn find_kx_group(
        &self,
        group: crate::types::NamedGroup,
    ) -> Option<&'static dyn SupportedKxGroup> {
        self.config.kx_groups().find(|g| g.name() == group)
    }

    // =========================================================================
    // Signature Verification
    // =========================================================================

    pub fn verify_signature(
        &self,
        cert_der: &[u8],
        data: &[u8],
        signature: &[u8],
        hash_alg: HashAlgorithm,
        sig_alg: crate::types::SignatureAlgorithm,
    ) -> Result<(), Error> {
        self.config
            .crypto_provider()
            .signature_verification
            .verify_signature(cert_der, data, signature, hash_alg, sig_alg)
            .map_err(Error::CryptoError)
    }

    // =========================================================================
    // Extract SRTP Keying Material
    // =========================================================================

    pub fn extract_srtp_keying_material(
        &self,
        profile: crate::crypto::SrtpProfile,
    ) -> Result<(ArrayVec<u8, 88>, crate::crypto::SrtpProfile), Error> {
        let hash = self.hash_algorithm();
        let hash_len = hash.output_len();
        let hmac = self.hmac();

        let exp_master = self
            .exporter_master_secret
            .as_ref()
            .ok_or(Error::CryptoError(
                crate::CryptoError::ExporterMasterSecretNotDerived,
            ))?;

        let total_len = profile.keying_material_len();

        // RFC 8446 Section 7.5:
        // 1. derived_secret = Derive-Secret(exporter_master_secret, label, "")
        let empty_hash = self.transcript_hash_of(b"");
        let mut derived = Buf::new();
        prf_hkdf::hkdf_expand_label_dtls13(
            hmac,
            hash,
            exp_master,
            b"EXTRACTOR-dtls_srtp",
            &empty_hash,
            &mut derived,
            hash_len,
        )
        .map_err(Error::CryptoError)?;

        // 2. result = HKDF-Expand-Label(derived_secret, "exporter", Hash(context), length)
        let context_hash = self.transcript_hash_of(b"");
        let mut keying_material_buf = Buf::new();
        prf_hkdf::hkdf_expand_label_dtls13(
            hmac,
            hash,
            &derived,
            b"exporter",
            &context_hash,
            &mut keying_material_buf,
            total_len,
        )
        .map_err(Error::CryptoError)?;

        let mut keying_material = ArrayVec::new();
        for &b in keying_material_buf.iter().take(total_len) {
            keying_material.push(b);
        }

        Ok((keying_material, profile))
    }

    pub fn random(&mut self) -> Random {
        Random::new(&mut self.rng)
    }

    pub fn random_arr<const N: usize>(&mut self) -> [u8; N] {
        self.rng.random()
    }
}

impl Entry {
    fn fragment(&self) -> &[u8] {
        &self.fragment[..self.fragment_len]
    }

    fn was_sent_as(&self, seq: u64) -> bool {
        self.send_seq == seq
            || self.fragment[self.fragment_len..]
                .as_chunks::<8>()
                .0
                .iter()
                .any(|previous| *previous == seq.to_be_bytes())
    }
}

// =========================================================================
// Helper Functions
// =========================================================================

/// Pick a random AEAD encryption threshold in `[3/4 * limit, limit]`.
///
/// Jittering prevents both sides from triggering KeyUpdate simultaneously
/// when they share the same configured limit.
fn jittered_aead_threshold(limit: u64, rng: &mut SeededRng) -> u64 {
    let quarter = limit / 4;
    if quarter == 0 {
        return limit;
    }
    let offset: u64 = rng.random::<u64>() % (quarter + 1);
    limit - quarter + offset
}

/// Determine the epoch for a handshake message type.
///
/// In DTLS 1.3, ClientHello and ServerHello are sent as plaintext (epoch 0).
/// All other handshake messages are encrypted (epoch 2).
fn epoch_for_message(msg_type: MessageType) -> u16 {
    match msg_type {
        MessageType::ClientHello | MessageType::ServerHello => 0,
        _ => 2,
    }
}

/// Reconstruct a full sequence number from a partial value (RFC 9147 Section 4.2.2).
///
/// Uses a sliding window centered on the expected sequence to find the full
/// 48-bit sequence that is closest to `expected`.
fn reconstruct_sequence(partial: u64, expected: u64, bits: u32) -> u64 {
    let mask = (1u64 << bits) - 1;
    let window = 1u64 << bits;
    let half = window / 2;

    let received_partial = partial & mask;
    let expected_partial = expected & mask;

    let diff = (received_partial as i64) - (expected_partial as i64);
    let diff = if diff > half as i64 {
        diff - window as i64
    } else if diff < -(half as i64) {
        diff + window as i64
    } else {
        diff
    };

    // Clamp to 0 to avoid underflow for very early sequence numbers
    (expected as i64 + diff).max(0) as u64
}

// =========================================================================
// RecordHandler Implementation
// =========================================================================

impl RecordHandler for Engine {
    fn pop_buffer(&mut self) -> Buf {
        Engine::pop_buffer(self)
    }

    fn push_buffer(&mut self, buffer: Buf) {
        Engine::push_buffer(self, buffer);
    }

    fn classify_record(&mut self, mut record: Record) -> Result<Option<Record>, Error> {
        if let Some(cn_seq) = self.close_notify_sequence
            && record.record().sequence > cn_seq
        {
            self.push_buffer(record.into_buffer());
            return Ok(None);
        }

        let epoch = record.record().sequence.epoch;
        if epoch == 0
            && self.peer_encryption_enabled
            && record.record().content_type == ContentType::Handshake
        {
            // Preserve duplicate-flight responses while discarding only the
            // plaintext record, including when protected records follow it.
            let dupe_seq = record
                .first_handshake()
                .and_then(|handshake| handshake.dupe_triggers_resend());
            self.push_buffer(record.into_buffer());
            if dupe_seq.is_some_and(|seq| seq < self.peer_handshake_seq_no)
                && !self.flight_saved_records.is_empty()
                && self.timers.request_resend(&mut self.rng)
            {
                self.flight_resend("dupe triggers resend")?;
            }
            return Ok(None);
        }

        if epoch == 0
            && self.peer_encryption_enabled
            && matches!(
                record.record().content_type,
                ContentType::Ack | ContentType::Alert
            )
        {
            // Plaintext ACKs and alerts after peer encryption is enabled are
            // unauthenticated and must not be acted on. Mirrors DTLS 1.2.
            self.push_buffer(record.into_buffer());
            return Ok(None);
        }

        match record.record().content_type {
            ContentType::Handshake if self.release_app_data && self.is_client && epoch >= 3 => {
                let count = record.handshakes().len();
                record.retain_handshakes(|handshake| {
                    handshake.header.msg_type != MessageType::NewSessionTicket
                        || handshake.header.message_seq >= self.peer_handshake_seq_no
                });
                if record.handshakes().len() != count {
                    // Re-ACK old tickets without discarding fresh messages beside them.
                    let seq = record.record().sequence;
                    let _ = self
                        .received_record_numbers
                        .try_push((seq.epoch as u64, seq.sequence_number));
                    let ack = self.send_ack();
                    if record.handshakes().is_empty() || ack.is_err() {
                        self.push_buffer(record.into_buffer());
                        ack?;
                        return Ok(None);
                    }
                }
                Ok(Some(record))
            }
            ContentType::Ack => {
                let fragment = record.record().fragment(record.buffer());
                self.process_ack(fragment)?;
                self.push_buffer(record.into_buffer());
                Ok(None)
            }
            ContentType::Alert => {
                // RFC 8446 §6: TLS 1.3 ignores the AlertLevel byte; severity is
                // implicit in the description (only close_notify and user_canceled
                // are non-fatal).
                let description = {
                    let fragment = record.record().fragment(record.buffer());
                    fragment.get(1).copied()
                };
                let sequence = record.record().sequence;
                self.push_buffer(record.into_buffer());

                match description {
                    Some(0) => {
                        self.close_notify_sequence.get_or_insert(sequence);
                        Ok(None)
                    }
                    Some(90) => Ok(None),
                    Some(description) => {
                        Err(Error::SecurityError(crate::SecurityError::FatalAlert {
                            description,
                        }))
                    }
                    None => Ok(None),
                }
            }
            ContentType::ChangeCipherSpec => {
                trace!("Discarding CCS record");
                self.push_buffer(record.into_buffer());
                Ok(None)
            }
            _ => Ok(Some(record)),
        }
    }

    fn is_peer_encryption_enabled(&self) -> bool {
        self.peer_encryption_enabled
    }

    fn resolve_epoch(&self, epoch_bits: u8) -> u16 {
        // Map 2-bit epoch field to full epoch.
        // In practice during handshake, epoch_bits=2 maps to epoch 2.
        // After KeyUpdate, epoch_bits cycles: 3→0→1→2→3→...
        let epoch_bits = epoch_bits as u16;

        // Check application recv epochs - return the newest (last) match
        // when multiple epochs share the same 2-bit value (e.g. epoch 3 and 7).
        let mut best = None;
        for entry in &self.app_recv_keys {
            if (entry.epoch & 0x03) == epoch_bits {
                best = Some(entry.epoch);
            }
        }

        if self.hs_recv_keys.is_some() && (2 & 0x03) == epoch_bits && !self.release_app_data {
            return 2;
        }

        if let Some(epoch) = best {
            return epoch;
        }

        if self.hs_recv_keys.is_some() && (2 & 0x03) == epoch_bits {
            return 2;
        }

        // Default to the epoch bits value
        epoch_bits
    }

    fn resolve_sequence(&self, epoch: u16, seq_bits: u64, s_flag: bool) -> u64 {
        let expected = if epoch == 2 {
            self.hs_expected_recv_seq
        } else {
            self.app_recv_keys
                .iter()
                .find(|e| e.epoch == epoch)
                .map(|e| e.expected_recv_seq)
                .unwrap_or(0)
        };

        let bits: u32 = if s_flag { 16 } else { 8 };
        reconstruct_sequence(seq_bits, expected, bits)
    }

    fn replay_check(&self, seq: Sequence) -> bool {
        // Route to the correct per-epoch replay window
        if seq.epoch == 2 {
            self.hs_replay.check(seq.sequence_number)
        } else {
            match self.app_recv_keys.iter().find(|e| e.epoch == seq.epoch) {
                Some(entry) => entry.replay.check(seq.sequence_number),
                None => false, // no keys for this epoch
            }
        }
    }

    fn replay_update(&mut self, seq: Sequence) {
        // Update the replay window for this epoch
        if seq.epoch == 2 {
            self.hs_replay.update(seq.sequence_number);
        } else if let Some(entry) = self.app_recv_keys.iter_mut().find(|e| e.epoch == seq.epoch) {
            entry.replay.update(seq.sequence_number);
        }

        // Advance expected receive sequence for this epoch
        let next = seq.sequence_number + 1;
        if seq.epoch == 2 {
            if next > self.hs_expected_recv_seq {
                self.hs_expected_recv_seq = next;
            }
        } else {
            for entry in &mut self.app_recv_keys {
                if entry.epoch == seq.epoch {
                    if next > entry.expected_recv_seq {
                        entry.expected_recv_seq = next;
                    }
                    break;
                }
            }
        }
    }

    fn min_protected_fragment_len(&self) -> usize {
        self.suite_provider().min_protected_fragment_len()
    }

    fn decrypt_record(
        &mut self,
        header: &[u8],
        seq: Sequence,
        ciphertext: &mut TmpBuf,
    ) -> Result<(), Error> {
        // Find the right keys based on epoch
        let keys = if seq.epoch == 2 {
            self.hs_recv_keys.as_mut()
        } else {
            // Look up in app recv keys
            self.app_recv_keys
                .iter_mut()
                .find(|e| e.epoch == seq.epoch)
                .map(|e| &mut e.keys)
        };

        let Some(keys) = keys else {
            return Err(Error::CryptoError(
                crate::CryptoError::RecvKeysNotAvailable { epoch: seq.epoch },
            ));
        };

        // Construct nonce: iv XOR padded_seq
        let nonce = Nonce::xor(&keys.iv, seq.sequence_number);

        // AAD is the raw header bytes
        let aad = Aad::new_dtls13(header);

        keys.cipher
            .decrypt(ciphertext, aad, nonce)
            .map_err(Error::CryptoError)?;

        Ok(())
    }

    fn decrypt_sequence_number(
        &self,
        epoch: u16,
        seq_bytes: &mut [u8],
        ciphertext_sample: &[u8; 16],
    ) {
        // Find the sn_key for this epoch
        let sn_key = if epoch == 2 {
            self.hs_recv_keys.as_ref().map(|k| &k.sn_key)
        } else {
            self.app_recv_keys
                .iter()
                .find(|e| e.epoch == epoch)
                .map(|e| &e.keys.sn_key)
        };

        let Some(sn_key) = sn_key else {
            return; // No keys yet, leave seq bytes as-is
        };

        let mask = self.suite_provider().encrypt_sn(sn_key, ciphertext_sample);
        for (i, byte) in seq_bytes.iter_mut().enumerate() {
            *byte ^= mask[i];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "rcgen")]
    use crate::certificate::generate_self_signed_certificate;

    #[cfg(feature = "rcgen")]
    fn test_engine() -> Engine {
        let cert = generate_self_signed_certificate().expect("gen cert");
        let config = Arc::new(Config::builder().build().expect("build config"));
        Engine::new(config, cert)
    }

    #[test]
    #[cfg(feature = "rcgen")]
    fn timing_resend_queue_failure_is_fatal() {
        let now = Instant::now();
        let certificate = generate_self_signed_certificate().expect("certificate");
        let config = Arc::new(
            Config::builder()
                .dangerously_set_rng_seed(42)
                .build()
                .expect("valid config"),
        );
        let mut engine = Engine::new(config, certificate);
        engine.flight_begin(1);
        engine
            .create_plaintext_record(ContentType::Handshake, true, |fragment| fragment.push(1))
            .expect("queue original flight");
        let mut buffer = [0; 64];
        assert!(matches!(
            engine.poll_output(&mut buffer, now),
            Output::Packet(_)
        ));
        let Output::Timeout(retry_at) = engine.poll_output(&mut buffer, now) else {
            panic!("expected flight timer");
        };
        engine.config = Arc::new(
            Config::builder()
                .max_queue_tx(0)
                .build()
                .expect("inject exhausted transmit capacity"),
        );
        assert_eq!(
            engine.handle_timeout(retry_at),
            Err(Error::TransmitQueueFull)
        );
    }

    #[derive(Default)]
    struct PassthroughRecordHandler {
        buffers: BufferPool,
    }

    impl RecordHandler for PassthroughRecordHandler {
        fn pop_buffer(&mut self) -> Buf {
            self.buffers.pop()
        }

        fn push_buffer(&mut self, buffer: Buf) {
            self.buffers.push(buffer);
        }

        fn classify_record(&mut self, record: Record) -> Result<Option<Record>, Error> {
            Ok(Some(record))
        }

        fn is_peer_encryption_enabled(&self) -> bool {
            true
        }

        fn resolve_epoch(&self, _epoch_bits: u8) -> u16 {
            2
        }

        fn resolve_sequence(&self, _epoch: u16, seq_bits: u64, _s_flag: bool) -> u64 {
            seq_bits
        }

        fn replay_check(&self, _seq: Sequence) -> bool {
            true
        }

        fn replay_update(&mut self, _seq: Sequence) {}

        fn min_protected_fragment_len(&self) -> usize {
            0
        }

        fn decrypt_record(
            &mut self,
            _header: &[u8],
            _seq: Sequence,
            _ciphertext: &mut TmpBuf,
        ) -> Result<(), Error> {
            Ok(())
        }

        fn decrypt_sequence_number(
            &self,
            _epoch: u16,
            _seq_bytes: &mut [u8],
            _ciphertext_sample: &[u8; 16],
        ) {
        }
    }

    fn encrypted_key_update_record(seq: u16) -> Vec<u8> {
        let mut fragment = Vec::new();
        fragment.push(MessageType::KeyUpdate.as_u8());
        fragment.extend_from_slice(&1u32.to_be_bytes()[1..]);
        fragment.extend_from_slice(&0u16.to_be_bytes());
        fragment.extend_from_slice(&0u32.to_be_bytes()[1..]);
        fragment.extend_from_slice(&1u32.to_be_bytes()[1..]);
        fragment.push(KeyUpdateRequest::UpdateRequested.as_u8());
        fragment.push(ContentType::Handshake.as_u8());

        let mut packet = Vec::new();
        packet.push(
            0b0010_0000
                | 0b0000_1000 // 2-byte sequence number.
                | 0b0000_0100 // explicit length.
                | 0b0000_0010, // epoch bits resolved by PassthroughRecordHandler.
        );
        packet.extend_from_slice(&seq.to_be_bytes());
        packet.extend_from_slice(&(fragment.len() as u16).to_be_bytes());
        packet.extend_from_slice(&fragment);
        packet
    }

    fn encrypted_application_data_record(seq: u16, data: &[u8]) -> Vec<u8> {
        let mut fragment = Vec::new();
        fragment.extend_from_slice(data);
        fragment.push(ContentType::ApplicationData.as_u8());

        let mut packet = Vec::new();
        packet.push(
            0b0010_0000
                | 0b0000_1000 // 2-byte sequence number.
                | 0b0000_0100 // explicit length.
                | 0b0000_0010, // epoch bits resolved by PassthroughRecordHandler.
        );
        packet.extend_from_slice(&seq.to_be_bytes());
        packet.extend_from_slice(&(fragment.len() as u16).to_be_bytes());
        packet.extend_from_slice(&fragment);
        packet
    }

    #[test]
    #[cfg(feature = "rcgen")]
    fn premature_datagram_recycles_its_pooled_buffer() {
        let mut engine = test_engine();
        let packet = encrypted_application_data_record(1, b"buffered ciphertext");
        engine
            .parse_packet(&packet)
            .expect("discard premature datagram");
        let mut output = [0u8; 2048];
        assert!(matches!(
            engine.poll_output(&mut output, Instant::now()),
            Output::Timeout(_)
        ));

        let mut buffer = engine.pop_buffer();
        buffer.resize(2048, 0xAA);
        engine.push_buffer(buffer);

        engine
            .parse_packet(&packet)
            .expect("discard duplicate datagram");
        assert!(matches!(
            engine.poll_output(&mut output, Instant::now()),
            Output::Timeout(_)
        ));
        assert!(engine.queue_rx.is_empty());
        assert!(
            engine.pop_buffer().into_vec().capacity() >= 2048,
            "discarding a duplicate must preserve its reusable receive allocation"
        );
    }

    fn parsed_key_update(seq: u16) -> Incoming {
        Incoming::parse_packet(
            &encrypted_key_update_record(seq),
            &mut PassthroughRecordHandler::default(),
            Some(Dtls13CipherSuite::AES_128_GCM_SHA256),
        )
        .expect("parse key update packet")
        .expect("packet contains a record")
    }

    fn parsed_key_update_with_app_data(key_update_seq: u16, app_seq: u16) -> Incoming {
        let mut packet = encrypted_key_update_record(key_update_seq);
        packet.extend_from_slice(&encrypted_application_data_record(app_seq, b"app-data"));
        Incoming::parse_packet(
            &packet,
            &mut PassthroughRecordHandler::default(),
            Some(Dtls13CipherSuite::AES_128_GCM_SHA256),
        )
        .expect("parse coalesced packet")
        .expect("packet contains records")
    }

    /// Issue 2: Epoch-0 sequence number must have an overflow guard.
    ///
    /// Per RFC 9147 §4.2, implementations MUST NOT allow the sequence number
    /// to exceed MAX_SEQUENCE_NUMBER (2^48 - 1). This test sets the counter
    /// to MAX and verifies that `create_plaintext_record` returns an error.
    #[test]
    #[cfg(feature = "rcgen")]
    fn epoch_0_sequence_number_rejects_overflow() {
        let mut engine = test_engine();

        // Set epoch-0 sequence to MAX — the next increment should be rejected
        engine.sequence_epoch_0.sequence_number = MAX_SEQUENCE_NUMBER;

        let result = engine.create_plaintext_record(ContentType::Handshake, false, |buf| {
            buf.extend_from_slice(b"test")
        });
        assert!(
            result.is_err(),
            "epoch-0 must reject sequence overflow at MAX_SEQUENCE_NUMBER"
        );
    }

    /// Issue 3: `derive_handshake_secrets` must return the handshake_secret
    /// alongside the traffic secrets, eliminating the need for a separate
    /// `derive_handshake_secret` method.
    #[test]
    #[cfg(feature = "rcgen")]
    fn derive_handshake_secrets_returns_handshake_secret() {
        let mut engine = test_engine();
        engine.set_cipher_suite(Dtls13CipherSuite::AES_128_GCM_SHA256);

        // Simulate some transcript content (normally built during handshake)
        engine
            .transcript
            .extend_from_slice(b"dummy transcript for test");

        let shared_secret = [0x42u8; 32];

        // derive_handshake_secrets returns a 3-tuple:
        //   (client_hs_traffic, server_hs_traffic, handshake_secret)
        let (c_hs_traffic, _s_hs_traffic, handshake_secret) =
            engine.derive_handshake_secrets(&shared_secret).unwrap();

        // Verify the handshake_secret can reproduce the same c_hs_traffic
        let hash = engine.hash_algorithm();
        let hash_len = hash.output_len();
        let hmac = engine.hmac();

        let mut transcript_hash = Buf::new();
        engine.transcript_hash(&mut transcript_hash);

        let mut c_hs_manual = Buf::new();
        prf_hkdf::hkdf_expand_label_dtls13(
            hmac,
            hash,
            &handshake_secret,
            b"c hs traffic",
            &transcript_hash,
            &mut c_hs_manual,
            hash_len,
        )
        .expect("hkdf_expand_label_dtls13");

        assert_eq!(
            c_hs_manual.as_ref(),
            c_hs_traffic.as_ref(),
            "handshake_secret from derive_handshake_secrets() must reproduce \
             the same traffic secrets"
        );
    }

    /// Issue 4: `derive_early_secret` must use the buffer pool.
    ///
    /// `derive_early_secret` takes `&mut self` and allocates its output buffer
    /// via `self.buffers_free.pop()`, reusing pooled allocations instead of
    /// creating a fresh `Buf::new()`.
    #[test]
    #[cfg(feature = "rcgen")]
    fn derive_early_secret_uses_buffer_pool() {
        let mut engine = test_engine();
        engine.set_cipher_suite(Dtls13CipherSuite::AES_128_GCM_SHA256);

        // Pre-fill the pool with a buffer that has allocated capacity.
        // BufferPool::push clears contents but retains the allocation.
        let mut marked = engine.pop_buffer();
        marked.extend_from_slice(&[0xAA; 256]);
        engine.buffers_free.push(marked);

        // derive_early_secret takes &mut self and uses the buffer pool.
        let early_secret = engine.derive_early_secret().unwrap();

        // The returned buffer should have pooled capacity (>= 256),
        // not 0 from Buf::new().
        assert!(
            early_secret.into_vec().capacity() >= 256,
            "derive_early_secret must use the buffer pool, returning a buffer with pooled capacity"
        );
    }

    #[test]
    #[cfg(feature = "rcgen")]
    fn ack_tracking_full_does_not_panic_on_handshake_replacement() {
        let mut engine = test_engine();

        let first = parsed_key_update(0);
        engine
            .insert_incoming(first)
            .expect("insert initial key update");
        engine.queue_rx[0]
            .first()
            .first_handshake()
            .expect("initial key update handshake")
            .set_handled();

        engine.received_record_numbers.clear();
        for sequence in 0..engine.received_record_numbers.capacity() {
            engine.received_record_numbers.push((2, sequence as u64));
        }

        let replacement = parsed_key_update(1);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            engine
                .insert_incoming(replacement)
                .expect("replace handled key update")
        }));

        assert!(
            result.is_ok(),
            "full ACK bookkeeping must not panic when a handled handshake is replaced"
        );
        assert_eq!(engine.queue_rx.len(), 1);
        assert_eq!(
            engine.queue_rx[0].first().record().sequence.sequence_number,
            1
        );
        assert_eq!(
            engine.received_record_numbers.len(),
            engine.received_record_numbers.capacity(),
            "overflowing ACK bookkeeping should keep existing entries and drop the extra one"
        );
    }

    #[test]
    #[cfg(feature = "rcgen")]
    fn ack_tracking_ignores_non_handshake_records_in_coalesced_datagram() {
        let mut engine = test_engine();

        let incoming = parsed_key_update_with_app_data(7, 8);
        assert_eq!(incoming.records().len(), 2);
        assert_eq!(
            incoming.records()[0].record().content_type,
            ContentType::Handshake
        );
        assert_eq!(
            incoming.records()[1].record().content_type,
            ContentType::ApplicationData
        );

        engine
            .insert_incoming(incoming)
            .expect("insert coalesced datagram");

        assert_eq!(
            engine.received_record_numbers.as_slice(),
            &[(2, 7)],
            "ACK bookkeeping must include only handshake records"
        );
    }

    #[test]
    #[cfg(feature = "rcgen")]
    fn malformed_ack_record_number_vector_is_ignored() {
        let mut engine = test_engine();
        let fragment = engine.pop_buffer();
        engine.flight_saved_records.push(Entry {
            content_type: ContentType::Handshake,
            epoch: 2,
            send_seq: 7,
            fragment_len: fragment.len(),
            fragment,
            acked: false,
        });

        let mut malformed_ack = Vec::new();
        malformed_ack.extend_from_slice(&17u16.to_be_bytes());
        malformed_ack.extend_from_slice(&2u64.to_be_bytes());
        malformed_ack.extend_from_slice(&7u64.to_be_bytes());
        malformed_ack.push(0);

        engine.process_ack(&malformed_ack).unwrap();

        assert!(
            !engine.flight_saved_records[0].acked,
            "malformed ACK vector length must not partially acknowledge records"
        );
    }

    #[cfg(feature = "rcgen")]
    fn protected_test_engine() -> Engine {
        let mut engine = test_engine();
        engine.set_cipher_suite(Dtls13CipherSuite::AES_128_GCM_SHA256);
        let mut secret = Buf::new();
        secret.extend_from_slice(&[0x42; 32]);
        engine
            .install_handshake_keys(&secret, &secret)
            .expect("handshake keys");
        engine
            .install_application_keys(&secret, &secret)
            .expect("application keys");
        engine
            .enable_peer_encryption()
            .expect("enable peer encryption");
        engine.release_application_data();
        engine.flight_stop_resend_timers();
        engine
    }

    #[cfg(feature = "rcgen")]
    fn engine_packets(engine: &mut Engine, now: Instant) -> Vec<Vec<u8>> {
        let mut packets = Vec::new();
        let mut buffer = [0; 2048];
        loop {
            match engine.poll_output(&mut buffer, now) {
                Output::Packet(packet) => packets.push(packet.to_vec()),
                Output::Timeout(_) => return packets,
                output => panic!("unexpected output: {output:?}"),
            }
        }
    }

    #[cfg(feature = "rcgen")]
    fn ack_for(records: &[(u64, u64)]) -> Vec<u8> {
        let mut ack = Vec::new();
        ack.extend_from_slice(&((records.len() * 16) as u16).to_be_bytes());
        for (epoch, seq) in records {
            ack.extend_from_slice(&epoch.to_be_bytes());
            ack.extend_from_slice(&seq.to_be_bytes());
        }
        ack
    }

    #[test]
    #[cfg(feature = "rcgen")]
    fn ack_matches_mixed_transmissions_and_preserves_selective_retransmission() {
        let now = Instant::now();
        let mut engine = protected_test_engine();
        engine.flight_begin(5);
        for n in 0..3 {
            engine
                .create_ciphertext_record(ContentType::Handshake, 2, true, |fragment| {
                    fragment.push(n)
                })
                .expect("save flight record");
        }
        engine_packets(&mut engine, now);
        engine
            .flight_resend("first retransmission")
            .expect("resend flight");
        engine_packets(&mut engine, now);
        engine
            .flight_resend("second retransmission")
            .expect("resend flight");
        engine_packets(&mut engine, now);

        engine
            .process_ack(&ack_for(&[(3, 0), (2, 100)]))
            .expect("unrelated ACK");
        assert!(engine.flight_saved_records.iter().all(|entry| !entry.acked));
        // Acknowledge the first record's original send and the second's latest send.
        engine
            .process_ack(&ack_for(&[(2, 0), (2, 7)]))
            .expect("mixed ACK");
        let acked: Vec<_> = engine
            .flight_saved_records
            .iter()
            .map(|entry| entry.acked)
            .collect();
        assert_eq!(acked, [true, true, false]);
        assert!(engine.timers.poll_timeout(now) < now + Duration::from_secs(60));

        let next_seq = engine.hs_send_seq;
        engine
            .flight_resend("selective retransmission")
            .expect("resend unACKed record");
        assert_eq!(
            engine.hs_send_seq,
            next_seq + 1,
            "only the third record must be resent"
        );
        engine_packets(&mut engine, now);
        // Acknowledge the third record's intermediate transmission after another resend.
        engine
            .process_ack(&ack_for(&[(2, 5)]))
            .expect("intermediate ACK");
        assert!(engine.flight_saved_records.is_empty());
        assert!(engine.timers.poll_timeout(now) > now + Duration::from_secs(60));
    }

    #[test]
    #[cfg(feature = "rcgen")]
    fn duplicate_key_update_does_not_ack_discarded_new_update() {
        let now = Instant::now();
        let mut sender = protected_test_engine();
        let mut peer = protected_test_engine();
        sender
            .create_key_update(KeyUpdateRequest::UpdateNotRequested)
            .expect("send first update");
        for packet in engine_packets(&mut sender, now) {
            peer.parse_packet(&packet).expect("receive first update");
            engine_packets(&mut peer, now);
        }
        let mut buffer = Buf::new();
        assert!(
            peer.next_handshake_no_transcript(MessageType::KeyUpdate, &mut buffer)
                .expect("parse first update")
                .is_some()
        );
        peer.advance_peer_handshake_seq();
        peer.update_recv_keys().expect("rotate receive keys");
        peer.send_ack().expect("ACK first update");
        let first_ack = engine_packets(&mut peer, now);

        // A retry can already be queued when the original ACK arrives. Bundle
        // that old retry with a new update sent after the ACK permits it.
        sender.flight_resend("queued retry").expect("resend update");
        let mut coalesced = engine_packets(&mut sender, now).concat();
        for packet in first_ack {
            sender.parse_packet(&packet).expect("receive first ACK");
            engine_packets(&mut sender, now);
        }
        assert_eq!(sender.app_send_epoch(), 4);
        sender
            .create_key_update(KeyUpdateRequest::UpdateNotRequested)
            .expect("send second update");
        coalesced.extend(engine_packets(&mut sender, now).concat());

        peer.parse_packet(&coalesced)
            .expect("receive old and new updates together");
        for packet in engine_packets(&mut peer, now) {
            sender.parse_packet(&packet).expect("receive duplicate ACK");
            engine_packets(&mut sender, now);
        }
        assert!(
            sender.is_key_update_in_flight() || peer.has_complete_handshake(MessageType::KeyUpdate),
            "ACK must not retire a fresh KeyUpdate that the receiver discarded"
        );
    }

    #[cfg(feature = "rcgen")]
    fn key_update_ack_after_retransmission(lost: bool) {
        let now = Instant::now();
        let mut sender = protected_test_engine();
        let mut peer = protected_test_engine();
        sender
            .create_key_update(KeyUpdateRequest::UpdateNotRequested)
            .expect("send KeyUpdate");
        sender.handle_timeout(now).expect("arm KeyUpdate timer");
        for packet in engine_packets(&mut sender, now) {
            peer.parse_packet(&packet).expect("receive KeyUpdate");
            engine_packets(&mut peer, now);
        }
        let mut buffer = Buf::new();
        assert!(
            peer.next_handshake_no_transcript(MessageType::KeyUpdate, &mut buffer)
                .expect("parse KeyUpdate")
                .is_some()
        );
        peer.advance_peer_handshake_seq();
        peer.update_recv_keys().expect("rotate peer receive keys");
        peer.send_ack_retransmittable()
            .expect("ACK original KeyUpdate");
        let original_ack = engine_packets(&mut peer, now);
        assert!(!original_ack.is_empty());

        sender
            .handle_timeout(now + Duration::from_secs(2))
            .expect("first KeyUpdate resend");
        assert!(!engine_packets(&mut sender, now + Duration::from_secs(2)).is_empty());
        let later = now + Duration::from_secs(7);
        sender
            .handle_timeout(later)
            .expect("second KeyUpdate resend");
        let retransmission = engine_packets(&mut sender, later);
        assert!(!retransmission.is_empty());
        let ack = if lost {
            for packet in retransmission {
                peer.parse_packet(&packet)
                    .expect("receive retransmitted KeyUpdate");
                engine_packets(&mut peer, later);
            }
            // Model a peer repeating the original ACK after the first copy was lost.
            peer.flight_resend("repeat KeyUpdate ACK")
                .expect("resend saved ACK");
            let replies = engine_packets(&mut peer, later);
            assert!(!replies.is_empty(), "peer must retransmit its saved ACK");
            replies
        } else {
            original_ack
        };
        for packet in ack {
            sender.parse_packet(&packet).expect("receive KeyUpdate ACK");
            engine_packets(&mut sender, later);
        }

        assert!(
            !sender.is_key_update_in_flight(),
            "earlier transmission must be ACKed"
        );
        assert_eq!(
            sender.app_send_epoch(),
            4,
            "ACK must rotate send keys exactly once"
        );
        assert!(sender.flight_saved_records.is_empty());
        let future = now + Duration::from_secs(120);
        sender.handle_timeout(future).expect("stay connected");
        assert!(engine_packets(&mut sender, future).is_empty());

        sender
            .create_ciphertext_record(ContentType::ApplicationData, 4, false, |fragment| {
                fragment.extend_from_slice(b"updated keys");
            })
            .expect("send on updated epoch");
        for packet in engine_packets(&mut sender, future) {
            peer.parse_packet(&packet).expect("decrypt updated epoch");
            let mut buffer = [0; 2048];
            assert!(matches!(
                peer.poll_output(&mut buffer, future),
                Output::ApplicationData(b"updated keys")
            ));
            assert!(engine_packets(&mut peer, future).is_empty());
        }
    }

    #[test]
    #[cfg(feature = "rcgen")]
    fn key_update_accepts_late_ack_for_original_transmission() {
        key_update_ack_after_retransmission(false);
    }

    #[test]
    #[cfg(feature = "rcgen")]
    fn key_update_recovers_after_lost_ack() {
        key_update_ack_after_retransmission(true);
    }

    #[test]
    #[cfg(feature = "rcgen")]
    fn exhausted_transmission_history_preserves_flight_and_accepts_old_ack() {
        let now = Instant::now();
        let mut engine = protected_test_engine();
        engine.flight_begin(5);
        engine
            .create_ciphertext_record(ContentType::Handshake, 2, true, |fragment| fragment.push(0))
            .expect("save flight record");
        engine_packets(&mut engine, now);
        for _ in 0..MAX_PREVIOUS_TRANSMISSIONS {
            engine
                .flight_resend("fill history")
                .expect("resend within capacity");
            engine_packets(&mut engine, now);
        }
        engine
            .create_ciphertext_record(ContentType::Handshake, 2, true, |fragment| fragment.push(1))
            .expect("save another flight record");
        engine_packets(&mut engine, now);
        // Put the record with room first: no partial resend may precede the error.
        engine.flight_saved_records.swap(0, 1);
        let next_seq = engine.hs_send_seq;
        assert_eq!(
            engine.flight_resend("history full"),
            Err(Error::TransmitHistoryFull)
        );
        assert_eq!(engine.hs_send_seq, next_seq);
        assert!(engine_packets(&mut engine, now).is_empty());
        assert_eq!(engine.flight_saved_records.len(), 2);

        engine
            .process_ack(&ack_for(&[(2, 0)]))
            .expect("ACK oldest transmission");
        assert!(engine.flight_saved_records[1].acked);
        engine
            .flight_resend("skip ACKed full history")
            .expect("resend remaining record");
        assert_eq!(engine.hs_send_seq, next_seq + 1);
        engine_packets(&mut engine, now);
        engine
            .process_ack(&ack_for(&[(2, next_seq)]))
            .expect("ACK remaining record");
        assert!(engine.flight_saved_records.is_empty());
        assert!(engine.timers.poll_timeout(now) > now + Duration::from_secs(60));
    }

    #[test]
    #[cfg(feature = "rcgen")]
    fn plaintext_and_ack_retransmissions_do_not_exhaust_history() {
        let now = Instant::now();
        let mut engine = protected_test_engine();
        engine
            .create_plaintext_record(ContentType::Handshake, true, |fragment| fragment.push(0))
            .expect("save plaintext record");
        engine
            .create_ciphertext_record(ContentType::Ack, 3, true, |fragment| {
                fragment.extend_from_slice(&[0, 0]);
            })
            .expect("save ACK record");
        engine_packets(&mut engine, now);
        for _ in 0..MAX_PREVIOUS_TRANSMISSIONS + 2 {
            engine
                .flight_resend("duplicate triggers resend")
                .expect("unbounded ACK resend");
            engine_packets(&mut engine, now);
        }
        assert!(
            engine
                .flight_saved_records
                .iter()
                .all(|entry| entry.fragment.len() == entry.fragment_len)
        );
    }
}
