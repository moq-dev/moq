use anyhow::Context;
use axum::handler::HandlerWithoutStateExt;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{http::Method, routing::get, Router};
use hang::moq_lite;
use std::net::SocketAddr;
use std::path::PathBuf;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;

use crate::Publish;

pub async fn server(
	config: moq_native::ServerConfig,
	name: String,
	public: Option<PathBuf>,
	publish: Publish,
) -> anyhow::Result<()> {
	let mut listen = config.bind.unwrap_or("[::]:443".parse().unwrap());
	listen = tokio::net::lookup_host(listen)
		.await
		.context("invalid listen address")?
		.next()
		.context("invalid listen address")?;

	let server = config.init()?;

	// Get the first certificate's fingerprint.
	// TODO serve all of them so we can support multiple signature algorithms.
	let fingerprint = server.fingerprints().first().context("missing certificate")?.clone();

	// Notify systemd that we're ready.
	let _ = sd_notify::notify(true, &[sd_notify::NotifyState::Ready]);

	tokio::select! {
		res = accept(server, name, publish.consume()) => res,
		res = publish.run() => res,
		res = web(listen, fingerprint, public) => res,
	}
}

async fn accept(
	mut server: moq_native::Server,
	name: String,
	consumer: moq_lite::BroadcastConsumer,
) -> anyhow::Result<()> {
	let mut conn_id = 0;

	tracing::info!(addr = ?server.local_addr(), "listening");

	while let Some(session) = server.accept().await {
		let id = conn_id;
		conn_id += 1;

		let name = name.clone();

		let consumer = consumer.clone();
		// Handle the connection in a new task.
		tokio::spawn(async move {
			if let Err(err) = run_session(id, session, name, consumer).await {
				tracing::warn!(%err, "failed to accept session");
			}
		});
	}

	Ok(())
}

#[tracing::instrument("session", skip_all, fields(id))]
async fn run_session(
	id: u64,
	session: moq_native::Request,
	name: String,
	consumer: moq_lite::BroadcastConsumer,
) -> anyhow::Result<()> {
	// Blindly accept the session (WebTransport or QUIC), regardless of the URL.
	let session = session.ok().await.context("failed to accept session")?;

	// Create an origin producer to publish to the broadcast.
	let origin = moq_lite::Origin::produce();
	origin.producer.publish_broadcast(&name, consumer);

	let session = moq_lite::Session::accept(session, origin.consumer, None)
		.await
		.context("failed to accept session")?;

	tracing::info!(id, "accepted session");

	session.closed().await.map_err(Into::into)
}

// Initialize the HTTP server (but don't serve yet).
async fn web(bind: SocketAddr, fingerprint: String, public: Option<PathBuf>) -> anyhow::Result<()> {
	async fn handle_404() -> impl IntoResponse {
		(StatusCode::NOT_FOUND, "Not found")
	}

	let mut app = Router::new()
		.route("/certificate.sha256", get(fingerprint))
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

	let server = hyper_serve::bind(bind);
	server.serve(app.into_make_service()).await?;

	Ok(())
}
