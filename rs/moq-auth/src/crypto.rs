/// Make rind jsonwebtoken's provider unless the process installed its own first.
pub(crate) fn init() {
	// An error means the process already picked a provider, which it is entitled to.
	let _ = rind_jsonwebtoken::DEFAULT_PROVIDER.install_default();
}

/// TLS on rind with the platform's roots, for an HTTP client given no config of its own.
///
/// reqwest has no crypto provider of its own and builds a TLS config for every scheme, so
/// each client hands it one.
#[cfg(feature = "client")]
pub(crate) fn tls() -> crate::Result<rustls::ClientConfig> {
	use rustls_platform_verifier::BuilderVerifierExt;
	let provider = moq_crypto::provider::provider();
	Ok(rustls::ClientConfig::builder_with_provider(provider)
		.with_safe_default_protocol_versions()
		.map_err(crate::Error::crypto)?
		.with_platform_verifier()
		.map_err(crate::Error::crypto)?
		.with_no_client_auth())
}

#[cfg(test)]
mod tests {
	/// Another crate may turn on a jsonwebtoken backend, so signing alone can't show that rind is
	/// the provider.
	#[test]
	fn installs_rind_when_none_is_installed() {
		super::init();
		let rind = &rind_jsonwebtoken::DEFAULT_PROVIDER;
		let installed = rind.install_default().unwrap_err();
		assert!(std::ptr::eq(installed, rind));
	}
}
