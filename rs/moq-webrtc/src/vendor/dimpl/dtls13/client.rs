// DTLS 1.3 Client Handshake Flow (RFC 9147):
//
// 1. Client sends ClientHello (plaintext, epoch 0)
// 2. Server may respond with HelloRetryRequest (special ServerHello with magic random)
//    - If so, Client replaces transcript with message_hash and sends new ClientHello
// 3. Server sends ServerHello (plaintext, epoch 0)
//    - Client derives handshake secrets, installs recv handshake keys, enables peer encryption
// 4. Server sends EncryptedExtensions (encrypted, epoch 2)
// 5. Server sends CertificateRequest (optional, encrypted, epoch 2)
// 6. Server sends Certificate (encrypted, epoch 2)
// 7. Server sends CertificateVerify (encrypted, epoch 2)
// 8. Server sends Finished (encrypted, epoch 2)
//    - Client derives application secrets, installs application keys
//    - Client installs send handshake keys for its own flight
// 9. Client sends Certificate (if requested, encrypted, epoch 2)
// 10. Client sends CertificateVerify (if cert present, encrypted, epoch 2)
// 11. Client sends Finished (encrypted, epoch 2)
// 12. Handshake complete, application data flows on epoch 3
//
// This implementation is a Sans-IO DTLS 1.3 client.

use std::collections::VecDeque;
use std::time::Instant;

use arrayvec::ArrayVec;
use subtle::ConstantTimeEq;

use crate::auto::HybridClientHello;
use crate::buffer::Buf;
use crate::buffer::ToBuf;
use crate::crypto::{ActiveKeyExchange, SrtpProfile};
use crate::dtls13::Server;
use crate::dtls13::engine::Engine;
use crate::dtls13::message::Asn1Cert;
use crate::dtls13::message::Body;
use crate::dtls13::message::Certificate;
use crate::dtls13::message::CertificateEntry;
use crate::dtls13::message::ClientHello;
use crate::dtls13::message::CompressionMethod;
use crate::dtls13::message::ContentType;
use crate::dtls13::message::Cookie;
use crate::dtls13::message::Dtls13CipherSuite;
use crate::dtls13::message::Extension;
use crate::dtls13::message::ExtensionType;
use crate::dtls13::message::KeyShareClientHello;
use crate::dtls13::message::KeyShareEntry;
use crate::dtls13::message::KeyUpdateRequest;
use crate::dtls13::message::MessageType;
use crate::dtls13::message::NamedGroup;
use crate::dtls13::message::ProtocolVersion;
use crate::dtls13::message::Random;
use crate::dtls13::message::SessionId;
use crate::dtls13::message::SignatureAlgorithmsExtension;
use crate::dtls13::message::SignatureScheme;
use crate::dtls13::message::SupportedGroupsExtension;
use crate::dtls13::message::SupportedVersionsClientHello;
use crate::dtls13::message::UseSrtpExtension;
use crate::timer::HandshakeTimers;
use crate::{Error, InternalError, KeyingMaterial, Output};

/// DTLS 1.3 client
pub struct Client {
    /// Current client state.
    state: State,

    /// Engine in common between server and client.
    engine: Engine,

    /// Random unique data. Used for ClientHello.
    random: Option<Random>,

    /// Legacy session ID echoed from ServerHello.
    session_id: Option<SessionId>,

    /// Original auto-sense offer, retained for cookie retries.
    hybrid: Option<Box<HybridClientHello>>,

    /// Storage for extension data
    extension_data: Buf,

    /// The negotiated SRTP profile (if any)
    negotiated_srtp_profile: Option<SrtpProfile>,

    /// Server certificates
    server_certificates: Vec<Buf>,

    /// Buffer for defragmenting handshakes
    defragment_buffer: Buf,

    /// Whether the server requested client authentication
    client_auth_requested: bool,

    /// Saved certificate_request_context from server's CertificateRequest
    cert_request_context: Option<Buf>,

    /// Cookie received from HRR, to echo in the retry ClientHello
    saved_cookie: Option<Buf>,

    /// The last now we seen
    last_now: Instant,

    /// Local events
    local_events: VecDeque<LocalEvent>,

    /// Data that is sent before we are connected.
    queued_data: Vec<Buf>,

    /// Whether we need to respond with our own KeyUpdate
    pending_key_update_response: bool,

    /// Active key exchange state (ECDHE)
    active_key_exchange: Option<Box<dyn ActiveKeyExchange>>,

    /// Whether we received a HelloRetryRequest
    hello_retry: bool,

    /// Group selected by HRR for retry
    hrr_selected_group: Option<NamedGroup>,

    /// Saved handshake secret for deriving application secrets
    handshake_secret: Option<Buf>,

    /// Client handshake traffic secret (for client Finished)
    client_hs_traffic_secret: Option<Buf>,

    /// Server handshake traffic secret (for server Finished verification)
    server_hs_traffic_secret: Option<Buf>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LocalEvent {
    PeerCert,
    Connected,
    KeyingMaterial(ArrayVec<u8, 88>, SrtpProfile),
}

impl Client {
    pub(crate) fn new_with_engine(mut engine: Engine, now: Instant) -> Client {
        engine.set_client(true);
        let extension_data = engine.pop_buffer();
        let defragment_buffer = engine.pop_buffer();

        Client {
            state: State::SendClientHello,
            engine,
            random: None,
            session_id: None,
            hybrid: None,
            extension_data,
            negotiated_srtp_profile: None,
            server_certificates: Vec::with_capacity(3),
            defragment_buffer,
            client_auth_requested: false,
            cert_request_context: None,
            saved_cookie: None,
            last_now: now,
            local_events: VecDeque::new(),
            queued_data: Vec::new(),
            pending_key_update_response: false,
            active_key_exchange: None,
            hello_retry: false,
            hrr_selected_group: None,
            handshake_secret: None,
            client_hs_traffic_secret: None,
            server_hs_traffic_secret: None,
        }
    }

    /// Create a client from a hybrid ClientHello probe.
    ///
    /// Injects the hybrid's transcript and ECDHE state into a fresh engine,
    /// then starts in `AwaitServerHello`. The engine's handshake sequence
    /// number is advanced to 1 (one CH has been sent). The hybrid CH was
    /// already sent on the wire by `ClientPending`, so no record is
    /// enqueued for output.
    pub(crate) fn new_from_hybrid(
        mut hybrid: HybridClientHello,
        config: std::sync::Arc<crate::Config>,
        certificate: crate::DtlsCertificate,
        now: Instant,
        timers: HandshakeTimers,
    ) -> Client {
        let mut engine = Engine::new(config, certificate);
        engine.set_client(true);

        // Inject transcript + sequence state from the hybrid CH that was
        // already sent on the wire by ClientPending.
        engine.inject_hybrid_client_hello(
            &hybrid.transcript_bytes,
            &hybrid.handshake_fragment,
            timers,
        );
        let extension_data = engine.pop_buffer();
        let defragment_buffer = engine.pop_buffer();

        let active_key_exchange = hybrid.active_key_exchange.take();

        Client {
            state: State::AwaitServerHello,
            engine,
            random: Some(hybrid.random),
            session_id: None,
            hybrid: Some(Box::new(hybrid)),
            extension_data,
            negotiated_srtp_profile: None,
            server_certificates: Vec::with_capacity(3),
            defragment_buffer,
            client_auth_requested: false,
            cert_request_context: None,
            saved_cookie: None,
            last_now: now,
            local_events: VecDeque::new(),
            queued_data: Vec::new(),
            pending_key_update_response: false,
            active_key_exchange,
            hello_retry: false,
            hrr_selected_group: None,
            handshake_secret: None,
            client_hs_traffic_secret: None,
            server_hs_traffic_secret: None,
        }
    }

    pub fn into_server(self) -> Server {
        Server::new_with_engine(self.engine, self.last_now, false)
    }

    pub(crate) fn state_name(&self) -> &'static str {
        self.state.name()
    }

    pub fn is_closing(&self) -> bool {
        (matches!(self.state, State::HalfClosedLocal | State::Closed) && !self.is_closed())
            || self.engine.close_notify_pending()
    }

    pub fn is_closed(&self) -> bool {
        self.state == State::Closed
            && self.local_events.is_empty()
            && !self.engine.has_pending_close_output()
    }

    pub fn handle_packet(&mut self, packet: &[u8]) -> Result<(), Error> {
        match self.engine.parse_packet(packet).and_then(|_| {
            let result = self.make_progress();
            self.engine.discard_undecryptable();
            result
        }) {
            Ok(()) => Ok(()),
            Err(e) => e.into_public_error().map_or(Ok(()), Err),
        }
    }

    pub fn poll_output<'a>(&mut self, buf: &'a mut [u8]) -> Output<'a> {
        if self.state == State::SendClientHello {
            return Output::Timeout(self.last_now);
        }
        if let Some(event) = self.local_events.pop_front() {
            return event.into_output(buf, &self.server_certificates);
        }
        self.engine.poll_output(buf, self.last_now)
    }

    /// Explicitly start the handshake process by sending a ClientHello
    pub fn handle_timeout(&mut self, now: Instant) -> Result<(), Error> {
        self.last_now = now;
        if self.random.is_none() {
            self.random = Some(self.engine.random());
        }
        self.engine.handle_timeout(now)?;
        match self.make_progress() {
            Ok(()) => Ok(()),
            Err(e) => e.into_public_error().map_or(Ok(()), Err),
        }
    }

    fn initiate_key_update(&mut self) -> Result<(), Error> {
        self.engine
            .create_key_update(KeyUpdateRequest::UpdateRequested)
    }

    /// Send application data when the client is connected.
    pub fn send_application_data(&mut self, data: &[u8]) -> Result<(), Error> {
        if self.state == State::Closed || self.state == State::HalfClosedLocal {
            return Err(Error::ConnectionClosed);
        }

        if self.state != State::AwaitApplicationData {
            self.queued_data.push(data.to_buf());
            return Ok(());
        }

        let epoch = self.engine.app_send_epoch();
        self.engine.create_ciphertext_record(
            ContentType::ApplicationData,
            epoch,
            false,
            |body| {
                body.extend_from_slice(data);
            },
        )?;

        Ok(())
    }

    /// Initiate graceful shutdown by sending a `close_notify` alert.
    pub fn close(&mut self) -> Result<(), Error> {
        if self.state == State::Closed || self.state == State::HalfClosedLocal {
            return Ok(());
        }
        if self.state != State::AwaitApplicationData {
            self.engine.abort();
            self.state = State::Closed;
            return Ok(());
        }
        let epoch = self.engine.app_send_epoch();
        self.engine
            .create_ciphertext_record(ContentType::Alert, epoch, false, |body| {
                body.push(1); // level: legacy (ignored in DTLS 1.3)
                body.push(0); // description: close_notify
            })?;
        self.engine.cancel_flights();
        self.state = if self.engine.close_notify_received() {
            State::Closed
        } else {
            State::HalfClosedLocal
        };
        Ok(())
    }

    fn make_progress(&mut self) -> Result<(), InternalError> {
        loop {
            let prev_state = self.state;

            let new_state = prev_state.make_progress(self)?;
            if prev_state != new_state {
                self.state = new_state;
                trace!("{:?} -> {:?}", prev_state, new_state);
            } else {
                break;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    SendClientHello,
    AwaitServerHello,
    AwaitEncryptedExtensions,
    AwaitCertificateRequest,
    AwaitCertificate,
    AwaitCertificateVerify,
    AwaitFinished,
    SendCertificate,
    SendCertificateVerify,
    SendFinished,
    AwaitApplicationData,
    HalfClosedLocal,
    Closed,
}

impl State {
    fn name(&self) -> &'static str {
        match self {
            State::SendClientHello => "SendClientHello",
            State::AwaitServerHello => "AwaitServerHello",
            State::AwaitEncryptedExtensions => "AwaitEncryptedExtensions",
            State::AwaitCertificateRequest => "AwaitCertificateRequest",
            State::AwaitCertificate => "AwaitCertificate",
            State::AwaitCertificateVerify => "AwaitCertificateVerify",
            State::AwaitFinished => "AwaitFinished",
            State::SendCertificate => "SendCertificate",
            State::SendCertificateVerify => "SendCertificateVerify",
            State::SendFinished => "SendFinished",
            State::AwaitApplicationData => "AwaitApplicationData",
            State::HalfClosedLocal => "HalfClosedLocal",
            State::Closed => "Closed",
        }
    }

    fn make_progress(self, client: &mut Client) -> Result<Self, InternalError> {
        if matches!(self, Self::AwaitApplicationData | Self::HalfClosedLocal) {
            // Session resumption is unsupported; consume tickets without changing the transcript.
            while client
                .engine
                .next_handshake_no_transcript(
                    MessageType::NewSessionTicket,
                    &mut client.defragment_buffer,
                )?
                .is_some()
            {
                client.engine.advance_peer_handshake_seq();
                client.engine.send_ack()?;
            }
        }

        match self {
            State::SendClientHello => self.send_client_hello(client),
            State::AwaitServerHello => self.await_server_hello(client),
            State::AwaitEncryptedExtensions => self.await_encrypted_extensions(client),
            State::AwaitCertificateRequest => self.await_certificate_request(client),
            State::AwaitCertificate => self.await_certificate(client),
            State::AwaitCertificateVerify => self.await_certificate_verify(client),
            State::AwaitFinished => self.await_finished(client),
            State::SendCertificate => self.send_certificate(client),
            State::SendCertificateVerify => self.send_certificate_verify(client),
            State::SendFinished => self.send_finished(client),
            State::AwaitApplicationData => self.await_application_data(client),
            State::HalfClosedLocal => self.half_closed_local(client),
            State::Closed => Ok(self),
        }
    }

    fn send_client_hello(self, client: &mut Client) -> Result<Self, InternalError> {
        // unwrap: is ok because we set the random in handle_timeout
        let random = client.random.unwrap();

        // Determine flight number: 1 for initial CH, 3 for HRR retry
        let flight_no = if client.hello_retry { 3 } else { 1 };
        client.engine.flight_begin(flight_no);

        // Generate key exchange for the first supported group (or HRR-selected group)
        let group = if let Some(hrr_group) = client.hrr_selected_group {
            hrr_group
        } else {
            // Use the first supported group from config (filtered)
            client
                .engine
                .config()
                .kx_groups()
                .next()
                .map(|g| g.name())
                .ok_or(Error::CryptoError(
                    crate::CryptoError::NoSupportedKeyExchangeGroups,
                ))?
        };

        let kx_group = client
            .engine
            .find_kx_group(group)
            .ok_or(Error::CryptoError(
                crate::CryptoError::KeyExchangeGroupNotFound(group),
            ))?;

        let key_exchange = if let Some(key_exchange) = client.active_key_exchange.take() {
            key_exchange
        } else {
            let kx_buf = client.engine.pop_buffer();
            kx_group
                .start_exchange(kx_buf)
                .map_err(Error::CryptoError)?
        };

        // Build the key_share extension data into extension_data buffer
        client.extension_data.clear();

        // Serialize extensions into extension_data
        let pub_key = key_exchange.pub_key();
        let pub_key_start = client.extension_data.len();
        client.extension_data.extend_from_slice(pub_key);
        let pub_key_end = client.extension_data.len();

        client.active_key_exchange = Some(key_exchange);

        // Store cookie data in extension_data if we have one from HRR
        let cookie_range = if let Some(ref cookie) = client.saved_cookie {
            let cookie_start = client.extension_data.len();
            client.extension_data.extend_from_slice(cookie);
            let cookie_end = client.extension_data.len();
            Some(cookie_start..cookie_end)
        } else {
            None
        };

        client
            .engine
            .create_handshake(MessageType::ClientHello, |body, engine| {
                if let Some(hybrid) = &mut client.hybrid {
                    if let Some(cookie) = &client.saved_cookie {
                        hybrid.set_retry_cookie(cookie);
                    }
                    if client.hrr_selected_group.is_some() {
                        hybrid.set_key_share(
                            group,
                            &client.extension_data[pub_key_start..pub_key_end],
                        );
                    }
                    hybrid.serialize(body);
                    return Ok(());
                }
                handshake_create_client_hello(
                    body,
                    engine,
                    random,
                    group,
                    pub_key_start..pub_key_end,
                    cookie_range.clone(),
                    &client.extension_data,
                )
            })?;

        Ok(Self::AwaitServerHello)
    }

    fn await_server_hello(self, client: &mut Client) -> Result<Self, InternalError> {
        // Save transcript length so we can roll back if a stale HRR
        // retransmission arrives after we already processed one.
        let transcript_len_before = client.engine.transcript.len();

        let maybe = client
            .engine
            .next_handshake(MessageType::ServerHello, &mut client.defragment_buffer)?;

        let Some(handshake) = maybe else {
            return Ok(self);
        };

        let Body::ServerHello(ref server_hello) = handshake.body else {
            unreachable!()
        };

        // Check for HelloRetryRequest (magic random)
        if server_hello.is_hello_retry_request() {
            if client.hello_retry {
                client.engine.transcript.resize(transcript_len_before, 0);
                return Err(Error::SecurityError(
                    crate::SecurityError::UnexpectedSecondHelloRetryRequest,
                )
                .into());
            }

            debug!("Received HelloRetryRequest");

            client.hrr_selected_group = server_hello.retry_group();

            // Extract the cookie from HRR extensions
            if let Some(ref extensions) = server_hello.extensions {
                for ext in extensions {
                    if ext.extension_type == ExtensionType::Cookie {
                        let ext_data = ext.extension_data(&client.defragment_buffer);
                        let mut cookie = client.engine.pop_buffer();
                        cookie.extend_from_slice(ext_data);
                        client.saved_cookie = Some(cookie);
                    }
                }
            }

            // Validate HRR cipher suite
            if !client
                .engine
                .is_cipher_suite_allowed(server_hello.cipher_suite)
            {
                return Err((Error::SecurityError(
                    crate::SecurityError::HrrSelectedDisallowedCipherSuite,
                ))
                .into());
            }
            client.engine.set_cipher_suite(server_hello.cipher_suite);

            // Validate HRR supported_versions
            let hrr_version_ok = server_hello.selected_version() == Some(ProtocolVersion::DTLS1_3);
            if !hrr_version_ok {
                return Err(
                    (Error::SecurityError(crate::SecurityError::HrrDidNotSelectDtls13)).into(),
                );
            }

            // Replace transcript with message_hash per RFC 8446 Section 4.4.1.
            // The HRR was already appended to the transcript by next_handshake().
            // We must hash only CH1, then re-append the HRR bytes.
            client
                .engine
                .replace_transcript_with_message_hash(transcript_len_before);
            client.engine.advance_peer_handshake_seq();
            client.engine.reset_for_hello_retry();
            client.hello_retry = true;

            // Per RFC 9846 Section 4.2.2: a retry may replace key_share only when HRR
            // contains key_share. Keep the original exchange for cookie-only
            // retries so ClientHello2 and the eventual shared secret use the
            // same key pair as ClientHello1.
            if client.hrr_selected_group.is_some() {
                client.active_key_exchange = None;
            }

            return Ok(Self::SendClientHello);
        }

        // Validate legacy_version (must be DTLS 1.2)
        if server_hello.legacy_version != ProtocolVersion::DTLS1_2 {
            return Err(Error::SecurityError(
                crate::SecurityError::ServerHelloLegacyVersionNotDtls12,
            )
            .into());
        }

        // Validate legacy_compression_method (must be null)
        if server_hello.legacy_compression_method != CompressionMethod::Null {
            return Err((Error::SecurityError(
                crate::SecurityError::ServerHelloCompressionMustBeNull,
            ))
            .into());
        }

        debug!(
            "Received ServerHello with cipher suite: {:?}",
            server_hello.cipher_suite
        );

        // Validate cipher suite
        let cs = server_hello.cipher_suite;
        if cs.is_unknown() {
            return Err((Error::SecurityError(
                crate::SecurityError::ServerSelectedUnknownCipherSuite,
            ))
            .into());
        }

        if !client.engine.is_cipher_suite_allowed(cs) {
            return Err(Error::SecurityError(
                crate::SecurityError::ServerSelectedDisallowedDtls13CipherSuite(cs),
            )
            .into());
        }

        client.engine.set_cipher_suite(cs);
        client.session_id = Some(server_hello.legacy_session_id);

        if server_hello.extensions.is_none() {
            return Err(Error::IncompleteServerHello.into());
        }
        if server_hello.selected_version() != Some(ProtocolVersion::DTLS1_3) {
            return Err(
                Error::SecurityError(crate::SecurityError::ServerDidNotNegotiateDtls13).into(),
            );
        }
        let key_share = server_hello.key_share().ok_or(Error::SecurityError(
            crate::SecurityError::ServerMissingKeyShare,
        ))?;
        let server_group = key_share.entry.group;

        // Complete ECDHE key exchange
        let key_exchange = client
            .active_key_exchange
            .take()
            .ok_or(Error::InvalidState(
                crate::InvalidStateError::NoActiveKeyExchange,
            ))?;

        if key_exchange.group() != server_group {
            return Err(
                Error::SecurityError(crate::SecurityError::ServerKeyShareGroupMismatch {
                    expected: key_exchange.group(),
                    actual: server_group,
                })
                .into(),
            );
        }

        let peer_pub_key = key_share.entry.key_exchange(&client.defragment_buffer);
        let mut shared_secret = client.engine.pop_buffer();
        key_exchange
            .complete(peer_pub_key, &mut shared_secret)
            .map_err(Error::CryptoError)?;

        // Derive handshake secrets
        let (c_hs_traffic, s_hs_traffic, handshake_secret) =
            client.engine.derive_handshake_secrets(&shared_secret)?;

        // Save handshake secret for later application key derivation
        client.handshake_secret = Some(handshake_secret);
        client.engine.push_buffer(shared_secret);

        // Install handshake keys (recv for server messages, send installed later)
        client
            .engine
            .install_handshake_keys(&c_hs_traffic, &s_hs_traffic)?;

        // Retain the derived buffers for Finished verification and client flight.
        client.server_hs_traffic_secret = Some(s_hs_traffic);
        client.client_hs_traffic_secret = Some(c_hs_traffic);

        // Enable peer encryption for server's epoch 2 messages
        client.engine.enable_peer_encryption()?;

        client.hybrid = None;

        client.engine.advance_peer_handshake_seq();
        Ok(Self::AwaitEncryptedExtensions)
    }

    fn await_encrypted_extensions(self, client: &mut Client) -> Result<Self, InternalError> {
        let maybe = client.engine.next_handshake(
            MessageType::EncryptedExtensions,
            &mut client.defragment_buffer,
        )?;

        let Some(handshake) = maybe else {
            return Ok(self);
        };

        let Body::EncryptedExtensions(ref ee) = handshake.body else {
            unreachable!()
        };

        if let Some(profile) = ee.srtp_profiles().and_then(|profiles| profiles.first()) {
            client.negotiated_srtp_profile = Some((*profile).into());
        }

        client.engine.advance_peer_handshake_seq();
        Ok(Self::AwaitCertificateRequest)
    }

    fn await_certificate_request(self, client: &mut Client) -> Result<Self, InternalError> {
        // CertificateRequest is optional. Check if Certificate is available instead.
        let has_cert = client
            .engine
            .has_complete_handshake(MessageType::Certificate);

        if has_cert {
            return Ok(Self::AwaitCertificate);
        }

        let maybe = client.engine.next_handshake(
            MessageType::CertificateRequest,
            &mut client.defragment_buffer,
        )?;

        let Some(ref handshake) = maybe else {
            return Ok(self);
        };

        let Body::CertificateRequest(ref request) = handshake.body else {
            unreachable!()
        };
        let context = request.context(&client.defragment_buffer);
        if !context.is_empty() {
            let mut ctx = client.engine.pop_buffer();
            ctx.extend_from_slice(context);
            client.cert_request_context = Some(ctx);
        }

        // CertificateRequest received - we'll send client Certificate + CertificateVerify
        debug!("Received CertificateRequest; enabling client authentication path");
        client.client_auth_requested = true;

        client.engine.advance_peer_handshake_seq();
        Ok(Self::AwaitCertificate)
    }

    fn await_certificate(self, client: &mut Client) -> Result<Self, InternalError> {
        let maybe = client
            .engine
            .next_handshake(MessageType::Certificate, &mut client.defragment_buffer)?;

        let Some(ref handshake) = maybe else {
            return Ok(self);
        };

        let Body::Certificate(ref certificate) = handshake.body else {
            unreachable!()
        };

        if !certificate.context_range.is_empty() {
            return Err(Error::CertificateError(
                crate::CertificateError::ServerCertificateContextMustBeEmpty,
            )
            .into());
        }

        if certificate.certificate_list.is_empty() {
            return Err((Error::CertificateError(
                crate::CertificateError::NoServerCertificateReceived,
            ))
            .into());
        }

        debug!(
            "Received Certificate message with {} certificate(s)",
            certificate.certificate_list.len()
        );

        // Extract certificate data before dropping handshake
        let cert_ranges: ArrayVec<_, 32> = certificate
            .certificate_list
            .iter()
            .map(|entry| entry.cert.as_slice(&client.defragment_buffer).to_vec())
            .collect();

        drop(maybe);

        for (i, cert_data) in cert_ranges.iter().enumerate() {
            trace!("Certificate #{} size: {} bytes", i + 1, cert_data.len());
            let mut buf = client.engine.pop_buffer();
            buf.extend_from_slice(cert_data);
            client.server_certificates.push(buf);
        }

        // Emit PeerCert event
        if !client.server_certificates.is_empty() {
            client.local_events.push_back(LocalEvent::PeerCert);
        }

        client.engine.advance_peer_handshake_seq();
        Ok(Self::AwaitCertificateVerify)
    }

    fn await_certificate_verify(self, client: &mut Client) -> Result<Self, InternalError> {
        // Compute transcript hash BEFORE consuming CertificateVerify.
        // Per RFC 8446 §4.4.3, the hash covers all messages up to but NOT
        // including CertificateVerify. next_handshake() will append CV to
        // the transcript, so we must capture the hash first.
        let mut transcript_hash = Buf::new();
        client.engine.transcript_hash(&mut transcript_hash);

        let maybe = client.engine.next_handshake(
            MessageType::CertificateVerify,
            &mut client.defragment_buffer,
        )?;

        let Some(ref handshake) = maybe else {
            return Ok(self);
        };

        let Body::CertificateVerify(ref cv) = handshake.body else {
            unreachable!()
        };

        let scheme = cv.signed.scheme;
        let signature = cv.signed.signature(&client.defragment_buffer);

        // RFC 8446 §4.4.3: The receiver MUST verify that the signature algorithm
        // is one that was offered in the signature_algorithms extension.
        if !SignatureScheme::supported().contains(&scheme) {
            return Err(
                Error::SecurityError(crate::SecurityError::SignatureSchemeNotOffered(scheme))
                    .into(),
            );
        }

        // Build the signed content per RFC 8446 Section 4.4.3:
        // 0x20 * 64 || "TLS 1.3, server CertificateVerify\0" || transcript_hash
        let mut signed_content = Buf::new();
        signed_content.extend_from_slice(&[0x20u8; 64]);
        signed_content.extend_from_slice(b"TLS 1.3, server CertificateVerify\0");
        signed_content.extend_from_slice(&transcript_hash);

        // Copy signature data since we need to drop handshake reference
        let mut signature_copy = ArrayVec::<u8, 512>::new();
        signature_copy
            .try_extend_from_slice(signature)
            .map_err(|_| Error::SecurityError(crate::SecurityError::SignatureTooLarge))?;

        drop(maybe);

        // Verify the signature
        let cert_der = client
            .server_certificates
            .first()
            .ok_or(Error::CertificateError(
                crate::CertificateError::NoServerCertificateForVerification,
            ))?;

        #[cfg(feature = "_crypto-common")]
        verify_scheme_curve(scheme, cert_der)?;

        let (hash_alg, sig_alg) = signature_scheme_to_components(scheme)?;

        client.engine.verify_signature(
            cert_der,
            &signed_content,
            &signature_copy,
            hash_alg,
            sig_alg,
        )?;

        trace!("Server CertificateVerify verified: {:?}", scheme);

        client.engine.advance_peer_handshake_seq();
        Ok(Self::AwaitFinished)
    }

    fn await_finished(self, client: &mut Client) -> Result<Self, InternalError> {
        // Compute expected verify_data BEFORE consuming Finished
        // (verify_data uses transcript hash up to but not including Finished)
        let server_hs_secret =
            client
                .server_hs_traffic_secret
                .as_ref()
                .ok_or(Error::InvalidState(
                    crate::InvalidStateError::NoServerHandshakeTrafficSecret,
                ))?;
        let expected_verify_data = client.engine.compute_verify_data(server_hs_secret)?;

        let maybe = client
            .engine
            .next_handshake(MessageType::Finished, &mut client.defragment_buffer)?;

        let Some(ref handshake) = maybe else {
            return Ok(self);
        };

        let Body::Finished(ref finished) = handshake.body else {
            unreachable!()
        };

        let verify_data = finished.verify_data(&client.defragment_buffer);

        trace!(
            "Finished.verify_data received len={}, expected len={}",
            verify_data.len(),
            expected_verify_data.len()
        );

        // Constant-time comparison
        let is_eq: bool = verify_data.ct_eq(&*expected_verify_data).into();
        if !is_eq {
            return Err((Error::SecurityError(
                crate::SecurityError::ServerFinishedVerificationFailed,
            ))
            .into());
        }

        trace!("Server Finished verified successfully");

        drop(maybe);

        client.engine.advance_peer_handshake_seq();

        // Derive application secrets from handshake secret + transcript through server Finished
        let handshake_secret = client.handshake_secret.as_ref().ok_or(Error::InvalidState(
            crate::InvalidStateError::NoHandshakeSecretForApplicationKeyDerivation,
        ))?;

        let (c_ap_traffic, s_ap_traffic) =
            client.engine.derive_application_secrets(handshake_secret)?;

        // Install application keys
        client
            .engine
            .install_application_keys(&c_ap_traffic, &s_ap_traffic)?;

        // Note: send handshake keys were already installed by install_handshake_keys()
        // after ServerHello. Do NOT reinstall them here — that would reset hs_send_seq
        // to 0, colliding with ACK records already sent on epoch 2.

        if client.client_auth_requested {
            Ok(Self::SendCertificate)
        } else {
            Ok(Self::SendFinished)
        }
    }

    fn send_certificate(self, client: &mut Client) -> Result<Self, InternalError> {
        debug!("Sending Certificate");

        // Start a new flight for the client's Certificate/CertificateVerify/Finished.
        // This clears the old ClientHello flight from flight_saved_records.
        client.engine.flight_begin(5);

        let has_cert = !client.engine.certificate_der().is_empty();

        if has_cert {
            let context = client.cert_request_context.as_deref().unwrap_or(&[]);
            let mut context_copy = ArrayVec::<u8, 255>::new();
            // unwrap: cert_request_context is u8-length-prefixed, max 255 bytes
            context_copy.try_extend_from_slice(context).unwrap();

            client
                .engine
                .create_handshake(MessageType::Certificate, |body, engine| {
                    handshake_create_certificate(body, engine, &context_copy)
                })?;

            Ok(Self::SendCertificateVerify)
        } else {
            // No client certificate: send empty Certificate and skip CertificateVerify
            let context = client.cert_request_context.as_deref().unwrap_or(&[]);
            let mut context_copy = ArrayVec::<u8, 255>::new();
            // unwrap: cert_request_context is u8-length-prefixed, max 255 bytes
            context_copy.try_extend_from_slice(context).unwrap();

            client
                .engine
                .create_handshake(MessageType::Certificate, |body, _engine| {
                    // certificate_request_context
                    body.push(context_copy.len() as u8);
                    body.extend_from_slice(&context_copy);
                    // empty certificate_list (3-byte zero length)
                    body.extend_from_slice(&[0, 0, 0]);
                    Ok(())
                })?;

            Ok(Self::SendFinished)
        }
    }

    fn send_certificate_verify(self, client: &mut Client) -> Result<Self, InternalError> {
        debug!("Sending CertificateVerify");

        client
            .engine
            .create_handshake(MessageType::CertificateVerify, |body, engine| {
                handshake_create_certificate_verify(
                    body,
                    engine,
                    b"TLS 1.3, client CertificateVerify\0",
                )
            })?;

        Ok(Self::SendFinished)
    }

    fn send_finished(self, client: &mut Client) -> Result<Self, InternalError> {
        trace!("Sending Finished message to complete handshake");

        // No records from the handshake epochs may survive into application data.
        let unexpected_record = client
            .engine
            .pending_records()
            .find(|record| record.record().sequence.epoch < 3);
        if let Some(record) = unexpected_record {
            debug!(
                "Unexpected epoch at handshake completion: record={:?}, handshake={:?}",
                record.record(),
                record.first_handshake().map(|handshake| &handshake.header),
            );
            return Err(
                Error::UnexpectedMessage(crate::UnexpectedMessageError::UnexpectedEpoch).into(),
            );
        }

        // When the server did NOT request client authentication, we skip
        // send_certificate (which calls flight_begin(5)). Start flight 5
        // here so the old ClientHello flight records are cleared before
        // building the Finished retransmission set.
        if !client.client_auth_requested {
            client.engine.flight_begin(5);
        }

        let client_hs_secret =
            client
                .client_hs_traffic_secret
                .as_ref()
                .ok_or(Error::InvalidState(
                    crate::InvalidStateError::NoClientHandshakeTrafficSecretForFinished,
                ))?;
        let mut client_hs_secret_copy = Buf::new();
        client_hs_secret_copy.extend_from_slice(client_hs_secret);
        let client_hs_secret = client_hs_secret_copy;

        client
            .engine
            .create_handshake(MessageType::Finished, |body, engine| {
                let verify_data = engine.compute_verify_data(&client_hs_secret)?;
                body.extend_from_slice(&verify_data);
                Ok(())
            })?;

        // Don't stop flight timers here - the server needs to receive our Finished.
        // Timers will stop when we receive an ACK from the server confirming it
        // received our epoch-2 flight.

        // Emit Connected event
        client.local_events.push_back(LocalEvent::Connected);

        // Extract and emit SRTP keying material if negotiated
        if let Some(profile) = client.negotiated_srtp_profile
            && let Ok((keying_material, profile)) =
                client.engine.extract_srtp_keying_material(profile)
        {
            debug!(
                "SRTP keying material extracted ({} bytes) for profile: {:?}",
                keying_material.len(),
                profile
            );
            client
                .local_events
                .push_back(LocalEvent::KeyingMaterial(keying_material, profile));
        }

        client.engine.release_application_data();

        debug!("Handshake complete; ready for application data");

        Ok(Self::AwaitApplicationData)
    }

    fn await_application_data(self, client: &mut Client) -> Result<Self, InternalError> {
        // Auto-trigger KeyUpdate when AEAD encryption limit is reached
        if client.engine.needs_key_update() && !client.engine.is_key_update_in_flight() {
            client.initiate_key_update()?;
        }

        // Send queued application data
        if !client.queued_data.is_empty() {
            let epoch = client.engine.app_send_epoch();
            debug!(
                "Sending queued application data: {}",
                client.queued_data.len()
            );
            for data in client.queued_data.drain(..) {
                client.engine.create_ciphertext_record(
                    ContentType::ApplicationData,
                    epoch,
                    false,
                    |body| {
                        body.extend_from_slice(&data);
                    },
                )?;
            }
        }

        // Send pending KeyUpdate response before processing new KeyUpdates
        if client.pending_key_update_response {
            client
                .engine
                .create_key_update(KeyUpdateRequest::UpdateNotRequested)?;
            client.pending_key_update_response = false;
        }

        // Check for incoming KeyUpdate
        if client.engine.has_complete_handshake(MessageType::KeyUpdate) {
            let maybe = client.engine.next_handshake_no_transcript(
                MessageType::KeyUpdate,
                &mut client.defragment_buffer,
            )?;

            if let Some(handshake) = maybe {
                let Body::KeyUpdate(request) = handshake.body else {
                    unreachable!()
                };

                // Install new recv keys
                client.engine.update_recv_keys()?;

                // ACK the KeyUpdate record
                client.engine.send_ack()?;

                // If peer requested us to update, schedule our own KeyUpdate
                if request == KeyUpdateRequest::UpdateRequested
                    && !client.engine.is_key_update_in_flight()
                {
                    client.pending_key_update_response = true;
                }

                client.engine.advance_peer_handshake_seq();
                debug!("Received KeyUpdate (request={:?})", request);
            }
        }

        Ok(self)
    }

    fn half_closed_local(self, client: &mut Client) -> Result<Self, InternalError> {
        // Write half is closed: drain incoming KeyUpdate to keep recv keys in sync,
        // but do not send our own KeyUpdate response.
        if client.engine.has_complete_handshake(MessageType::KeyUpdate) {
            let maybe = client.engine.next_handshake_no_transcript(
                MessageType::KeyUpdate,
                &mut client.defragment_buffer,
            )?;
            if let Some(handshake) = maybe {
                let Body::KeyUpdate(_) = handshake.body else {
                    unreachable!()
                };
                client.engine.update_recv_keys()?;
                client.engine.advance_peer_handshake_seq();
            }
        }

        if client.engine.close_notify_received() {
            return Ok(State::Closed);
        }

        Ok(self)
    }
}

// =========================================================================
// Helper Functions
// =========================================================================

fn handshake_create_client_hello(
    body: &mut Buf,
    engine: &mut Engine,
    random: Random,
    kx_group: NamedGroup,
    pub_key_range: std::ops::Range<usize>,
    cookie_range: Option<std::ops::Range<usize>>,
    extension_data: &Buf,
) -> Result<(), Error> {
    let legacy_version = ProtocolVersion::DTLS1_2;
    let legacy_session_id = SessionId::empty();
    // DTLS 1.3: legacy_cookie MUST be zero length
    let legacy_cookie = Cookie::empty();

    // Cipher suites from config (filtered)
    let cipher_suites: ArrayVec<Dtls13CipherSuite, 3> = engine
        .config()
        .dtls13_cipher_suites()
        .map(|cs| cs.suite())
        .take(3)
        .collect();

    debug!(
        "Sending ClientHello: offering {} cipher suites",
        cipher_suites.len()
    );

    let mut compression_methods = ArrayVec::new();
    compression_methods.push(CompressionMethod::Null);

    // Build extensions
    let mut extensions: ArrayVec<Extension, 8> = ArrayVec::new();
    let mut ext_buf = Buf::new();

    // 1. supported_versions extension (DTLS 1.3)
    let sv_start = ext_buf.len();
    let mut versions = ArrayVec::new();
    versions.push(ProtocolVersion::DTLS1_3);
    let sv = SupportedVersionsClientHello { versions };
    sv.serialize(&mut ext_buf);
    let sv_end = ext_buf.len();
    extensions.push(Extension {
        extension_type: ExtensionType::SupportedVersions,
        extension_data_range: sv_start..sv_end,
    });

    // 2. supported_groups extension
    let sg_start = ext_buf.len();
    let groups: ArrayVec<NamedGroup, 4> = engine.config().kx_groups().map(|g| g.name()).collect();
    let sg = SupportedGroupsExtension { groups };
    sg.serialize(&mut ext_buf);
    let sg_end = ext_buf.len();
    extensions.push(Extension {
        extension_type: ExtensionType::SupportedGroups,
        extension_data_range: sg_start..sg_end,
    });

    // 3. key_share extension
    let ks_start = ext_buf.len();
    let mut entries = ArrayVec::new();
    entries.push(KeyShareEntry {
        group: kx_group,
        key_exchange_range: pub_key_range,
    });
    let ks = KeyShareClientHello { entries };
    ks.serialize(extension_data, &mut ext_buf);
    let ks_end = ext_buf.len();
    extensions.push(Extension {
        extension_type: ExtensionType::KeyShare,
        extension_data_range: ks_start..ks_end,
    });

    // 4. signature_algorithms extension
    let sa_start = ext_buf.len();
    let sa = SignatureAlgorithmsExtension::default();
    sa.serialize(&mut ext_buf);
    let sa_end = ext_buf.len();
    extensions.push(Extension {
        extension_type: ExtensionType::SignatureAlgorithms,
        extension_data_range: sa_start..sa_end,
    });

    // 5. use_srtp extension
    let srtp_start = ext_buf.len();
    let use_srtp = UseSrtpExtension::default();
    use_srtp.serialize(&mut ext_buf);
    let srtp_end = ext_buf.len();
    extensions.push(Extension {
        extension_type: ExtensionType::UseSrtp,
        extension_data_range: srtp_start..srtp_end,
    });

    // 6. cookie extension (echo from HRR if present)
    if let Some(cookie_range) = cookie_range {
        let cookie_start = ext_buf.len();
        // Cookie data already includes the u16 length prefix from the HRR
        ext_buf.extend_from_slice(&extension_data[cookie_range]);
        let cookie_end = ext_buf.len();
        extensions.push(Extension {
            extension_type: ExtensionType::Cookie,
            extension_data_range: cookie_start..cookie_end,
        });
    }

    // 7. padding extension (RFC 7685) — pad ClientHello to fill the MTU,
    // reducing the server-to-client amplification factor.
    let mtu = engine.config().mtu();
    let record_header = 13; // DTLSPlaintext header
    let handshake_header = 12; // DTLS handshake header
    let body_without_padding = 2 // legacy_version
        + 32 // random
        + 1 + legacy_session_id.len()
        + 1 + legacy_cookie.len()
        + 2 + cipher_suites.len() * 2
        + 1 + compression_methods.len()
        + 2 // extensions_len
        + extensions.len() * 4 // type(2) + len(2) per extension
        + ext_buf.len(); // all extension data
    let total_without_padding = record_header + handshake_header + body_without_padding;
    let deficit = mtu.saturating_sub(total_without_padding);
    if deficit >= 4 {
        let pad_data_len = deficit - 4;
        let pad_start = ext_buf.len();
        for _ in 0..pad_data_len {
            ext_buf.push(0);
        }
        let pad_end = ext_buf.len();
        extensions.push(Extension {
            extension_type: ExtensionType::Padding,
            extension_data_range: pad_start..pad_end,
        });
    }

    let mut client_hello = ClientHello::new(
        legacy_version,
        random,
        legacy_session_id,
        legacy_cookie,
        cipher_suites,
        compression_methods,
    );
    client_hello.extensions = extensions;

    client_hello.serialize(&ext_buf, body);
    Ok(())
}

pub(crate) fn handshake_create_certificate(
    body: &mut Buf,
    engine: &mut Engine,
    context: &[u8],
) -> Result<(), Error> {
    // Build a source buffer: context bytes then cert DER
    let mut src = Buf::new();
    let context_start = src.len();
    src.extend_from_slice(context);
    let context_end = src.len();

    let cert_start = src.len();
    src.extend_from_slice(engine.certificate_der());
    let cert_end = src.len();

    let mut certificate_list = ArrayVec::new();
    certificate_list.push(CertificateEntry {
        cert: Asn1Cert(cert_start..cert_end),
        extensions_range: 0..0,
    });

    let certificate = Certificate {
        context_range: context_start..context_end,
        certificate_list,
    };

    certificate.serialize(&src, body);
    Ok(())
}

pub(crate) fn handshake_create_certificate_verify(
    body: &mut Buf,
    engine: &mut Engine,
    context_string: &[u8],
) -> Result<(), Error> {
    // Build signed content: 0x20*64 || context_string || transcript_hash
    let mut signed_content = Buf::new();
    signed_content.extend_from_slice(&[0x20u8; 64]);
    signed_content.extend_from_slice(context_string);

    let mut transcript_hash = Buf::new();
    engine.transcript_hash(&mut transcript_hash);
    signed_content.extend_from_slice(&transcript_hash);

    // Sign with our private key
    let hash_alg = engine.signing_key().hash_algorithm();
    let sig_alg = engine.signing_key().algorithm();

    let mut signature = Buf::new();
    engine
        .signing_key()
        .sign(&signed_content, hash_alg, &mut signature)
        .map_err(Error::CryptoError)?;

    // Determine the SignatureScheme from hash_alg + sig_alg
    let scheme = match (sig_alg, hash_alg) {
        (crate::types::SignatureAlgorithm::ECDSA, crate::types::HashAlgorithm::SHA256) => {
            SignatureScheme::ECDSA_SECP256R1_SHA256
        }
        (crate::types::SignatureAlgorithm::ECDSA, crate::types::HashAlgorithm::SHA384) => {
            SignatureScheme::ECDSA_SECP384R1_SHA384
        }
        (crate::types::SignatureAlgorithm::RSA, crate::types::HashAlgorithm::SHA256) => {
            SignatureScheme::RSA_PSS_RSAE_SHA256
        }
        _ => {
            return Err(Error::CryptoError(
                crate::CryptoError::UnsupportedSignaturePair {
                    signature: sig_alg,
                    hash: hash_alg,
                },
            ));
        }
    };

    // Write CertificateVerify: scheme(2) + signature_len(2) + signature
    body.extend_from_slice(&scheme.as_u16().to_be_bytes());
    body.extend_from_slice(&(signature.len() as u16).to_be_bytes());
    body.extend_from_slice(&signature);

    Ok(())
}

/// Map a TLS 1.3 SignatureScheme to the (HashAlgorithm, SignatureAlgorithm) pair
/// needed by the SignatureVerifier trait.
pub(crate) fn signature_scheme_to_components(
    scheme: SignatureScheme,
) -> Result<
    (
        crate::types::HashAlgorithm,
        crate::types::SignatureAlgorithm,
    ),
    Error,
> {
    use crate::types::{HashAlgorithm, SignatureAlgorithm};
    match scheme {
        SignatureScheme::ECDSA_SECP256R1_SHA256 => {
            Ok((HashAlgorithm::SHA256, SignatureAlgorithm::ECDSA))
        }
        SignatureScheme::ECDSA_SECP384R1_SHA384 => {
            Ok((HashAlgorithm::SHA384, SignatureAlgorithm::ECDSA))
        }
        SignatureScheme::ECDSA_SECP521R1_SHA512 => {
            Ok((HashAlgorithm::SHA512, SignatureAlgorithm::ECDSA))
        }
        SignatureScheme::ED25519 => Err(Error::SecurityError(
            crate::SecurityError::UnsupportedSignatureScheme(scheme),
        )),
        SignatureScheme::RSA_PSS_RSAE_SHA256 => {
            Ok((HashAlgorithm::SHA256, SignatureAlgorithm::RSA))
        }
        SignatureScheme::RSA_PSS_RSAE_SHA384 => {
            Ok((HashAlgorithm::SHA384, SignatureAlgorithm::RSA))
        }
        SignatureScheme::RSA_PSS_RSAE_SHA512 => {
            Ok((HashAlgorithm::SHA512, SignatureAlgorithm::RSA))
        }
        SignatureScheme::RSA_PSS_PSS_SHA256 => Ok((HashAlgorithm::SHA256, SignatureAlgorithm::RSA)),
        SignatureScheme::RSA_PSS_PSS_SHA384 => Ok((HashAlgorithm::SHA384, SignatureAlgorithm::RSA)),
        SignatureScheme::RSA_PSS_PSS_SHA512 => Ok((HashAlgorithm::SHA512, SignatureAlgorithm::RSA)),
        SignatureScheme::RSA_PKCS1_SHA256
        | SignatureScheme::RSA_PKCS1_SHA384
        | SignatureScheme::RSA_PKCS1_SHA512 => Err(Error::SecurityError(
            crate::SecurityError::UnsupportedSignatureScheme(scheme),
        )),
        _ => Err(Error::SecurityError(
            crate::SecurityError::UnsupportedSignatureScheme(scheme),
        )),
    }
}

/// RFC 8446 §4.4.3: For ECDSA schemes, verify the curve in the [`SignatureScheme`]
/// matches the certificate's public key curve.
#[cfg(feature = "_crypto-common")]
pub(crate) fn verify_scheme_curve(scheme: SignatureScheme, cert_der: &[u8]) -> Result<(), Error> {
    if let Some(expected_group) = scheme.named_group() {
        let cert_group =
            crate::crypto::cert_named_group(cert_der).map_err(Error::CertificateError)?;
        if expected_group != cert_group {
            return Err(Error::SecurityError(
                crate::SecurityError::SignatureSchemeCertificateCurveMismatch {
                    scheme,
                    expected: expected_group,
                    actual: cert_group,
                },
            ));
        }
    }
    Ok(())
}

// =========================================================================
// Standard Trait Impls
// =========================================================================

impl LocalEvent {
    pub fn into_output<'a>(self, buf: &'a mut [u8], peer_certs: &[Buf]) -> Output<'a> {
        match self {
            LocalEvent::PeerCert => {
                let l = peer_certs[0].len();
                assert!(
                    l <= buf.len(),
                    "Output buffer too small for peer certificate"
                );
                buf[..l].copy_from_slice(&peer_certs[0]);
                Output::PeerCert(&buf[..l])
            }
            LocalEvent::Connected => Output::Connected,
            LocalEvent::KeyingMaterial(m, profile) => {
                Output::KeyingMaterial(KeyingMaterial::new(&m), profile)
            }
        }
    }
}

#[cfg(all(test, feature = "rcgen"))]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::Config;
    use crate::certificate::generate_self_signed_certificate;
    use crate::{CryptoError, SecurityError};

    #[derive(Debug)]
    struct TestKeyExchange {
        group: NamedGroup,
    }

    impl ActiveKeyExchange for TestKeyExchange {
        fn pub_key(&self) -> &[u8] {
            &[]
        }

        fn complete(self: Box<Self>, _peer_pub: &[u8], _out: &mut Buf) -> Result<(), CryptoError> {
            unreachable!("mismatched server key share should fail before completing ECDHE")
        }

        fn group(&self) -> NamedGroup {
            self.group
        }
    }

    fn client() -> Client {
        let cert = generate_self_signed_certificate().expect("generate cert");
        let engine = Engine::new(Arc::new(Config::default()), cert);
        Client::new_with_engine(engine, Instant::now())
    }

    fn epoch0_handshake_packet(msg_type: MessageType, message_seq: u16, body: &[u8]) -> Vec<u8> {
        let handshake_len = 12 + body.len();
        let mut packet = Vec::new();
        packet.push(ContentType::Handshake.as_u8());
        packet.extend_from_slice(&[0xfe, 0xfd]);
        packet.extend_from_slice(&0u16.to_be_bytes());
        packet.extend_from_slice(&0u64.to_be_bytes()[2..]);
        packet.extend_from_slice(&(handshake_len as u16).to_be_bytes());
        packet.push(msg_type.as_u8());
        packet.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
        packet.extend_from_slice(&message_seq.to_be_bytes());
        packet.extend_from_slice(&0u32.to_be_bytes()[1..]);
        packet.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
        packet.extend_from_slice(body);
        packet
    }

    #[test]
    fn retransmitted_ticket_does_not_discard_coalesced_new_ticket() {
        for same_record in [false, true] {
            let mut client = client();
            let cert = generate_self_signed_certificate().expect("server certificate");
            let mut server = Engine::new(Arc::new(Config::default()), cert);
            let mut secret = Buf::new();
            secret.extend_from_slice(&[0x42; 32]);
            for engine in [&mut client.engine, &mut server] {
                engine.set_cipher_suite(Dtls13CipherSuite::AES_128_GCM_SHA256);
                engine
                    .install_application_keys(&secret, &secret)
                    .expect("application keys");
                engine.enable_peer_encryption().expect("enable encryption");
                engine.release_application_data();
            }
            client.state = State::AwaitApplicationData;
            let now = Instant::now();
            let mut buffer = [0; 2048];
            let ticket = &[0, 0, 0, 60, 0, 0, 0, 0, 0, 0, 1, 42, 0, 0][..];
            // Lose the first ACK, then coalesce the retransmission and a fresh ticket.
            for messages in [
                &[(0, MessageType::NewSessionTicket, ticket)][..],
                &[
                    (0, MessageType::NewSessionTicket, ticket),
                    (1, MessageType::NewSessionTicket, ticket),
                ][..],
                &[(2, MessageType::KeyUpdate, &[0][..])][..],
            ] {
                if same_record {
                    server
                        .create_ciphertext_record(ContentType::Handshake, 3, false, |fragment| {
                            for &(seq, msg_type, body) in messages {
                                let packet = epoch0_handshake_packet(msg_type, seq, body);
                                fragment.extend_from_slice(&packet[13..]);
                            }
                        })
                        .expect("send coalesced handshake messages");
                } else {
                    for &(seq, msg_type, body) in messages {
                        let packet = epoch0_handshake_packet(msg_type, seq, body);
                        server
                            .create_ciphertext_record(
                                ContentType::Handshake,
                                3,
                                false,
                                |fragment| {
                                    fragment.extend_from_slice(&packet[13..]);
                                },
                            )
                            .expect("send post-handshake message");
                    }
                }
                let mut packets = 0;
                loop {
                    match server.poll_output(&mut buffer, now) {
                        Output::Packet(packet) => {
                            packets += 1;
                            client.handle_packet(packet).expect("accept messages");
                            loop {
                                match client.poll_output(&mut [0; 2048]) {
                                    Output::Packet(_) => {}
                                    Output::Timeout(_) => break,
                                    output => panic!("unexpected client output: {output:?}"),
                                }
                            }
                        }
                        Output::Timeout(_) => break,
                        output => panic!("unexpected server output: {output:?}"),
                    }
                }
                assert_eq!(packets, 1, "messages must share one datagram");
            }
            assert_eq!(
                crate::dtls13::incoming::RecordHandler::resolve_epoch(&client.engine, 0),
                4,
                "the fresh ticket must advance the sequence so KeyUpdate can be processed"
            );
        }
    }

    #[test]
    fn post_handshake_tickets_are_discarded_and_acked() {
        for half_closed in [false, true] {
            let mut client = client();
            let cert = generate_self_signed_certificate().expect("server certificate");
            let mut server = Engine::new(Arc::new(Config::default()), cert);
            let mut secret = Buf::new();
            secret.extend_from_slice(&[0x42; 32]);
            for engine in [&mut client.engine, &mut server] {
                engine.set_cipher_suite(Dtls13CipherSuite::AES_128_GCM_SHA256);
                engine
                    .install_application_keys(&secret, &secret)
                    .expect("application keys");
                engine.enable_peer_encryption().expect("enable encryption");
                engine.release_application_data();
            }
            client.state = if half_closed {
                State::HalfClosedLocal
            } else {
                State::AwaitApplicationData
            };
            let now = Instant::now();
            let mut buffer = [0; 2048];
            // A ticket, its retransmission in a new record, another ticket, then KeyUpdate.
            for (seq, msg_type, body) in [
                (
                    0,
                    MessageType::NewSessionTicket,
                    &[0, 0, 0, 60, 0, 0, 0, 0, 0, 0, 1, 42, 0, 0][..],
                ),
                (
                    0,
                    MessageType::NewSessionTicket,
                    &[0, 0, 0, 60, 0, 0, 0, 0, 0, 0, 1, 42, 0, 0][..],
                ),
                (
                    1,
                    MessageType::NewSessionTicket,
                    &[0, 0, 0, 60, 0, 0, 0, 0, 0, 0, 1, 43, 0, 0][..],
                ),
                (2, MessageType::KeyUpdate, &[0][..]),
            ] {
                let packet = epoch0_handshake_packet(msg_type, seq, body);
                server
                    .create_ciphertext_record(ContentType::Handshake, 3, false, |fragment| {
                        fragment.extend_from_slice(&packet[13..]);
                    })
                    .expect("send post-handshake message");
                loop {
                    match server.poll_output(&mut buffer, now) {
                        Output::Packet(packet) => client
                            .handle_packet(packet)
                            .expect("accept post-handshake message"),
                        Output::Timeout(_) => break,
                        output => panic!("unexpected server output: {output:?}"),
                    }
                }
                let mut acks = 0;
                loop {
                    match client.poll_output(&mut buffer) {
                        Output::Packet(packet) => {
                            server.parse_packet(packet).expect("receive ACK");
                            acks += 1;
                        }
                        Output::Timeout(_) => break,
                        output => panic!("unexpected client output: {output:?}"),
                    }
                }
                if msg_type == MessageType::NewSessionTicket || !half_closed {
                    assert_eq!(acks, 1, "ACK each ticket, including retransmissions");
                }
            }
            assert_eq!(
                crate::dtls13::incoming::RecordHandler::resolve_epoch(&client.engine, 0),
                4
            );
            server
                .create_ciphertext_record(ContentType::ApplicationData, 3, false, |body| {
                    body.extend_from_slice(b"after tickets");
                })
                .expect("send application data");
            loop {
                match server.poll_output(&mut buffer, now) {
                    Output::Packet(packet) => client
                        .handle_packet(packet)
                        .expect("receive application data"),
                    Output::Timeout(_) => break,
                    output => panic!("unexpected server output: {output:?}"),
                }
            }
            let mut received = false;
            loop {
                match client.poll_output(&mut buffer) {
                    Output::ApplicationData(data) => {
                        assert_eq!(data, b"after tickets");
                        received = true;
                    }
                    Output::Timeout(_) => break,
                    output => panic!("unexpected client output: {output:?}"),
                }
            }
            assert!(received, "tickets must not block the following KeyUpdate");
        }
    }

    fn server_hello_with_key_share(group: NamedGroup) -> Vec<u8> {
        let mut key_share = Vec::new();
        key_share.extend_from_slice(&group.as_u16().to_be_bytes());
        key_share.extend_from_slice(&1u16.to_be_bytes());
        key_share.push(0);

        let mut extensions = Vec::new();
        extensions.extend_from_slice(&ExtensionType::SupportedVersions.as_u16().to_be_bytes());
        extensions.extend_from_slice(&2u16.to_be_bytes());
        extensions.extend_from_slice(&ProtocolVersion::DTLS1_3.as_u16().to_be_bytes());
        extensions.extend_from_slice(&ExtensionType::KeyShare.as_u16().to_be_bytes());
        extensions.extend_from_slice(&(key_share.len() as u16).to_be_bytes());
        extensions.extend_from_slice(&key_share);

        let mut body = Vec::new();
        body.extend_from_slice(&ProtocolVersion::DTLS1_2.as_u16().to_be_bytes());
        body.extend_from_slice(&[7; 32]);
        body.push(0); // legacy_session_id
        body.extend_from_slice(&Dtls13CipherSuite::AES_128_GCM_SHA256.as_u16().to_be_bytes());
        body.push(CompressionMethod::Null.as_u8());
        body.extend_from_slice(&(extensions.len() as u16).to_be_bytes());
        body.extend_from_slice(&extensions);
        body
    }

    #[test]
    fn empty_server_certificate_is_certificate_error() {
        let mut client = client();
        client.engine.advance_peer_handshake_seq(); // ServerHello
        client.engine.advance_peer_handshake_seq(); // EncryptedExtensions

        client
            .engine
            .parse_packet(&epoch0_handshake_packet(
                MessageType::Certificate,
                2,
                &[0, 0, 0, 0],
            ))
            .expect("queue empty Certificate");

        let err = State::AwaitCertificate
            .await_certificate(&mut client)
            .expect_err("empty server Certificate should fail");

        assert!(matches!(
            err,
            crate::InternalError::Fatal(Error::CertificateError(_))
        ));
    }

    #[test]
    fn server_key_share_group_mismatch_reports_expected_and_actual_groups() {
        let mut client = client();
        client.active_key_exchange = Some(Box::new(TestKeyExchange {
            group: NamedGroup::X25519,
        }));

        client
            .engine
            .parse_packet(&epoch0_handshake_packet(
                MessageType::ServerHello,
                0,
                &server_hello_with_key_share(NamedGroup::Secp256r1),
            ))
            .expect("queue mismatched ServerHello");

        let err = State::AwaitServerHello
            .await_server_hello(&mut client)
            .expect_err("mismatched server key share group should fail");

        assert!(matches!(
            err,
            crate::InternalError::Fatal(Error::SecurityError(
                SecurityError::ServerKeyShareGroupMismatch {
                    expected: NamedGroup::X25519,
                    actual: NamedGroup::Secp256r1,
                }
            ))
        ));
    }
}
