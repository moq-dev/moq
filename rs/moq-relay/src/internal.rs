//! Internal (ops) listener.
//!
//! A tiny plain-HTTP server, separate from the customer-facing [`Web`](crate::Web)
//! server, for endpoints that should never touch the public port. It's a
//! trusted-plane surface named for WHO may reach it, not for any single
//! endpoint, so it's the home for operational endpoints in general.
//!
//! Today it serves:
//! - `/metrics` - this node's own traffic counters as Prometheus text
//!   exposition. A distinct plane from both the customer `web` surface and the
//!   MoQ `.stats` broadcast: the same atomics, but a different transport and
//!   audience (an ops scraper, not a customer or the dashboard/billing
//!   aggregators).
//! - `/health` - a liveness mirror of the public probe, for internal checks
//!   that don't want to hit the customer port.
//! - `/nodes` - the cluster nodes visible through gossip plus established
//!   direct relay connections.
//!
//! Everything here is unauthenticated, so bind it only to a trusted plane -
//! loopback for a co-located scraper/agent, or a private overlay address; see
//! [`InternalConfig::listen`]. Unset by default (opt-in). Any future endpoint
//! added here inherits that "unauthenticated, trusted-plane-only" contract; a
//! mutating/control endpoint would need its own auth and doesn't belong on an
//! unauthenticated bind as-is.

use std::net;

use anyhow::Context as _;
use axum::{
	Json, Router,
	extract::State,
	http::{self, StatusCode},
	response::{IntoResponse, Response},
	routing::get,
};
use clap::Parser;

/// Configuration for the internal (ops) listener.
#[derive(Parser, Clone, Debug, serde::Deserialize, serde::Serialize, Default)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct InternalConfig {
	/// Socket address for the internal listener (plain HTTP), serving the ops
	/// endpoints (`/metrics`, `/health`, and `/nodes`).
	///
	/// These endpoints are unauthenticated, so bind it only to a trusted plane:
	/// loopback (e.g. `127.0.0.1:9101`) for a co-located scraper/agent, or a
	/// private overlay address. Never the public internet. Plain HTTP is
	/// intentional: on loopback there's nothing to encrypt, and a private
	/// overlay (e.g. a mesh VPN) already provides transport encryption and peer
	/// identity. Unset (the default) disables the listener entirely.
	#[arg(long = "internal-listen", env = "MOQ_INTERNAL_LISTEN")]
	pub listen: Option<net::SocketAddr>,
}

/// The internal (ops) service: a plain-HTTP server over the node's stats
/// registry ([`moq_net::stats::Registry`]).
pub struct Internal {
	config: InternalConfig,
	stats: moq_net::stats::Registry,
	nodes: Option<crate::nodes::Nodes>,
}

#[derive(Clone)]
struct InternalState {
	stats: moq_net::stats::Registry,
	nodes: Option<crate::nodes::Nodes>,
}

impl Internal {
	/// Create the service from its config and the node's stats registry.
	pub fn new(config: InternalConfig, stats: moq_net::stats::Registry) -> Self {
		Self {
			config,
			stats,
			nodes: None,
		}
	}

	/// Attach the relay cluster used to serve the `/nodes` topology snapshot.
	pub fn with_cluster(mut self, cluster: &crate::Cluster) -> Self {
		self.nodes = Some(cluster.nodes.clone());
		self
	}

	/// Build the ops router (`/metrics`, `/health`, and `/nodes`), returning a
	/// state-erased [`Router`] an embedder can extend (`merge`/`nest` its own ops
	/// routes) before handing it back to [`serve`](Self::serve).
	///
	/// Anything merged in inherits this listener's "unauthenticated,
	/// trusted-plane-only" contract; see the module docs.
	pub fn routes(&self) -> Router {
		Router::new()
			.route("/metrics", get(serve_metrics))
			.route("/health", get(serve_health))
			.route("/nodes", get(serve_nodes))
			.with_state(InternalState {
				stats: self.stats.clone(),
				nodes: self.nodes.clone(),
			})
	}

	/// Serve `app` on [`InternalConfig::listen`] until it shuts down.
	///
	/// The mirror of [`Web::serve`](crate::Web::serve): the caller builds `app`
	/// from [`routes`](Self::routes) plus whatever extra ops routes it merged in,
	/// and this owns the listener. An embedder that binds the socket itself
	/// instead would fork both the socket options and the disabled-listener
	/// contract below.
	///
	/// When no listen address is configured the future stays pending (never
	/// resolves), so it drops cleanly into a `select!` as a disabled no-op -
	/// mirroring how the relay treats other optional services.
	pub async fn serve(self, app: Router) -> anyhow::Result<()> {
		let Some(listen) = self.config.listen else {
			std::future::pending::<()>().await;
			return Ok(());
		};

		let listener = moq_native::bind::tcp(listen).context("failed to bind internal listener")?;
		// No blanket "…server failed" context here: the caller (main.rs) adds
		// that single top-level layer, matching `Web::serve` / `Cluster::run`.
		axum_server::from_tcp(listener)?.serve(app.into_make_service()).await?;
		Ok(())
	}

	/// Serves the default ops router on the configured listener until it shuts
	/// down. Convenience for the standalone binary; equivalent to
	/// `internal.serve(internal.routes())`.
	pub async fn run(self) -> anyhow::Result<()> {
		let app = self.routes();
		self.serve(app).await
	}
}

/// Liveness probe mirror for the internal listener. Always `200 ok`. The
/// customer-facing [`Web`](crate::Web) server serves its own public `/health`;
/// this one lets an internal prober check the process over the trusted plane
/// without touching the public port.
async fn serve_health() -> Response {
	(StatusCode::OK, "ok\n").into_response()
}

/// Prometheus text-exposition metrics for this node's own MoQ traffic counters
/// (bytes/frames/groups, subscriptions, viewers, and connected sessions),
/// summed across broadcasts and split by `tier`/`role`.
///
/// Unauthenticated, which is why it lives on the internal listener rather than
/// the public web one; a scraper needs no JWT. Host system metrics
/// (CPU/memory/disk/network) are deliberately out of scope: run a dedicated node
/// exporter for those, per the relay's separation of concerns. Returns the
/// current cumulative snapshot; a downstream scraper derives rates and live
/// counts (`open - closed`).
async fn serve_metrics(State(state): State<InternalState>) -> Response {
	let body = render_metrics(&state.stats.snapshot());
	([(http::header::CONTENT_TYPE, "text/plain; version=0.0.4")], body).into_response()
}

/// Cluster nodes currently visible through gossip or a direct outbound dial.
///
/// Inbound connections appear only after their SETUP origin identity resolves
/// to a unique `.internal/origins` node advertisement. Sessions without a
/// unique match are omitted.
async fn serve_nodes(State(state): State<InternalState>) -> Json<crate::nodes::Snapshot> {
	Json(state.nodes.map(|nodes| nodes.snapshot()).unwrap_or_default())
}

/// Render a [`moq_net::stats::Snapshot`] as Prometheus text exposition (v0.0.4).
///
/// Hand-formatted rather than pulling in a metrics registry crate: the atomics
/// already are the registry, and a snapshot is a fixed handful of labeled
/// counters, so a registry would only add a second source of truth to keep in
/// sync.
fn render_metrics(snap: &moq_net::stats::Snapshot) -> String {
	use std::fmt::Write as _;

	let traffic = snap.traffic();
	let mut out = String::new();

	// One HELP/TYPE header followed by a (tier, role) row per active tier for a
	// counter selected out of `Traffic` by `field`.
	let counter = |out: &mut String, name: &str, help: &str, field: fn(&moq_net::stats::Traffic) -> u64| {
		let _ = writeln!(out, "# HELP {name} {help}");
		let _ = writeln!(out, "# TYPE {name} counter");
		for (tier, role, totals) in &traffic {
			let _ = writeln!(
				out,
				"{name}{{tier=\"{}\",role=\"{}\"}} {}",
				tier.as_str(),
				role.as_str(),
				field(totals)
			);
		}
	};

	counter(
		&mut out,
		"moq_relay_bytes_total",
		"Media payload bytes transferred.",
		|c| c.bytes,
	);
	counter(&mut out, "moq_relay_frames_total", "Media frames transferred.", |c| {
		c.frames
	});
	counter(&mut out, "moq_relay_groups_total", "Media groups transferred.", |c| {
		c.groups
	});
	counter(
		&mut out,
		"moq_relay_datagrams_total",
		"Single-frame groups carried over unreliable QUIC datagrams; a subset of groups.",
		|c| c.datagrams,
	);
	counter(
		&mut out,
		"moq_relay_fetches_total",
		"One-shot group fetches requested.",
		|c| c.fetches,
	);
	counter(
		&mut out,
		"moq_relay_subscriptions_opened_total",
		"Track subscriptions opened.",
		|c| c.subscriptions,
	);
	counter(
		&mut out,
		"moq_relay_subscriptions_closed_total",
		"Track subscriptions closed; subtract from opened for live subscriptions.",
		|c| c.subscriptions_closed,
	);
	counter(
		&mut out,
		"moq_relay_viewers_opened_total",
		"Distinct (broadcast, session) subscriptions opened.",
		|c| c.broadcasts,
	);
	counter(
		&mut out,
		"moq_relay_viewers_closed_total",
		"Distinct (broadcast, session) subscriptions closed; subtract from opened for live viewers.",
		|c| c.broadcasts_closed,
	);

	// Sessions are per-tier only (no role), so they don't fit the helper above.
	let _ = writeln!(out, "# HELP moq_relay_sessions_opened_total Connected sessions opened.");
	let _ = writeln!(out, "# TYPE moq_relay_sessions_opened_total counter");
	for (tier, sessions) in &snap.sessions() {
		let _ = writeln!(
			out,
			"moq_relay_sessions_opened_total{{tier=\"{}\"}} {}",
			tier.as_str(),
			sessions.sessions
		);
	}
	let _ = writeln!(
		out,
		"# HELP moq_relay_sessions_closed_total Connected sessions closed; subtract from opened for live sessions."
	);
	let _ = writeln!(out, "# TYPE moq_relay_sessions_closed_total counter");
	for (tier, sessions) in &snap.sessions() {
		let _ = writeln!(
			out,
			"moq_relay_sessions_closed_total{{tier=\"{}\"}} {}",
			tier.as_str(),
			sessions.sessions_closed
		);
	}

	out
}

#[cfg(test)]
mod tests {
	use super::*;

	/// `serve` hosts the router it is HANDED, so an embedder's extra ops routes
	/// answer on the same internal listener as the built-in ones. That is what
	/// lets an embedder extend this surface without binding a socket of its own
	/// and forking both the socket options and the disabled-listener contract.
	#[tokio::test]
	async fn serve_hosts_merged_routes_alongside_the_defaults() {
		// A throwaway bind picks a free port, released before `serve` claims it
		// for real (`bind::tcp` sets SO_REUSEADDR, and nothing ever connected).
		let listen = std::net::TcpListener::bind("127.0.0.1:0")
			.expect("probe bind")
			.local_addr()
			.expect("probe addr");

		let internal = Internal::new(
			InternalConfig { listen: Some(listen) },
			moq_net::stats::Registry::disabled(),
		);
		let app = internal
			.routes()
			.merge(Router::new().route("/embedder", get(async || "embedded\n")));
		let server = tokio::spawn(internal.serve(app));

		// `serve` binds inside the task, so poll rather than assume it is up the
		// instant the spawn returns.
		let client = reqwest::Client::new();
		let url = format!("http://{listen}");
		let mut embedder = None;
		for _ in 0..200 {
			if let Ok(res) = client.get(format!("{url}/embedder")).send().await {
				embedder = Some(res);
				break;
			}
			tokio::time::sleep(std::time::Duration::from_millis(10)).await;
		}
		let embedder = embedder.expect("internal listener never accepted a connection");

		assert_eq!(embedder.status(), reqwest::StatusCode::OK);
		assert_eq!(embedder.text().await.expect("embedder body"), "embedded\n");

		let health = client
			.get(format!("{url}/health"))
			.send()
			.await
			.expect("health request");
		assert_eq!(health.status(), reqwest::StatusCode::OK);

		server.abort();
	}

	/// An unconfigured listener leaves `serve` pending rather than resolving, so
	/// dropping it into a `select!` disables the listener instead of completing
	/// an arm immediately and tearing the rest of the process down with it.
	#[tokio::test(start_paused = true)]
	async fn serve_stays_pending_without_a_listen_address() {
		let internal = Internal::new(InternalConfig::default(), moq_net::stats::Registry::disabled());
		let app = internal.routes();

		let elapsed = tokio::time::timeout(std::time::Duration::from_secs(60), internal.serve(app)).await;
		assert!(elapsed.is_err(), "serve resolved with no listen address configured");
	}

	#[tokio::test]
	async fn nodes_endpoint_is_empty_without_an_attached_cluster() {
		let state = InternalState {
			stats: moq_net::stats::Registry::disabled(),
			nodes: None,
		};

		let Json(snapshot) = serve_nodes(State(state)).await;
		assert!(snapshot.nodes.is_empty());
	}

	#[tokio::test]
	async fn nodes_endpoint_uses_the_attached_cluster_registry() {
		let origin = moq_net::Origin::new(100).unwrap().produce();
		let nodes = crate::nodes::Nodes::new(origin);
		let _connection = nodes.connect_outbound(0, "https://relay-b.example/");
		let state = InternalState {
			stats: moq_net::stats::Registry::disabled(),
			nodes: Some(nodes),
		};

		let Json(snapshot) = serve_nodes(State(state)).await;
		assert_eq!(snapshot.nodes[0].node, "https://relay-b.example/");
	}

	/// The `/metrics` renderer emits well-formed Prometheus exposition: a
	/// HELP/TYPE header per metric and a labeled line carrying the live counter
	/// value, summed across broadcasts.
	#[tokio::test(start_paused = true)]
	async fn metrics_render_exposition() {
		use moq_net::stats::{Registry, Tier};
		use moq_net::{Origin, Timestamp, broadcast};

		let stats = Registry::new(Default::default());

		// Default-tier egress: an untagged local publisher writes, a tagged egress
		// consumer reads it out, so publisher `bytes` advance on the default tier.
		let default_ctx = stats.tier(Tier::default()).session("acme");
		let pub_origin = Origin::random().produce();
		let egress = pub_origin.consume().with_stats(default_ctx.clone());
		let mut announced = egress.announced();
		let mut pub_source = pub_origin
			.create_broadcast("demo/x", broadcast::Route::announced())
			.unwrap();
		let mut pub_track = pub_source.create_track("video", None).unwrap();

		// Named-tier ingress: a tagged ingress producer writes, so subscriber
		// `bytes` advance on the regional tier.
		let regional_ctx = stats.tier(Tier::new("region/sjc")).session("peer");
		let sub_origin = Origin::random().produce().with_stats(regional_ctx.clone());
		let mut sub_source = sub_origin
			.create_broadcast("demo/x", broadcast::Route::announced())
			.unwrap();
		let mut sub_track = sub_source.create_track("audio", None).unwrap();

		tokio::time::sleep(std::time::Duration::from_millis(1)).await;
		tokio::time::sleep(std::time::Duration::from_millis(1)).await;

		// Read 1234 egress bytes out of the default-tier broadcast.
		let bc = announced.next().await.unwrap().broadcast.unwrap();
		let mut egress_sub = bc.track("video").unwrap().subscribe(None).await.unwrap();
		{
			let mut group = pub_track.append_group().unwrap();
			group.write_frame(Timestamp::ZERO, vec![0u8; 1234]).unwrap();
			group.finish().unwrap();
		}
		let mut group = egress_sub.recv_group().await.unwrap().unwrap();
		while group.read_frame().await.unwrap().is_some() {}

		// Write 56 ingress bytes into the regional-tier broadcast.
		{
			let mut group = sub_track.append_group().unwrap();
			group.write_frame(Timestamp::ZERO, vec![0u8; 56]).unwrap();
			group.finish().unwrap();
		}

		let body = render_metrics(&stats.snapshot());

		assert!(
			body.contains("# TYPE moq_relay_bytes_total counter"),
			"type header:\n{body}"
		);
		assert!(
			body.contains("moq_relay_bytes_total{tier=\"\",role=\"publisher\"} 1234"),
			"default-tier egress bytes (empty tier label):\n{body}"
		);
		assert!(
			body.contains("moq_relay_bytes_total{tier=\"region/sjc\",role=\"subscriber\"} 56"),
			"named tier gets its own row:\n{body}"
		);
		assert!(
			body.contains("moq_relay_sessions_opened_total{tier=\"\"} 1"),
			"session presence:\n{body}"
		);
	}
}
