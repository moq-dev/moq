//! A programmatic relay fixture for downstream integration tests.

use std::net::SocketAddr;

use crate::{Config, Relay};

/// A bound relay with generated TLS and ephemeral QUIC and HTTP ports.
pub struct TestRelay {
	/// The assembled relay; call `run` to start serving.
	pub relay: Relay,
	/// The bound QUIC address.
	pub quic: SocketAddr,
	/// The bound HTTP address.
	pub http: SocketAddr,
	/// The SHA-256 fingerprint of the generated TLS certificate.
	pub fingerprint: String,
	/// The URL for dialing the relay over QUIC.
	pub url: url::Url,
}

/// Bind a public test relay on ephemeral loopback ports without TOML or port probes.
pub async fn test_relay() -> anyhow::Result<TestRelay> {
	let mut config = Config::default();
	config.drain_timeout = std::time::Duration::ZERO;
	config.listen.bind = Some("127.0.0.1:0".parse()?);
	config.listen.tls.generate = vec!["localhost".into()];
	config.web.http.listen = Some("127.0.0.1:0".parse()?);
	config.auth.public = vec![moq_auth::Pattern::all()];

	let relay = Relay::load(config).await?;
	let quic = relay.quic_addr()?;
	let http = relay.web_addrs().http.expect("HTTP listener was configured");
	let fingerprint = relay
		.web()
		.certificate_fingerprints()
		.into_iter()
		.next()
		.ok_or_else(|| anyhow::anyhow!("test relay generated no certificate"))?;
	let url = format!("https://{quic}/").parse()?;
	Ok(TestRelay {
		relay,
		quic,
		http,
		fingerprint,
		url,
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn fixture_binds_ephemeral_ports_and_reports_ready() {
		let fixture = test_relay().await.expect("load test relay");
		assert_ne!(fixture.quic.port(), 0);
		assert_ne!(fixture.http.port(), 0);
		assert_eq!(fixture.relay.web_addrs().http, Some(fixture.http));
		assert_eq!(fixture.fingerprint.len(), 64);
		let ready = fixture.relay.ready();
		let trigger = fixture.relay.shutdown_trigger().clone();
		let running = tokio::spawn(fixture.relay.run());
		ready.wait().await.expect("relay ready");
		let served = reqwest::get(format!("http://{}/certificate.sha256", fixture.http))
			.await
			.expect("fetch fingerprint")
			.text()
			.await
			.expect("read fingerprint");
		assert_eq!(served.trim(), fixture.fingerprint);
		trigger.start();
		running.await.expect("run task").expect("run success");
	}
}
