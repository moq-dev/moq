use std::collections::VecDeque;
use std::io;
use std::ops::RangeInclusive;
use std::panic::{RefUnwindSafe, UnwindSafe};
use std::time::Instant;

use crate::crypto::Fingerprint;
use crate::crypto::Sha256Provider;
use crate::crypto::dtls::{DtlsCert, DtlsOutput, ProtocolVersion};
use crate::crypto::dtls::{DtlsInstance, DtlsProvider, DtlsVersion};
use crate::crypto::{CryptoError, DtlsError};
use crate::io::DatagramSend;
use crate::util::{already_happened, not_happening};

/// Encapsulation of DTLS.
///
/// This is a thin wrapper around `DtlsInstance` that adds fingerprint tracking
/// and active/passive state management. The API follows dimpl's sans-IO pattern.
pub struct Dtls {
    /// The underlying DTLS instance.
    instance: Box<dyn DtlsInstance>,

    /// The fingerprint of the local certificate.
    fingerprint: Fingerprint,

    /// Remote fingerprint (set when received via poll_output).
    remote_fingerprint: Option<Fingerprint>,

    /// Whether set_active has been called.
    active_state: Option<bool>,

    /// Packets to be sent.
    pending_packets: VecDeque<DatagramSend>,

    /// Target MTU (start) and warn threshold (end). The target is forwarded to
    /// the backend's fragmenter; outgoing records larger than the warn
    /// threshold log a warning.
    mtu: RangeInclusive<usize>,
}

pub(crate) fn is_would_block(error: &DtlsError) -> bool {
    match error {
        DtlsError::Io(e) => e.kind() == io::ErrorKind::WouldBlock,
        DtlsError::CryptoError(crypto_err) => match crypto_err {
            CryptoError::Io(e) => e.kind() == io::ErrorKind::WouldBlock,
            #[allow(unreachable_patterns)]
            _ => false,
        },
    }
}

impl UnwindSafe for Dtls {}
impl RefUnwindSafe for Dtls {}

impl Dtls {
    /// Creates a new DTLS instance.
    pub fn new(
        cert: &DtlsCert,
        dtls_provider: &dyn DtlsProvider,
        sha256_provider: &dyn Sha256Provider,
        now: Instant,
        dtls_version: DtlsVersion,
        mtu: RangeInclusive<usize>,
    ) -> Result<Self, DtlsError> {
        let instance = dtls_provider
            .new_dtls(cert, now, dtls_version, Some(*mtu.start()))
            .map_err(DtlsError::CryptoError)?;

        // Compute fingerprint from the certificate DER bytes
        let fingerprint = Fingerprint {
            hash_func: "sha-256".to_string(),
            bytes: sha256_provider.sha256(&cert.certificate).to_vec(),
        };

        Ok(Self {
            instance,
            fingerprint,
            remote_fingerprint: None,
            active_state: None,
            pending_packets: VecDeque::new(),
            mtu,
        })
    }

    /// Threshold above which an outgoing DTLS record triggers an MTU warning.
    pub fn mtu_warn(&self) -> usize {
        *self.mtu.end()
    }

    /// Tells if this instance has been inited (set_active called).
    pub fn is_inited(&self) -> bool {
        self.active_state.is_some()
    }

    /// Set whether this instance is active (client) or passive (server).
    pub fn set_active(&mut self, active: bool) {
        self.active_state = Some(active);
        self.instance.set_active(active)
    }

    /// If set_active was called, returns what was set.
    pub fn is_active(&self) -> Option<bool> {
        self.active_state
    }

    /// The local certificate fingerprint.
    pub fn local_fingerprint(&self) -> &Fingerprint {
        &self.fingerprint
    }

    /// Remote fingerprint, if received.
    pub fn remote_fingerprint(&self) -> Option<&Fingerprint> {
        self.remote_fingerprint.as_ref()
    }

    /// The negotiated DTLS protocol version, or `None` before handshake completion.
    pub fn protocol_version(&self) -> Option<ProtocolVersion> {
        self.instance.protocol_version()
    }

    pub fn is_closed(&self) -> bool {
        self.pending_packets.is_empty() && self.instance.is_closed()
    }

    /// Set the remote fingerprint (extracted from peer certificate).
    pub fn set_remote_fingerprint(&mut self, fingerprint: Fingerprint) {
        self.remote_fingerprint = Some(fingerprint);
    }

    /// Poll for output from the DTLS instance.
    pub fn poll_output<'a>(&mut self, buf: &'a mut [u8]) -> DtlsOutput<'a> {
        if !self.is_inited() {
            return DtlsOutput::Timeout(not_happening());
        }

        let next = self.instance.poll_output(buf);

        if let DtlsOutput::Packet(packet) = next {
            if packet.len() > self.mtu_warn() {
                warn!("DTLS above MTU {}: {}", self.mtu_warn(), packet.len());
            }
            self.pending_packets.push_back(packet.to_vec().into());

            // Return timeout indicating we want another poll straight away
            return DtlsOutput::Timeout(already_happened());
        }

        next
    }

    pub fn poll_packet(&mut self) -> Option<DatagramSend> {
        self.pending_packets.pop_front()
    }

    /// Handle an incoming DTLS packet.
    pub fn handle_receive(&mut self, packet: &[u8]) -> Result<(), DtlsError> {
        if !self.is_inited() {
            debug!("Ignoring DTLS datagram prior to DTLS start");
            return Ok(());
        }

        self.instance
            .handle_packet(packet)
            .map_err(|e| DtlsError::CryptoError(CryptoError::Other(format!("DTLS error: {}", e))))
    }

    /// Send application data over DTLS.
    pub fn handle_input(&mut self, data: &[u8]) -> Result<(), DtlsError> {
        fn make_handshake_pending_err() -> DtlsError {
            DtlsError::Io(io::Error::new(
                io::ErrorKind::WouldBlock,
                "DTLS handshake pending",
            ))
        }

        // Don't rely on the inner instance for proper pre-init behavior.
        if !self.is_inited() {
            return Err(make_handshake_pending_err());
        }

        let accepted = self.instance.send_application_data(data).map_err(|e| {
            DtlsError::CryptoError(CryptoError::Other(format!("DTLS error: {}", e)))
        })?;

        if accepted != data.len() {
            return Err(make_handshake_pending_err());
        }

        Ok(())
    }

    /// Handle a timeout event.
    pub fn handle_timeout(&mut self, now: Instant) -> Result<(), DtlsError> {
        if !self.is_inited() {
            return Ok(());
        }

        self.instance
            .handle_timeout(now)
            .map_err(|e| DtlsError::CryptoError(CryptoError::Other(format!("DTLS error: {}", e))))
    }

    /// Initiate graceful shutdown by sending a close_notify alert.
    pub fn close(&mut self) -> Result<(), DtlsError> {
        self.instance
            .close()
            .map_err(|e| DtlsError::CryptoError(CryptoError::Other(format!("DTLS error: {}", e))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_dtls(now: Instant, version: DtlsVersion) -> Dtls {
        let provider = crate::crypto::test_default_provider();
        let cert = provider.dtls_provider.generate_certificate().unwrap();
        Dtls::new(
            &cert,
            provider.dtls_provider,
            provider.sha256_provider,
            now,
            version,
            1200..=1500,
        )
        .unwrap()
    }

    #[test]
    fn input_before_initialization_would_block() {
        let mut dtls = new_dtls(Instant::now(), DtlsVersion::Dtls12);
        let err = dtls.handle_input(b"application data").unwrap_err();
        assert!(is_would_block(&err));
    }

    #[test]
    #[cfg(any(
        feature = "aws-lc-rs",
        feature = "rust-crypto",
        feature = "openssl-dimpl",
        feature = "wincrypto",
        feature = "apple-crypto",
    ))]
    fn auto_client_retries_unaccepted_application_data() {
        fn exchange(
            from: &mut Dtls,
            to: &mut Dtls,
            now: Instant,
            received: &mut Vec<Vec<u8>>,
        ) -> bool {
            from.handle_timeout(now).unwrap();
            let mut connected = false;
            let mut buf = [0; 2048];
            loop {
                let timeout = match from.poll_output(&mut buf) {
                    DtlsOutput::Connected => {
                        connected = true;
                        false
                    }
                    DtlsOutput::ApplicationData(data) => {
                        received.push(data.to_vec());
                        false
                    }
                    DtlsOutput::Timeout(_) => true,
                    _ => false,
                };
                if let Some(packet) = from.poll_packet() {
                    to.handle_receive(&packet).unwrap();
                } else if timeout {
                    break;
                }
            }
            connected
        }

        let now = Instant::now();
        let mut client = new_dtls(now, DtlsVersion::Auto);
        let mut server = new_dtls(now, DtlsVersion::Dtls12);
        client.set_active(true);
        server.set_active(false);
        client.handle_timeout(now).unwrap();
        server.handle_timeout(now).unwrap();

        let data = b"retry after auto-sense";
        let err = client.handle_input(data).unwrap_err();
        assert!(is_would_block(&err));
        // An empty write succeeds even though its accepted byte count is zero.
        client.handle_input(b"").unwrap();

        let mut received = Vec::new();
        let mut client_connected = false;
        let mut server_connected = false;
        for _ in 0..20 {
            client_connected |= exchange(&mut client, &mut server, now, &mut received);
            server_connected |= exchange(&mut server, &mut client, now, &mut received);
            if client_connected && server_connected {
                break;
            }
        }
        assert!(client_connected && server_connected);
        assert!(received.is_empty());
        assert_eq!(client.protocol_version(), Some(ProtocolVersion::DTLS1_2));
        assert_eq!(server.protocol_version(), Some(ProtocolVersion::DTLS1_2));

        client.handle_input(data).unwrap();
        exchange(&mut client, &mut server, now, &mut received);
        exchange(&mut server, &mut client, now, &mut received);
        assert_eq!(received, vec![data.to_vec()]);

        client.close().unwrap();
        let err = client.handle_input(data).unwrap_err();
        assert!(!is_would_block(&err));
    }
}
