use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use dimpl::SrtpProfile;
use openssl::dh::Dh;
use openssl::ssl::{Ssl, SslContext, SslContextBuilder, SslMethod, SslOptions, SslVerifyMode};

use super::cert::OsslDtlsCert;
use super::io_buf::IoBuffer;
use super::stream::TlsStream;
use super::{CryptoError, DATAGRAM_MTU, DATAGRAM_MTU_WARN, DtlsEvent};

// We restrict cipher suites to those that include ephemeral Diffie-Hellman or
// ephemeral Elliptic Curve Diffie-Hellman and modern AEAD ciphers.
const DTLS_CIPHERS: &str =
    "ECDHE+CHACHA20:DHE+CHACHA20:ECDHE+AESGCM:DHE+AESGCM:ECDHE+AES256:DHE+AES256";

pub struct OsslDtlsImpl {
    /// Certificate for the DTLS session.
    _cert: OsslDtlsCert,

    /// Context belongs together with Fingerprint.
    ///
    /// This just needs to be kept alive since it pins the entire openssl context
    /// from which `Ssl` is created.
    _context: SslContext,

    /// The actual openssl TLS stream.
    tls: TlsStream<IoBuffer>,
}

impl OsslDtlsImpl {
    pub fn new(cert: OsslDtlsCert) -> Result<Self, CryptoError> {
        Self::new_with_groups(cert, None)
    }

    pub fn new_with_groups(
        cert: OsslDtlsCert,
        groups_list: Option<&str>,
    ) -> Result<Self, CryptoError> {
        let context = dtls_create_ctx_with_groups(&cert, groups_list)?;
        let ssl = dtls_ssl_create(&context)?;
        Ok(OsslDtlsImpl {
            _cert: cert,
            _context: context,
            tls: TlsStream::new(ssl, IoBuffer::default()),
        })
    }
}

impl OsslDtlsImpl {
    pub fn set_active(&mut self, active: bool) {
        self.tls.set_active(active);
    }

    pub fn is_active(&self) -> Option<bool> {
        self.tls.is_active()
    }

    pub fn handle_receive(
        &mut self,
        m: &[u8],
        o: &mut VecDeque<DtlsEvent>,
    ) -> Result<(), CryptoError> {
        self.tls.inner_mut().set_incoming(m);

        if self.handle_handshake(o)? {
            // early return as long as we're handshaking
            return Ok(());
        }

        let mut buf = vec![0; 2000];
        let n = match self.tls.read(&mut buf) {
            Ok(v) => v,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        };
        buf.truncate(n);

        o.push_back(DtlsEvent::Data(buf));

        Ok(())
    }

    pub fn poll_datagram(&mut self) -> Option<super::DatagramSend> {
        let x = self.tls.inner_mut().pop_outgoing();
        if let Some(x) = &x
            && x.len() > DATAGRAM_MTU_WARN
        {
            eprintln!("DTLS above MTU {}: {}", DATAGRAM_MTU_WARN, x.len());
        }
        x
    }

    pub fn poll_timeout(&mut self, now: Instant) -> Option<Instant> {
        // OpenSSL has a built-in timeout of 1 second that is doubled for
        // each retry. There is a way to get direct control over the
        // timeout (using DTLS_set_timer_cb), but that function doesn't
        // appear to be exposed in openssl crate yet.
        // TODO(martin): Write PR for openssl crate to be able to use this
        // callback to make a tighter timeout handling here.
        self.tls
            .is_handshaking()
            .then(|| now + Duration::from_millis(500))
    }

    pub fn handle_input(&mut self, data: &[u8]) -> Result<(), CryptoError> {
        Ok(self.tls.write_all(data)?)
    }

    pub fn is_connected(&self) -> bool {
        self.tls.is_connected()
    }

    pub fn handle_handshake(
        &mut self,
        output: &mut VecDeque<DtlsEvent>,
    ) -> Result<bool, CryptoError> {
        if self.tls.is_connected() {
            // Nice. Nothing to do.
            Ok(false)
        } else if self.tls.complete_handshake_until_block()? {
            output.push_back(DtlsEvent::Connected);

            let (keying_material, srtp_profile, fingerprint) = self
                .tls
                .take_srtp_keying_material()
                .expect("Exported keying material");

            output.push_back(DtlsEvent::RemoteFingerprint(fingerprint));

            output.push_back(DtlsEvent::SrtpKeyingMaterial(keying_material, srtp_profile));
            Ok(false)
        } else {
            Ok(true)
        }
    }
}

pub fn dtls_create_ctx(cert: &OsslDtlsCert) -> Result<SslContext, CryptoError> {
    dtls_create_ctx_with_groups(cert, None)
}

pub fn dtls_create_ctx_with_groups(
    cert: &OsslDtlsCert,
    groups_list: Option<&str>,
) -> Result<SslContext, CryptoError> {
    // TODO: Technically we want to disallow DTLS < 1.2, but that requires
    // us to use this commented out unsafe. We depend on browsers disallowing
    // it instead.
    // let method = unsafe { SslMethod::from_ptr(DTLSv1_2_method()) };
    let mut ctx = SslContextBuilder::new(SslMethod::dtls())?;

    ctx.set_cipher_list(DTLS_CIPHERS)?;
    if let Some(groups) = groups_list {
        ctx.set_groups_list(groups)?;
    }
    let srtp_profiles = {
        // Rust can't join directly to a string, need to allocate a vec first :(
        // This happens very rarely so the extra allocations don't matter
        let all: Vec<_> = SrtpProfile::ALL.iter().map(openssl_name).collect();

        all.join(":")
    };
    ctx.set_tlsext_use_srtp(&srtp_profiles)?;

    let mut mode = SslVerifyMode::empty();
    mode.insert(SslVerifyMode::PEER);
    mode.insert(SslVerifyMode::FAIL_IF_NO_PEER_CERT);
    ctx.set_verify_callback(mode, |_ok, _ctx| true);

    ctx.set_private_key(&cert.pkey)?;
    ctx.set_certificate(&cert.x509)?;

    let mut options = SslOptions::empty();
    options.insert(SslOptions::SINGLE_ECDH_USE);
    options.insert(SslOptions::NO_DTLSV1);
    ctx.set_options(options);

    // Enable DHE cipher suites by setting temporary DH parameters
    if let Ok(dh) = Dh::get_2048_256() {
        ctx.set_tmp_dh(dh.as_ref())?;
    }

    let ctx = ctx.build();

    Ok(ctx)
}

fn openssl_name(profile: &SrtpProfile) -> &'static str {
    match profile {
        SrtpProfile::AES128_CM_SHA1_80 => "SRTP_AES128_CM_SHA1_80",
        SrtpProfile::AEAD_AES_128_GCM => "SRTP_AEAD_AES_128_GCM",
        SrtpProfile::AEAD_AES_256_GCM => "SRTP_AEAD_AES_256_GCM",
        _ => unreachable!(),
    }
}

pub fn dtls_ssl_create(ctx: &SslContext) -> Result<Ssl, CryptoError> {
    let mut ssl = Ssl::new(ctx)?;
    ssl.set_mtu(DATAGRAM_MTU as u32)?;
    Ok(ssl)
}
