//! Relay-side task categories and the SIGUSR2 snapshot handler.
//!
//! New categories for detached tasks spawned by `moq-relay` live here. The
//! [`run`] future listens for `SIGUSR2` and logs a snapshot of every tracked
//! category (both moq-lite and moq-relay), and also passively logs the same
//! snapshot every 10 minutes so slow leaks are visible from the journal
//! without operator intervention.
//!
//! Trigger an on-demand dump with:
//!
//! ```sh
//! systemctl kill -s SIGUSR2 moq-relay
//! # or
//! kill -USR2 $(pidof moq-relay)
//! ```

use std::time::Duration;

use moq_lite::task::Category;

/// The accept-loop task spawned per incoming QUIC/WebTransport connection.
pub static CONNECTION: Category = Category::new("moq-relay/connection");

/// Per-remote-cluster-node connection task (`Cluster::run_remote`).
pub static CLUSTER_REMOTE: Category = Category::new("moq-relay/cluster-remote");

/// Background TLS certificate reload watcher (`Web::reload_certs`).
pub static CERT_RELOAD: Category = Category::new("moq-relay/cert-reload");

/// Listen for SIGUSR2 and log the current task snapshot. Also logs the
/// snapshot passively every 10 minutes.
pub async fn run() -> anyhow::Result<()> {
	let mut sig = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined2())?;

	// 10 minutes between passive dumps — cheap enough to leave on, noisy
	// enough to catch a leak within an alert window.
	let mut ticker = tokio::time::interval(Duration::from_secs(600));
	// `interval` fires immediately on the first tick; we want the first
	// passive dump to land 10 minutes after startup, not at t=0.
	ticker.tick().await;

	loop {
		tokio::select! {
			_ = sig.recv() => {
				tracing::info!("task snapshot requested (SIGUSR2)");
				log_snapshot();
			}
			_ = ticker.tick() => {
				log_snapshot();
			}
		}
	}
}

fn log_snapshot() {
	for entry in moq_lite::task::snapshot() {
		tracing::info!(target: "task_snapshot", %entry);
	}
}
