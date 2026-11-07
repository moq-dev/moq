use std::mem;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use super::queue::{QueueRx, QueueTx};
use crate::buffer::{Buf, BufferPool, TmpBuf};
use crate::crypto::{Aad, Iv, Nonce};
use crate::dtls12::context::{AuthMode, CryptoContext};
use crate::dtls12::incoming::{Incoming, Record, RecordHandler};
use crate::dtls12::message::{Body, HashAlgorithm, Header, MessageType, ProtocolVersion, Sequence};
use crate::dtls12::message::{ContentType, DTLSRecord, Dtls12CipherSuite, Handshake};
use crate::error::bounded_error_len;
use crate::timer::{HandshakeTimers, Timeout, deadline};
use crate::window::ReplayWindow;
use crate::{Config, Error, InternalError, Output, SeededRng};

const MAX_DEFRAGMENT_PACKETS: usize = 50;

// Using debug_ignore_primary since CryptoContext doesn't implement Debug
pub struct Engine {
    config: Arc<Config>,

    /// Seedable random number generator for deterministic testing
    pub(crate) rng: SeededRng,

    /// Pool of buffers
    buffers_free: BufferPool,

    /// Counters for sending DTLSRecord during epoch 0.
    ///
    /// This is kept separate since resends might force us to
    /// "go back" to these sequence number even if we technically
    /// progressed to epoch 1.
    sequence_epoch_0: Sequence,

    /// Counters for epoch 1 and beyond.
    sequence_epoch_n: Sequence,

    /// Queue of incoming packets.
    queue_rx: QueueRx,

    /// Queue of outgoing packets.
    queue_tx: QueueTx,

    /// The cipher suite in use. Set by ServerHello.
    cipher_suite: Option<Dtls12CipherSuite>,

    /// Per-record explicit nonce length, cached from the provider suite at
    /// `set_cipher_suite` time. 0 for ChaCha20-Poly1305, 8 for AES-GCM.
    explicit_nonce_len: usize,

    /// Minimum length of a protected record's encrypted fragment, cached from
    /// the provider suite. See
    /// [`crate::crypto::SupportedDtls12CipherSuite::min_protected_fragment_len`].
    /// For AEAD suites this is `explicit_nonce + tag`; used both to reject
    /// short incoming records and to size outgoing record overhead.
    min_protected_fragment_len: usize,

    /// Cryptographic context for handling encryption/decryption
    pub(crate) crypto_context: CryptoContext,

    /// Whether the remote peer has enabled encryption
    peer_encryption_enabled: bool,

    /// Whether this engine is for a client (true) or server (false)
    is_client: bool,

    /// Expected peer handshake sequence number
    peer_handshake_seq_no: u16,

    /// Next handshake message sequence number for sending
    next_handshake_seq_no: u16,

    /// Handshakes collected for hash computation.
    ///
    /// NB: pub(crate) because we need to sign it in client.rs
    pub(crate) transcript: Buf,

    /// Anti-replay window state (per current epoch)
    replay: ReplayWindow,

    /// The records that have been sent in the current flight.
    flight_saved_records: Vec<Entry>,

    timers: HandshakeTimers,

    /// Cooldown for duplicate-triggered resends of the current flight.
    /// Disabled allows a resend; Unarmed and Armed suppress it. Expiry only
    /// allows another duplicate response, including after periodic retries stop.
    flight_dupe_timeout: Timeout,

    /// Whether we are ready to release application data from poll_output.
    release_app_data: bool,

    /// Whether we have confirmation that the peer completed the handshake, i.e.
    /// that the peer received our final flight. Once set, a stale plaintext
    /// handshake (which is unauthenticated and replayable after encryption is
    /// enabled) no longer triggers a courtesy flight retransmission.
    ///
    /// This is set from two signals, each meaningful for one role:
    /// - the client stops its resend timer when it completes, which means the
    ///   server received our final flight (the client confirms at completion);
    /// - receiving authenticated application data, which means the peer is past
    ///   its handshake (the server's only proof the client received flight 6).
    peer_handshake_confirmed: bool,

    /// Whether a close_notify alert has been received from the peer.
    close_notify_received: bool,

    /// Whether [`Output::CloseNotify`] has already been emitted.
    close_notify_reported: bool,
}

#[derive(Debug)]
struct Entry {
    content_type: ContentType,
    epoch: u16,
    fragment: Buf,
}

enum PollOutput<'a> {
    Data(&'a [u8]),
    BufferTooSmall { needed: usize },
    None(&'a mut [u8]),
}

impl Engine {
    pub fn new(config: Arc<Config>, auth: AuthMode) -> Self {
        let mut rng = SeededRng::new(config.rng_seed());

        let timers = HandshakeTimers::new(&config, &mut rng);

        let crypto_context = CryptoContext::new(auth, Arc::clone(&config));
        let mut buffers_free = BufferPool::default();
        let transcript = buffers_free.pop();

        Self {
            config,
            rng,
            buffers_free,
            sequence_epoch_0: Sequence::new(0),
            sequence_epoch_n: Sequence::new(1),
            queue_rx: QueueRx::new(),
            queue_tx: QueueTx::new(),
            cipher_suite: None,
            explicit_nonce_len: 0,
            min_protected_fragment_len: 0,
            crypto_context,
            peer_encryption_enabled: false,
            is_client: false,
            peer_handshake_seq_no: 0,
            next_handshake_seq_no: 0,
            transcript,
            replay: ReplayWindow::new(),
            flight_saved_records: Vec::new(),
            timers,
            flight_dupe_timeout: Timeout::Disabled,
            release_app_data: false,
            peer_handshake_confirmed: false,
            close_notify_received: false,
            close_notify_reported: false,
        }
    }

    pub fn set_client(&mut self, is_client: bool) {
        self.is_client = is_client;
    }

    pub fn set_handshake_deadline(&mut self, deadline: Timeout) {
        self.timers.set_handshake_deadline(deadline);
    }

    pub fn start_handshake(&mut self, now: Instant) {
        self.timers.start_handshake(now);
    }

    /// Restore an already emitted hybrid ClientHello and its outstanding timers.
    pub fn inject_hybrid_client_hello(&mut self, fragment: &[u8], timers: HandshakeTimers) {
        self.timers = timers;
        self.transcript.extend_from_slice(fragment);
        let mut saved_fragment = self.pop_buffer();
        saved_fragment.extend_from_slice(fragment);
        self.flight_saved_records.push(Entry {
            content_type: ContentType::Handshake,
            epoch: 0,
            fragment: saved_fragment,
        });
        self.next_handshake_seq_no = 1;
        self.sequence_epoch_0.sequence_number += 1;
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Get a reference to the cipher suite
    pub fn cipher_suite(&self) -> Option<Dtls12CipherSuite> {
        self.cipher_suite
    }

    /// Minimum length of a protected record's encrypted fragment for the
    /// negotiated suite. See
    /// [`crate::crypto::SupportedDtls12CipherSuite::min_protected_fragment_len`].
    pub fn min_protected_fragment_len(&self) -> usize {
        self.min_protected_fragment_len
    }

    /// Is the given cipher suite allowed by configuration
    pub fn is_cipher_suite_allowed(&self, suite: Dtls12CipherSuite) -> bool {
        self.config
            .dtls12_cipher_suites()
            .any(|cs| cs.suite() == suite)
    }

    /// Get a reference to the crypto context
    pub fn crypto_context(&self) -> &CryptoContext {
        &self.crypto_context
    }

    /// Get a mutable reference to the crypto context
    pub fn crypto_context_mut(&mut self) -> &mut CryptoContext {
        &mut self.crypto_context
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
            if !self.peer_encryption_enabled && incoming.first().record().sequence.epoch != 0 {
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
                if record.record().sequence.epoch != 0 {
                    record.set_handled();
                }
            }
        }
        self.purge_handled_queue_rx();
    }

    /// Insert a parsed datagram into the receive queue.
    fn insert_incoming(&mut self, incoming: Incoming) -> Result<bool, Error> {
        // Capacity guard before iterating records.
        if self.queue_rx.len() >= self.config.max_queue_rx() {
            warn!(
                "Receive queue full (max {}): {:?}",
                self.config.max_queue_rx(),
                self.queue_rx
            );
            self.recycle_incoming(incoming);
            return Err(Error::ReceiveQueueFull);
        }

        // Dispatch to specialized handlers
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

        // Some MessageType when resent, means we must trigger
        // an immediate resend of the entire flight. Once the peer is confirmed
        // to have completed the handshake, a stale plaintext handshake is just
        // unauthenticated noise (or a replay/amplification attempt) and must not
        // drive a resend.
        if let Some(dupe_seq) = maybe_dupe_seq
            && dupe_seq < self.peer_handshake_seq_no
            && !self.peer_handshake_confirmed
            && let Err(error) = self.flight_resend_on_dupe()
        {
            self.recycle_incoming(incoming);
            return Err(error);
        }

        // Drop old duplicates we've already processed - don't let them block newer messages.
        if handshake.header.message_seq < self.peer_handshake_seq_no {
            self.recycle_incoming(incoming);
            return Ok(false);
        }

        if self.peer_encryption_enabled && first_record.record().sequence.epoch == 0 {
            // Keep old plaintext handshake records available long enough to
            // trigger flight resends above, but never queue or process them as
            // new messages after peer encryption is enabled.
            self.recycle_incoming(incoming);
            return Ok(false);
        }

        // Reject new handshakes after initial handshake is complete (renegotiation not supported).
        if self.release_app_data && handshake.header.message_seq >= self.peer_handshake_seq_no {
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
                // Insert in order of handshake key
                self.queue_rx.insert(index, incoming);
            }
            Ok(_) => {
                // Exact duplicate handshake fragment
                self.recycle_incoming(incoming);
            }
        }

        Ok(is_client_hello)
    }

    fn insert_incoming_non_handshake(&mut self, incoming: Incoming) -> Result<(), Error> {
        let first = incoming.first();
        let seq_current = first.record().sequence;

        if self.peer_encryption_enabled
            && seq_current.epoch == 0
            && first.record().content_type == ContentType::Handshake
        {
            self.recycle_incoming(incoming);
            return Ok(());
        }

        if self.peer_encryption_enabled {
            for record in incoming.records().iter() {
                if record.record().sequence.epoch == 0
                    && record.record().content_type == ContentType::Handshake
                {
                    if record.handshakes().is_empty() {
                        record.set_handled();
                    } else {
                        for handshake in record.handshakes() {
                            handshake.set_handled();
                        }
                    }
                }
            }
        }

        let search_result = self
            .queue_rx
            .binary_search_by_key(&seq_current, |item| item.first().record().sequence);

        match search_result {
            Err(index) => self.queue_rx.insert(index, incoming),
            Ok(_) => {
                // Exact duplicate. For epoch 0 these come from resends. Before
                // peer encryption is enabled, epoch-1 records are queued
                // undecrypted and bypass the replay window, so duplicates can
                // land here too. After that, Record::parse replay-checks every
                // epoch-1 record before it is queued.
                assert!(seq_current.epoch == 0 || !self.peer_encryption_enabled);
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
        if let Timeout::Armed(deadline) = self.flight_dupe_timeout
            && now >= deadline
        {
            self.flight_dupe_timeout = Timeout::Disabled;
        }
        if self
            .timers
            .handle_timeout(now, &mut self.rng)
            .map_err(Error::Timeout)?
        {
            self.flight_resend("flight timeout")?;
            self.flight_dupe_timeout = Timeout::Unarmed;
        }

        Ok(())
    }

    pub fn poll_output<'a>(&mut self, buf: &'a mut [u8], now: Instant) -> Output<'a> {
        // Drain incoming queue of processed records.
        self.purge_handled_queue_rx();

        let buf = match self.poll_app_data(buf) {
            PollOutput::Data(p) => return Output::ApplicationData(p),
            PollOutput::BufferTooSmall { needed } => return Output::BufferTooSmall { needed },
            PollOutput::None(b) => b,
        };

        match self.poll_packet_tx(buf) {
            PollOutput::Data(p) => {
                if self.is_client {
                    self.timers.start_handshake(now);
                }
                self.timers.flight_sent(now);
                if self.flight_dupe_timeout == Timeout::Unarmed {
                    self.flight_dupe_timeout = Timeout::Armed(deadline(now, self.timers.rto()));
                }
                return Output::Packet(p);
            }
            PollOutput::BufferTooSmall { needed } => return Output::BufferTooSmall { needed },
            PollOutput::None(_) => {}
        }

        if self.close_notify_received && !self.close_notify_reported {
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

    fn poll_timeout(&self, now: Instant) -> Instant {
        let timeout = self.timers.poll_timeout(now);
        match self.flight_dupe_timeout {
            Timeout::Armed(deadline) => timeout.min(deadline),
            _ => timeout,
        }
    }

    pub fn flight_begin(&mut self, flight_no: u8) {
        debug!("Begin flight {}", flight_no);
        self.timers.begin_flight(&mut self.rng);
        self.flight_clear_resends();
        self.flight_dupe_timeout = Timeout::Disabled;
    }

    pub fn flight_stop_resend_timers(&mut self) {
        debug!("Stop connect and flight timeouts");
        self.timers.stop();

        // The client stops its resend timer only once it has received the
        // server's final flight, which proves the server received the client's
        // final flight — so the peer is confirmed. The server, by contrast,
        // stops its timer right after sending flight 6, before the client has
        // confirmed anything, so it must NOT confirm here (it relies on later
        // authenticated application data instead).
        if self.is_client {
            self.peer_handshake_confirmed = true;
            self.flight_dupe_timeout = Timeout::Disabled;
        }
    }

    fn flight_clear_resends(&mut self) {
        for entry in self.flight_saved_records.drain(..) {
            self.buffers_free.push(entry.fragment);
        }
    }

    fn flight_resend_on_dupe(&mut self) -> Result<(), Error> {
        // Rate-limit duplicate responses and avoid spending a retry when there
        // is no saved flight.
        if self.flight_dupe_timeout != Timeout::Disabled || self.flight_saved_records.is_empty() {
            return Ok(());
        }

        // Reserve a resend from the same budget used by timed retries, so
        // duplicates cannot bypass flight_retries. This also advances backoff,
        // but allows final-flight responses after local completion regardless
        // of budget (RFC 6347 section 4.2.4).
        let resend_requested = self.timers.request_resend(&mut self.rng);
        if !resend_requested {
            return Ok(());
        }

        self.flight_resend("dupe triggers resend")?;
        self.flight_dupe_timeout = Timeout::Unarmed;
        Ok(())
    }

    fn flight_resend(&mut self, reason: &str) -> Result<(), Error> {
        debug!("Resending flight due to {}", reason);

        let replace_pending_handshake_output = !self.release_app_data;

        if replace_pending_handshake_output {
            self.queue_tx.clear();
        }

        // For lifetime issues, we take the entries out of self
        let records = mem::take(&mut self.flight_saved_records);

        let mut result = Ok(());
        for (index, entry) in records.iter().enumerate() {
            result = self.create_record_inner(
                entry.content_type,
                entry.epoch,
                false,
                replace_pending_handshake_output && index == 0,
                |fragment| {
                    fragment.extend_from_slice(&entry.fragment);
                },
            );
            if result.is_err() {
                break;
            }
        }

        // Put the entries back into self
        self.flight_saved_records = records;

        result
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
            // Cap to MAX_DEFRAGMENT_PACKETS to avoid misbehaving peers
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
            // A different seq means we're looking at a different handshake
            if wanted_seq != h.header.message_seq {
                continue;
            }

            // Check fragment contiguity
            if h.header.fragment_offset != last_fragment_end {
                return false;
            }
            last_fragment_end = h.header.fragment_offset + h.header.fragment_length;

            // Found the last fragment to complete the wanted handshake.
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

        // This sets the handled flag on the handshake.
        // Passing Some(&mut self.transcript) to have defragment write to transcript
        // before creating the handshake, avoiding borrow conflicts.
        let handshake = Handshake::defragment(
            iter,
            defragment_buffer,
            self.cipher_suite,
            Some(&mut self.transcript),
        )?;

        // Move the expected seq_no along
        self.peer_handshake_seq_no = handshake.header.message_seq + 1;

        Ok(Some(handshake))
    }

    pub(crate) fn next_record(&mut self, ctype: ContentType) -> Option<&Record> {
        let record = self
            .queue_rx
            .iter()
            .flat_map(|i| i.records().iter())
            .find(|r| !r.is_handled())?;

        if record.record().content_type != ctype {
            return None;
        }

        record.set_handled();

        Some(record)
    }

    /// Mark any pending ChangeCipherSpec records as handled and purge them.
    /// We can accumulate multiple ChangeCipherSpec due to resends. Since they
    /// don't have any Handshake message_seq and each resend gives a new DTLSRecord
    /// sequence number, we might have multiple.
    pub fn drop_pending_ccs(&mut self) {
        for incoming in self.queue_rx.iter() {
            for record in incoming.records().iter() {
                if record.record().content_type == ContentType::ChangeCipherSpec {
                    record.set_handled();
                }
            }
        }
    }

    /// Create a DTLS record and serialize it into a buffer
    pub fn create_record<F>(
        &mut self,
        content_type: ContentType,
        epoch: u16,
        save_fragment: bool,
        f: F,
    ) -> Result<(), Error>
    where
        F: FnOnce(&mut Buf),
    {
        self.create_record_inner(content_type, epoch, save_fragment, false, f)
    }

    fn create_record_inner<F>(
        &mut self,
        content_type: ContentType,
        epoch: u16,
        save_fragment: bool,
        force_new_datagram: bool,
        f: F,
    ) -> Result<(), Error>
    where
        F: FnOnce(&mut Buf),
    {
        let maybe_suite = if epoch >= 1 {
            Some(self.cipher_suite().ok_or(Error::InvalidState(
                crate::InvalidStateError::NoCipherSuiteSelected,
            ))?)
        } else {
            None
        };

        // Prepare the plaintext fragment
        let mut fragment = self.buffers_free.pop();

        // Let the caller fill the fragment (plaintext)
        f(&mut fragment);

        // Use this as a marker to know whether we are to record fragments for resends.
        if save_fragment {
            let mut clone = self.buffers_free.pop();
            clone.extend_from_slice(&fragment);
            self.flight_saved_records.push(Entry {
                content_type,
                epoch,
                fragment: clone,
            });
        }

        // Compute wire length of the record if serialized into a datagram.
        // Record header (13) + handshake/change/app data bytes + per-suite
        // protection overhead (if epoch >= 1). For AEAD suites the protection
        // overhead equals the min-protected-fragment-len.
        let overhead = if maybe_suite.is_some() {
            self.min_protected_fragment_len()
        } else {
            0
        };
        let record_wire_len = DTLSRecord::HEADER_LEN + fragment.len() + overhead;

        // Decide whether to append to the existing last datagram or create a new one
        let can_append = self
            .queue_tx
            .back()
            .map(|b| !force_new_datagram && b.len() + record_wire_len <= self.config.mtu())
            .unwrap_or(false);

        // If we cannot append, ensure we have space for a new datagram
        if !can_append && self.queue_tx.len() >= self.config.max_queue_tx() {
            warn!(
                "Transmit queue full (max {}): {:?}",
                self.config.max_queue_tx(),
                self.queue_tx
            );
            return Err(Error::TransmitQueueFull);
        }

        // Sequence number to use for this record
        let sequence = if epoch == 0 {
            self.sequence_epoch_0
        } else {
            self.sequence_epoch_n
        };
        let length = fragment.len() as u16;

        // Handle encryption for epochs >= 1
        if epoch >= 1 {
            let suite = maybe_suite.expect("cipher suite must be set for encrypted epochs");

            // Get the fixed part of the IV
            let iv = if self.is_client {
                self.crypto_context.get_client_write_iv()
            } else {
                self.crypto_context.get_server_write_iv()
            };

            let Some(iv) = iv else {
                return Err(Error::CryptoError(
                    crate::CryptoError::WriteIvNotAvailable {
                        is_client: self.is_client,
                    },
                ));
            };

            let explicit_nonce_len = self.explicit_nonce_len;
            let mut explicit_nonce = [0u8; DTLSRecord::EXPLICIT_NONCE_LEN];
            let seq64 = ((sequence.epoch as u64) << 48) | sequence.sequence_number;
            let nonce = match explicit_nonce_len {
                0 => Nonce::xor(iv.as_12_bytes(), seq64),
                DTLSRecord::EXPLICIT_NONCE_LEN => {
                    explicit_nonce = self.rng.random();
                    Nonce::new(iv, &explicit_nonce)
                }
                _ => {
                    return Err(Error::CryptoError(
                        crate::CryptoError::UnsupportedDtls12RecordIvLen {
                            len: bounded_error_len(explicit_nonce_len),
                            suite,
                        },
                    ));
                }
            };

            // DTLS 1.2 AEAD: AAD uses the plaintext length (DTLSCompressed.length).
            let aad = Aad::new_dtls12(content_type, sequence, length);

            // Encrypt the fragment in-place
            self.encrypt_data(&mut fragment, aad, nonce)?;
            let ctext_len = fragment.len();

            // For suites with a per-record nonce (e.g. AES-GCM), prefix it on the wire.
            if explicit_nonce_len > 0 {
                fragment.resize(explicit_nonce_len + ctext_len, 0);
                fragment.copy_within(0..ctext_len, explicit_nonce_len);
                fragment[..explicit_nonce_len]
                    .copy_from_slice(&explicit_nonce[..explicit_nonce_len]);
            }
        }

        // Build the record structure referencing the (possibly encrypted) fragment
        let record = DTLSRecord {
            content_type,
            version: ProtocolVersion::DTLS1_2,
            sequence,
            length: fragment.len() as u16,
            fragment_range: 0..fragment.len(),
        };

        // Increment the sequence number for the next transmission
        if epoch == 0 {
            self.sequence_epoch_0.sequence_number += 1;
        } else {
            self.sequence_epoch_n.sequence_number += 1;
        }

        // Serialize the record into the chosen datagram buffer
        if can_append {
            let last = self.queue_tx.back_mut().unwrap();
            record.serialize(&fragment, last);
        } else {
            let mut buffer = self.buffers_free.pop();
            buffer.clear();
            record.serialize(&fragment, &mut buffer);
            self.queue_tx.push_back(buffer);
        }

        // Return the fragment buffer to the pool
        self.buffers_free.push(fragment);

        Ok(())
    }

    /// Create a handshake message and wrap it in a DTLS record
    pub fn create_handshake<F>(&mut self, msg_type: MessageType, f: F) -> Result<(), Error>
    where
        F: FnOnce(&mut Buf, &mut Self) -> Result<(), Error>,
    {
        // Get a buffer for the handshake body
        let mut body_buffer = self.buffers_free.pop();

        // Let the callback fill the handshake body
        f(&mut body_buffer, self)?;

        // Create the handshake header with the next sequence number
        let handshake_header = Header {
            msg_type,
            length: body_buffer.len() as u32,
            message_seq: self.next_handshake_seq_no,
            fragment_offset: 0,
            fragment_length: body_buffer.len() as u32,
        };

        let mut buffer_full = self.buffers_free.pop();
        {
            let handshake = Handshake {
                header: handshake_header,
                body: Body::Fragment(0..body_buffer.len()),
                handled: AtomicBool::new(false),
            };
            // Serialize with body_buffer as source
            handshake.serialize(&body_buffer, &mut buffer_full);
        }
        self.transcript.extend_from_slice(&buffer_full);
        self.buffers_free.push(buffer_full);

        // Increment the sequence number for the next handshake message
        self.next_handshake_seq_no += 1;

        // We want to pack as much as possible into the outgoing datagram and
        // remain within the MTU. Fragment the handshake across records as needed.

        let epoch = msg_type.epoch();
        let total_len = body_buffer.len();
        let mut offset: usize = 0;

        // Handshake header is 12 bytes
        let handshake_header_len = 12usize;
        // Per-record protection overhead on the wire (for AEAD suites this is
        // explicit_nonce + tag). Used to size fragments to fit the MTU.
        let protection_overhead = if epoch >= 1 {
            self.cipher_suite().ok_or(Error::InvalidState(
                crate::InvalidStateError::NoCipherSuiteSelected,
            ))?;
            self.min_protected_fragment_len()
        } else {
            0
        };

        // At least one record must be created even if total_len == 0
        while offset < total_len || (total_len == 0 && offset == 0) {
            // How many bytes are already used in the current datagram (if any)?
            let already_used_in_current = self.queue_tx.back().map(|b| b.len()).unwrap_or(0);
            let available_in_current = self.config.mtu().saturating_sub(already_used_in_current);

            // Fixed overhead per handshake record on the wire:
            // DTLS record header + handshake header + protection overhead (if epoch >= 1)
            let fixed_overhead =
                DTLSRecord::HEADER_LEN + handshake_header_len + protection_overhead;

            // Prefer to pack into the current datagram. If the current one cannot fit even
            // the fixed overhead, we will start a fresh datagram and compute space again.
            let available_for_body = if available_in_current > fixed_overhead {
                // There is room for at least 1 byte of handshake body in the current datagram
                available_in_current - fixed_overhead
            } else {
                // Not enough space in the current datagram for any body bytes; start a fresh datagram
                self.config.mtu().saturating_sub(fixed_overhead)
            };

            // Remaining bytes from the handshake body we still need to send.
            let remaining_body_bytes = total_len.saturating_sub(offset);

            // For empty-body handshakes (e.g., ServerHelloDone), we still send a header-only record.
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

            // Emit the record; packing into current datagram happens inside create_record
            self.create_record(ContentType::Handshake, epoch, true, |fragment| {
                // Serialize with body_buffer as source
                frag_handshake.serialize(&body_buffer, fragment);
            })?;

            if total_len == 0 {
                // Nothing more to send for empty-body handshake
                break;
            }

            offset += chunk_len;
        }

        // Return the buffer
        self.buffers_free.push(body_buffer);

        Ok(())
    }

    /// Release application data from the incoming queue
    pub fn release_application_data(&mut self) {
        self.release_app_data = true;
    }

    /// Whether a close_notify alert has been received from the peer.
    pub fn close_notify_received(&self) -> bool {
        self.close_notify_received
    }

    /// Whether a received close_notify still needs to be surfaced.
    pub fn close_notify_pending(&self) -> bool {
        self.close_notify_received && !self.close_notify_reported
    }

    /// Whether close-related output remains to be polled.
    pub fn has_pending_close_output(&self) -> bool {
        !self.queue_tx.is_empty() || self.close_notify_pending()
    }

    /// Discard all pending outgoing data.
    ///
    /// RFC 5246 §7.2.1: on receiving close_notify, discard any pending writes.
    pub fn discard_pending_writes(&mut self) {
        self.queue_tx.clear();
    }

    /// Abort the connection: flush all queued output, retransmission state, and
    /// disable timers so that no further packets are emitted.
    pub fn abort(&mut self) {
        self.queue_tx.clear();
        self.flight_saved_records.clear();
        self.flight_dupe_timeout = Timeout::Disabled;
        self.timers.stop();
    }

    /// Pop a buffer from the buffer pool for temporary use
    pub(crate) fn pop_buffer(&mut self) -> Buf {
        self.buffers_free.pop()
    }

    /// Return a buffer to the buffer pool
    pub(crate) fn push_buffer(&mut self, buf: Buf) {
        self.buffers_free.push(buf);
    }

    /// Encrypt data appropriate for the role (client or server)
    fn encrypt_data(&mut self, plaintext: &mut Buf, aad: Aad, nonce: Nonce) -> Result<(), Error> {
        if self.is_client {
            self.crypto_context
                .encrypt_client_to_server(plaintext, aad, nonce)
                .map_err(Error::CryptoError)
        } else {
            self.crypto_context
                .encrypt_server_to_client(plaintext, aad, nonce)
                .map_err(Error::CryptoError)
        }
    }

    /// Decrypt data appropriate for the role (client or server)
    pub fn decrypt_data(
        &mut self,
        ciphertext: &mut TmpBuf,
        aad: Aad,
        nonce: Nonce,
    ) -> Result<(), Error> {
        if self.is_client {
            self.crypto_context
                .decrypt_server_to_client(ciphertext, aad, nonce)
                .map_err(Error::CryptoError)
        } else {
            self.crypto_context
                .decrypt_client_to_server(ciphertext, aad, nonce)
                .map_err(Error::CryptoError)
        }
    }

    /// Reset server handshake state after sending HelloVerifyRequest.
    ///
    /// Per RFC 6347 §4.2.2, the HelloVerifyRequest exchange is stateless. After sending
    /// HVR, the server expects a fresh ClientHello containing the cookie with message_seq=1.
    ///
    /// The message flow per RFC 6347 §4.2.2:
    ///   ClientHello (seq=0)  ------>
    ///                    <------  HelloVerifyRequest (seq=0)
    ///   ClientHello (seq=1)  ------>  (with cookie)
    ///                    <------  ServerHello (seq=1)
    pub fn reset_server_for_hello_verify_request(&mut self) {
        self.transcript.clear();
        // Per RFC 6347 §4.2.2, the next ClientHello (with cookie) has message_seq=1.
        // We keep peer_handshake_seq_no at 1 (already incremented after first ClientHello).
        // Clear queued incoming handshakes so the next ClientHello (with cookie)
        // isn't rejected as a duplicate of the first ClientHello (without cookie).
        self.queue_rx.clear();
        // Note: Don't clear flight_saved_records here - the HelloVerifyRequest should
        // still be resendable via timeout until we receive the valid ClientHello with cookie.
        // The flight_begin(4) call when processing the cookie-bearing ClientHello will
        // clear the old records.
    }

    /// Reset client handshake state after receiving HelloVerifyRequest.
    ///
    /// Per RFC 6347 §4.2.2, the client sends the next ClientHello (with cookie) using
    /// message_seq=1. The transcript is cleared because the initial ClientHello and
    /// HelloVerifyRequest are not part of the handshake transcript per RFC 6347 §4.2.1.
    ///
    /// Note: next_handshake_seq_no is already 1 after sending the first ClientHello,
    /// so we don't reset it - the next ClientHello will correctly have message_seq=1.
    pub fn reset_client_for_hello_verify_request(&mut self) {
        self.transcript.clear();
        // Note: next_handshake_seq_no stays at 1 - the next ClientHello (with cookie)
        // will have message_seq=1 per RFC 6347 §4.2.2.
        // Note: peer_handshake_seq_no stays at 1 - the next message from server
        // (ServerHello) will have message_seq=1 per RFC 6347 §4.2.2.
    }

    pub fn transcript_hash(&self, algorithm: HashAlgorithm, out: &mut Buf) {
        let mut hash = self.crypto_context.create_hash(algorithm);
        hash.update(&self.transcript);
        hash.clone_and_finalize(out);
    }

    pub fn transcript(&self) -> &[u8] {
        &self.transcript
    }

    pub fn set_cipher_suite(&mut self, cipher_suite: Dtls12CipherSuite) {
        // Cache AEAD record parameters from the provider suite. The formula
        // (explicit_nonce + tag) lives on the suite trait; Engine just stores
        // the resolved values for hot-path access.
        let provider_suite = self
            .crypto_context
            .provider()
            .cipher_suites
            .iter()
            .find(|cs| cs.suite() == cipher_suite)
            .expect("cipher suite must be in provider");
        self.explicit_nonce_len = provider_suite.explicit_nonce_len();
        self.min_protected_fragment_len = provider_suite.min_protected_fragment_len();
        self.cipher_suite = Some(cipher_suite);
    }

    pub fn enable_peer_encryption(&mut self) -> Result<(), InternalError> {
        debug!("Peer encryption enabled");
        self.peer_encryption_enabled = true;

        let maybe_index_epoch1 = self
            .queue_rx
            .iter()
            .position(|i| i.records().iter().any(|r| r.record().sequence.epoch == 1));

        let Some(index_epoch1) = maybe_index_epoch1 else {
            return Ok(());
        };

        // Now decrypt all entries remaining.
        let all = self.queue_rx.split_off(index_epoch1);

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

    fn peer_iv(&self) -> Iv {
        if self.is_client {
            self.crypto_context
                .get_server_write_iv()
                .expect("Server write IV not available - keys not derived yet")
        } else {
            self.crypto_context
                .get_client_write_iv()
                .expect("Client write IV not available - keys not derived yet")
        }
    }

    pub fn decryption_aad_and_nonce(&self, dtls: &DTLSRecord, buf: &[u8]) -> (Aad, Nonce) {
        // DTLS 1.2 AEAD: AAD uses the plaintext length. Recover plaintext length
        // from the record header by subtracting this suite's wire overhead.
        let plaintext_len = dtls
            .length
            .saturating_sub(self.min_protected_fragment_len() as u16);
        let aad = Aad::new_dtls12(dtls.content_type, dtls.sequence, plaintext_len);
        let iv = self.peer_iv();
        let seq64 = ((dtls.sequence.epoch as u64) << 48) | dtls.sequence.sequence_number;
        let nonce = match self.explicit_nonce_len {
            0 => Nonce::xor(iv.as_12_bytes(), seq64),
            DTLSRecord::EXPLICIT_NONCE_LEN => Nonce::new(iv, dtls.nonce(buf)),
            len => Nonce::new(iv, dtls.nonce_with_len(buf, len)),
        };
        (aad, nonce)
    }

    pub fn generate_verify_data(&mut self, is_client: bool) -> Result<[u8; 12], Error> {
        let Some(suite) = self.cipher_suite() else {
            return Err(Error::InvalidState(
                crate::InvalidStateError::NoCipherSuiteSelected,
            ));
        };
        let algorithm = suite.hash_algorithm();
        let mut handshake_hash = self.buffers_free.pop();
        self.transcript_hash(algorithm, &mut handshake_hash);

        let suite_hash = suite.hash_algorithm();
        let mut out = self.buffers_free.pop();
        let mut scratch = self.buffers_free.pop();
        let verify_data_vec = self
            .crypto_context()
            .generate_verify_data(
                &handshake_hash,
                is_client,
                suite_hash,
                &mut out,
                &mut scratch,
            )
            .map_err(Error::CryptoError)?;

        if verify_data_vec.len() != 12 {
            return Err(Error::CryptoError(
                crate::CryptoError::InvalidVerifyDataLength,
            ));
        }

        let mut verify_data = [0u8; 12];
        verify_data.copy_from_slice(&verify_data_vec);

        self.buffers_free.push(handshake_hash);
        self.buffers_free.push(out);
        self.buffers_free.push(scratch);

        Ok(verify_data)
    }
}

impl RecordHandler for Engine {
    fn pop_buffer(&mut self) -> Buf {
        Engine::pop_buffer(self)
    }

    fn push_buffer(&mut self, buffer: Buf) {
        Engine::push_buffer(self, buffer);
    }

    fn classify_record(&mut self, record: Record) -> Result<Option<Record>, Error> {
        let epoch = record.record().sequence.epoch;

        if record.record().content_type == ContentType::ChangeCipherSpec
            && epoch == 0
            && self.peer_encryption_enabled
        {
            // DTLS 1.2 peers may retransmit their last handshake flight after
            // we have already enabled peer encryption. A late plaintext CCS is
            // no longer actionable; queuing it would leave an unhandled control
            // record in queue_rx and prevent handled app-data records behind it
            // from being purged.
            self.push_buffer(record.into_buffer());
            return Ok(None);
        }

        if record.record().content_type == ContentType::Handshake
            && epoch == 0
            && self.peer_encryption_enabled
        {
            // A stale plaintext handshake can request our previous flight, but
            // must not discard authenticated records alongside it in this datagram.
            let dupe_seq = record
                .first_handshake()
                .and_then(|handshake| handshake.dupe_triggers_resend());
            self.push_buffer(record.into_buffer());
            if dupe_seq.is_some_and(|seq| seq < self.peer_handshake_seq_no)
                && !self.peer_handshake_confirmed
            {
                self.flight_resend_on_dupe()?;
            }
            return Ok(None);
        }

        if record.record().content_type == ContentType::Alert {
            if epoch == 0 {
                if self.peer_encryption_enabled {
                    // Post-handshake: epoch 0 alerts are unauthenticated, discard.
                    self.push_buffer(record.into_buffer());
                    return Ok(None);
                }

                let fatal_description = {
                    let fragment = record.record().fragment(record.buffer());
                    (fragment.len() >= 2 && fragment[0] == 2).then(|| fragment[1])
                };
                self.push_buffer(record.into_buffer());

                if let Some(description) = fatal_description {
                    return Err(Error::SecurityError(crate::SecurityError::FatalAlert {
                        description,
                    }));
                }

                return Ok(None);
            }

            if !self.peer_encryption_enabled {
                // A CCS earlier in this datagram may enable authentication.
                // Otherwise discard_undecryptable() will discard this record.
                return Ok(Some(record));
            }

            let alert = {
                let fragment = record.record().fragment(record.buffer());
                (fragment.len() >= 2).then(|| (fragment[0], fragment[1]))
            };
            self.push_buffer(record.into_buffer());

            if let Some((level, description)) = alert {
                if description == 0 {
                    self.close_notify_received = true;
                    return Ok(None);
                }

                if level == 2 {
                    return Err(Error::SecurityError(crate::SecurityError::FatalAlert {
                        description,
                    }));
                }
            }

            return Ok(None);
        }

        if self.close_notify_received
            && record.record().content_type == ContentType::ApplicationData
        {
            self.push_buffer(record.into_buffer());
            return Ok(None);
        }

        Ok(Some(record))
    }

    fn is_peer_encryption_enabled(&self) -> bool {
        self.peer_encryption_enabled
    }

    fn replay_check(&self, seq: Sequence) -> bool {
        // Only epoch 1 (encrypted) records reach here; epoch 0 records are
        // returned early by the DTLS 1.2 incoming parser.
        self.replay.check(seq.sequence_number)
    }

    fn replay_update(&mut self, seq: Sequence) {
        self.replay.update(seq.sequence_number);
    }

    fn note_decrypted_record(&mut self, content_type: ContentType) {
        // A decrypted (so authenticated) application-data record proves the peer
        // is past its handshake, which means it received our final flight. Once
        // that's known, a stale plaintext handshake (unauthenticated, replayable)
        // must no longer drive a courtesy flight retransmission. The client
        // confirms separately at its own completion (flight_stop_resend_timers).
        if content_type == ContentType::ApplicationData {
            self.peer_handshake_confirmed = true;
            self.flight_dupe_timeout = Timeout::Disabled;
        }
    }

    fn decryption_aad_and_nonce(&self, dtls: &DTLSRecord, buf: &[u8]) -> (Aad, Nonce) {
        Engine::decryption_aad_and_nonce(self, dtls, buf)
    }

    fn explicit_nonce_len(&self) -> usize {
        self.explicit_nonce_len
    }

    fn min_protected_fragment_len(&self) -> usize {
        self.min_protected_fragment_len
    }

    fn decrypt_data(
        &mut self,
        ciphertext: &mut TmpBuf,
        aad: Aad,
        nonce: Nonce,
    ) -> Result<(), Error> {
        Engine::decrypt_data(self, ciphertext, aad, nonce)
    }

    fn can_discard_bad_protected_record(&self) -> bool {
        self.release_app_data
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn poll_packets(engine: &mut Engine, now: Instant) -> (usize, Instant) {
        let mut packets = 0;
        let mut output = [0; 2048];
        loop {
            match engine.poll_output(&mut output, now) {
                Output::Packet(_) => packets += 1,
                Output::Timeout(deadline) => return (packets, deadline),
                _ => panic!("unexpected engine output"),
            }
        }
    }

    fn duplicate_handshake(seq: u16) -> Vec<u8> {
        let mut packet = vec![22, 0xFE, 0xFD, 0, 0, 0, 0, 0, 0, 0, 1, 0, 12, 14, 0, 0, 0];
        packet.extend_from_slice(&seq.to_be_bytes());
        packet.extend_from_slice(&[0; 6]);
        packet
    }

    fn resend_engine(now: Instant) -> Engine {
        let config = Config::builder()
            .dangerously_set_rng_seed(42)
            .build()
            .expect("valid config");
        let mut engine = Engine::new(Arc::new(config), AuthMode::Psk);
        engine.peer_handshake_seq_no = 3;
        engine.flight_begin(4);
        engine
            .create_record(ContentType::ChangeCipherSpec, 0, true, |buf| buf.push(1))
            .expect("save a flight");
        assert_eq!(poll_packets(&mut engine, now).0, 1);
        engine.handle_timeout(now).expect("arm flight timer");
        assert_eq!(poll_packets(&mut engine, now).0, 0);
        engine
    }

    #[test]
    fn ordered_datagrams_can_arrive_out_of_order() {
        let now = Instant::now();
        let mut engine = Engine::new(Arc::new(Config::default()), AuthMode::Psk);
        engine.peer_handshake_seq_no = 2;
        let later = [duplicate_handshake(3), duplicate_handshake(4)].concat();
        engine
            .parse_packet(&later)
            .expect("queue sequences 3 and 4 first");
        assert_eq!(poll_packets(&mut engine, now).0, 0);
        assert!(!engine.has_complete_handshake(MessageType::ServerHelloDone));
        engine
            .parse_packet(&duplicate_handshake(2))
            .expect("queue sequence 2 later");
        assert_eq!(poll_packets(&mut engine, now).0, 0);

        let mut buffer = engine.pop_buffer();
        for seq in 2..=4 {
            let handshake = engine
                .next_handshake(MessageType::ServerHelloDone, &mut buffer)
                .expect("reassemble in message order")
                .expect("expected sequence is now available");
            assert_eq!(handshake.header.message_seq, seq);
            assert_eq!(poll_packets(&mut engine, now).0, 0);
        }
        engine.push_buffer(buffer);
    }

    #[test]
    fn duplicate_resend_restarts_backoff_and_suppresses_other_old_messages() {
        let now = Instant::now();
        let mut engine = resend_engine(now);
        let initial_deadline = poll_packets(&mut engine, now).1;
        let resend_at = initial_deadline - Duration::from_millis(1);
        engine.handle_timeout(resend_at).expect("advance time");
        assert_eq!(poll_packets(&mut engine, resend_at).0, 0);

        engine
            .parse_packet(&duplicate_handshake(1))
            .expect("first duplicate");
        let (packets, deadline) = poll_packets(&mut engine, resend_at);
        assert_eq!(packets, 1);
        assert_eq!(deadline, resend_at + engine.timers.rto());
        engine
            .handle_timeout(resend_at)
            .expect("resend backoff remains armed");
        assert_eq!(poll_packets(&mut engine, resend_at), (0, deadline));
        assert!(deadline > initial_deadline + Duration::from_secs(1));

        for seq in [1, 2, 1, 2] {
            engine
                .parse_packet(&duplicate_handshake(seq))
                .expect("further duplicate");
            assert_eq!(poll_packets(&mut engine, resend_at), (0, deadline));
        }
        engine
            .handle_timeout(initial_deadline)
            .expect("old deadline is cancelled");
        assert_eq!(poll_packets(&mut engine, initial_deadline), (0, deadline));
        engine
            .handle_timeout(deadline)
            .expect("retry at new deadline");
        let (packets, next_deadline) = poll_packets(&mut engine, deadline);
        assert_eq!(packets, 1);
        assert!(next_deadline - deadline > deadline - resend_at);
        engine
            .parse_packet(&duplicate_handshake(2))
            .expect("duplicate after timer resend");
        assert_eq!(poll_packets(&mut engine, deadline), (0, next_deadline));

        engine.flight_begin(6);
        engine
            .create_record(ContentType::ChangeCipherSpec, 0, true, |buf| buf.push(1))
            .expect("save new flight");
        assert_eq!(poll_packets(&mut engine, deadline).0, 1);
        engine
            .parse_packet(&duplicate_handshake(2))
            .expect("new flight duplicate");
        assert_eq!(poll_packets(&mut engine, deadline).0, 1);
    }

    #[test]
    fn final_flight_duplicates_back_off_without_periodic_retransmissions() {
        let now = Instant::now();
        let mut engine = resend_engine(now);
        engine.flight_stop_resend_timers();
        poll_packets(&mut engine, now);

        engine
            .parse_packet(&duplicate_handshake(1))
            .expect("first final-flight duplicate");
        let (packets, deadline) = poll_packets(&mut engine, now);
        assert_eq!(packets, 1);
        assert_eq!(deadline, now + engine.timers.rto());
        engine
            .handle_timeout(now)
            .expect("final-flight cooldown remains armed");
        assert_eq!(poll_packets(&mut engine, now), (0, deadline));
        assert!(deadline < now + Duration::from_secs(10));
        engine
            .parse_packet(&duplicate_handshake(2))
            .expect("further final-flight duplicate");
        assert_eq!(poll_packets(&mut engine, now), (0, deadline));

        engine.handle_timeout(deadline).expect("expire cooldown");
        assert_eq!(poll_packets(&mut engine, deadline).0, 0);
        engine
            .parse_packet(&duplicate_handshake(2))
            .expect("retry after cooldown");
        let (packets, next_deadline) = poll_packets(&mut engine, deadline);
        assert_eq!(packets, 1);
        assert_eq!(next_deadline, deadline + engine.timers.rto());
        engine
            .handle_timeout(deadline)
            .expect("longer cooldown remains armed");
        assert_eq!(poll_packets(&mut engine, deadline), (0, next_deadline));
        assert!(next_deadline - deadline > deadline - now);
    }

    #[test]
    fn duplicate_cooldown_starts_when_resend_is_emitted() {
        let now = Instant::now();
        let mut engine = resend_engine(now);
        engine.flight_stop_resend_timers();
        engine
            .parse_packet(&duplicate_handshake(1))
            .expect("queue final-flight resend");
        assert!(matches!(
            engine.poll_output(&mut [], now),
            Output::BufferTooSmall { .. }
        ));

        let emitted_at = now + Duration::from_secs(10);
        engine
            .handle_timeout(emitted_at)
            .expect("unsent resend has no cooldown");
        assert_eq!(engine.flight_dupe_timeout, Timeout::Unarmed);
        let (packets, deadline) = poll_packets(&mut engine, emitted_at);
        assert_eq!(packets, 1);
        assert_eq!(deadline, emitted_at + engine.timers.rto());
        engine
            .parse_packet(&duplicate_handshake(2))
            .expect("suppress duplicate during cooldown");
        assert_eq!(poll_packets(&mut engine, emitted_at), (0, deadline));
    }

    #[test]
    fn duplicate_datagram_recycles_its_pooled_buffer() {
        let mut engine = Engine::new(Arc::new(Config::default()), AuthMode::Psk);
        let packet = [
            0x14, 0xFE, 0xFD, // ChangeCipherSpec, DTLS 1.2.
            0, 0, // Epoch 0.
            0, 0, 0, 0, 0, 1, // Record sequence 1.
            0, 1, 1, // Fragment length 1 and ChangeCipherSpec body.
        ];
        engine.parse_packet(&packet).expect("queue first datagram");
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
        assert_eq!(engine.queue_rx.len(), 1);
        assert!(
            engine.pop_buffer().into_vec().capacity() >= 2048,
            "discarding a duplicate must preserve its reusable receive allocation"
        );
    }

    #[test]
    fn timing_resend_queue_failure_is_fatal() {
        let now = Instant::now();
        let config = Arc::new(
            Config::builder()
                .dangerously_set_rng_seed(42)
                .build()
                .expect("valid config"),
        );
        let mut engine = Engine::new(config, AuthMode::Psk);
        engine.flight_begin(1);
        engine
            .create_record(ContentType::Handshake, 0, true, |fragment| fragment.push(1))
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
}
