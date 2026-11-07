// DTLS 1.3 Server Handshake Flow (RFC 9147):
//
// 1. Client sends ClientHello (plaintext, epoch 0)
//    - Server validates: legacy_version==DTLS1.2, null compression offered
//    - Server checks supported_versions for DTLS1.3
//    - Server selects cipher suite, key exchange group, SRTP profile
//    - If no cookie: send HelloRetryRequest with HMAC-based cookie
//    - If cookie present: validate HMAC, proceed
//    - If no matching key_share but supported_groups match: HRR for key exchange
// 2. Server sends ServerHello (plaintext, epoch 0)
//    - Derives handshake secrets, installs handshake keys
// 3. Server sends EncryptedExtensions (encrypted, epoch 2)
// 4. Server sends CertificateRequest (optional, encrypted, epoch 2)
// 5. Server sends Certificate (encrypted, epoch 2)
// 6. Server sends CertificateVerify (encrypted, epoch 2)
// 7. Server sends Finished (encrypted, epoch 2)
//    - Derives application secrets, installs application keys
//    - Enables peer encryption for client's epoch 2
// 8. Client sends Certificate (if requested, encrypted, epoch 2)
// 9. Client sends CertificateVerify (if cert present, encrypted, epoch 2)
// 10. Client sends Finished (encrypted, epoch 2)
//     - Server verifies, emits Connected, extracts SRTP keying material
// 11. Application data flows on epoch 3
//
// This implementation is a Sans-IO DTLS 1.3 server.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use arrayvec::ArrayVec;
use subtle::ConstantTimeEq;

use crate::buffer::Buf;
use crate::buffer::ToBuf;
use crate::crypto::{ActiveKeyExchange, SrtpProfile};
use crate::dtls13::Client;
use crate::dtls13::client::LocalEvent;
use crate::dtls13::client::handshake_create_certificate;
use crate::dtls13::client::handshake_create_certificate_verify;
use crate::dtls13::client::signature_scheme_to_components;
#[cfg(feature = "_crypto-common")]
use crate::dtls13::client::verify_scheme_curve;
use crate::dtls13::engine::Engine;
use crate::dtls13::message::Body;
use crate::dtls13::message::CompressionMethod;
use crate::dtls13::message::ContentType;
use crate::dtls13::message::Dtls13CipherSuite;
use crate::dtls13::message::Extension;
use crate::dtls13::message::ExtensionType;
use crate::dtls13::message::KeyShareEntry;
use crate::dtls13::message::KeyShareHelloRetryRequest;
use crate::dtls13::message::KeyShareServerHello;
use crate::dtls13::message::KeyUpdateRequest;
use crate::dtls13::message::MessageType;
use crate::dtls13::message::NamedGroup;
use crate::dtls13::message::ProtocolVersion;
use crate::dtls13::message::Random;
use crate::dtls13::message::ServerHello;
use crate::dtls13::message::SessionId;
use crate::dtls13::message::SignatureAlgorithmsExtension;
use crate::dtls13::message::SignatureScheme;
use crate::dtls13::message::SupportedVersionsServerHello;
use crate::dtls13::message::UseSrtpExtension;
use crate::dtls13::message::parse_cookie_extension;
use crate::timer::Timeout;
use crate::{Config, DtlsCertificate, Error, InternalError, Output};

/// Magic random value indicating HelloRetryRequest (RFC 8446 Section 4.1.3).
const HRR_RANDOM: [u8; 32] = [
    0xCF, 0x21, 0xAD, 0x74, 0xE5, 0x9A, 0x61, 0x11, 0xBE, 0x1D, 0x8C, 0x02, 0x1E, 0x65, 0xB8, 0x91,
    0xC2, 0xA2, 0x11, 0x16, 0x7A, 0xBB, 0x8C, 0x5E, 0x07, 0x9E, 0x09, 0xE2, 0xC8, 0xA8, 0x33, 0x9C,
];

const MAX_RETAINED_CLIENT_HELLO: usize = 64;

/// DTLS 1.3 server
pub struct Server {
    /// Current server state.
    state: State,

    /// Engine in common between server and client.
    engine: Engine,

    /// Random unique data. Used for ServerHello.
    random: Option<Random>,

    /// Client's session ID echoed from ClientHello.
    client_session_id: Option<SessionId>,

    /// Storage for extension data.
    extension_data: Buf,

    /// The negotiated SRTP profile (if any).
    negotiated_srtp_profile: Option<SrtpProfile>,

    /// Client certificates.
    client_certificates: Vec<Buf>,

    /// Buffer for defragmenting handshakes.
    defragment_buffer: Buf,

    /// The last now we seen.
    last_now: Instant,

    /// Local events.
    local_events: VecDeque<LocalEvent>,

    /// Data that is sent before we are connected.
    queued_data: Vec<Buf>,

    /// Active key exchange state (ECDHE).
    active_key_exchange: Option<Box<dyn ActiveKeyExchange>>,

    /// Saved shared secret for deriving application secrets later.
    shared_secret: Option<Buf>,

    /// Saved handshake secret for deriving application secrets.
    handshake_secret: Option<Buf>,

    /// Client handshake traffic secret (for client Finished verification).
    client_hs_traffic_secret: Option<Buf>,

    /// Server handshake traffic secret (for server Finished).
    server_hs_traffic_secret: Option<Buf>,

    /// Whether we requested client authentication.
    client_auth_requested: bool,

    /// Whether we sent a HelloRetryRequest.
    hello_retry: bool,

    /// Cookie secret for HMAC-based DoS protection.
    cookie_secret: [u8; 32],

    /// Whether we need to respond with our own KeyUpdate.
    pending_key_update_response: bool,

    /// When true, a ClientHello without DTLS 1.3 in `supported_versions`
    /// returns [`Error::Dtls12Fallback`] instead of a security error.
    /// Used by the auto-sense server path.
    auto_mode: bool,

    /// Raw packets buffered during auto-sense so they can be replayed
    /// to a DTLS 1.2 server on fallback.
    retained_hello: VecDeque<Buf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    AwaitClientHello,
    SendServerHello,
    SendEncryptedExtensions,
    SendCertificateRequest,
    SendCertificate,
    SendCertificateVerify,
    SendFinished,
    AwaitCertificate,
    AwaitCertificateVerify,
    AwaitFinished,
    AwaitApplicationData,
    HalfClosedLocal,
    Closed,
}

impl Server {
    /// Create a new DTLS 1.3 server.
    pub fn new(config: Arc<Config>, certificate: DtlsCertificate, now: Instant) -> Server {
        let engine = Engine::new(config, certificate);
        Self::new_with_engine(engine, now, false)
    }

    /// Create a new DTLS 1.3 server in auto-sense mode.
    ///
    /// In auto-sense mode, if the ClientHello does not offer DTLS 1.3
    /// in `supported_versions`, the server returns [`Error::Dtls12Fallback`]
    /// instead of a fatal security error, allowing the caller to switch
    /// to a DTLS 1.2 server.
    pub fn new_auto(config: Arc<Config>, certificate: DtlsCertificate, now: Instant) -> Server {
        let engine = Engine::new(config, certificate);
        Self::new_with_engine(engine, now, true)
    }

    pub fn new_with_engine(mut engine: Engine, now: Instant, auto_mode: bool) -> Server {
        let cookie_secret = engine.random_arr();
        let extension_data = engine.pop_buffer();
        let defragment_buffer = engine.pop_buffer();

        Server {
            state: State::AwaitClientHello,
            engine,
            random: None,
            client_session_id: None,
            extension_data,
            negotiated_srtp_profile: None,
            client_certificates: Vec::with_capacity(3),
            defragment_buffer,
            last_now: now,
            local_events: VecDeque::new(),
            queued_data: Vec::new(),
            active_key_exchange: None,
            shared_secret: None,
            handshake_secret: None,
            client_hs_traffic_secret: None,
            server_hs_traffic_secret: None,
            client_auth_requested: false,
            hello_retry: false,
            cookie_secret,
            pending_key_update_response: false,
            auto_mode,
            retained_hello: VecDeque::with_capacity(10),
        }
    }

    pub fn into_client(self) -> Client {
        Client::new_with_engine(self.engine, self.last_now)
    }

    /// Whether this server is in auto-sense mode.
    pub fn is_auto_mode(&self) -> bool {
        self.auto_mode
    }

    /// Take all relevant config from this server instance.
    ///
    /// This is used in two cases:
    ///
    /// 1. Switching a server pending (auto-mode) to dtls12 server
    /// 2. set_active(true), turning a server pending (auto-mode) to a ClientPending
    pub fn into_parts(
        self,
    ) -> (
        Arc<Config>,
        DtlsCertificate,
        Instant,
        VecDeque<Buf>,
        Timeout,
    ) {
        let (config, cert, deadline) = self.engine.into_fallback();
        (config, cert, self.last_now, self.retained_hello, deadline)
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
        if self.state == State::Closed {
            return Ok(());
        }

        // In auto-sense mode, buffer raw packets while still waiting for
        // the ClientHello so they can be replayed to Server12 on fallback.
        if self.auto_mode && self.state == State::AwaitClientHello {
            // Cap buffered fragments to prevent unbounded growth from malicious traffic
            if self.retained_hello.len() >= MAX_RETAINED_CLIENT_HELLO {
                return Err(Error::TooManyClientHelloFragments);
            }
            self.retained_hello.push_back(packet.to_buf());
        }

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
            Ok(()) => {}
            Err(e) => {
                if let Some(err) = e.into_public_error() {
                    return Err(err);
                }
                return Ok(());
            }
        }

        // Once past AwaitClientHello, DTLS 1.3 is committed — free the buffer.
        if self.auto_mode && self.state != State::AwaitClientHello {
            self.retained_hello.clear();
            self.auto_mode = false;
        }

        Ok(())
    }

    pub fn poll_output<'a>(&mut self, buf: &'a mut [u8]) -> Output<'a> {
        if let Some(event) = self.local_events.pop_front() {
            return event.into_output(buf, &self.client_certificates);
        }
        self.engine.poll_output(buf, self.last_now)
    }

    /// Handle a timeout event.
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

    /// Send application data when the server is connected.
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

impl State {
    fn name(&self) -> &'static str {
        match self {
            State::AwaitClientHello => "AwaitClientHello",
            State::SendServerHello => "SendServerHello",
            State::SendEncryptedExtensions => "SendEncryptedExtensions",
            State::SendCertificateRequest => "SendCertificateRequest",
            State::SendCertificate => "SendCertificate",
            State::SendCertificateVerify => "SendCertificateVerify",
            State::SendFinished => "SendFinished",
            State::AwaitCertificate => "AwaitCertificate",
            State::AwaitCertificateVerify => "AwaitCertificateVerify",
            State::AwaitFinished => "AwaitFinished",
            State::AwaitApplicationData => "AwaitApplicationData",
            State::HalfClosedLocal => "HalfClosedLocal",
            State::Closed => "Closed",
        }
    }

    fn make_progress(self, server: &mut Server) -> Result<Self, InternalError> {
        match self {
            State::AwaitClientHello => self.await_client_hello(server),
            State::SendServerHello => self.send_server_hello(server),
            State::SendEncryptedExtensions => self.send_encrypted_extensions(server),
            State::SendCertificateRequest => self.send_certificate_request(server),
            State::SendCertificate => self.send_certificate(server),
            State::SendCertificateVerify => self.send_certificate_verify(server),
            State::SendFinished => self.send_finished(server),
            State::AwaitCertificate => self.await_certificate(server),
            State::AwaitCertificateVerify => self.await_certificate_verify(server),
            State::AwaitFinished => self.await_finished(server),
            State::AwaitApplicationData => self.await_application_data(server),
            State::HalfClosedLocal => self.half_closed_local(server),
            State::Closed => Ok(self),
        }
    }

    fn await_client_hello(self, server: &mut Server) -> Result<Self, InternalError> {
        // Save transcript length so we can roll back if a stale ClientHello
        // arrives after HelloRetryRequest (no cookie → must discard).
        let transcript_len_before = server.engine.transcript.len();

        let maybe = if server.auto_mode {
            server
                .engine
                .next_client_hello_for_auto_sense(&mut server.defragment_buffer)
                .inspect_err(|_| {
                    // Body rejection consumes the fragments; assembly errors
                    // leave the same candidate queued and still needing replay.
                    if !server
                        .engine
                        .has_complete_handshake(MessageType::ClientHello)
                    {
                        server.retained_hello.clear();
                    }
                })?
        } else {
            server
                .engine
                .next_handshake(MessageType::ClientHello, &mut server.defragment_buffer)?
        };

        let Some(handshake) = maybe else {
            return Ok(self);
        };

        let Body::ClientHello(ref client_hello) = handshake.body else {
            unreachable!()
        };

        if !client_hello.legacy_cookie.is_empty() {
            return Err(Error::SecurityError(
                crate::SecurityError::InvalidLegacyCookieInClientHello,
            )
            .into());
        }

        // Validate legacy_version
        if client_hello.legacy_version != ProtocolVersion::DTLS1_2 {
            return Err(Error::SecurityError(
                crate::SecurityError::ClientHelloLegacyVersionNotDtls12,
            )
            .into());
        }

        // Validate null compression is offered
        let has_null_compression = client_hello
            .legacy_compression_methods
            .contains(&CompressionMethod::Null);
        if !has_null_compression {
            return Err(Error::SecurityError(
                crate::SecurityError::ClientHelloMustOfferNullCompression,
            )
            .into());
        }

        let supported_versions_ok = client_hello
            .supported_versions()
            .is_some_and(|sv| sv.versions.contains(&ProtocolVersion::DTLS1_3));
        let key_shares = client_hello
            .key_share()
            .map(|ks| ks.entries.as_slice())
            .unwrap_or(&[]);
        let client_groups = client_hello
            .supported_groups()
            .map(|groups| groups.as_slice())
            .unwrap_or(&[]);
        let client_srtp_profiles = client_hello.srtp_profiles();
        let mut client_cookie_data: Option<ArrayVec<u8, 32>> = None;
        for ext in &client_hello.extensions {
            if ext.extension_type == ExtensionType::Cookie {
                // This parser only checks a u16 length and returns a borrowed slice.
                let (_, cookie) =
                    parse_cookie_extension(ext.extension_data(&server.defragment_buffer))
                        .map_err(InternalError::from)?;
                client_cookie_data = Some(ArrayVec::try_from(cookie).map_err(|_| {
                    Error::SecurityError(crate::SecurityError::InvalidCookieInClientHello)
                })?);
            }
        }

        if !supported_versions_ok {
            if server.auto_mode {
                return Err((Error::Dtls12Fallback).into());
            }
            return Err(Error::SecurityError(
                crate::SecurityError::ClientHelloMissingDtls13SupportedVersions,
            )
            .into());
        }

        // Select cipher suite: first from client's list that is in our provider
        let selected_cipher_suite = client_hello
            .cipher_suites
            .iter()
            .find(|cs| server.engine.is_cipher_suite_allowed(**cs))
            .copied()
            .ok_or(Error::SecurityError(
                crate::SecurityError::NoCommonCipherSuite,
            ))?;

        // Save the client random and session_id early so HRR can use them
        let client_random = client_hello.random;
        server.client_session_id = Some(client_hello.legacy_session_id);

        // Cookie-based DoS protection
        let need_cookie = server.engine.config().use_server_cookie();

        // Pre-compute whether we also need a key_share group selection, so
        // we can piggyback it on a cookie HRR (avoiding two sequential HRRs).
        let our_groups: ArrayVec<NamedGroup, 4> = server
            .engine
            .config()
            .kx_groups()
            .map(|g| g.name())
            .collect();
        let has_matching_key_share = key_shares
            .iter()
            .any(|entry| our_groups.contains(&entry.group));

        // Determine which group to request if key_share is missing
        let hrr_group = if !has_matching_key_share {
            our_groups
                .iter()
                .find(|g| client_groups.contains(g))
                .copied()
        } else {
            None
        };

        if need_cookie {
            let expected_cookie = compute_cookie(
                &server.cookie_secret,
                &client_random,
                server.engine.config().crypto_provider().hmac_provider,
            );

            if let Some(ref cookie_bytes) = client_cookie_data {
                // Validate the cookie
                let is_valid: bool = cookie_bytes.as_slice().ct_eq(&expected_cookie).into();
                if !is_valid {
                    return Err(Error::SecurityError(
                        crate::SecurityError::InvalidCookieInClientHello,
                    )
                    .into());
                }
                debug!("Cookie validated successfully");
            } else {
                // No cookie: send HelloRetryRequest with cookie
                if server.hello_retry {
                    // Stale retransmission of the pre-HRR ClientHello. Roll back the
                    // transcript that next_handshake just appended and silently
                    // discard. The HRR flight will be retransmitted by the normal
                    // flight timeout mechanism if the client hasn't responded.
                    debug!("Discarding stale ClientHello retransmission after HRR");
                    server.engine.transcript.resize(transcript_len_before, 0);
                    return Ok(Self::AwaitClientHello);
                }

                debug!("No cookie in ClientHello; sending HelloRetryRequest");

                // Build HRR as a special ServerHello with magic random
                server.engine.set_cipher_suite(selected_cipher_suite);
                // Replace transcript BEFORE sending HRR so only CH1 is hashed
                // per RFC 8446 Section 4.4.1.
                let transcript_len = server.engine.transcript.len();
                server
                    .engine
                    .replace_transcript_with_message_hash(transcript_len);
                send_hello_retry_request(
                    server,
                    selected_cipher_suite,
                    &expected_cookie,
                    hrr_group, // also request key_share group if needed
                )?;

                server.engine.advance_peer_handshake_seq();
                server.engine.reset_for_hello_retry();
                server.hello_retry = true;

                return Ok(Self::AwaitClientHello);
            }
        } else if let Some(ref cookie_bytes) = client_cookie_data {
            // Validate if a cookie was provided even when not required
            let expected_cookie = compute_cookie(
                &server.cookie_secret,
                &client_random,
                server.engine.config().crypto_provider().hmac_provider,
            );
            let is_valid: bool = cookie_bytes.as_slice().ct_eq(&expected_cookie).into();
            if !is_valid {
                return Err((Error::SecurityError(
                    crate::SecurityError::InvalidCookieInClientHello,
                ))
                .into());
            }
        }

        // Select key exchange: find first client key_share with a group we support
        let matching_entry = key_shares
            .iter()
            .find(|entry| our_groups.contains(&entry.group));

        let (selected_group, peer_key_range) = if let Some(entry) = matching_entry {
            (entry.group, entry.key_exchange_range.clone())
        } else {
            // No matching key_share, but check supported_groups for a common group
            let common_group = our_groups
                .iter()
                .find(|g| client_groups.contains(g))
                .copied();

            return if let Some(group) = common_group {
                // Need HRR for key exchange
                if server.hello_retry {
                    return Err(Error::SecurityError(
                        crate::SecurityError::CannotSendSecondHelloRetryRequest,
                    )
                    .into());
                }

                debug!(
                    "No matching key_share; sending HelloRetryRequest for group {:?}",
                    group
                );

                server.engine.set_cipher_suite(selected_cipher_suite);
                // Replace transcript BEFORE sending HRR so only CH1 is hashed
                // per RFC 8446 Section 4.4.1.
                let transcript_len = server.engine.transcript.len();
                server
                    .engine
                    .replace_transcript_with_message_hash(transcript_len);
                let cookie_for_hrr = compute_cookie(
                    &server.cookie_secret,
                    &client_random,
                    server.engine.config().crypto_provider().hmac_provider,
                );
                send_hello_retry_request(
                    server,
                    selected_cipher_suite,
                    &cookie_for_hrr,
                    Some(group),
                )?;

                server.engine.advance_peer_handshake_seq();
                server.engine.reset_for_hello_retry();
                server.hello_retry = true;

                Ok(Self::AwaitClientHello)
            } else {
                Err(Error::SecurityError(crate::SecurityError::NoCommonKeyExchangeGroup).into())
            };
        };

        // Start ECDHE key exchange
        let kx_group = server
            .engine
            .find_kx_group(selected_group)
            .ok_or(Error::CryptoError(
                crate::CryptoError::KeyExchangeGroupNotFound(selected_group),
            ))?;

        let kx_buf = server.engine.pop_buffer();
        let key_exchange = kx_group
            .start_exchange(kx_buf)
            .map_err(Error::CryptoError)?;

        // Store server's public key in extension_data
        server.extension_data.clear();
        let pub_key = key_exchange.pub_key();
        let pub_key_start = server.extension_data.len();
        server.extension_data.extend_from_slice(pub_key);
        let pub_key_end = server.extension_data.len();

        // Complete ECDHE with client's public key
        let peer_pub_key = &server.defragment_buffer[peer_key_range];
        let mut shared_secret = server.engine.pop_buffer();
        key_exchange
            .complete(peer_pub_key, &mut shared_secret)
            .map_err(Error::CryptoError)?;

        server.shared_secret = Some(shared_secret);

        // Select SRTP profile: first from client list that the server supports.
        // Per RFC 5764 Section 4.1.1, the server MUST select a profile that
        // both sides support.
        if let Some(profiles) = client_srtp_profiles {
            for profile_id in profiles {
                let profile: SrtpProfile = (*profile_id).into();
                if SrtpProfile::ALL.contains(&profile) {
                    server.negotiated_srtp_profile = Some(profile);
                    break;
                }
            }
        }

        // Store selected group and public key range for ServerHello
        // already completed
        server.active_key_exchange = None;

        // Save selected cipher suite and key data for SendServerHello
        server.engine.set_cipher_suite(selected_cipher_suite);

        // Store the selected group and pub key range in a way SendServerHello can access.
        // We encode them into extension_data: first the group as 2 bytes, then the start/end
        // Actually, we already wrote the pub key above. We'll store selected_group separately.
        // Re-use extension_data: [pub_key_bytes...] at indices pub_key_start..pub_key_end.
        // We'll pass this info through the state transition.
        // For simplicity, store group and range in the Buf after pub_key:
        server
            .extension_data
            .extend_from_slice(&selected_group.as_u16().to_be_bytes());
        server
            .extension_data
            .extend_from_slice(&(pub_key_start as u32).to_be_bytes());
        server
            .extension_data
            .extend_from_slice(&(pub_key_end as u32).to_be_bytes());

        drop(handshake);

        server.engine.advance_peer_handshake_seq();

        debug!(
            "ClientHello processed: cipher_suite={:?}, group={:?}",
            selected_cipher_suite, selected_group
        );

        Ok(Self::SendServerHello)
    }

    fn send_server_hello(self, server: &mut Server) -> Result<Self, InternalError> {
        // unwrap: is ok because we set the random in handle_timeout
        let random = server.random.unwrap();

        // Extract stored data from extension_data
        let ext_data_len = server.extension_data.len();
        // Last 10 bytes: group(2) + start(4) + end(4)
        let meta_start = ext_data_len - 10;
        let group_bytes = [
            server.extension_data[meta_start],
            server.extension_data[meta_start + 1],
        ];
        let selected_group = NamedGroup::from_u16(u16::from_be_bytes(group_bytes));
        let pub_key_start = u32::from_be_bytes([
            server.extension_data[meta_start + 2],
            server.extension_data[meta_start + 3],
            server.extension_data[meta_start + 4],
            server.extension_data[meta_start + 5],
        ]) as usize;
        let pub_key_end = u32::from_be_bytes([
            server.extension_data[meta_start + 6],
            server.extension_data[meta_start + 7],
            server.extension_data[meta_start + 8],
            server.extension_data[meta_start + 9],
        ]) as usize;

        // Truncate the metadata we appended
        server.extension_data.resize(meta_start, 0);

        let client_session_id = SessionId::empty();

        server.engine.flight_begin(2);

        server
            .engine
            .create_handshake(MessageType::ServerHello, |body, engine| {
                handshake_create_server_hello(
                    body,
                    engine,
                    random,
                    client_session_id,
                    selected_group,
                    pub_key_start..pub_key_end,
                    &server.extension_data,
                )
            })?;

        // Derive handshake secrets
        let shared_secret = server.shared_secret.take().ok_or(Error::InvalidState(
            crate::InvalidStateError::NoSharedSecretForHandshakeKeyDerivation,
        ))?;

        let (c_hs_traffic, s_hs_traffic, handshake_secret) =
            server.engine.derive_handshake_secrets(&shared_secret)?;

        // Save handshake secret for later application key derivation
        server.handshake_secret = Some(handshake_secret);
        server.engine.push_buffer(shared_secret);

        // Install handshake keys
        server
            .engine
            .install_handshake_keys(&c_hs_traffic, &s_hs_traffic)?;

        // Retain the derived buffers for Finished verification.
        server.server_hs_traffic_secret = Some(s_hs_traffic);
        server.client_hs_traffic_secret = Some(c_hs_traffic);

        Ok(Self::SendEncryptedExtensions)
    }

    fn send_encrypted_extensions(self, server: &mut Server) -> Result<Self, InternalError> {
        debug!("Sending EncryptedExtensions");

        let negotiated_srtp = server.negotiated_srtp_profile;

        server
            .engine
            .create_handshake(MessageType::EncryptedExtensions, |body, _engine| {
                handshake_create_encrypted_extensions(body, negotiated_srtp)
            })?;

        if server.engine.config().require_client_certificate() {
            Ok(Self::SendCertificateRequest)
        } else {
            Ok(Self::SendCertificate)
        }
    }

    fn send_certificate_request(self, server: &mut Server) -> Result<Self, InternalError> {
        debug!("Sending CertificateRequest");

        server
            .engine
            .create_handshake(MessageType::CertificateRequest, |body, _engine| {
                handshake_create_certificate_request(body)
            })?;

        server.client_auth_requested = true;

        Ok(Self::SendCertificate)
    }

    fn send_certificate(self, server: &mut Server) -> Result<Self, InternalError> {
        debug!("Sending Certificate");

        server
            .engine
            .create_handshake(MessageType::Certificate, |body, engine| {
                handshake_create_certificate(body, engine, &[])
            })?;

        Ok(Self::SendCertificateVerify)
    }

    fn send_certificate_verify(self, server: &mut Server) -> Result<Self, InternalError> {
        debug!("Sending CertificateVerify");

        server
            .engine
            .create_handshake(MessageType::CertificateVerify, |body, engine| {
                handshake_create_certificate_verify(
                    body,
                    engine,
                    b"TLS 1.3, server CertificateVerify\0",
                )
            })?;

        Ok(Self::SendFinished)
    }

    fn send_finished(self, server: &mut Server) -> Result<Self, InternalError> {
        trace!("Sending server Finished message");

        let server_hs_secret =
            server
                .server_hs_traffic_secret
                .as_ref()
                .ok_or(Error::InvalidState(
                    crate::InvalidStateError::NoServerHandshakeTrafficSecretForFinished,
                ))?;
        let mut server_hs_secret_copy = Buf::new();
        server_hs_secret_copy.extend_from_slice(server_hs_secret);
        let server_hs_secret = server_hs_secret_copy;

        server
            .engine
            .create_handshake(MessageType::Finished, |body, engine| {
                let verify_data = engine.compute_verify_data(&server_hs_secret)?;
                body.extend_from_slice(&verify_data);
                Ok(())
            })?;

        // Derive application secrets from handshake secret + transcript through server Finished
        let handshake_secret = server.handshake_secret.as_ref().ok_or(Error::InvalidState(
            crate::InvalidStateError::NoHandshakeSecretForApplicationKeyDerivation,
        ))?;

        let (c_ap_traffic, s_ap_traffic) =
            server.engine.derive_application_secrets(handshake_secret)?;

        // Install application keys
        server
            .engine
            .install_application_keys(&c_ap_traffic, &s_ap_traffic)?;

        // Enable peer encryption so we can decrypt client's epoch 2 messages
        server.engine.enable_peer_encryption()?;

        if server.client_auth_requested {
            Ok(Self::AwaitCertificate)
        } else {
            Ok(Self::AwaitFinished)
        }
    }

    fn await_certificate(self, server: &mut Server) -> Result<Self, InternalError> {
        let maybe = server
            .engine
            .next_handshake(MessageType::Certificate, &mut server.defragment_buffer)?;

        let Some(ref handshake) = maybe else {
            return Ok(self);
        };

        let Body::Certificate(ref certificate) = handshake.body else {
            unreachable!()
        };

        if certificate.certificate_list.is_empty() {
            server.engine.abort();
            // Server Finished was already sent, so the client now reads epoch 3.
            let epoch = server.engine.app_send_epoch();
            server
                .engine
                .create_ciphertext_record(ContentType::Alert, epoch, false, |body| {
                    body.push(2); // legacy level (ignored in DTLS 1.3)
                    body.push(116); // certificate_required
                })?;
            server.state = State::Closed;
            return Err(Error::CertificateError(
                crate::CertificateError::NoClientCertificateReceived,
            )
            .into());
        }

        debug!(
            "Received client Certificate message with {} certificate(s)",
            certificate.certificate_list.len()
        );

        // Extract certificate data before dropping handshake
        let cert_ranges: ArrayVec<_, 32> = certificate
            .certificate_list
            .iter()
            .map(|entry| entry.cert.as_slice(&server.defragment_buffer).to_vec())
            .collect();

        drop(maybe);

        for (i, cert_data) in cert_ranges.iter().enumerate() {
            trace!(
                "Client certificate #{} size: {} bytes",
                i + 1,
                cert_data.len()
            );
            let mut buf = server.engine.pop_buffer();
            buf.extend_from_slice(cert_data);
            server.client_certificates.push(buf);
        }

        // Emit PeerCert event
        if !server.client_certificates.is_empty() {
            server.local_events.push_back(LocalEvent::PeerCert);
        }

        // Client's Certificate means our flight was received; stop retransmitting
        server.engine.flight_stop_resend_timers();

        server.engine.advance_peer_handshake_seq();
        Ok(Self::AwaitCertificateVerify)
    }

    fn await_certificate_verify(self, server: &mut Server) -> Result<Self, InternalError> {
        // Compute transcript hash BEFORE consuming CertificateVerify.
        // Per RFC 8446 §4.4.3, the hash covers all messages up to but NOT
        // including CertificateVerify. next_handshake() will append CV to
        // the transcript, so we must capture the hash first.
        let mut transcript_hash = Buf::new();
        server.engine.transcript_hash(&mut transcript_hash);

        let maybe = server.engine.next_handshake(
            MessageType::CertificateVerify,
            &mut server.defragment_buffer,
        )?;

        let Some(ref handshake) = maybe else {
            return Ok(self);
        };

        let Body::CertificateVerify(ref cv) = handshake.body else {
            unreachable!()
        };

        let scheme = cv.signed.scheme;
        let signature = cv.signed.signature(&server.defragment_buffer);

        // RFC 8446 §4.4.3: The signature algorithm MUST be one offered in
        // the signature_algorithms field of the CertificateRequest message.
        if !SignatureScheme::supported().contains(&scheme) {
            return Err(
                Error::SecurityError(crate::SecurityError::SignatureSchemeNotOffered(scheme))
                    .into(),
            );
        }

        // Build the signed content per RFC 8446 Section 4.4.3:
        // 0x20 * 64 || "TLS 1.3, client CertificateVerify\0" || transcript_hash
        let mut signed_content = Buf::new();
        signed_content.extend_from_slice(&[0x20u8; 64]);
        signed_content.extend_from_slice(b"TLS 1.3, client CertificateVerify\0");
        signed_content.extend_from_slice(&transcript_hash);

        // Copy signature data since we need to drop handshake reference
        let mut signature_copy = ArrayVec::<u8, 512>::new();
        signature_copy
            .try_extend_from_slice(signature)
            .map_err(|_| Error::SecurityError(crate::SecurityError::SignatureTooLarge))?;

        drop(maybe);

        // Verify the signature
        let cert_der = server
            .client_certificates
            .first()
            .ok_or(Error::CertificateError(
                crate::CertificateError::NoClientCertificateForVerification,
            ))?;

        #[cfg(feature = "_crypto-common")]
        verify_scheme_curve(scheme, cert_der)?;

        let (hash_alg, sig_alg) = signature_scheme_to_components(scheme)?;

        server.engine.verify_signature(
            cert_der,
            &signed_content,
            &signature_copy,
            hash_alg,
            sig_alg,
        )?;

        trace!("Client CertificateVerify verified: {:?}", scheme);

        server.engine.advance_peer_handshake_seq();
        Ok(Self::AwaitFinished)
    }

    fn await_finished(self, server: &mut Server) -> Result<Self, InternalError> {
        // Compute expected verify_data BEFORE consuming Finished
        // (verify_data uses transcript hash up to but not including Finished)
        let client_hs_secret =
            server
                .client_hs_traffic_secret
                .as_ref()
                .ok_or(Error::InvalidState(
                    crate::InvalidStateError::NoClientHandshakeTrafficSecret,
                ))?;
        let expected_verify_data = server.engine.compute_verify_data(client_hs_secret)?;

        let maybe = server
            .engine
            .next_handshake(MessageType::Finished, &mut server.defragment_buffer)?;

        let Some(ref handshake) = maybe else {
            return Ok(self);
        };

        let Body::Finished(ref finished) = handshake.body else {
            unreachable!()
        };

        let verify_data = finished.verify_data(&server.defragment_buffer);

        trace!(
            "Client Finished.verify_data received len={}, expected len={}",
            verify_data.len(),
            expected_verify_data.len()
        );

        // Constant-time comparison
        let is_eq: bool = verify_data.ct_eq(&*expected_verify_data).into();
        if !is_eq {
            return Err((Error::SecurityError(
                crate::SecurityError::ClientFinishedVerificationFailed,
            ))
            .into());
        }

        trace!("Client Finished verified successfully");

        drop(maybe);

        server.engine.advance_peer_handshake_seq();

        // No records from the handshake epochs may survive into application data.
        let unexpected_record = server
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

        // ACK the client's epoch-2 flight so it stops retransmitting
        server.engine.send_ack_retransmittable()?;

        // Stop flight timers - handshake complete
        server.engine.flight_stop_resend_timers();

        // Emit Connected event
        server.local_events.push_back(LocalEvent::Connected);

        // Extract and emit SRTP keying material if negotiated
        if let Some(profile) = server.negotiated_srtp_profile
            && let Ok((keying_material, profile)) =
                server.engine.extract_srtp_keying_material(profile)
        {
            debug!(
                "SRTP keying material extracted ({} bytes) for profile: {:?}",
                keying_material.len(),
                profile
            );
            server
                .local_events
                .push_back(LocalEvent::KeyingMaterial(keying_material, profile));
        }

        server
            .engine
            .release_application_data_retaining_handshake_keys();

        debug!("Handshake complete; ready for application data");

        Ok(Self::AwaitApplicationData)
    }

    fn await_application_data(self, server: &mut Server) -> Result<Self, InternalError> {
        // Auto-trigger KeyUpdate when AEAD encryption limit is reached
        if server.engine.needs_key_update() && !server.engine.is_key_update_in_flight() {
            server.initiate_key_update()?;
        }

        // Send queued application data
        if !server.queued_data.is_empty() {
            let epoch = server.engine.app_send_epoch();
            debug!(
                "Sending queued application data: {}",
                server.queued_data.len()
            );
            for data in server.queued_data.drain(..) {
                server.engine.create_ciphertext_record(
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
        if server.pending_key_update_response {
            server
                .engine
                .create_key_update(KeyUpdateRequest::UpdateNotRequested)?;
            server.pending_key_update_response = false;
        }

        // Check for incoming KeyUpdate
        if server.engine.has_complete_handshake(MessageType::KeyUpdate) {
            let maybe = server.engine.next_handshake_no_transcript(
                MessageType::KeyUpdate,
                &mut server.defragment_buffer,
            )?;

            if let Some(handshake) = maybe {
                let Body::KeyUpdate(request) = handshake.body else {
                    unreachable!()
                };

                // Install new recv keys
                server.engine.update_recv_keys()?;

                // ACK the KeyUpdate record
                server.engine.send_ack()?;

                // If peer requested us to update, schedule our own KeyUpdate
                if request == KeyUpdateRequest::UpdateRequested
                    && !server.engine.is_key_update_in_flight()
                {
                    server.pending_key_update_response = true;
                }

                server.engine.advance_peer_handshake_seq();
                debug!("Received KeyUpdate (request={:?})", request);
            }
        }

        Ok(self)
    }

    fn half_closed_local(self, server: &mut Server) -> Result<Self, InternalError> {
        // Write half is closed: drain incoming KeyUpdate to keep recv keys in sync,
        // but do not send our own KeyUpdate response.
        if server.engine.has_complete_handshake(MessageType::KeyUpdate) {
            let maybe = server.engine.next_handshake_no_transcript(
                MessageType::KeyUpdate,
                &mut server.defragment_buffer,
            )?;
            if let Some(handshake) = maybe {
                let Body::KeyUpdate(_) = handshake.body else {
                    unreachable!()
                };
                server.engine.update_recv_keys()?;
                server.engine.advance_peer_handshake_seq();
            }
        }

        if server.engine.close_notify_received() {
            return Ok(State::Closed);
        }

        Ok(self)
    }
}

// =========================================================================
// Helper Functions
// =========================================================================

/// Compute a cookie for DoS protection using HMAC-SHA256.
///
/// The cookie is HMAC-SHA256(cookie_secret, client_random.bytes), truncated to 32 bytes.
fn compute_cookie(
    cookie_secret: &[u8; 32],
    client_random: &Random,
    hmac_provider: &'static dyn crate::crypto::HmacProvider,
) -> [u8; 32] {
    // unwrap: HMAC-SHA256 should not fail with valid key/data
    hmac_provider
        .hmac_sha256(cookie_secret, &client_random.bytes)
        .unwrap()
}

/// Send a HelloRetryRequest (encoded as a ServerHello with magic random).
fn send_hello_retry_request(
    server: &mut Server,
    cipher_suite: Dtls13CipherSuite,
    cookie: &[u8; 32],
    selected_group: Option<NamedGroup>,
) -> Result<(), Error> {
    let hrr_random = Random { bytes: HRR_RANDOM };
    let client_session_id = SessionId::empty();

    server.engine.flight_begin(2);

    // Build extension data into a local buffer
    let mut ext_buf = Buf::new();
    let mut extensions: ArrayVec<Extension, 5> = ArrayVec::new();

    // 1. supported_versions extension (DTLS 1.3)
    let sv_start = ext_buf.len();
    let sv = SupportedVersionsServerHello {
        selected_version: ProtocolVersion::DTLS1_3,
    };
    sv.serialize(&mut ext_buf);
    let sv_end = ext_buf.len();
    extensions.push(Extension {
        extension_type: ExtensionType::SupportedVersions,
        extension_data_range: sv_start..sv_end,
    });

    // 2. key_share extension (selected group for retry) if needed
    if let Some(group) = selected_group {
        let ks_start = ext_buf.len();
        let hrr_ks = KeyShareHelloRetryRequest {
            selected_group: group,
        };
        hrr_ks.serialize(&mut ext_buf);
        let ks_end = ext_buf.len();
        extensions.push(Extension {
            extension_type: ExtensionType::KeyShare,
            extension_data_range: ks_start..ks_end,
        });
    }

    // 3. cookie extension
    let cookie_start = ext_buf.len();
    // Cookie extension format: cookie<1..2^16-1> (u16-length-prefixed)
    ext_buf.extend_from_slice(&(cookie.len() as u16).to_be_bytes());
    ext_buf.extend_from_slice(cookie);
    let cookie_end = ext_buf.len();
    extensions.push(Extension {
        extension_type: ExtensionType::Cookie,
        extension_data_range: cookie_start..cookie_end,
    });

    let server_hello = ServerHello::new(
        ProtocolVersion::DTLS1_2,
        hrr_random,
        client_session_id,
        cipher_suite,
        CompressionMethod::Null,
        Some(extensions),
    );

    server
        .engine
        .create_handshake(MessageType::ServerHello, |body, _engine| {
            server_hello.serialize(&ext_buf, body);
            Ok(())
        })?;

    Ok(())
}

fn handshake_create_server_hello(
    body: &mut Buf,
    engine: &mut Engine,
    random: Random,
    client_session_id: SessionId,
    selected_group: NamedGroup,
    pub_key_range: std::ops::Range<usize>,
    extension_data: &Buf,
) -> Result<(), Error> {
    // unwrap: cipher_suite is set by AwaitClientHello
    let cipher_suite = engine.cipher_suite().unwrap();

    // Build extensions
    let mut ext_buf = Buf::new();
    let mut extensions: ArrayVec<Extension, 5> = ArrayVec::new();

    // 1. supported_versions extension (DTLS 1.3)
    let sv_start = ext_buf.len();
    let sv = SupportedVersionsServerHello {
        selected_version: ProtocolVersion::DTLS1_3,
    };
    sv.serialize(&mut ext_buf);
    let sv_end = ext_buf.len();
    extensions.push(Extension {
        extension_type: ExtensionType::SupportedVersions,
        extension_data_range: sv_start..sv_end,
    });

    // 2. key_share extension (server's public key)
    let ks_start = ext_buf.len();
    let ks = KeyShareServerHello {
        entry: KeyShareEntry {
            group: selected_group,
            key_exchange_range: pub_key_range,
        },
    };
    ks.serialize(extension_data, &mut ext_buf);
    let ks_end = ext_buf.len();
    extensions.push(Extension {
        extension_type: ExtensionType::KeyShare,
        extension_data_range: ks_start..ks_end,
    });

    let server_hello = ServerHello::new(
        ProtocolVersion::DTLS1_2,
        random,
        client_session_id,
        cipher_suite,
        CompressionMethod::Null,
        Some(extensions),
    );

    server_hello.serialize(&ext_buf, body);
    Ok(())
}

fn handshake_create_encrypted_extensions(
    body: &mut Buf,
    negotiated_srtp: Option<SrtpProfile>,
) -> Result<(), Error> {
    let mut ext_buf = Buf::new();
    let mut extensions: ArrayVec<Extension, 5> = ArrayVec::new();

    // use_srtp extension if negotiated
    if let Some(profile) = negotiated_srtp {
        let srtp_start = ext_buf.len();
        let profile_id: crate::dtls13::message::SrtpProfileId = profile.into();
        let mut profiles = ArrayVec::new();
        profiles.push(profile_id);
        let use_srtp = UseSrtpExtension::new(profiles, ArrayVec::new());
        use_srtp.serialize(&mut ext_buf);
        let srtp_end = ext_buf.len();
        extensions.push(Extension {
            extension_type: ExtensionType::UseSrtp,
            extension_data_range: srtp_start..srtp_end,
        });
    }

    // Serialize EncryptedExtensions: extensions_len(2) + extensions
    let mut extensions_len = 0usize;
    for ext in &extensions {
        let ext_data = ext.extension_data(&ext_buf);
        extensions_len += 4 + ext_data.len();
    }

    body.extend_from_slice(&(extensions_len as u16).to_be_bytes());

    for ext in &extensions {
        ext.serialize(&ext_buf, body);
    }

    Ok(())
}

fn handshake_create_certificate_request(body: &mut Buf) -> Result<(), Error> {
    // CertificateRequest format (RFC 8446 Section 4.3.2):
    // certificate_request_context<0..255>
    body.push(0); // empty context

    // extensions<2..2^16-1>
    let mut ext_buf = Buf::new();
    let mut extensions: ArrayVec<Extension, 5> = ArrayVec::new();

    // signature_algorithms extension (required)
    let sa_start = ext_buf.len();
    let sa = SignatureAlgorithmsExtension::default();
    sa.serialize(&mut ext_buf);
    let sa_end = ext_buf.len();
    extensions.push(Extension {
        extension_type: ExtensionType::SignatureAlgorithms,
        extension_data_range: sa_start..sa_end,
    });

    // Calculate total extensions length
    let mut extensions_len = 0usize;
    for ext in &extensions {
        let ext_data = ext.extension_data(&ext_buf);
        extensions_len += 4 + ext_data.len();
    }

    body.extend_from_slice(&(extensions_len as u16).to_be_bytes());

    for ext in &extensions {
        ext.serialize(&ext_buf, body);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "rcgen")]
    fn auto_retains_hello_when_assembly_is_incomplete() {
        // The coverage check sees both bytes, but defragmentation stops at the
        // different message type, leaving both fragments queued for later.
        let fragments = [
            (MessageType::ClientHello, 0, 1),
            (MessageType::ServerHello, 1, 1),
        ];
        let packet = client_hello_fragments(2, &fragments);
        assert_auto_retains_unconsumed_hello(packet, false);
    }

    #[test]
    #[cfg(feature = "rcgen")]
    fn auto_retains_hello_when_fragment_capacity_is_exceeded() {
        let fragments: Vec<_> = (0..51)
            .map(|offset| (MessageType::ClientHello, offset, 1))
            .collect();
        let packet = client_hello_fragments(51, &fragments);
        assert_auto_retains_unconsumed_hello(packet, true);
    }

    #[cfg(feature = "rcgen")]
    fn client_hello_fragments(length: u32, fragments: &[(MessageType, u32, u32)]) -> Vec<u8> {
        let mut packet = Vec::new();
        // Each record holds at most eight parsed handshake fragments.
        for (seq, record_fragments) in fragments.chunks(8).enumerate() {
            let mut record = Vec::new();
            for &(kind, offset, fragment_len) in record_fragments {
                record.push(kind.as_u8());
                record.extend_from_slice(&length.to_be_bytes()[1..]);
                record.extend_from_slice(&0u16.to_be_bytes());
                record.extend_from_slice(&offset.to_be_bytes()[1..]);
                record.extend_from_slice(&fragment_len.to_be_bytes()[1..]);
                record.resize(record.len() + fragment_len as usize, 0);
            }
            packet.push(ContentType::Handshake.as_u8());
            packet.extend_from_slice(&[0xfe, 0xfd]);
            packet.extend_from_slice(&0u16.to_be_bytes());
            packet.extend_from_slice(&(seq as u64).to_be_bytes()[2..]);
            packet.extend_from_slice(&(record.len() as u16).to_be_bytes());
            packet.extend_from_slice(&record);
        }
        packet
    }

    #[cfg(feature = "rcgen")]
    fn assert_auto_retains_unconsumed_hello(packet: Vec<u8>, capacity_error: bool) {
        use crate::certificate::generate_self_signed_certificate;
        use crate::error::TransientError;

        let now = Instant::now();
        let certificate = generate_self_signed_certificate().expect("certificate");
        let mut server = Server::new_auto(Arc::new(Config::default()), certificate, now);
        server.handle_timeout(now).expect("server clock");
        let mut output = [0; 2048];
        while !matches!(server.poll_output(&mut output), Output::Timeout(_)) {}

        server
            .handle_packet(&packet)
            .expect("discard transient error");
        while !matches!(server.poll_output(&mut output), Output::Timeout(_)) {}
        assert!(server.engine.transcript.is_empty());

        // Reattempting assembly proves that the candidate was not consumed.
        let error = server
            .engine
            .next_client_hello_for_auto_sense(&mut server.defragment_buffer)
            .expect_err("unconsumed fragments still fail assembly");
        if capacity_error {
            assert!(matches!(
                error,
                InternalError::Transient(TransientError::TooManyRecords)
            ));
        } else {
            assert!(matches!(
                error,
                InternalError::Transient(TransientError::ParseIncomplete)
            ));
        }

        let (_, _, _, retained, _) = server.into_parts();
        assert_eq!(
            retained.len(),
            1,
            "preserve raw packets for DTLS 1.2 fallback"
        );
        assert_eq!(&retained[0][..], packet.as_slice());
    }

    #[test]
    fn certificate_request_omits_empty_certificate_authorities() {
        let mut body = Buf::new();
        handshake_create_certificate_request(&mut body).expect("create CertificateRequest");

        assert_eq!(body[0], 0, "certificate_request_context");
        let ext_len = u16::from_be_bytes([body[1], body[2]]) as usize;
        assert_eq!(body.len(), 3 + ext_len);

        let mut extension_types = Vec::new();
        let mut pos = 3;
        while pos < body.len() {
            assert!(pos + 4 <= body.len());
            let extension_type =
                ExtensionType::from_u16(u16::from_be_bytes([body[pos], body[pos + 1]]));
            let extension_data_len = u16::from_be_bytes([body[pos + 2], body[pos + 3]]) as usize;
            pos += 4;
            assert!(pos + extension_data_len <= body.len());

            extension_types.push(extension_type);
            pos += extension_data_len;
        }

        assert!(extension_types.contains(&ExtensionType::SignatureAlgorithms));
        assert!(!extension_types.contains(&ExtensionType::CertificateAuthorities));
    }
}
