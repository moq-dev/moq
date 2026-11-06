/// Refuse a CPU that rind can't run on, naming the missing feature, and make rind
/// jsonwebtoken's provider unless the process installed its own first.
///
/// rind panics on such a CPU instead, so every key runs this before its first operation.
pub(crate) fn init() -> crate::Result<()> {
	rind::cpu::check().map_err(crate::Error::unsupported_cpu)?;
	// An error means the process already picked a provider, which it is entitled to.
	let _ = rind_jsonwebtoken::DEFAULT_PROVIDER.install_default();
	Ok(())
}

/// TLS on rind with the platform's roots, for an HTTP client given no config of its own.
///
/// reqwest has no crypto provider of its own and builds a TLS config for every scheme, so
/// each client hands it one.
#[cfg(feature = "client")]
pub(crate) fn tls() -> crate::Result<rustls::ClientConfig> {
	use rustls_platform_verifier::BuilderVerifierExt;
	let provider = rind_rustls::default_provider().map_err(crate::Error::unsupported_cpu)?;
	Ok(
		rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(provider))
			.with_safe_default_protocol_versions()
			.map_err(crate::Error::crypto)?
			.with_platform_verifier()
			.map_err(crate::Error::crypto)?
			.with_no_client_auth(),
	)
}

#[cfg(test)]
mod tests {
	/// The dev-dependencies turn on jsonwebtoken's aws-lc backend, so signing alone can't show
	/// that rind is the provider.
	#[test]
	fn installs_rind_when_none_is_installed() {
		super::init().unwrap();
		let installed = rind_jsonwebtoken::DEFAULT_PROVIDER.install_default().unwrap_err();
		assert!(std::ptr::eq(installed, &rind_jsonwebtoken::DEFAULT_PROVIDER));
	}
}
