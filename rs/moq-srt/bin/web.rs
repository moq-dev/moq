//! HTTP sidecar for `serve` mode.
//!
//! Serves `/certificate.sha256` (the TLS fingerprint browsers fetch when
//! connecting to a self-signed `http://` origin) and, optionally, a static
//! directory for local development. Mirrors `moq-cli`'s web server.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use anyhow::Context;
use axum::handler::HandlerWithoutStateExt;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Router, http::Method, routing::get};
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;

/// Serve the cert-fingerprint endpoint (and optional static `public` dir) on `bind`.
pub async fn run(
	bind: &str,
	tls_info: Arc<RwLock<moq_native::tls::Info>>,
	public: Option<PathBuf>,
) -> anyhow::Result<()> {
	let listen = tokio::net::lookup_host(bind)
		.await
		.context("invalid listen address")?
		.next()
		.context("invalid listen address")?;

	async fn handle_404() -> impl IntoResponse {
		(StatusCode::NOT_FOUND, "Not found")
	}

	let fingerprint_handler = move || async move {
		tls_info
			.read()
			.expect("tls_info read lock poisoned")
			.fingerprints
			.first()
			.expect("missing certificate")
			.clone()
	};

	let mut app = Router::new()
		.route("/certificate.sha256", get(fingerprint_handler))
		.layer(CorsLayer::new().allow_origin(Any).allow_methods([Method::GET]));

	if let Some(public) = public.as_ref() {
		tracing::info!(public = %public.display(), "serving directory");

		let public = ServeDir::new(public).not_found_service(handle_404.into_service());
		app = app.fallback_service(public);
	} else {
		app = app.fallback_service(handle_404.into_service());
	}

	// Dual-stack so the cert endpoint answers over IPv4 too, even on Windows
	// where `[::]` is IPv6-only by default.
	let listener = moq_native::bind::tcp(listen).context("failed to bind web listener")?;
	let server = axum_server::from_tcp(listener)?;
	server.serve(app.into_make_service()).await?;

	Ok(())
}
