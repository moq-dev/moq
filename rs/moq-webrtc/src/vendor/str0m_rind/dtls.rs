use std::sync::Arc;
use std::time::Instant;

use crate::vendor::dimpl;
use crate::vendor::str0m_proto::crypto::CryptoError;
use crate::vendor::str0m_proto::crypto::dtls::{
	DtlsCert, DtlsImplError, DtlsInstance, DtlsOutput, DtlsProvider, ProtocolVersion,
};

pub(super) static DTLS: RindDtls = RindDtls;

#[derive(Debug)]
pub(super) struct RindDtls;

impl DtlsProvider for RindDtls {
	fn generate_certificate(&self) -> Option<DtlsCert> {
		// Only a failing system random source fails it, on which str0m panics.
		dimpl::certificate::generate_self_signed_certificate().ok()
	}

	fn new_dtls(
		&self,
		cert: &DtlsCert,
		now: Instant,
		mtu: Option<usize>,
	) -> Result<Box<dyn DtlsInstance>, CryptoError> {
		let provider = dimpl::crypto::rind::default_provider().map_err(|e| CryptoError::Other(e.to_string()))?;

		// dimpl panics on an empty certificate or a key it cannot load, and
		// `RtcConfig::set_dtls_cert` takes both from the caller.
		if cert.certificate.is_empty() {
			return Err(CryptoError::Other("empty DTLS certificate".into()));
		}
		provider
			.key_provider
			.load_private_key(&cert.private_key)
			.map_err(|e| CryptoError::Other(format!("DTLS private key: {e}")))?;

		// ICE checks return routability before DTLS starts, so a server cookie
		// adds a round trip without protecting anything.
		let mut builder = dimpl::Config::builder()
			.with_crypto_provider(provider)
			.use_server_cookie(false);
		if let Some(mtu) = mtu {
			builder = builder.mtu(mtu);
		}
		if self.is_test() {
			// Deterministic handshakes for the stack's own tests.
			builder = builder.dangerously_set_rng_seed(42);
		}
		let config = Arc::new(builder.build().map_err(|e| CryptoError::Other(e.to_string()))?);

		Ok(Box::new(Dtls(dimpl::Dtls::new_12(config, cert.clone(), now))))
	}
}

/// A dimpl connection, forwarded as is.
struct Dtls(dimpl::Dtls);

impl std::fmt::Debug for Dtls {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Dtls").finish_non_exhaustive()
	}
}

impl DtlsInstance for Dtls {
	fn set_active(&mut self, active: bool) {
		self.0.set_active(active);
	}

	fn handle_packet(&mut self, packet: &[u8]) -> Result<(), DtlsImplError> {
		self.0.handle_packet(packet)
	}

	fn poll_output<'a>(&mut self, buf: &'a mut [u8]) -> DtlsOutput<'a> {
		self.0.poll_output(buf)
	}

	fn handle_timeout(&mut self, now: Instant) -> Result<(), DtlsImplError> {
		self.0.handle_timeout(now)
	}

	fn send_application_data(&mut self, data: &[u8]) -> Result<usize, DtlsImplError> {
		self.0.send_application_data(data)
	}

	fn is_active(&self) -> bool {
		self.0.is_active()
	}

	fn protocol_version(&self) -> Option<ProtocolVersion> {
		self.0.protocol_version()
	}

	fn is_closing(&self) -> bool {
		self.0.is_closing()
	}

	fn is_closed(&self) -> bool {
		self.0.is_closed()
	}

	fn close(&mut self) -> Result<(), DtlsImplError> {
		self.0.close()
	}
}
