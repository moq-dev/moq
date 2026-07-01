use anyhow::Context;
use axum::handler::HandlerWithoutStateExt;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Router, http::Method, routing::get};
use std::sync::{Arc, RwLock};
use tower_http::cors::{Any, CorsLayer};

/// Serve an axum router over TCP, optionally terminating TLS. Used by the HLS
/// and WebRTC (WHIP/WHEP) HTTP endpoints.
pub async fn serve(
	listener: std::net::TcpListener,
	app: Router,
	tls: Option<Arc<rustls::ServerConfig>>,
) -> anyhow::Result<()> {
	let service = app.into_make_service();
	match tls {
		Some(config) => {
			let config = axum_server::tls_rustls::RustlsConfig::from_config(config);
			axum_server::from_tcp_rustls(listener, config)?.serve(service).await?;
		}
		None => {
			axum_server::from_tcp(listener)?.serve(service).await?;
		}
	}
	Ok(())
}

/// Serve the `/certificate.sha256` self-signed fingerprint over HTTP, so an
/// `http://` client can pin a `--server-bind` server's generated cert.
pub async fn run_web(bind: &str, tls_info: Arc<RwLock<moq_native::tls::Info>>) -> anyhow::Result<()> {
	let listen = tokio::net::lookup_host(bind)
		.await
		.context("invalid listen address")?
		.next()
		.context("invalid listen address")?;

	async fn handle_404() -> impl IntoResponse {
		(StatusCode::NOT_FOUND, "Not found")
	}

	let fingerprint_handler = move || async move {
		// Get the first certificate's fingerprint.
		// TODO serve all of them so we can support multiple signature algorithms.
		tls_info
			.read()
			.expect("tls_info read lock poisoned")
			.fingerprints
			.first()
			.expect("missing certificate")
			.clone()
	};

	let app = Router::new()
		.route("/certificate.sha256", get(fingerprint_handler))
		.layer(CorsLayer::new().allow_origin(Any).allow_methods([Method::GET]))
		.fallback_service(handle_404.into_service());

	// Dual-stack so the cert endpoint answers over IPv4 too, even on Windows
	// where `[::]` is IPv6-only by default.
	let listener = moq_native::bind::tcp(listen).context("failed to bind web listener")?;
	let server = axum_server::from_tcp(listener)?;
	server.serve(app.into_make_service()).await?;

	Ok(())
}
