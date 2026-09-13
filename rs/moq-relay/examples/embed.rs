//! Embed the relay in an application: extra HTTP routes, cloned handles,
//! the owner keeps the listeners and workers.
//!
//! ```ignore
//! let relay = Relay::load(config).await?;
//! let origin = relay.cluster().origin.clone();
//! let web = relay.web().routes().route("/hello", get(hello));
//! relay.with_web(web).run().await
//! ```
//!
//! Application tasks publish into `origin`. Extra listeners (RTMP, SRT, ...)
//! sit beside `run` in the application's `select!`; they never take the QUIC
//! sockets out of the relay.

use axum::routing::get;
use moq_relay::{Config, PublicConfig, Relay};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	rustls::crypto::aws_lc_rs::default_provider()
		.install_default()
		.expect("failed to install default crypto provider");

	let mut config = Config::default();
	config.listen.bind = Some("127.0.0.1:0".into());
	config.listen.tls.generate = vec!["localhost".into()];
	config.web.http.listen = Some("127.0.0.1:0".parse()?);
	#[allow(deprecated)]
	{
		config.auth.public = Some(PublicConfig::Simple(vec![String::new()]));
	}

	let relay = Relay::load(config).await?;
	// In-process workers publish here, the same origin every session sees.
	let _origin = relay.cluster().origin.clone();
	let web = relay.web().routes().route("/hello", get(|| async { "hello\n" }));
	relay.with_web(web).run().await
}
