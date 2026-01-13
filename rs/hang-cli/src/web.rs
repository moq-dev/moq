use anyhow::Context;
use axum::handler::HandlerWithoutStateExt;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Router, http::Method, routing::get};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;

// Initialize the HTTP server (but don't serve yet).
pub async fn run_web(
	bind: SocketAddr,
	tls_info: Arc<RwLock<moq_native::ServerTlsInfo>>,
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

	let mut app = Router::new()
		.route("/certificate.sha256", get(fingerprint_handler))
		.layer(CorsLayer::new().allow_origin(Any).allow_methods([Method::GET]));

	// If a public directory is provided, serve it.
	// We use this for local development to serve the index.html file and friends.
	if let Some(public) = public.as_ref() {
		tracing::info!(public = %public.display(), "serving directory");

		let public = ServeDir::new(public).not_found_service(handle_404.into_service());
		app = app.fallback_service(public);
	} else {
		app = app.fallback_service(handle_404.into_service());
	}

	let server = axum_server::bind(listen);
	server.serve(app.into_make_service()).await?;

	Ok(())
}
