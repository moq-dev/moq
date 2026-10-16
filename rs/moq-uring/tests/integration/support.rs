//! Shared test certificates.

#![cfg(target_os = "linux")]
#![allow(dead_code)]

/// A self-signed localhost certificate on disk.
pub struct Certs {
	pub dir: tempfile::TempDir,
	pub cert: std::path::PathBuf,
	pub key: std::path::PathBuf,
}

pub fn certs() -> anyhow::Result<Certs> {
	let signed = rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
	let dir = tempfile::tempdir()?;
	let cert = dir.path().join("cert.pem");
	let key = dir.path().join("key.pem");
	std::fs::write(&cert, signed.cert.pem())?;
	std::fs::write(&key, signed.signing_key.serialize_pem())?;
	Ok(Certs { dir, cert, key })
}

/// A CA-signed server certificate plus a root file holding two CAs, only the
/// second of which signed it.
pub struct Bundle {
	pub dir: tempfile::TempDir,
	pub cert: std::path::PathBuf,
	pub key: std::path::PathBuf,
	/// Both CAs concatenated, in that order.
	pub roots: std::path::PathBuf,
}

/// Build one, for a test that a root file is loaded whole rather than to its
/// first certificate.
pub fn bundle() -> anyhow::Result<Bundle> {
	let ca = |name: &str| -> anyhow::Result<rcgen::CertifiedIssuer<'static, rcgen::KeyPair>> {
		let key = rcgen::KeyPair::generate()?;
		let mut params = rcgen::CertificateParams::new(Vec::new())?;
		params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
		params.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign, rcgen::KeyUsagePurpose::CrlSign];
		params.distinguished_name.push(rcgen::DnType::CommonName, name);
		Ok(rcgen::CertifiedIssuer::self_signed(params, key)?)
	};
	let unrelated = ca("moq unrelated ca")?;
	let signer = ca("moq signing ca")?;

	let key = rcgen::KeyPair::generate()?;
	let mut params = rcgen::CertificateParams::new(vec!["localhost".into()])?;
	params.use_authority_key_identifier_extension = true;
	let cert = params.signed_by(&key, &signer)?;

	let dir = tempfile::tempdir()?;
	let cert_path = dir.path().join("cert.pem");
	let key_path = dir.path().join("key.pem");
	let roots_path = dir.path().join("roots.pem");
	std::fs::write(&cert_path, cert.pem())?;
	std::fs::write(&key_path, key.serialize_pem())?;
	// The signer is second on purpose: reading only the first certificate
	// leaves the store unable to verify anything this server presents.
	std::fs::write(&roots_path, format!("{}{}", unrelated.pem(), signer.pem()))?;
	Ok(Bundle {
		dir,
		cert: cert_path,
		key: key_path,
		roots: roots_path,
	})
}
