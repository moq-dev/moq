//! Shared test certificates and tokio timers.

#![allow(dead_code)]

use std::task::Poll;

/// A `Send` [`moq_net::runtime::Timers`] over tokio, for the origin drivers
/// the session tests run on a tokio thread.
#[derive(Clone, Default)]
pub struct TokioTimers;

impl moq_net::runtime::Timers for TokioTimers {
	type Timer = TokioTimer;

	fn timer(&self) -> Self::Timer {
		TokioTimer { at: None, sleep: None }
	}

	fn now(&self) -> moq_net::runtime::Instant {
		tokio::time::Instant::now().into_std()
	}
}

pub struct TokioTimer {
	at: Option<moq_net::runtime::Instant>,
	// Allocated on the first poll after arming, then re-armed in place;
	// construction panics without a live tokio time driver.
	sleep: Option<std::pin::Pin<Box<tokio::time::Sleep>>>,
}

impl moq_net::runtime::Timer for TokioTimer {
	fn set(&mut self, at: Option<moq_net::runtime::Instant>) {
		self.at = at;
		if let (Some(at), Some(sleep)) = (at, &mut self.sleep) {
			sleep.as_mut().reset(tokio::time::Instant::from_std(at));
		}
	}

	fn poll(&mut self, waiter: &kio::Waiter) -> Poll<()> {
		let Some(at) = self.at else { return Poll::Pending };
		let sleep = self
			.sleep
			.get_or_insert_with(|| Box::pin(tokio::time::sleep_until(tokio::time::Instant::from_std(at))));
		if sleep.is_elapsed() {
			return Poll::Ready(());
		}
		waiter.poll_future(sleep.as_mut())
	}
}

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
