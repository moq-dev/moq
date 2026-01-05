use anyhow::Context;
use axum::handler::HandlerWithoutStateExt;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{http::Method, routing::get, Router};
use hang::moq_lite;
#[cfg(feature = "iroh")]
use moq_native::iroh::EndpointConfig;
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;

use crate::Publish;

pub async fn server(
	config: moq_native::ServerConfig,
	#[cfg(feature = "iroh")] iroh_config: Option<EndpointConfig>,
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
	tracing::info!(addr = ?server.local_addr(), "listening");

	// Init iroh server if enabled.
	#[cfg(feature = "iroh")]
	let iroh_fut = if let Some(iroh_config) = iroh_config {
		let server = iroh_config.init_server().await?;
		tracing::info!(endpoint_id = %server.endpoint().id(), "iroh listening");
		Box::pin(accept(server, name.clone(), publish.consume())) as Pin<Box<dyn Future<Output = _>>>
	} else {
		Box::pin(std::future::pending::<anyhow::Result<()>>())
	};

	// tokio::select! does not support feature flags on match arms, thus we set the future to pending
	// if the iroh feature is disabled.
	#[cfg(not(feature = "iroh"))]
	let iroh_fut = Box::pin(std::future::pending::<anyhow::Result<()>>()) as Pin<Box<dyn Future<Output = _>>>;

	#[cfg(unix)]
	// Notify systemd that we're ready.
	let _ = sd_notify::notify(true, &[sd_notify::NotifyState::Ready]);

	let tls_info = server.tls_info();

	tokio::select! {
		res = accept(server, name, publish.consume()) => res,
		res = iroh_fut => res,
		res = publish.run() => res,
		res = web(listen, tls_info, public) => res,
	}
}

async fn accept(
	mut server: impl moq_native::MoqServer,
	name: String,
	consumer: moq_lite::BroadcastConsumer,
) -> anyhow::Result<()> {
	let mut conn_id = 0;

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
	// Create an origin producer to publish to the broadcast.
	let origin = moq_lite::Origin::produce();
	origin.producer.publish_broadcast(&name, consumer);

	// Blindly accept the session (WebTransport or QUIC), regardless of the URL.
	let session = session.accept(origin.consumer, None).await?;

	tracing::info!(id, "accepted session");

	session.closed().await.map_err(Into::into)
}

// Initialize the HTTP server (but don't serve yet).
async fn web(
	bind: SocketAddr,
	tls_info: Arc<RwLock<moq_native::TlsInfo>>,
	public: Option<PathBuf>,
) -> anyhow::Result<()> {
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

	let server = axum_server::bind(bind);
	server.serve(app.into_make_service()).await?;

	Ok(())
}
