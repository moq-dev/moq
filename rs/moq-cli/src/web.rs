use anyhow::Context;
use axum::handler::HandlerWithoutStateExt;
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::IntoResponse;
use axum::{Router, routing::get};
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};

/// Browser CORS policy for HTTP gateway listeners.
#[derive(clap::Args, Clone, Default)]
pub struct Cors {
	/// Browser origin allowed to call this listener. Repeat to allow multiple.
	#[arg(long = "cors-origin", value_name = "ORIGIN", value_parser = parse_origin)]
	pub origin: Vec<HeaderValue>,
}

impl Cors {
	/// Build a CORS layer for the given listener methods.
	pub fn layer<const N: usize>(&self, methods: [Method; N]) -> anyhow::Result<CorsLayer> {
		let layer = CorsLayer::new().allow_methods(methods).allow_headers(Any);
		let wildcard = HeaderValue::from_static("*");

		Ok(match self.origin.as_slice() {
			[] => layer,
			[origin] if origin == wildcard => layer.allow_origin(Any),
			origins => {
				anyhow::ensure!(
					!origins.contains(&wildcard),
					"`--cors-origin *` cannot be combined with specific origins"
				);
				layer.allow_origin(origins.to_vec())
			}
		})
	}
}

fn parse_origin(origin: &str) -> Result<HeaderValue, axum::http::header::InvalidHeaderValue> {
	origin.parse()
}

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
pub async fn run_web(bind: &str, certificates: moq_native::tls::Certificates) -> anyhow::Result<()> {
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
		match certificates.fingerprints().into_iter().next() {
			Some(fingerprint) => fingerprint.into_response(),
			// A stream-only server has no certificate to pin.
			None => (StatusCode::NOT_FOUND, "no certificate\n").into_response(),
		}
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

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn cors_origin_defaults_to_no_browser_origin() {
		let cors = Cors::default();

		assert!(cors.layer([Method::GET]).is_ok());
	}

	#[test]
	fn cors_origin_allows_specific_allowlist() {
		let cors = Cors {
			origin: vec![HeaderValue::from_static("https://example.com")],
		};

		assert!(cors.layer([Method::GET]).is_ok());
	}

	#[test]
	fn cors_origin_rejects_wildcard_with_allowlist() {
		let cors = Cors {
			origin: vec![
				HeaderValue::from_static("*"),
				HeaderValue::from_static("https://example.com"),
			],
		};

		assert!(cors.layer([Method::GET]).is_err());
	}
}
