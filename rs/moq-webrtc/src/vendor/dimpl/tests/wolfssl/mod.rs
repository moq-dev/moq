#![allow(unused)]

//! WolfSSL DTLS 1.3 implementation for interop testing.

use std::cell::RefCell;
use std::collections::VecDeque;

use bytes::BytesMut;
use wolfssl::ContextBuilder;
use wolfssl::IOCallbackResult;
use wolfssl::IOCallbacks;
use wolfssl::Method;
use wolfssl::Poll;
use wolfssl::RootCertificate;
use wolfssl::Secret;
use wolfssl::Session;
use wolfssl::SessionConfig;
use wolfssl::SslVerifyMode;

/// Errors that can arise in DTLS.
#[derive(Debug)]
pub enum WolfError {
    /// Some error from WolfSSL layer.
    Wolf(wolfssl::Error),
    /// Context builder error.
    ContextBuilder(wolfssl::NewContextBuilderError),
    /// Session error.
    Session(wolfssl::NewSessionError),
}

impl From<wolfssl::Error> for WolfError {
    fn from(value: wolfssl::Error) -> Self {
        WolfError::Wolf(value)
    }
}

impl From<wolfssl::NewContextBuilderError> for WolfError {
    fn from(value: wolfssl::NewContextBuilderError) -> Self {
        WolfError::ContextBuilder(value)
    }
}

impl From<wolfssl::NewSessionError> for WolfError {
    fn from(value: wolfssl::NewSessionError) -> Self {
        WolfError::Session(value)
    }
}

/// Events arising from a [`WolfDtlsImpl`] instance.
#[derive(Debug)]
pub enum DtlsEvent {
    /// When the DTLS has finished handshaking.
    Connected,

    /// Application data received.
    Data(Vec<u8>),
}

/// IO buffer that allows pushing/popping datagrams for non-blocking I/O.
#[derive(Default)]
pub struct IoBuffer {
    /// Incoming data (received from network).
    incoming: RefCell<VecDeque<Vec<u8>>>,
    /// Outgoing data (to send to network).
    outgoing: RefCell<VecDeque<Vec<u8>>>,
}

impl IoBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push incoming data (simulating receiving from network).
    pub fn set_incoming(&self, data: &[u8]) {
        self.incoming.borrow_mut().push_back(data.to_vec());
    }

    /// Pop outgoing data (to send to network).
    pub fn pop_outgoing(&self) -> Option<Vec<u8>> {
        self.outgoing.borrow_mut().pop_front()
    }
}

impl IOCallbacks for IoBuffer {
    fn recv(&mut self, buf: &mut [u8]) -> IOCallbackResult<usize> {
        let mut incoming = self.incoming.borrow_mut();
        if let Some(data) = incoming.pop_front() {
            let n = std::cmp::min(buf.len(), data.len());
            buf[..n].copy_from_slice(&data[..n]);
            IOCallbackResult::Ok(n)
        } else {
            IOCallbackResult::WouldBlock
        }
    }

    fn send(&mut self, buf: &[u8]) -> IOCallbackResult<usize> {
        self.outgoing.borrow_mut().push_back(buf.to_vec());
        IOCallbackResult::Ok(buf.len())
    }
}

/// DTLS 1.3 certificate and key for WolfSSL.
pub struct WolfDtlsCert {
    /// DER-encoded certificate.
    pub cert_der: Vec<u8>,
    /// DER-encoded private key.
    pub key_der: Vec<u8>,
}

impl WolfDtlsCert {
    /// Create a new WolfSSL DTLS certificate from DER-encoded cert and key.
    pub fn new(cert_der: Vec<u8>, key_der: Vec<u8>) -> Self {
        Self { cert_der, key_der }
    }

    /// Create a DTLS 1.3 implementation from this certificate.
    pub fn new_dtls13_impl(&self, is_server: bool) -> Result<WolfDtlsImpl, WolfError> {
        WolfDtlsImpl::new(self, is_server)
    }
}

/// WolfSSL DTLS 1.3 implementation wrapper.
pub struct WolfDtlsImpl {
    session: Session<IoBuffer>,
    is_connected: bool,
}

impl WolfDtlsImpl {
    /// Create a new WolfSSL DTLS 1.3 session.
    pub fn new(cert: &WolfDtlsCert, is_server: bool) -> Result<Self, WolfError> {
        let method = if is_server {
            Method::DtlsServerV1_3
        } else {
            Method::DtlsClientV1_3
        };

        let ctx = ContextBuilder::new(method)?
            .with_certificate(Secret::Asn1Buffer(&cert.cert_der))?
            .with_private_key(Secret::Asn1Buffer(&cert.key_der))?
            .build();

        let io = IoBuffer::new();
        let session_config = SessionConfig::new(io)
            .with_dtls_nonblocking(true)
            .with_ssl_verify_mode(SslVerifyMode::SslVerifyNone);

        let session = ctx.new_session(session_config)?;

        Ok(Self {
            session,
            is_connected: false,
        })
    }

    /// Check if the session is connected.
    pub fn is_connected(&self) -> bool {
        self.is_connected
    }

    /// Handle received data from the network.
    pub fn handle_receive(
        &mut self,
        data: &[u8],
        events: &mut VecDeque<DtlsEvent>,
    ) -> Result<(), WolfError> {
        // First, try to read any pending application data before processing new input
        if self.is_connected {
            loop {
                let mut buf = BytesMut::with_capacity(2048);
                match self.session.try_read(&mut buf) {
                    Ok(Poll::Ready(n)) if n > 0 => {
                        events.push_back(DtlsEvent::Data(buf[..n].to_vec()));
                    }
                    Ok(Poll::PendingRead | Poll::PendingWrite) => break,
                    Ok(Poll::Ready(_)) => break,
                    Ok(Poll::AppData(data)) => {
                        events.push_back(DtlsEvent::Data(data.to_vec()));
                    }
                    Err(_) => break, // Ignore errors when draining
                }
            }
        }

        // Push data to the IO buffer
        self.session.io_cb_mut().set_incoming(data);

        // Try to progress the handshake or read data
        if !self.is_connected {
            self.handle_handshake(events)?;
        } else {
            // Try to read all available application data
            loop {
                let mut buf = BytesMut::with_capacity(2048);
                match self.session.try_read(&mut buf) {
                    Ok(Poll::Ready(n)) if n > 0 => {
                        events.push_back(DtlsEvent::Data(buf[..n].to_vec()));
                    }
                    Ok(Poll::PendingRead | Poll::PendingWrite) => break,
                    Ok(Poll::Ready(_)) => break,
                    Ok(Poll::AppData(data)) => {
                        events.push_back(DtlsEvent::Data(data.to_vec()));
                    }
                    Err(e) => return Err(e.into()),
                }
            }
        }

        Ok(())
    }

    /// Try to progress the handshake.
    ///
    /// Loops `try_negotiate` until wolfssl returns `PendingRead` (needs peer
    /// data) so that the full server flight (ServerHello + encrypted records)
    /// is flushed in one call rather than requiring an extra round-trip.
    fn handle_handshake(&mut self, events: &mut VecDeque<DtlsEvent>) -> Result<(), WolfError> {
        loop {
            match self.session.try_negotiate() {
                Ok(Poll::Ready(())) => {
                    self.is_connected = true;
                    events.push_back(DtlsEvent::Connected);
                    break;
                }
                Ok(Poll::PendingRead) => {
                    // wolfssl needs more data from the peer — stop looping.
                    if self.session.is_init_finished() && !self.is_connected {
                        self.is_connected = true;
                        events.push_back(DtlsEvent::Connected);
                    }
                    break;
                }
                Ok(Poll::PendingWrite) => {
                    // wolfssl has more data to produce — keep going.
                    if self.session.is_init_finished() && !self.is_connected {
                        self.is_connected = true;
                        events.push_back(DtlsEvent::Connected);
                        break;
                    }
                }
                Ok(Poll::AppData(data)) => {
                    events.push_back(DtlsEvent::Data(data.to_vec()));
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    /// Poll for outgoing datagrams.
    pub fn poll_datagram(&mut self) -> Option<Vec<u8>> {
        self.session.io_cb_mut().pop_outgoing()
    }

    /// Drain any pending application data (call before handle_receive if needed)
    pub fn drain_pending_data(&mut self, events: &mut VecDeque<DtlsEvent>) {
        if !self.is_connected {
            return;
        }
        loop {
            let mut buf = BytesMut::with_capacity(2048);
            match self.session.try_read(&mut buf) {
                Ok(Poll::Ready(n)) if n > 0 => {
                    events.push_back(DtlsEvent::Data(buf[..n].to_vec()));
                }
                Ok(Poll::AppData(data)) => {
                    events.push_back(DtlsEvent::Data(data.to_vec()));
                }
                _ => break,
            }
        }
    }

    /// Write application data.
    pub fn write(&mut self, data: &[u8]) -> Result<(), WolfError> {
        let mut buf = BytesMut::from(data);
        match self.session.try_write(&mut buf) {
            Ok(Poll::Ready(_)) => Ok(()),
            Ok(Poll::PendingRead | Poll::PendingWrite) => Ok(()),
            Ok(Poll::AppData(_)) => Ok(()),
            Err(e) => Err(WolfError::Wolf(e)),
        }
    }

    /// Drive the handshake forward (call after setting up, for client).
    pub fn initiate(&mut self) -> Result<(), WolfError> {
        match self.session.try_negotiate() {
            Ok(_) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}
