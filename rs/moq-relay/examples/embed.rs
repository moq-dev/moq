//! Embed the relay in an application: extra HTTP routes, cloned handles,
//! the owner keeps the listeners and workers.
//!
//! ```ignore
//! let relay = Relay::load(config).await?;
//! let origin = relay.cluster().origin.clone();
//! let trigger = relay.shutdown_trigger().clone();
//! let web = relay.web().routes().route("/hello", get(hello));
//! relay.with_web(web).run().await
//! ```
//!
//! The accessors borrow and `run` consumes the relay, so every handle the
//! application keeps is cloned first. Application tasks publish into
//! `origin`; `trigger.start()` drains the sessions and `run` returns. Extra
//! listeners (RTMP, SRT, ...) sit beside `run` in the application's
//! `select!`; they never take the QUIC sockets out of the relay.

use axum::routing::get;
use moq_relay::{Config, Relay};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	rustls::crypto::aws_lc_rs::default_provider()
		.install_default()
		.expect("failed to install default crypto provider");

	let mut config = Config::default();
	config.listen.bind = Some("127.0.0.1:0".into());
	config.listen.tls.generate = vec!["localhost".into()];
	config.web.http.listen = Some("127.0.0.1:0".parse()?);
	config.auth.public = vec![moq_auth::Pattern::all()];

	let relay = Relay::load(config).await?;
	// Cloned before `run` consumes the relay. In-process workers publish into
	// the origin every session sees; the trigger stops the relay from any task.
	let _origin = relay.cluster().origin.clone();
	let _trigger = relay.shutdown_trigger().clone();
	// Start from the built-in routes: `with_web(Router::new())` would drop them.
	let web = relay.web().routes().route("/hello", get(|| async { "hello\n" }));
	relay.with_web(web).run().await
}
