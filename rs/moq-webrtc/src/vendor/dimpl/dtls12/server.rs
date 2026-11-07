// DTLS Server Handshake Flow:
//
// 1. Client sends ClientHello (maybe without cookie)
// 2. If cookie missing/invalid, Server sends HelloVerifyRequest (stateless cookie)
//    - Client resends ClientHello with cookie
// 3. Server sends ServerHello, Certificate, ServerKeyExchange,
//    CertificateRequest (required), ServerHelloDone
// 4. Client sends Certificate (required when requested), ClientKeyExchange,
//    CertificateVerify (if client cert), ChangeCipherSpec, Finished
// 5. Server verifies Finished, then sends ChangeCipherSpec, Finished
// 6. Handshake complete, application data can flow
//
// This implementation mirrors the client structure and ordering for a DTLS server.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use arrayvec::ArrayVec;
use subtle::ConstantTimeEq;

use crate::buffer::{Buf, ToBuf};
use crate::crypto::SrtpProfile;
use crate::dtls12::Client;
use crate::dtls12::client::LocalEvent;
use crate::dtls12::context::AuthMode;
use crate::dtls12::engine::Engine;
use crate::dtls12::message::PskParams;
use crate::dtls12::message::SignatureAndHashAlgorithm;
use crate::dtls12::message::{Body, CertificateRequest, CertificateTypeVec, Dtls12CipherSuite};
use crate::dtls12::message::{ClientCertificateType, CompressionMethod, ContentType};
use crate::dtls12::message::{Cookie, CurveType, DistinguishedName, ExchangeKeys, ExtensionType};
use crate::dtls12::message::{HashAlgorithm, HelloVerifyRequest, KeyExchangeAlgorithm};
use crate::dtls12::message::{MessageType, NamedGroup, NamedGroupVec, ProtocolVersion, Random};
use crate::dtls12::message::{ServerHello, SessionId, SignatureAlgorithm};
use crate::dtls12::message::{SignatureAndHashAlgorithmVec, SrtpProfileId};
use crate::timer::Timeout;
use crate::{Config, Error, InternalError, Output};

/// Length of the random dummy PSK used when identity resolution fails.
/// Fixed so handshake timing does not leak the failure, and long enough
/// that a coincidental match with a real 32-byte PSK is cryptographically
/// impossible.
const DUMMY_PSK_LEN: usize = 32;

/// DTLS server
pub struct Server {
    /// Current server state.
    state: State,

    /// Engine in common between server and client.
    engine: Engine,

    /// Random unique data (with gmt timestamp). Used for signature checks.
    random: Option<Random>,

    /// SessionId we provide to the client (unused/resumption not implemented).
    session_id: Option<SessionId>,

    /// Cookie secret for HMAC, generated per-server instance
    cookie_secret: [u8; 32],

    /// Storage for extension data
    extension_data: Buf,

    /// The negotiated SRTP profile (if any)
    negotiated_srtp_profile: Option<SrtpProfile>,

    /// Client's offered supported_groups (if any)
    client_supported_groups: Option<NamedGroupVec>,

    /// Client's offered signature_algorithms (if any)
    client_signature_algorithms: Option<SignatureAndHashAlgorithmVec>,

    /// Client random. Set by ClientHello.
    client_random: Option<Random>,

    /// Client certificates
    client_certificates: Vec<Buf>,

    /// Buffer for defragmenting handshakes
    defragment_buffer: Buf,

    /// Captured session hash for Extended Master Secret (RFC 7627)
    captured_session_hash: Option<Buf>,

    /// Whether the PSK identity resolved to a real key.
    ///
    /// `None` until the PSK ClientKeyExchange is processed. Non-PSK handshakes
    /// leave this `None` and the Finished check skips the PSK branch entirely.
    /// Defaulting to `None` (rather than `true`) means a future refactor that
    /// forgets to set it in the PSK path fails closed instead of silently
    /// bypassing identity validation.
    psk_valid: Option<bool>,

    /// The last now we seen
    last_now: Instant,

    /// Events we are to emit from this Server.
    local_events: VecDeque<LocalEvent>,

    /// Data that is sent before we are connected.
    queued_data: Vec<Buf>,
}

/// Current state of the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    AwaitClientHello,
    SendServerHello,
    SendCertificate,
    SendServerKeyExchange,
    SendCertificateRequest,
    SendServerHelloDone,
    AwaitCertificate,
    AwaitClientKeyExchange,
    AwaitCertificateVerify,
    AwaitChangeCipherSpec,
    AwaitFinished,
    SendChangeCipherSpec,
    SendFinished,
    AwaitApplicationData,
    Closed,
}

impl Server {
    /// Create a new DTLS server
    pub fn new(config: Arc<Config>, certificate: crate::DtlsCertificate, now: Instant) -> Server {
        assert!(
            !certificate.certificate.is_empty(),
            "Server certificate cannot be empty"
        );
        // unwrap: malformed private_key bytes are a programmer error from the
        // caller who constructed DtlsCertificate; panic matches the prior
        // CryptoContext::new behavior which also panicked on empty/invalid
        // key material.
        let private_key = config
            .crypto_provider()
            .key_provider
            .load_private_key(&certificate.private_key)
            .expect("Failed to parse server private key");
        let auth = AuthMode::Certificate {
            certificate: certificate.certificate,
            private_key,
        };
        let engine = Engine::new(config, auth);
        Self::new_with_engine(engine, now)
    }

    /// Create a new PSK-only DTLS server (no certificate).
    pub fn new_psk(config: Arc<Config>, now: Instant) -> Server {
        let engine = Engine::new(config, AuthMode::Psk);
        Self::new_with_engine(engine, now)
    }

    pub(crate) fn new_with_engine(mut engine: Engine, now: Instant) -> Server {
        engine.set_client(false);

        let cookie_secret: [u8; 32] = engine.rng.random();
        let extension_data = engine.pop_buffer();
        let defragment_buffer = engine.pop_buffer();

        Server {
            state: State::AwaitClientHello,
            engine,
            random: None,
            session_id: None,
            cookie_secret,
            extension_data,
            negotiated_srtp_profile: None,
            client_supported_groups: None,
            client_signature_algorithms: None,
            client_random: None,
            client_certificates: Vec::with_capacity(3),
            defragment_buffer,
            captured_session_hash: None,
            psk_valid: None,
            last_now: now,
            local_events: VecDeque::new(),
            queued_data: Vec::new(),
        }
    }

    pub fn into_client(self) -> Client {
        Client::new_with_engine(self.engine, self.last_now)
    }

    pub fn set_handshake_deadline(&mut self, deadline: Timeout) {
        self.engine.set_handshake_deadline(deadline);
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
        match self
            .engine
            .parse_packet(packet)
            .and_then(|is_client_hello| {
                if is_client_hello {
                    self.engine.start_handshake(self.last_now);
                }
                let result = self.make_progress();
                self.engine.discard_undecryptable();
                result
            }) {
            Ok(()) => Ok(()),
            Err(e) => e.into_public_error().map_or(Ok(()), Err),
        }
    }

    pub fn poll_output<'a>(&mut self, buf: &'a mut [u8]) -> Output<'a> {
        if let Some(event) = self.local_events.pop_front() {
            return event.into_output(buf, &self.client_certificates);
        }
        self.engine.poll_output(buf, self.last_now)
    }

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

    /// Send application data when the server is in the Running state
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

impl State {
    fn name(&self) -> &'static str {
        match self {
            State::AwaitClientHello => "AwaitClientHello",
            State::SendServerHello => "SendServerHello",
            State::SendCertificate => "SendCertificate",
            State::SendServerKeyExchange => "SendServerKeyExchange",
            State::SendCertificateRequest => "SendCertificateRequest",
            State::SendServerHelloDone => "SendServerHelloDone",
            State::AwaitCertificate => "AwaitCertificate",
            State::AwaitClientKeyExchange => "AwaitClientKeyExchange",
            State::AwaitCertificateVerify => "AwaitCertificateVerify",
            State::AwaitChangeCipherSpec => "AwaitChangeCipherSpec",
            State::AwaitFinished => "AwaitFinished",
            State::SendChangeCipherSpec => "SendChangeCipherSpec",
            State::SendFinished => "SendFinished",
            State::AwaitApplicationData => "AwaitApplicationData",
            State::Closed => "Closed",
        }
    }

    fn make_progress(self, server: &mut Server) -> Result<Self, InternalError> {
        match self {
            State::AwaitClientHello => self.await_client_hello(server),
            State::SendServerHello => self.send_server_hello(server),
            State::SendCertificate => self.send_certificate(server),
            State::SendServerKeyExchange => self.send_server_key_exchange(server),
            State::SendCertificateRequest => self.send_certificate_request(server),
            State::SendServerHelloDone => self.send_server_hello_done(server),
            State::AwaitCertificate => self.await_certificate(server),
            State::AwaitClientKeyExchange => self.await_client_key_exchange(server),
            State::AwaitCertificateVerify => self.await_certificate_verify(server),
            State::AwaitChangeCipherSpec => self.await_change_cipher_spec(server),
            State::AwaitFinished => self.await_finished(server),
            State::SendChangeCipherSpec => self.send_change_cipher_spec(server),
            State::SendFinished => self.send_finished(server),
            State::AwaitApplicationData => self.await_application_data(server),
            State::Closed => Ok(self),
        }
    }

    fn await_client_hello(self, server: &mut Server) -> Result<Self, InternalError> {
        let maybe = server
            .engine
            .next_handshake(MessageType::ClientHello, &mut server.defragment_buffer)?;

        let Some(handshake) = maybe else {
            // Stay in same state
            return Ok(self);
        };

        let Body::ClientHello(ch) = handshake.body else {
            unreachable!()
        };

        // Enforce DTLS1.2
        if ch.client_version != ProtocolVersion::DTLS1_2 {
            return Err(
                Error::SecurityError(crate::SecurityError::UnsupportedClientVersion(
                    ch.client_version,
                ))
                .into(),
            );
        }

        // Enforce Null compression only (client must offer it)
        let has_null = ch.compression_methods.contains(&CompressionMethod::Null);
        if !has_null {
            return Err(
                Error::SecurityError(crate::SecurityError::UnsupportedClientCompression).into(),
            );
        }

        trace!(
            "ClientHello: cookie_len={}, offered_suites={}",
            ch.cookie.len(),
            ch.cipher_suites.len()
        );

        // Stateless cookie: require 32-byte cookie matching HMAC(secret, client_random)
        let client_random = ch.random;
        let hmac_provider = server.engine.config().crypto_provider().hmac_provider;
        let need_cookie = server.engine.config().use_server_cookie();
        let cookie_valid = !need_cookie
            || verify_cookie(
                hmac_provider,
                &server.cookie_secret,
                client_random,
                ch.cookie,
            );
        if !cookie_valid {
            debug!("Invalid/missing cookie; sending HelloVerifyRequest");

            let cookie = compute_cookie(hmac_provider, &server.cookie_secret, client_random)?;
            // Start/restart flight timer for server Flight 2 (HelloVerifyRequest)
            server.engine.flight_begin(2);
            server
                .engine
                .create_handshake(MessageType::HelloVerifyRequest, |body, _engine| {
                    // RFC 6347 4.2.1: The server_version field in the HelloVerifyRequest
                    // message MUST be set to DTLS 1.0
                    let hvr = HelloVerifyRequest::new(ProtocolVersion::DTLS1_0, cookie);
                    hvr.serialize(body);
                    Ok(())
                })?;

            // The HelloVerifyRequest exchange is stateless per RFC 6347.
            // Reset all handshake state so the next ClientHello (with cookie) is processed fresh.
            server.engine.reset_server_for_hello_verify_request();
            return Ok(self);
        }

        trace!("Accepted ClientHello cookie; proceeding with handshake");

        // Client offered suites; we pick per client order intersecting allowed and server key compatibility
        let mut selected: Option<Dtls12CipherSuite> = None;
        for s in ch.cipher_suites.iter() {
            let is_allowed = server.engine.is_cipher_suite_allowed(*s);
            let is_compatible = server
                .engine
                .crypto_context()
                .is_cipher_suite_compatible(*s);
            if is_allowed && is_compatible {
                selected = Some(*s);
                break;
            }
        }

        let Some(cs) = selected else {
            return Err((Error::SecurityError(
                crate::SecurityError::NoMutuallyAcceptableCipherSuite,
            ))
            .into());
        };

        server.engine.set_cipher_suite(cs);
        server.client_random = Some(client_random);

        debug!("Selected cipher suite: {:?}", cs);

        let client_offers_ems = ch
            .extensions
            .iter()
            .any(|ext| ext.extension_type == ExtensionType::ExtendedMasterSecret);
        let client_srtp_profiles = ch.srtp_profiles();
        let client_supported_groups = ch.supported_groups().cloned();
        let client_signature_algorithms = ch.signature_algorithms().cloned();

        // EMS is mandatory
        if !client_offers_ems {
            return Err(Error::SecurityError(
                crate::SecurityError::ExtendedMasterSecretNotNegotiated,
            )
            .into());
        }

        // Select SRTP profile according to server priority: AES256GCM, AES128GCM, then SHA1
        if let Some(profiles) = client_srtp_profiles {
            // Map client profile ids to SrtpProfile, then pick our preferred
            let mut selected_profile: Option<SrtpProfile> = None;
            for preferred in [
                SrtpProfile::AEAD_AES_256_GCM,
                SrtpProfile::AEAD_AES_128_GCM,
                SrtpProfile::AES128_CM_SHA1_80,
            ] {
                if profiles.iter().any(|pid| preferred == (*pid).into()) {
                    selected_profile = Some(preferred);
                    break;
                }
            }
            server.negotiated_srtp_profile = selected_profile;
            if let Some(profile) = server.negotiated_srtp_profile {
                debug!("Negotiated SRTP profile: {:?}", profile);
            }
        }

        // Store client's offers for later selection
        server.client_supported_groups = client_supported_groups;
        server.client_signature_algorithms = client_signature_algorithms;

        // Proceed to send the server flight
        trace!("Extended Master Secret enabled");
        Ok(Self::SendServerHello)
    }

    fn send_server_hello(self, server: &mut Server) -> Result<Self, InternalError> {
        trace!("Sending ServerHello");

        // Start/restart flight timer for server Flight 4
        server.engine.flight_begin(4);

        let session_id = server.session_id.unwrap_or_else(SessionId::empty);
        // unwrap: is ok because we set the random in handle_timeout
        let random = server.random.unwrap();
        let negotiated_srtp_profile = server.negotiated_srtp_profile;
        let extension_data = &mut server.extension_data;

        // Send ServerHello
        server
            .engine
            .create_handshake(MessageType::ServerHello, move |body, engine| {
                handshake_create_server_hello(
                    body,
                    engine,
                    random,
                    session_id,
                    negotiated_srtp_profile,
                    extension_data,
                )
            })?;

        let cs = server.engine.cipher_suite().ok_or(Error::InvalidState(
            crate::InvalidStateError::NoCipherSuiteSelected,
        ))?;

        // PSK suites skip Certificate
        if cs.is_psk() {
            Ok(Self::SendServerKeyExchange)
        } else {
            Ok(Self::SendCertificate)
        }
    }

    fn send_certificate(self, server: &mut Server) -> Result<Self, InternalError> {
        trace!("Sending Certificate");

        server
            .engine
            .create_handshake(MessageType::Certificate, handshake_create_certificate)?;

        Ok(Self::SendServerKeyExchange)
    }

    fn send_server_key_exchange(self, server: &mut Server) -> Result<Self, InternalError> {
        trace!("Sending ServerKeyExchange");

        let cs = server.engine.cipher_suite().ok_or(Error::InvalidState(
            crate::InvalidStateError::NoCipherSuiteSelected,
        ))?;

        if cs.is_psk() {
            return self.send_server_key_exchange_psk(server);
        }

        let client_random = server.client_random.ok_or(Error::InvalidState(
            crate::InvalidStateError::NoClientRandom,
        ))?;
        // unwrap: is ok because we set the random in handle_timeout
        let server_random = server.random.unwrap();

        // Select ECDHE group from the intersection of:
        // - client offers, and
        // - server-allowed DTLS 1.2 groups (provider + config filter)
        // Preference follows Config::kx_groups() order.
        let allowed_named_groups: Vec<NamedGroup> = server
            .engine
            .config()
            .kx_groups()
            .map(|g| g.name())
            .collect();
        let selected_named_group = select_named_group(
            server.client_supported_groups.as_ref(),
            &allowed_named_groups,
        )
        .ok_or_else(|| {
            if server.client_supported_groups.is_some() {
                Error::SecurityError(crate::SecurityError::NoCommonKeyExchangeGroup)
            } else {
                Error::CryptoError(crate::CryptoError::NoDtls12KeyExchangeGroupsConfigured)
            }
        })?;

        // Select signature/hash for SKE by intersecting client's list
        // with our key type, preferring the key's native hash algorithm.
        // unwrap: ServerKeyExchange signature only needed for certificate-based suites
        let selected_signature = select_ske_signature_algorithm(
            server.client_signature_algorithms.as_ref(),
            server
                .engine
                .crypto_context()
                .signature_algorithm()
                .unwrap(),
            server
                .engine
                .crypto_context()
                .private_key_default_hash_algorithm()
                .unwrap(),
            server
                .engine
                .crypto_context()
                .private_key_supported_hash_algorithms(),
        );

        debug!(
            "ServerKeyExchange params: group={:?}, signature_alg={:?}",
            selected_named_group, selected_signature
        );

        server
            .engine
            .create_handshake(MessageType::ServerKeyExchange, |body, engine| {
                handshake_create_server_key_exchange(
                    body,
                    engine,
                    client_random,
                    server_random,
                    selected_named_group,
                    selected_signature,
                )
            })?;

        if server.engine.config().require_client_certificate() {
            Ok(Self::SendCertificateRequest)
        } else {
            Ok(Self::SendServerHelloDone)
        }
    }

    /// PSK ServerKeyExchange: send identity hint only (no ECDHE, no signature).
    /// Per RFC 4279 §2, the message is omitted entirely when no hint is configured.
    fn send_server_key_exchange_psk(self, server: &mut Server) -> Result<Self, InternalError> {
        let Some(hint) = server
            .engine
            .config()
            .psk_identity_hint()
            .map(<[u8]>::to_vec)
        else {
            return Ok(Self::SendServerHelloDone);
        };

        server
            .engine
            .create_handshake(MessageType::ServerKeyExchange, move |body, _engine| {
                PskParams::serialize_from_bytes(&hint, body);
                Ok(())
            })?;

        // PSK never sends CertificateRequest
        Ok(Self::SendServerHelloDone)
    }

    fn send_certificate_request(self, server: &mut Server) -> Result<Self, InternalError> {
        debug!("Sending CertificateRequest");
        // Select CertificateRequest.signature_algorithms as intersection of client's list and our supported
        let sig_algs =
            select_certificate_request_sig_algs(server.client_signature_algorithms.as_ref());
        debug!(
            "CertificateRequest will advertise {} signature algorithms",
            sig_algs.len()
        );

        server
            .engine
            .create_handshake(MessageType::CertificateRequest, move |body, _| {
                handshake_serialize_certificate_request(body, &sig_algs)
            })?;

        Ok(Self::SendServerHelloDone)
    }

    fn send_server_hello_done(self, server: &mut Server) -> Result<Self, InternalError> {
        trace!("Sending ServerHelloDone");

        server
            .engine
            .create_handshake(MessageType::ServerHelloDone, |_, _| Ok(()))?;

        let cs = server.engine.cipher_suite().ok_or(Error::InvalidState(
            crate::InvalidStateError::NoCipherSuiteSelected,
        ))?;

        // PSK: no client certificates
        if cs.is_psk() {
            return Ok(Self::AwaitClientKeyExchange);
        }

        if server.engine.config().require_client_certificate() {
            Ok(Self::AwaitCertificate)
        } else {
            Ok(Self::AwaitClientKeyExchange)
        }
    }

    fn await_certificate(self, server: &mut Server) -> Result<Self, InternalError> {
        let maybe = server
            .engine
            .next_handshake(MessageType::Certificate, &mut server.defragment_buffer)?;

        let Some(ref handshake) = maybe else {
            // Stay in same state
            return Ok(self);
        };

        let Body::Certificate(certificate) = &handshake.body else {
            unreachable!()
        };

        // Extract certificate ranges before dropping handshake
        let cert_ranges: ArrayVec<_, 32> = certificate
            .certificate_list
            .iter()
            .map(|cert| cert.0.clone())
            .collect();

        drop(maybe);

        if cert_ranges.is_empty() {
            // The handshake has no key material yet, so send a fatal alert in epoch 0.
            server.engine.abort();
            server
                .engine
                .create_record(ContentType::Alert, 0, false, |body| {
                    body.push(2); // fatal
                    body.push(40); // handshake_failure
                })?;
            server.state = State::Closed;
            return Err(Error::CertificateError(
                crate::CertificateError::NoClientCertificateReceived,
            )
            .into());
        } else {
            // Store and verify via callback
            debug!(
                "Received client certificate chain with {} certificate(s)",
                cert_ranges.len()
            );
            for (i, range) in cert_ranges.iter().enumerate() {
                let cert_data = &server.defragment_buffer[range.clone()];
                trace!(
                    "Client Certificate #{} size: {} bytes",
                    i + 1,
                    cert_data.len()
                );
                server.client_certificates.push(cert_data.to_buf());
            }

            server.local_events.push_back(LocalEvent::PeerCert);
        }

        Ok(Self::AwaitClientKeyExchange)
    }

    fn await_client_key_exchange(self, server: &mut Server) -> Result<Self, InternalError> {
        let maybe = server.engine.next_handshake(
            MessageType::ClientKeyExchange,
            &mut server.defragment_buffer,
        )?;

        let Some(ref handshake) = maybe else {
            // Stay in same state
            return Ok(self);
        };

        let Body::ClientKeyExchange(ckx) = &handshake.body else {
            unreachable!()
        };

        let suite = server.engine.cipher_suite().ok_or(Error::InvalidState(
            crate::InvalidStateError::NoCipherSuiteSelected,
        ))?;

        if suite.is_psk() {
            // Extract PSK identity range before dropping handshake
            let identity_range = match &ckx.exchange_keys {
                ExchangeKeys::Psk(keys) => keys.identity_range.clone(),
                _ => {
                    return Err(Error::UnexpectedMessage(
                        crate::UnexpectedMessageError::EcdheClientKeyExchangeInPskPath,
                    )
                    .into());
                }
            };

            drop(maybe);

            let identity = &server.defragment_buffer[identity_range];
            trace!("PSK identity ({} bytes)", identity.len());

            // Resolve PSK via the configured resolver. On failure we derive a
            // random dummy of fixed length so the handshake proceeds identically
            // to a valid-identity flow (no timing oracle) and the Finished MAC
            // is guaranteed to mismatch — not merely likely to.
            let resolved = server
                .engine
                .config()
                .psk_resolver()
                .ok_or(Error::PskError(crate::PskError::NoPskResolverConfigured))?
                .resolve(identity);

            let (psk, psk_valid) = match resolved {
                Some(key) => (key, true),
                None => {
                    let dummy: [u8; DUMMY_PSK_LEN] = server.engine.rng.random();
                    (dummy.to_vec(), false)
                }
            };

            server.psk_valid = Some(psk_valid);

            let crypto = server.engine.crypto_context_mut();
            crypto.set_psk(psk);
            crypto
                .compute_psk_pre_master_secret()
                .map_err(Error::CryptoError)?;
        } else {
            // Extract client's public key range before dropping handshake
            let public_key_range = match &ckx.exchange_keys {
                ExchangeKeys::Ecdh(keys) => keys.public_key_range.clone(),
                ExchangeKeys::Psk(_) => {
                    return Err(Error::UnexpectedMessage(
                        crate::UnexpectedMessageError::PskClientKeyExchangeInEcdhePath,
                    )
                    .into());
                }
            };

            drop(maybe);

            // Get the actual public key data from defragment_buffer
            let client_pub = &server.defragment_buffer[public_key_range];

            // Compute shared secret
            let buf = server.engine.pop_buffer();
            server
                .engine
                .crypto_context_mut()
                .compute_shared_secret(client_pub, buf)
                .map_err(Error::CryptoError)?;
        }

        // Capture session hash for EMS now (up to ClientKeyExchange)
        let suite_hash = suite.hash_algorithm();
        let mut buf = server.engine.pop_buffer();
        server.engine.transcript_hash(suite_hash, &mut buf);
        server.captured_session_hash = Some(buf);

        // Derive master secret and keys (needed to decrypt client's Finished)
        let client_random_buf = {
            let mut b = Buf::new();
            server.client_random.unwrap().serialize(&mut b);
            b
        };
        let server_random_buf = {
            let mut b = Buf::new();
            // unwrap: is ok because we set the random in handle_timeout
            server.random.unwrap().serialize(&mut b);
            b
        };

        let session_hash = server
            .captured_session_hash
            .as_ref()
            .ok_or(Error::InvalidState(
                crate::InvalidStateError::ExtendedMasterSecretSessionHashMissing,
            ))?;

        let mut out = server.engine.pop_buffer();
        let mut scratch = server.engine.pop_buffer();
        server
            .engine
            .crypto_context_mut()
            .derive_extended_master_secret(session_hash, suite_hash, &mut out, &mut scratch)
            .map_err(Error::CryptoError)?;

        server
            .engine
            .crypto_context_mut()
            .derive_keys(
                suite,
                &client_random_buf,
                &server_random_buf,
                &mut out,
                &mut scratch,
            )
            .map_err(Error::CryptoError)?;

        server.engine.push_buffer(out);
        server.engine.push_buffer(scratch);

        trace!(
            "Captured session hash length for EMS: {}",
            session_hash.len()
        );
        trace!("Derived session keys (EMS) and ready to verify Finished");

        if !server.client_certificates.is_empty() {
            Ok(Self::AwaitCertificateVerify)
        } else {
            Ok(Self::AwaitChangeCipherSpec)
        }
    }

    fn await_certificate_verify(self, server: &mut Server) -> Result<Self, InternalError> {
        // Get handshake data BEFORE processing CertificateVerify message
        // According to TLS spec, signature is over all handshake messages
        // up to but not including CertificateVerify
        let data = server.engine.transcript().to_buf();

        let maybe = server.engine.next_handshake(
            MessageType::CertificateVerify,
            &mut server.defragment_buffer,
        )?;

        if maybe.is_none() {
            // Stay in same state
            return Ok(self);
        };

        // Extract signature data before accessing buffer
        let (signature_range, signature_algorithm) = {
            let handshake = maybe.as_ref().unwrap();
            let Body::CertificateVerify(cv) = &handshake.body else {
                unreachable!()
            };

            (cv.signed.signature_range.clone(), cv.signed.algorithm)
        };

        // Drop maybe to release buffer borrow
        drop(maybe);

        // Now access the buffer
        let signature_bytes = &server.defragment_buffer[signature_range];

        if server.client_certificates.is_empty() {
            return Err(Error::CertificateError(
                crate::CertificateError::NoClientCertificateForVerification,
            )
            .into());
        }

        // Create temp DigitallySigned for verification
        let temp_signed = crate::dtls12::message::DigitallySigned {
            algorithm: signature_algorithm,
            signature_range: 0..signature_bytes.len(),
        };

        server
            .engine
            .crypto_context()
            .verify_signature(
                &data,
                &temp_signed,
                signature_bytes,
                &server.client_certificates[0],
            )
            .map_err(Error::CryptoError)?;

        debug!("Client CertificateVerify verified successfully");

        Ok(Self::AwaitChangeCipherSpec)
    }

    fn await_change_cipher_spec(self, server: &mut Server) -> Result<Self, InternalError> {
        let maybe = server.engine.next_record(ContentType::ChangeCipherSpec);

        let Some(_) = maybe else {
            // Stay in same state
            return Ok(self);
        };

        // Drop any extra CCS resends to avoid being blocked
        trace!("Dropping any pending CCS resends from peer");
        server.engine.drop_pending_ccs();

        // Expect every record to be decrypted from now on.
        trace!("Received ChangeCipherSpec; enabling peer encryption");
        server.engine.enable_peer_encryption()?;

        Ok(Self::AwaitFinished)
    }

    fn await_finished(self, server: &mut Server) -> Result<Self, InternalError> {
        // Generate expected verify data based on current transcript.
        // This must be done before next_handshake() below since
        // it should not include Finished itself.
        let expected = server.engine.generate_verify_data(true /* client */)?;

        let maybe = server
            .engine
            .next_handshake(MessageType::Finished, &mut server.defragment_buffer)?;

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
        let verify_data = &server.defragment_buffer[verify_data_range];
        // Use constant-time comparison to prevent timing attacks
        let is_eq: bool = verify_data.ct_eq(expected.as_slice()).into();
        if !is_eq {
            return Err((Error::SecurityError(
                crate::SecurityError::ClientFinishedVerificationFailed,
            ))
            .into());
        }

        // Invariant: full PSK handshakes always set psk_valid in
        // await_client_key_exchange before reaching Finished. A violation is a
        // state-machine bug, not network input — `assert!` (not debug_assert!)
        // because a silent bypass could let a forged-PSK Finished through.
        // Loosen when dtls-conn-id lands and abbreviated handshakes legitimately
        // skip ClientKeyExchange (reusing a cached master secret).
        if server.engine.cipher_suite().is_some_and(|cs| cs.is_psk()) {
            assert!(
                server.psk_valid.is_some(),
                "PSK handshake reached Finished without processing ClientKeyExchange"
            );
        }

        // Defense-in-depth for PSK: the random dummy key should already make
        // the MAC above fail when the identity was unknown. We additionally
        // reject `Some(false)` explicitly so a future refactor that lets the
        // dummy path survive the MAC check still fails closed here.
        //
        // `None` is legitimate for abbreviated (resumption) handshakes, which
        // skip ClientKeyExchange and therefore never set psk_valid — those
        // paths reuse a cached master_secret and don't consult the resolver.
        if server.psk_valid == Some(false) {
            return Err((Error::SecurityError(
                crate::SecurityError::ClientFinishedVerificationFailed,
            ))
            .into());
        }

        trace!("Client Finished verified successfully");

        Ok(Self::SendChangeCipherSpec)
    }

    fn send_change_cipher_spec(self, server: &mut Server) -> Result<Self, InternalError> {
        trace!("Sending ChangeCipherSpec");

        // Start/restart flight timer for server Flight 6 (CCS+Finished)
        server.engine.flight_begin(6);

        // Send ChangeCipherSpec
        server
            .engine
            .create_record(ContentType::ChangeCipherSpec, 0, true, |body| {
                body.push(1);
            })?;

        Ok(Self::SendFinished)
    }

    fn send_finished(self, server: &mut Server) -> Result<Self, InternalError> {
        trace!("Sending Finished message to complete handshake");

        // No records from the handshake epochs may survive into application data.
        let unexpected_record = server
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

        server
            .engine
            .create_handshake(MessageType::Finished, |body, engine| {
                let verify_data = engine.generate_verify_data(false /* server */)?;
                trace!("Finished.verify_data length: {}", verify_data.len());
                // Directly write the verify data without creating Finished struct
                body.extend_from_slice(&verify_data);
                Ok(())
            })?;

        // Final flight sent; stop periodic retransmission timers per RFC 6347 FINISHED state.
        // If this flight need resending, it relies on the client to resend its last flight.
        server.engine.flight_stop_resend_timers();

        // Handshake complete
        debug!("Handshake complete; ready for application data");
        server.local_events.push_back(LocalEvent::Connected);

        // Emit SRTP keying material if negotiated
        if let Some(profile) = server.negotiated_srtp_profile {
            let suite_hash = server.engine.cipher_suite().unwrap().hash_algorithm();
            let mut out = server.engine.pop_buffer();
            let mut scratch = server.engine.pop_buffer();
            if let Ok(keying_material) = server
                .engine
                .crypto_context()
                .extract_srtp_keying_material(profile, suite_hash, &mut out, &mut scratch)
            {
                server.engine.push_buffer(out);
                server.engine.push_buffer(scratch);
                debug!(
                    "SRTP keying material extracted ({} bytes) for profile: {:?}",
                    keying_material.len(),
                    profile
                );
                // expect should be correct here since we negotiated the profile
                let profile = server
                    .negotiated_srtp_profile
                    .expect("SRTP profile should be negotiated");
                server
                    .local_events
                    .push_back(LocalEvent::KeyingMaterial(keying_material, profile));
            } else {
                server.engine.push_buffer(out);
                server.engine.push_buffer(scratch);
            }
        }

        server.engine.release_application_data();

        Ok(Self::AwaitApplicationData)
    }

    fn await_application_data(self, server: &mut Server) -> Result<Self, InternalError> {
        if server.engine.close_notify_received() {
            // RFC 5246 §7.2.1: respond with a reciprocal close_notify and
            // close down immediately, discarding any pending writes.
            server.engine.discard_pending_writes();
            server
                .engine
                .create_record(ContentType::Alert, 1, false, |body| {
                    body.push(1); // level: warning
                    body.push(0); // description: close_notify
                })?;
            return Ok(State::Closed);
        }

        // Now send any application data that was queued before we were connected.
        if !server.queued_data.is_empty() {
            debug!(
                "Sending queued application data: {}",
                server.queued_data.len()
            );
            for data in server.queued_data.drain(..) {
                server
                    .engine
                    .create_record(ContentType::ApplicationData, 1, false, |body| {
                        body.extend_from_slice(&data);
                    })?;
            }
        }

        Ok(self)
    }
}

fn compute_cookie(
    hmac_provider: &dyn crate::crypto::HmacProvider,
    secret: &[u8],
    client_random: Random,
) -> Result<Cookie, Error> {
    // cookie = trunc_32(HMAC(secret, client_random))
    let mut buf = Buf::new();
    client_random.serialize(&mut buf);
    let tag = hmac_provider
        .hmac_sha256(secret, &buf)
        .map_err(Error::CryptoError)?;
    let cookie = Cookie::try_new(&tag).map_err(|_| {
        Error::CryptoError(crate::CryptoError::OperationFailed(
            crate::CryptoOperation::ComputeCookie,
        ))
    })?;
    Ok(cookie)
}

fn verify_cookie(
    hmac_provider: &dyn crate::crypto::HmacProvider,
    secret: &[u8],
    client_random: Random,
    cookie: Cookie,
) -> bool {
    if cookie.len() != 32 {
        return false;
    }
    match compute_cookie(hmac_provider, secret, client_random) {
        // Use constant-time comparison to prevent timing attacks
        Ok(expected) => expected.as_ref().ct_eq(cookie.as_ref()).into(),
        Err(_) => false,
    }
}

fn handshake_create_certificate(body: &mut Buf, engine: &mut Engine) -> Result<(), Error> {
    let crypto = engine.crypto_context();
    crypto.serialize_client_certificate(body);
    Ok(())
}

fn handshake_create_server_hello(
    body: &mut Buf,
    engine: &mut Engine,
    random: Random,
    session_id: SessionId,
    negotiated_srtp_profile: Option<SrtpProfile>,
    extension_data: &mut Buf,
) -> Result<(), Error> {
    let server_version = ProtocolVersion::DTLS1_2;

    let cs = engine
        .cipher_suite()
        .ok_or(Error::InvalidState(crate::InvalidStateError::NoCipherSuite))?;

    let srtp_pid = negotiated_srtp_profile.map(|p| match p {
        SrtpProfile::AEAD_AES_256_GCM => SrtpProfileId::SRTP_AEAD_AES_256_GCM,
        SrtpProfile::AEAD_AES_128_GCM => SrtpProfileId::SRTP_AEAD_AES_128_GCM,
        SrtpProfile::AES128_CM_SHA1_80 => SrtpProfileId::SRTP_AES128_CM_SHA1_80,
    });

    let sh = ServerHello::new(
        server_version,
        random,
        session_id,
        cs,
        CompressionMethod::Null,
        None,
    )
    .with_extensions(extension_data, srtp_pid);

    sh.serialize(extension_data, body);
    Ok(())
}

fn handshake_create_server_key_exchange(
    body: &mut Buf,
    engine: &mut Engine,
    client_random: Random,
    server_random: Random,
    named_group: NamedGroup,
    algorithm: SignatureAndHashAlgorithm,
) -> Result<(), Error> {
    let Some(cipher_suite) = engine.cipher_suite() else {
        return Err(Error::InvalidState(
            crate::InvalidStateError::NoCipherSuiteSelected,
        ));
    };

    let key_exchange_algorithm = cipher_suite.as_key_exchange_algorithm();
    debug!("Using key exchange algorithm: {:?}", key_exchange_algorithm);

    // Use hash part from selected algorithm
    let hash_alg = algorithm.hash;

    match key_exchange_algorithm {
        KeyExchangeAlgorithm::EECDH => {
            let (curve_type, named_group) = (CurveType::NamedCurve, named_group);
            let kx_buf = engine.pop_buffer();
            let pubkey = engine
                .crypto_context_mut()
                .init_ecdh_server(named_group, kx_buf)
                .map_err(Error::CryptoError)?;

            trace!(
                "SKE ECDHE: group={:?}, pubkey_len={}",
                named_group,
                pubkey.len()
            );

            // Build signed_data = client_random || server_random || params(without signature)
            let mut signed_data = Buf::new();
            client_random.serialize(&mut signed_data);
            server_random.serialize(&mut signed_data);
            // Write params directly for signing
            signed_data.push(curve_type.as_u8());
            signed_data.extend_from_slice(&named_group.as_u16().to_be_bytes());
            signed_data.push(pubkey.len() as u8);
            signed_data.extend_from_slice(pubkey);

            let mut signature = engine.pop_buffer();

            trace!("SKE signature hash: {:?}", hash_alg);
            engine
                .crypto_context
                .sign_data(&signed_data, hash_alg, &mut signature)
                .map_err(Error::CryptoError)?;

            // unwrap: safe because init_ecdh_server() above sets key_exchange = Some(...).
            // If that failed, we returned Err earlier and never reach this point.
            let pubkey = engine
                .crypto_context_mut()
                .maybe_init_key_exchange()
                .unwrap();

            // For sending, we don't use DigitallySigned struct, just write the params and signature directly
            body.push(curve_type.as_u8());
            body.extend_from_slice(&named_group.as_u16().to_be_bytes());
            body.push(pubkey.len() as u8);
            body.extend_from_slice(pubkey);

            // Write signature
            body.extend_from_slice(&algorithm.as_u16().to_be_bytes());
            body.extend_from_slice(&(signature.len() as u16).to_be_bytes());
            body.extend_from_slice(&signature);

            engine.push_buffer(signature);

            Ok(())
        }
        _ => Err(Error::SecurityError(
            crate::SecurityError::UnsupportedKeyExchangeAlgorithm,
        )),
    }
}

fn handshake_serialize_certificate_request(
    body: &mut Buf,
    sig_algs: &SignatureAndHashAlgorithmVec,
) -> Result<(), Error> {
    // Only advertise ECDSA_SIGN (the only supported client cert type)
    let mut cert_types = CertificateTypeVec::new();
    cert_types.push(ClientCertificateType::ECDSA_SIGN);

    // If intersection is empty (e.g., client didn't advertise), fall back to our supported set
    // Build the selected list with the capacity expected by CertificateRequest
    let mut selected = SignatureAndHashAlgorithmVec::new();
    if sig_algs.is_empty() {
        let fallback = SignatureAndHashAlgorithm::supported();
        for alg in fallback.iter() {
            selected.push(*alg);
        }
    } else {
        for alg in sig_algs.iter() {
            selected.push(*alg);
        }
    }

    let cert_auths: ArrayVec<DistinguishedName, 32> = ArrayVec::new();

    let cr = CertificateRequest::new(cert_types, selected, cert_auths);
    cr.serialize(&[], body);
    Ok(())
}

fn select_named_group(
    client_groups: Option<&NamedGroupVec>,
    server_groups: &[NamedGroup],
) -> Option<NamedGroup> {
    if let Some(groups) = client_groups {
        // Server preference order from Config::kx_groups()
        for sg in server_groups {
            if groups.iter().any(|g| g == sg) {
                return Some(*sg);
            }
        }
        // Client advertised supported_groups, but there is no overlap.
        return None;
    }

    // Fallback only when client did not advertise supported_groups:
    // pick the first server-configured group.
    server_groups.first().copied()
}

fn select_ske_signature_algorithm(
    client_algs: Option<&SignatureAndHashAlgorithmVec>,
    our_sig: SignatureAlgorithm,
    our_hash: HashAlgorithm,
    supported_hashes: &[HashAlgorithm],
) -> SignatureAndHashAlgorithm {
    // Prefer the key's native hash first, then fall back to the other
    let hash_pref = match our_hash {
        HashAlgorithm::SHA384 => [HashAlgorithm::SHA384, HashAlgorithm::SHA256],
        _ => [HashAlgorithm::SHA256, HashAlgorithm::SHA384],
    };

    if let Some(list) = client_algs {
        for h in hash_pref.iter() {
            // Only consider hash algorithms the backend can actually sign with
            if !supported_hashes.contains(h) {
                continue;
            }
            if let Some(chosen) = list
                .iter()
                .find(|alg| alg.signature == our_sig && alg.hash == *h)
            {
                return *chosen;
            }
        }
    }

    // Fallback: use the key's native hash
    SignatureAndHashAlgorithm::new(our_hash, our_sig)
}

fn select_certificate_request_sig_algs(
    client_algs: Option<&SignatureAndHashAlgorithmVec>,
) -> SignatureAndHashAlgorithmVec {
    // Only select algorithms supported by dimpl's signature verifiers.
    let ours = SignatureAndHashAlgorithm::supported();

    // Build intersection preserving client preference order
    let mut out = ArrayVec::new();
    if let Some(list) = client_algs {
        for alg in list.iter() {
            if ours
                .iter()
                .any(|a| a.hash == alg.hash && a.signature == alg.signature)
            {
                out.push(*alg);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named_group_vec(groups: &[NamedGroup]) -> NamedGroupVec {
        let mut out = NamedGroupVec::new();
        for g in groups {
            out.push(*g);
        }
        out
    }

    #[test]
    fn select_named_group_prefers_x25519_when_available() {
        let client = named_group_vec(&[
            NamedGroup::Secp256r1,
            NamedGroup::X25519,
            NamedGroup::Secp384r1,
        ]);
        let provider = [NamedGroup::X25519, NamedGroup::Secp256r1];

        let selected = select_named_group(Some(&client), &provider);

        assert_eq!(selected, Some(NamedGroup::X25519));
    }

    #[test]
    fn select_named_group_respects_provider_capabilities() {
        let client = named_group_vec(&[NamedGroup::X25519, NamedGroup::Secp256r1]);
        let provider = [NamedGroup::Secp256r1];

        let selected = select_named_group(Some(&client), &provider);

        assert_eq!(selected, Some(NamedGroup::Secp256r1));
    }

    #[test]
    fn select_named_group_falls_back_to_provider_when_client_missing() {
        let provider = [NamedGroup::Secp384r1];

        let selected = select_named_group(None, &provider);

        assert_eq!(selected, Some(NamedGroup::Secp384r1));
    }

    #[test]
    fn select_named_group_rejects_when_client_has_no_overlap() {
        let client = named_group_vec(&[NamedGroup::X25519]);
        let provider = [NamedGroup::Secp256r1];

        let selected = select_named_group(Some(&client), &provider);

        assert_eq!(selected, None);
    }

    #[test]
    fn certificate_request_does_not_select_rsa_signatures() {
        let mut client = SignatureAndHashAlgorithmVec::new();
        client.push(SignatureAndHashAlgorithm::new(
            HashAlgorithm::SHA256,
            SignatureAlgorithm::ECDSA,
        ));
        client.push(SignatureAndHashAlgorithm::new(
            HashAlgorithm::SHA256,
            SignatureAlgorithm::RSA,
        ));

        let selected = select_certificate_request_sig_algs(Some(&client));

        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].signature, SignatureAlgorithm::ECDSA);
        assert_eq!(selected[0].hash, HashAlgorithm::SHA256);
    }

    #[test]
    fn certificate_request_rejects_rsa_only_signatures() {
        let mut client = SignatureAndHashAlgorithmVec::new();
        client.push(SignatureAndHashAlgorithm::new(
            HashAlgorithm::SHA256,
            SignatureAlgorithm::RSA,
        ));

        let selected = select_certificate_request_sig_algs(Some(&client));

        assert!(selected.is_empty());
    }
}
