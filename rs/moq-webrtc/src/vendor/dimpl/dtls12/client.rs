// DTLS Client Handshake Flow:
//
// 1. Client sends ClientHello
// 2. Server may respond with HelloVerifyRequest containing a cookie
//    - If so, Client sends another ClientHello with the cookie
// 3. Server sends ServerHello, Certificate, ServerKeyExchange,
//    CertificateRequest (optional), ServerHelloDone
// 4. Client sends Certificate (if requested), ClientKeyExchange,
//    CertificateVerify (if client cert present), ChangeCipherSpec, Finished
// 5. Server sends ChangeCipherSpec, Finished
// 6. Handshake complete, application data can flow
//
// This implementation is a Sans-IO DTLS client.

use std::collections::VecDeque;
use std::time::Instant;

use arrayvec::ArrayVec;
use subtle::ConstantTimeEq;

use crate::auto::HybridClientHello;
use crate::buffer::{Buf, ToBuf};
use crate::crypto::SrtpProfile;
use crate::dtls12::Server;
use crate::dtls12::context::AuthMode;
use crate::dtls12::engine::Engine;
use crate::dtls12::message::{Body, CipherSuiteVec, ClientHello, ClientKeyExchange};
use crate::dtls12::message::{ClientPskKeys, ServerKeyExchangeParams};
use crate::dtls12::message::{CompressionMethod, ContentType, Cookie};
use crate::dtls12::message::{DigitallySigned, Dtls12CipherSuite};
use crate::dtls12::message::{ExtensionType, KeyExchangeAlgorithm, MessageType, ProtocolVersion};
use crate::dtls12::message::{Random, SessionId, SignatureAndHashAlgorithm};
use crate::timer::HandshakeTimers;
use crate::{Config, DtlsCertificate, Error, InternalError, KeyingMaterial, Output};

/// DTLS client
pub struct Client {
    /// Current client state.
    state: State,

    /// Engine in common between server and client.
    engine: Engine,

    /// Random unique data (with gmt timestamp). Used for signature checks.
    random: Option<Random>,

    /// SessionId is set by the server and only sent by the client if we
    /// are reusing a session (key renegotiation).
    session_id: Option<SessionId>,

    /// Cookie is sent by the server in the optional HelloVerifyRequest.
    /// It might remain null if there is no HelloVerifyRequest.
    cookie: Option<Cookie>,

    /// Original auto-sense offer, retained for cookie retries.
    hybrid: Option<Box<HybridClientHello>>,

    /// Storage for extension data
    extension_data: Buf,

    /// The negotiated SRTP profile (if any)
    negotiated_srtp_profile: Option<SrtpProfile>,

    /// Server random. Set by ServerHello.
    server_random: Option<Random>,

    /// Server certificates
    server_certificates: Vec<Buf>,

    /// Buffer for defragmenting handshakes
    defragment_buffer: Buf,

    /// Whether we requested a CertificateVerify
    certificate_verify: bool,

    /// Captured session hash for Extended Master Secret (RFC 7627)
    /// This is captured after ServerHelloDone to include the correct handshake messages
    captured_session_hash: Option<Buf>,

    /// The last now we seen
    last_now: Instant,

    /// Local events
    local_events: VecDeque<LocalEvent>,

    /// Data that is sent before we are connected.
    queued_data: Vec<Buf>,
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
            cookie: None,
            hybrid: None,
            extension_data,
            negotiated_srtp_profile: None,
            server_random: None,
            server_certificates: Vec::with_capacity(3),
            defragment_buffer,
            certificate_verify: false,
            captured_session_hash: None,
            last_now: now,
            local_events: VecDeque::new(),
            queued_data: Vec::new(),
        }
    }

    /// Create a client from a hybrid ClientHello probe (DTLS 1.2 fork).
    ///
    /// Accepts the hybrid's random and DTLS-framed handshake bytes so that:
    /// - The with-cookie ClientHello uses the same random (HMAC cookie verification).
    /// - The transcript contains the initial ClientHello (needed when the
    ///   server skips HelloVerifyRequest and sends ServerHello directly).
    ///
    /// If the server does send HVR, `reset_client_for_hello_verify_request`
    /// clears the transcript anyway, so the injected bytes are harmless.
    pub(crate) fn new_from_hybrid(
        hybrid: HybridClientHello,
        config: std::sync::Arc<Config>,
        certificate: DtlsCertificate,
        now: Instant,
        timers: HandshakeTimers,
    ) -> Client {
        assert!(
            !certificate.certificate.is_empty(),
            "Client certificate cannot be empty"
        );
        // unwrap: malformed private_key bytes are a programmer error from the
        // caller who constructed DtlsCertificate; panic matches the prior
        // CryptoContext::new behavior which also panicked on empty/invalid
        // key material.
        let private_key = config
            .crypto_provider()
            .key_provider
            .load_private_key(&certificate.private_key)
            .expect("Failed to parse client private key");
        let auth = AuthMode::Certificate {
            certificate: certificate.certificate,
            private_key,
        };
        let mut engine = Engine::new(config, auth);
        engine.set_client(true);
        engine.inject_hybrid_client_hello(&hybrid.handshake_fragment, timers);
        let extension_data = engine.pop_buffer();
        let defragment_buffer = engine.pop_buffer();

        Client {
            state: State::AwaitHelloVerifyRequest,
            engine,
            random: Some(hybrid.random),
            session_id: None,
            cookie: None,
            hybrid: Some(Box::new(hybrid)),
            extension_data,
            negotiated_srtp_profile: None,
            server_random: None,
            server_certificates: Vec::with_capacity(3),
            defragment_buffer,
            certificate_verify: false,
            captured_session_hash: None,
            last_now: now,
            local_events: VecDeque::new(),
            queued_data: Vec::new(),
        }
    }

    pub fn into_server(self) -> Server {
        Server::new_with_engine(self.engine, self.last_now)
    }

    pub(crate) fn state_name(&self) -> &'static str {
        self.state.name()
    }

    pub fn is_closing(&self) -> bool {
        self.state == State::Closed && !self.is_closed()
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
            self.random = Some(Random::new_with_time(now, &mut self.engine.rng));
        }
        self.engine.handle_timeout(now)?;
        match self.make_progress() {
            Ok(()) => Ok(()),
            Err(e) => e.into_public_error().map_or(Ok(()), Err),
        }
    }

    /// Send application data when the client is in the Running state
    ///
    /// This should only be called when the client is in the Running state,
    /// after the handshake is complete.
    pub fn send_application_data(&mut self, data: &[u8]) -> Result<(), Error> {
        if self.state == State::Closed {
            return Err(Error::ConnectionClosed);
        }

        if self.state != State::AwaitApplicationData {
            self.queued_data.push(data.to_buf());
            return Ok(());
        }

        // Use the engine's create_record to send application data
        // The encryption is now handled in the engine
        self.engine
            .create_record(ContentType::ApplicationData, 1, false, |body| {
                body.extend_from_slice(data);
            })?;

        Ok(())
    }

    /// Initiate graceful shutdown by sending a `close_notify` alert.
    pub fn close(&mut self) -> Result<(), Error> {
        if self.state == State::Closed {
            return Ok(());
        }
        if self.state != State::AwaitApplicationData {
            self.engine.abort();
            self.state = State::Closed;
            return Ok(());
        }
        self.engine
            .create_record(ContentType::Alert, 1, false, |body| {
                body.push(1); // level: warning
                body.push(0); // description: close_notify
            })?;
        self.state = State::Closed;
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
    AwaitHelloVerifyRequest,
    AwaitServerHello,
    AwaitCertificate,
    AwaitServerKeyExchange,
    AwaitCertificateRequest,
    AwaitServerHelloDone,
    SendCertificate,
    SendClientKeyExchange,
    SendCertificateVerify,
    SendChangeCipherSpec,
    SendFinished,
    AwaitChangeCipherSpec,
    AwaitNewSessionTicket,
    AwaitFinished,
    AwaitApplicationData,
    Closed,
}

impl State {
    fn name(&self) -> &'static str {
        match self {
            State::SendClientHello => "SendClientHello",
            State::AwaitHelloVerifyRequest => "AwaitHelloVerifyRequest",
            State::AwaitServerHello => "AwaitServerHello",
            State::AwaitCertificate => "AwaitCertificate",
            State::AwaitServerKeyExchange => "AwaitServerKeyExchange",
            State::AwaitCertificateRequest => "AwaitCertificateRequest",
            State::AwaitServerHelloDone => "AwaitServerHelloDone",
            State::SendCertificate => "SendCertificate",
            State::SendClientKeyExchange => "SendClientKeyExchange",
            State::SendCertificateVerify => "SendCertificateVerify",
            State::SendChangeCipherSpec => "SendChangeCipherSpec",
            State::SendFinished => "SendFinished",
            State::AwaitChangeCipherSpec => "AwaitChangeCipherSpec",
            State::AwaitNewSessionTicket => "AwaitNewSessionTicket",
            State::AwaitFinished => "AwaitFinished",
            State::AwaitApplicationData => "AwaitApplicationData",
            State::Closed => "Closed",
        }
    }

    fn make_progress(self, client: &mut Client) -> Result<Self, InternalError> {
        match self {
            State::SendClientHello => self.send_client_hello(client),
            State::AwaitHelloVerifyRequest => self.await_hello_verify_request(client),
            State::AwaitServerHello => self.await_server_hello(client),
            State::AwaitCertificate => self.await_certificate(client),
            State::AwaitServerKeyExchange => self.await_server_key_exchange(client),
            State::AwaitCertificateRequest => self.await_certificate_request(client),
            State::AwaitServerHelloDone => self.await_server_hello_done(client),
            State::SendCertificate => self.send_certificate(client),
            State::SendClientKeyExchange => self.send_client_key_exchange(client),
            State::SendCertificateVerify => self.send_certificate_verify(client),
            State::SendChangeCipherSpec => self.send_change_cipher_spec(client),
            State::SendFinished => self.send_finished(client),
            State::AwaitChangeCipherSpec => self.await_change_cipher_spec(client),
            State::AwaitNewSessionTicket => self.await_new_session_ticket(client),
            State::AwaitFinished => self.await_finished(client),
            State::AwaitApplicationData => self.await_application_data(client),
            State::Closed => Ok(self),
        }
    }

    fn send_client_hello(self, client: &mut Client) -> Result<Self, InternalError> {
        let session_id = client.session_id.unwrap_or_else(SessionId::empty);
        let cookie = client.cookie.unwrap_or_else(Cookie::empty);
        // unwrap: is ok because we set the random in handle_timeout
        let random = client.random.unwrap();

        // Determine flight number: 1 for initial CH, 3 for retransmit with cookie
        let flight_no = if client.cookie.is_none() { 1 } else { 3 };
        client.engine.flight_begin(flight_no);

        client
            .engine
            .create_handshake(MessageType::ClientHello, |body, engine| {
                if let Some(hybrid) = &mut client.hybrid {
                    hybrid.set_legacy_cookie(cookie);
                    hybrid.serialize(body);
                    return Ok(());
                }
                handshake_create_client_hello(
                    body,
                    engine,
                    cookie,
                    random,
                    session_id,
                    &mut client.extension_data,
                )
            })?;

        let can_hello_verify = client.cookie.is_none();

        if can_hello_verify {
            Ok(Self::AwaitHelloVerifyRequest)
        } else {
            Ok(Self::AwaitServerHello)
        }
    }

    fn await_hello_verify_request(self, client: &mut Client) -> Result<Self, InternalError> {
        let has_hello = client
            .engine
            .has_complete_handshake(MessageType::ServerHello);

        // Got ServerHello, skip HelloVerifyRequest
        if has_hello {
            return Ok(Self::AwaitServerHello);
        }

        let maybe = client.engine.next_handshake(
            MessageType::HelloVerifyRequest,
            &mut client.defragment_buffer,
        )?;

        let Some(handshake) = maybe else {
            // Stay in this state
            return Ok(self);
        };

        let Body::HelloVerifyRequest(h) = handshake.body else {
            unreachable!()
        };

        // RFC 6347 4.2.1: The server_version field in the HelloVerifyRequest message MUST be set to DTLS 1.0
        // https://datatracker.ietf.org/doc/html/rfc6347#section-4.2.1
        if h.server_version != ProtocolVersion::DTLS1_2
            && h.server_version != ProtocolVersion::DTLS1_0
        {
            return Err(Error::SecurityError(
                crate::SecurityError::UnsupportedHelloVerifyRequestVersion(h.server_version),
            )
            .into());
        }

        debug!(
            "Received HelloVerifyRequest with cookie length: {}",
            h.cookie.len()
        );

        // Set cookie for next ClientHello
        client.cookie = Some(h.cookie);

        // HelloVerifyRequest exchange must not be part of the handshake transcript.
        // Per RFC 6347 §4.2.2, the next ClientHello (with cookie) has message_seq=1.
        trace!("Resetting handshake state after HelloVerifyRequest");
        client.engine.reset_client_for_hello_verify_request();

        // Redo ClientHello, now with cookie.
        Ok(Self::SendClientHello)
    }

    fn await_server_hello(self, client: &mut Client) -> Result<Self, InternalError> {
        let maybe = client
            .engine
            .next_handshake(MessageType::ServerHello, &mut client.defragment_buffer)?;

        let Some(handshake) = maybe else {
            // Stay in same state
            return Ok(self);
        };

        let Body::ServerHello(server_hello) = &handshake.body else {
            unreachable!()
        };

        debug!(
            "Received ServerHello with cipher suite: {:?}",
            server_hello.cipher_suite
        );

        // Enforce DTLS version
        if server_hello.server_version != ProtocolVersion::DTLS1_2 {
            return Err(
                Error::SecurityError(crate::SecurityError::UnsupportedServerVersion(
                    server_hello.server_version,
                ))
                .into(),
            );
        }

        // Enforce Null compression only
        if server_hello.compression_method != CompressionMethod::Null {
            return Err(
                Error::SecurityError(crate::SecurityError::UnsupportedServerCompression(
                    server_hello.compression_method,
                ))
                .into(),
            );
        }

        // Enforce cipher suite is known and allowed
        let cs = server_hello.cipher_suite;
        if cs.is_unknown() {
            return Err((Error::SecurityError(
                crate::SecurityError::ServerSelectedUnknownCipherSuite,
            ))
            .into());
        }

        // Enforce cipher suite is compatible with our private key and allowed by config
        let is_compatible = client
            .engine
            .crypto_context()
            .is_cipher_suite_compatible(cs);

        if !is_compatible {
            return Err(Error::SecurityError(
                crate::SecurityError::ServerSelectedIncompatibleCipherSuite(cs),
            )
            .into());
        }

        if !client.engine.is_cipher_suite_allowed(cs) {
            return Err(Error::SecurityError(
                crate::SecurityError::ServerSelectedDisallowedCipherSuite(cs),
            )
            .into());
        }

        // Note: we keep offered suites local; we don't enforce echo here
        client.engine.set_cipher_suite(cs);
        client.session_id = Some(server_hello.session_id);
        client.server_random = Some(server_hello.random);

        let Some(extensions) = &server_hello.extensions else {
            return Err((Error::IncompleteServerHello).into());
        };
        let extended_master_secret = extensions
            .iter()
            .any(|ext| ext.extension_type == ExtensionType::ExtendedMasterSecret);
        if let Some(profile) = server_hello
            .srtp_profiles()
            .and_then(|profiles| profiles.first())
        {
            client.negotiated_srtp_profile = Some((*profile).into());
        }

        // Without extended master secret, in DTLS1.2 a security attack
        // reusing the same master secret is possible.
        if !extended_master_secret {
            return Err(Error::SecurityError(
                crate::SecurityError::ExtendedMasterSecretNotNegotiated,
            )
            .into());
        }

        if let Some(profile) = client.negotiated_srtp_profile {
            debug!("Negotiated SRTP profile: {:?}", profile);
        }
        trace!("Extended Master Secret enabled");

        client.hybrid = None;

        // PSK suites skip Certificate; go directly to ServerKeyExchange
        if cs.is_psk() {
            Ok(Self::AwaitServerKeyExchange)
        } else {
            Ok(Self::AwaitCertificate)
        }
    }

    fn await_certificate(self, client: &mut Client) -> Result<Self, InternalError> {
        let maybe = client
            .engine
            .next_handshake(MessageType::Certificate, &mut client.defragment_buffer)?;

        let Some(ref handshake) = maybe else {
            // Stay in same state
            return Ok(self);
        };

        let Body::Certificate(certificate) = &handshake.body else {
            unreachable!()
        };

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

        // Extract certificate ranges before dropping handshake
        let cert_ranges: ArrayVec<_, 32> = certificate
            .certificate_list
            .iter()
            .map(|cert| cert.0.clone())
            .collect();

        drop(maybe);

        // Convert ASN.1 certificates to byte arrays
        for (i, range) in cert_ranges.iter().enumerate() {
            let cert_data = &client.defragment_buffer[range.clone()];
            trace!("Certificate #{} size: {} bytes", i + 1, cert_data.len());
            client.server_certificates.push(cert_data.to_buf());
        }

        Ok(Self::AwaitServerKeyExchange)
    }

    fn await_server_key_exchange(self, client: &mut Client) -> Result<Self, InternalError> {
        let cipher_suite = client.engine.cipher_suite().ok_or(Error::InvalidState(
            crate::InvalidStateError::NoCipherSuiteSelected,
        ))?;

        if cipher_suite.is_psk() {
            self.await_server_key_exchange_psk(client)
        } else {
            self.await_server_key_exchange_ecdhe(client)
        }
    }

    fn await_server_key_exchange_ecdhe(self, client: &mut Client) -> Result<Self, InternalError> {
        let maybe = client.engine.next_handshake(
            MessageType::ServerKeyExchange,
            &mut client.defragment_buffer,
        )?;

        if maybe.is_none() {
            // Stay in same state
            return Ok(self);
        };

        // Extract all ranges/data we need before accessing buffer
        let (signature_range, signature_algorithm, curve_type, named_group, public_key_range) = {
            let handshake = maybe.as_ref().unwrap();
            let Body::ServerKeyExchange(server_key_exchange) = &handshake.body else {
                unreachable!()
            };

            let Some(d_signed) = server_key_exchange.signature() else {
                // We do not support anonymous key exchange
                return Err(Error::UnexpectedMessage(
                    crate::UnexpectedMessageError::ServerKeyExchangeWithoutSignature,
                )
                .into());
            };

            let signature_range = d_signed.signature_range.clone();
            let signature_algorithm = d_signed.algorithm;

            // Extract ECDH params ranges
            let (curve_type, named_group, public_key_range) = match &server_key_exchange.params {
                ServerKeyExchangeParams::Ecdh(ecdh) => (
                    ecdh.curve_type,
                    ecdh.named_group,
                    ecdh.public_key_range.clone(),
                ),
                ServerKeyExchangeParams::Psk(_) => {
                    return Err(Error::UnexpectedMessage(
                        crate::UnexpectedMessageError::PskServerKeyExchangeInEcdhePath,
                    )
                    .into());
                }
            };

            (
                signature_range,
                signature_algorithm,
                curve_type,
                named_group,
                public_key_range,
            )
        };

        // unwrap: is ok because we verify the order of the flight
        let client_random = client.random.unwrap();
        let server_random = client.server_random.unwrap();

        // Drop maybe to release the buffer borrow
        drop(maybe);

        // Now we can access the buffer to get the actual bytes
        let signature_bytes = &client.defragment_buffer[signature_range];
        let public_key_vec = &client.defragment_buffer[public_key_range];

        // Build signed_data = client_random || server_random || SKE params (without signature)
        let mut signed_data = Buf::new();
        client_random.serialize(&mut signed_data);
        server_random.serialize(&mut signed_data);
        // Manually serialize SKE params
        signed_data.push(curve_type.as_u8());
        signed_data.extend_from_slice(&named_group.as_u16().to_be_bytes());
        signed_data.push(public_key_vec.len() as u8);
        signed_data.extend_from_slice(public_key_vec);

        let cipher_suite = client.engine.cipher_suite().ok_or(Error::InvalidState(
            crate::InvalidStateError::NoCipherSuiteSelected,
        ))?;

        // Ensure the server's (hash, signature) pair was offered by the client
        let offered = SignatureAndHashAlgorithm::supported().contains(&signature_algorithm);
        if !offered {
            return Err(Error::CryptoError(
                crate::CryptoError::SignatureAlgorithmNotOfferedByClient,
            )
            .into());
        }

        // Ensure the signature algorithm is compatible with the cipher suite
        if let Some(expected_sig) = cipher_suite.signature_algorithm()
            && signature_algorithm.signature != expected_sig
        {
            return Err(
                Error::CryptoError(crate::CryptoError::SignatureAlgorithmMismatch {
                    expected: expected_sig,
                    actual: signature_algorithm.signature,
                })
                .into(),
            );
        }

        // unwrap: is ok because we verify the order of the flight
        let cert_der = client.server_certificates.first().unwrap();

        // Create a temporary DigitallySigned for verification (we only need the algorithm)
        let temp_signed = DigitallySigned {
            algorithm: signature_algorithm,
            signature_range: 0..signature_bytes.len(),
        };

        client
            .engine
            .crypto_context_mut()
            .verify_signature(&signed_data, &temp_signed, signature_bytes, cert_der)
            .map_err(Error::CryptoError)?;

        trace!(
            "ServerKeyExchange signature verified: {:?}",
            signature_algorithm
        );

        // Process the server key exchange parameters
        // We already have the curve and public key extracted
        let kx_buf = client.engine.pop_buffer();
        let shared_secret = client.engine.pop_buffer();
        client
            .engine
            .crypto_context_mut()
            .process_ecdh_params(named_group, public_key_vec, kx_buf, shared_secret)
            .map_err(Error::CryptoError)?;

        Ok(Self::AwaitCertificateRequest)
    }

    /// PSK ServerKeyExchange carries only an optional identity hint (no signature).
    /// Per RFC 4279 §2, ServerKeyExchange is omitted when the server has no hint.
    fn await_server_key_exchange_psk(self, client: &mut Client) -> Result<Self, InternalError> {
        // If the server skipped ServerKeyExchange (no hint), go straight to ServerHelloDone
        let has_done = client
            .engine
            .has_complete_handshake(MessageType::ServerHelloDone);
        if has_done {
            return Ok(Self::AwaitServerHelloDone);
        }

        let maybe = client.engine.next_handshake(
            MessageType::ServerKeyExchange,
            &mut client.defragment_buffer,
        )?;

        let Some(handshake) = maybe else {
            return Ok(self);
        };

        let Body::ServerKeyExchange(ske) = &handshake.body else {
            unreachable!()
        };

        // PSK ServerKeyExchange contains only an identity hint per RFC 4279 §2
        // (no curve_type or named_group — those are ECDHE-only parameters).
        let hint_range = match &ske.params {
            ServerKeyExchangeParams::Psk(psk) => psk.hint_range.clone(),
            _ => {
                return Err(Error::UnexpectedMessage(
                    crate::UnexpectedMessageError::EcdheServerKeyExchangeInPskPath,
                )
                .into());
            }
        };

        drop(handshake);

        let hint = &client.defragment_buffer[hint_range];
        trace!("PSK identity hint ({} bytes)", hint.len());
        // Hint is informational only; we don't use it for PSK lookup currently

        // PSK has no CertificateRequest
        Ok(Self::AwaitServerHelloDone)
    }

    fn await_certificate_request(self, client: &mut Client) -> Result<Self, InternalError> {
        let has_done = client
            .engine
            .has_complete_handshake(MessageType::ServerHelloDone);

        if has_done {
            return Ok(Self::AwaitServerHelloDone);
        }

        let maybe = client.engine.next_handshake(
            MessageType::CertificateRequest,
            &mut client.defragment_buffer,
        )?;

        let Some(handshake) = maybe else {
            // stay in same state
            return Ok(self);
        };

        let Body::CertificateRequest(cr) = &handshake.body else {
            unreachable!()
        };

        // Check that the hash algorithm that is default fo the PrivateKey in use
        // is one of the supported by the CertificateRequest
        // unwrap: CertificateRequest only received for certificate-based suites
        let hash_algorithm = client
            .engine
            .crypto_context()
            .private_key_default_hash_algorithm()
            .unwrap();

        if !cr.supports_hash_algorithm(hash_algorithm) {
            return Err(Error::CertificateError(
                crate::CertificateError::UnsupportedHashAlgorithm(hash_algorithm),
            )
            .into());
        }

        debug!(
            "Server supports CertificateVerify hash algorithm: {:?}",
            hash_algorithm
        );

        debug!("Received CertificateRequest; enabling client authentication path");
        client.certificate_verify = true;

        Ok(Self::AwaitServerHelloDone)
    }

    fn await_server_hello_done(self, client: &mut Client) -> Result<Self, InternalError> {
        let maybe = client
            .engine
            .next_handshake(MessageType::ServerHelloDone, &mut client.defragment_buffer)?;

        let Some(handshake) = maybe else {
            // stay in same state
            return Ok(self);
        };

        let Body::ServerHelloDone = handshake.body else {
            unreachable!()
        };

        trace!("Received ServerHelloDone");

        let cipher_suite = client.engine.cipher_suite().ok_or(Error::InvalidState(
            crate::InvalidStateError::NoCipherSuiteSelected,
        ))?;

        if cipher_suite.is_psk() {
            // PSK: no certificates involved
            return Ok(Self::SendClientKeyExchange);
        }

        // Validate the server certificate
        if client.server_certificates.is_empty() {
            return Err((Error::CertificateError(
                crate::CertificateError::NoServerCertificateReceived,
            ))
            .into());
        }

        // Send the server certificate as an event
        if !client.server_certificates.is_empty() {
            client.local_events.push_back(LocalEvent::PeerCert);
        }

        if client.certificate_verify {
            Ok(Self::SendCertificate)
        } else {
            Ok(Self::SendClientKeyExchange)
        }
    }

    fn send_certificate(self, client: &mut Client) -> Result<Self, InternalError> {
        debug!("Sending Certificate");

        // Start/restart flight timer for client Flight 5
        client.engine.flight_begin(5);

        // Now use the engine with the stored data
        client
            .engine
            .create_handshake(MessageType::Certificate, handshake_create_certificate)?;

        Ok(Self::SendClientKeyExchange)
    }

    fn send_client_key_exchange(self, client: &mut Client) -> Result<Self, InternalError> {
        trace!("Sending ClientKeyExchange");

        // Start/restart flight timer only if this flight did not start with Certificate
        if !client.certificate_verify {
            client.engine.flight_begin(5);
        }

        // Send client key exchange message
        client.engine.create_handshake(
            MessageType::ClientKeyExchange,
            handshake_create_client_key_exchange,
        )?;

        // Capture session hash now for Extended Master Secret (RFC 7627)
        // At this point, the session hash includes: ClientHello, ServerHello, Certificate,
        // ServerKeyExchange, CertificateRequest, ServerHelloDone, Certificate, ClientKeyExchange
        // This is correct per RFC 7627 - session hash should include messages
        // up to and including ClientKeyExchange
        let cipher_suite = client.engine.cipher_suite().ok_or(Error::InvalidState(
            crate::InvalidStateError::NoCipherSuiteSelected,
        ))?;

        let suite_hash = cipher_suite.hash_algorithm();
        let mut buf = client.engine.pop_buffer();
        client.engine.transcript_hash(suite_hash, &mut buf);
        client.captured_session_hash = Some(buf);

        if client.certificate_verify {
            Ok(Self::SendCertificateVerify)
        } else {
            Ok(Self::SendChangeCipherSpec)
        }
    }

    fn send_certificate_verify(self, client: &mut Client) -> Result<Self, InternalError> {
        debug!("Sending CertificateVerify");

        // Send the certificate verify message
        client.engine.create_handshake(
            MessageType::CertificateVerify,
            handshake_create_certificate_verify,
        )?;

        Ok(Self::SendChangeCipherSpec)
    }

    fn send_change_cipher_spec(self, client: &mut Client) -> Result<Self, InternalError> {
        Self::derive_keys(client)?;

        // Send change cipher spec
        trace!("Sending ChangeCipherSpec");
        client
            .engine
            .create_record(ContentType::ChangeCipherSpec, 0, true, |body| {
                // Change cipher spec is just a single byte with value 1
                body.push(1);
            })?;

        Ok(Self::SendFinished)
    }

    fn derive_keys(client: &mut Client) -> Result<(), Error> {
        trace!("Deriving keys");
        let Some(cipher_suite) = client.engine.cipher_suite() else {
            return Err(Error::InvalidState(
                crate::InvalidStateError::NoCipherSuiteSelected,
            ));
        };

        trace!("Using cipher suite for key derivation: {:?}", cipher_suite);

        let Some(server_random) = &client.server_random else {
            return Err(Error::InvalidState(
                crate::InvalidStateError::NoServerRandom,
            ));
        };

        // Extract and format the random values for key derivation
        let mut client_random_buf_b = Buf::new();
        let mut server_random_buf_b = Buf::new();

        // Serialize the random values to raw bytes
        // unwrap: is ok because we set the random in handle_timeout
        client.random.unwrap().serialize(&mut client_random_buf_b);
        server_random.serialize(&mut server_random_buf_b);
        let client_random_buf = client_random_buf_b;
        let server_random_buf = server_random_buf_b;

        // Derive master secret (use EMS if negotiated)
        let suite_hash = cipher_suite.hash_algorithm();

        // Use the captured session hash from when ServerHelloDone was received
        let session_hash = client
            .captured_session_hash
            .as_ref()
            .ok_or(Error::InvalidState(
                crate::InvalidStateError::ExtendedMasterSecretSessionHashMissing,
            ))?;
        trace!(
            "Using captured session hash for Extended Master Secret (length: {})",
            session_hash.len()
        );
        let mut out = client.engine.pop_buffer();
        let mut scratch = client.engine.pop_buffer();
        client
            .engine
            .crypto_context_mut()
            .derive_extended_master_secret(session_hash, suite_hash, &mut out, &mut scratch)
            .map_err(Error::CryptoError)?;

        // Derive the encryption/decryption keys
        client
            .engine
            .crypto_context_mut()
            .derive_keys(
                cipher_suite,
                &client_random_buf,
                &server_random_buf,
                &mut out,
                &mut scratch,
            )
            .map_err(Error::CryptoError)?;

        client.engine.push_buffer(out);
        client.engine.push_buffer(scratch);

        Ok(())
    }

    fn send_finished(self, client: &mut Client) -> Result<Self, InternalError> {
        trace!("Sending Finished message to complete handshake");

        client
            .engine
            .create_handshake(MessageType::Finished, |body, engine| {
                // Calculate verify data for Finished message using PRF
                let verify_data = engine.generate_verify_data(true)?;

                debug!("Generated verify data for Finished message (12 bytes)");

                // Directly write the verify data without creating Finished struct
                body.extend_from_slice(&verify_data);
                Ok(())
            })?;

        Ok(Self::AwaitChangeCipherSpec)
    }

    fn await_change_cipher_spec(self, client: &mut Client) -> Result<Self, InternalError> {
        let maybe = client.engine.next_record(ContentType::ChangeCipherSpec);

        let Some(_) = maybe else {
            // Stay in same state
            return Ok(self);
        };

        // Drop any extra CCS resends to avoid being blocked
        trace!("Dropping any pending CCS resends from peer");
        client.engine.drop_pending_ccs();

        // Expect every record to be decrypted from now on.
        trace!("Received ChangeCipherSpec; enabling peer encryption");
        client.engine.enable_peer_encryption()?;

        Ok(Self::AwaitNewSessionTicket)
    }

    fn await_new_session_ticket(self, client: &mut Client) -> Result<Self, InternalError> {
        let has_finished = client.engine.has_complete_handshake(MessageType::Finished);

        if has_finished {
            return Ok(Self::AwaitFinished);
        }

        let maybe = client
            .engine
            .next_handshake(MessageType::NewSessionTicket, &mut client.defragment_buffer)?;

        let Some(handshake) = maybe else {
            // Stay in same state
            return Ok(self);
        };

        let Body::NewSessionTicket(_t) = handshake.body else {
            unreachable!()
        };

        // TODO(martin): handle ticket for restart

        trace!("Received NewSessionTicket");

        Ok(Self::AwaitFinished)
    }

    fn await_finished(self, client: &mut Client) -> Result<Self, InternalError> {
        // Generate expected verify data based on current transcript.
        // This must be done before next_handshake() below since
        // it should not include Finished itself.
        let expected = client.engine.generate_verify_data(false)?;

        let maybe = client
            .engine
            .next_handshake(MessageType::Finished, &mut client.defragment_buffer)?;

        if maybe.is_none() {
            // stay in same state
            return Ok(self);
        }

        // Extract the range from the handshake
        let verify_data_range = if let Some(ref handshake) = maybe {
            if let Body::Finished(finished) = &handshake.body {
                finished.verify_data_range.clone()
            } else {
                panic!("Finished message should have been parsed");
            }
        } else {
            unreachable!()
        };

        // Drop maybe to release the buffer borrow
        drop(maybe);

        // Now we can access the buffer
        let verify_data = &client.defragment_buffer[verify_data_range];
        trace!(
            "Finished.verify_data received len={}, expected len={}",
            verify_data.len(),
            expected.len()
        );
        // Use constant-time comparison to prevent timing attacks
        let is_eq: bool = verify_data.ct_eq(expected.as_slice()).into();
        if !is_eq {
            return Err((Error::SecurityError(
                crate::SecurityError::ServerFinishedVerificationFailed,
            ))
            .into());
        }

        // No records from the handshake epochs may survive into application data.
        let unexpected_record = client
            .engine
            .pending_records()
            .find(|record| record.record().sequence.epoch != 1);
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

        trace!("Server Finished verified successfully");

        // Receiving server Finished implicitly acks our Flight 5; stop resends
        client.engine.flight_stop_resend_timers();

        // Emit Connected event
        client.local_events.push_back(LocalEvent::Connected);

        // Extract and emit SRTP keying material if we have a negotiated profile
        if let Some(profile) = client.negotiated_srtp_profile {
            let suite_hash = client.engine.cipher_suite().unwrap().hash_algorithm();

            let mut out = client.engine.pop_buffer();
            let mut scratch = client.engine.pop_buffer();
            if let Ok(keying_material) = client
                .engine
                .crypto_context()
                .extract_srtp_keying_material(profile, suite_hash, &mut out, &mut scratch)
            {
                client.engine.push_buffer(out);
                client.engine.push_buffer(scratch);
                // Emit the keying material event with the negotiated profile
                debug!(
                    "SRTP keying material extracted ({} bytes) for profile: {:?}",
                    keying_material.len(),
                    profile
                );
                // expect should be correct here since we negotiated the profile
                let profile = client
                    .negotiated_srtp_profile
                    .expect("SRTP profile should be negotiated");
                client
                    .local_events
                    .push_back(LocalEvent::KeyingMaterial(keying_material, profile));
            } else {
                client.engine.push_buffer(out);
                client.engine.push_buffer(scratch);
            }
        }

        client.engine.release_application_data();

        debug!("Handshake complete; ready for application data");

        Ok(Self::AwaitApplicationData)
    }

    fn await_application_data(self, client: &mut Client) -> Result<Self, InternalError> {
        if client.engine.close_notify_received() {
            // RFC 5246 §7.2.1: respond with a reciprocal close_notify and
            // close down immediately, discarding any pending writes.
            client.engine.discard_pending_writes();
            client
                .engine
                .create_record(ContentType::Alert, 1, false, |body| {
                    body.push(1); // level: warning
                    body.push(0); // description: close_notify
                })?;
            return Ok(State::Closed);
        }

        if !client.queued_data.is_empty() {
            debug!(
                "Sending queued application data: {}",
                client.queued_data.len()
            );
            for data in client.queued_data.drain(..) {
                client
                    .engine
                    .create_record(ContentType::ApplicationData, 1, false, |body| {
                        body.extend_from_slice(&data);
                    })?;
            }
        }

        Ok(self)
    }
}

fn handshake_create_client_hello(
    body: &mut Buf,
    engine: &mut Engine,
    cookie: Cookie,
    random: Random,
    session_id: SessionId,
    extension_data: &mut Buf,
) -> Result<(), Error> {
    let client_version = ProtocolVersion::DTLS1_2;

    // Get cipher suites from config that are compatible with our key
    let cipher_suites: CipherSuiteVec = engine
        .config()
        .dtls12_cipher_suites()
        .map(|cs| cs.suite())
        .filter(|suite| engine.crypto_context().is_cipher_suite_compatible(*suite))
        .take(Dtls12CipherSuite::supported().len())
        .collect();

    debug!(
        "Sending ClientHello: DTLS version={:?}, cookie_len={}, offering {} cipher suites",
        client_version,
        cookie.len(),
        cipher_suites.len()
    );

    let mut compression_methods = ArrayVec::new();
    compression_methods.push(CompressionMethod::Null);

    // Create ClientHello with all required extensions
    let client_hello = ClientHello::new(
        client_version,
        random,
        session_id,
        cookie,
        cipher_suites,
        compression_methods,
    )
    .with_extensions(extension_data, engine.config());

    client_hello.serialize(extension_data, body);
    Ok(())
}

fn handshake_create_certificate(body: &mut Buf, engine: &mut Engine) -> Result<(), Error> {
    let crypto = engine.crypto_context();
    crypto.serialize_client_certificate(body);
    Ok(())
}

fn handshake_create_client_key_exchange(body: &mut Buf, engine: &mut Engine) -> Result<(), Error> {
    // Just check that a cipher suite exists without binding to unused variable
    let Some(cipher_suite) = engine.cipher_suite() else {
        return Err(Error::InvalidState(
            crate::InvalidStateError::NoCipherSuiteSelected,
        ));
    };
    let key_exchange_algorithm = cipher_suite.as_key_exchange_algorithm();

    debug!("Using key exchange algorithm: {:?}", key_exchange_algorithm);

    match key_exchange_algorithm {
        KeyExchangeAlgorithm::EECDH => {
            // Get group info before the mutable borrow
            let group_info = engine.crypto_context().get_key_exchange_group_info();

            // For ECDHE, use the group information we retrieved earlier
            let Some((curve_type, named_group)) = group_info else {
                unreachable!("No group info available for ECDHE");
            };

            trace!(
                "Using ECDHE group info: {:?}, {:?}",
                curve_type, named_group
            );

            let public_key = engine
                .crypto_context_mut()
                .maybe_init_key_exchange()
                .map_err(Error::CryptoError)?;

            trace!("Generated public key size: {} bytes", public_key.len());
            ClientKeyExchange::serialize_from_bytes(public_key, body);
        }
        KeyExchangeAlgorithm::PSK => {
            let identity = engine
                .config()
                .psk_identity()
                .ok_or(Error::PskError(crate::PskError::NoPskIdentityConfigured))?
                .to_vec();

            // Resolve the PSK via the configured resolver
            let psk = engine
                .config()
                .psk_resolver()
                .ok_or(Error::PskError(crate::PskError::NoPskResolverConfigured))?
                .resolve(&identity)
                .ok_or(Error::PskError(crate::PskError::ResolverReturnedNoKey))?;

            // Set the PSK and compute pre-master secret
            let crypto = engine.crypto_context_mut();
            crypto.set_psk(psk);
            crypto
                .compute_psk_pre_master_secret()
                .map_err(Error::CryptoError)?;

            ClientPskKeys::serialize_from_bytes(&identity, body);
        }
        _ => {
            return Err(Error::SecurityError(
                crate::SecurityError::UnsupportedKeyExchangeAlgorithm,
            ));
        }
    }

    Ok(())
}

fn handshake_create_certificate_verify(body: &mut Buf, engine: &mut Engine) -> Result<(), Error> {
    // The hash algorithm to use is the default for the private key type, not
    // the one negotiated to use with the selected cipher suite. I.e.
    // if we negotiate ECDHE_ECDSA_AES256_GCM_SHA384, we are gogin to use
    // SHA384 for the signature of the main crypto, but not for CertificateVerify
    // where a private key using P256 curve means we use SHA256.
    // unwrap: CertificateVerify only sent for certificate-based suites
    let hash_alg = engine
        .crypto_context()
        .private_key_default_hash_algorithm()
        .unwrap();
    debug!("Using hash algorithm for signature: {:?}", hash_alg);

    // Get the signature algorithm type
    // unwrap: CertificateVerify only sent for certificate-based suites
    let sig_alg = engine.crypto_context().signature_algorithm().unwrap();
    debug!("Using signature algorithm: {:?}", sig_alg);

    // Create the signature algorithm
    let algorithm = SignatureAndHashAlgorithm::new(hash_alg, sig_alg);

    let mut signature = engine.pop_buffer();

    // Sign all handshake messages
    let handshake_data = &engine.transcript;
    engine
        .crypto_context
        .sign_data(handshake_data, hash_alg, &mut signature)
        .map_err(Error::CryptoError)?;

    debug!("Generated signature size: {} bytes", signature.len());

    // For sending, directly write the CertificateVerify bytes
    body.extend_from_slice(&algorithm.as_u16().to_be_bytes());
    body.extend_from_slice(&(signature.len() as u16).to_be_bytes());
    body.extend_from_slice(&signature);

    engine.push_buffer(signature);

    Ok(())
}

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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn psk_client() -> Client {
        let engine = Engine::new(Arc::new(Config::default()), AuthMode::Psk);
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
    fn empty_server_certificate_is_certificate_error() {
        let mut client = psk_client();
        client
            .engine
            .parse_packet(&epoch0_handshake_packet(
                MessageType::Certificate,
                0,
                &[0, 0, 0],
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
    fn derive_keys_without_cipher_suite_is_invalid_state() {
        let mut client = psk_client();

        let err = State::derive_keys(&mut client)
            .expect_err("derive_keys requires negotiated cipher suite");

        assert!(matches!(err, Error::InvalidState(_)));
    }

    #[test]
    fn derive_keys_without_server_random_is_invalid_state() {
        let mut client = psk_client();
        client
            .engine
            .set_cipher_suite(Dtls12CipherSuite::PSK_AES128_CCM_8);

        let err = State::derive_keys(&mut client).expect_err("derive_keys requires server random");

        assert!(matches!(err, Error::InvalidState(_)));
    }
}
