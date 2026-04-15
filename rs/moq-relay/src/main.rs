use moq_relay::*;

use anyhow::Context;

#[cfg(feature = "jemalloc")]
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	// TODO: It would be nice to remove this and rely on feature flags only.
	// However, some dependency is pulling in `ring` and I don't know why, so meh for now.
	rustls::crypto::aws_lc_rs::default_provider()
		.install_default()
		.expect("failed to install default crypto provider");

	let mut config = Config::load()?;

	let addr = config.server.bind.unwrap_or("[::]:443".parse().unwrap());

	config.client.max_streams.get_or_insert(DEFAULT_MAX_STREAMS);
	config.server.max_streams.get_or_insert(DEFAULT_MAX_STREAMS);

	let mtls_enabled = !config.server.tls.root.is_empty();

	#[allow(unused_mut)]
	let mut server = config.server.init()?;
	let client = config.client.clone().init()?;

	#[cfg(feature = "iroh")]
	let (server, client) = {
		let iroh = config.iroh.bind().await?;
		(server.with_iroh(iroh.clone()), client.with_iroh(iroh))
	};

	let auth = config.auth.init(client.tls(), mtls_enabled).await?;

	// If we're dialing a remote cluster with an mTLS identity, verify locally
	// that:
	//   1. The cert has a DNS SAN (otherwise the root will reject us);
	//   2. If `cluster.node` is set, it either equals the SAN or extends it
	//      with a `:port` suffix (DNS SANs cannot carry ports).
	// When `cluster.node` is unset we default it to the SAN.
	if config.cluster.root.is_some() && config.client.tls.identity.is_some() {
		let san = config
			.client
			.tls
			.identity_dns_name()?
			.context("client.tls.identity has no DNS SAN; cluster peers cannot authenticate")?;
		match config.cluster.node.as_deref() {
			None => {
				tracing::info!(%san, "deriving cluster.node from client.tls.identity SAN");
				config.cluster.node = Some(san);
			}
			Some(node) => {
				anyhow::ensure!(
					node == san || is_san_with_port(&san, node),
					"cluster.node {node:?} does not match client.tls.identity SAN {san:?}",
				);
			}
		}
	}

	// Every server cert must carry exactly one DNS SAN, and (if cluster.node
	// is set) that SAN must equal the hostname portion of the node name —
	// the same rule we apply to peers that connect in via mTLS.
	let server_sans = server.tls_info().read().unwrap().single_dns_sans()?;
	if let Some(node) = config.cluster.node.as_deref() {
		let expected = node.rsplit_once(':').map(|(host, _)| host).unwrap_or(node);
		for san in &server_sans {
			anyhow::ensure!(
				san == expected,
				"server TLS certificate SAN {san:?} does not match cluster.node {node:?}",
			);
		}
	}

	let cluster = Cluster::new(config.cluster, client);

	// Create a web server too.
	let web = Web::new(
		WebState {
			auth: auth.clone(),
			cluster: cluster.clone(),
			tls_info: server.tls_info(),
			conn_id: Default::default(),
		},
		config.web,
	);

	tracing::info!(%addr, "listening");

	#[cfg(unix)]
	// Notify systemd that we're ready after all initialization is complete
	let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

	#[cfg(feature = "jemalloc")]
	let jemalloc = jemalloc::run();
	#[cfg(not(feature = "jemalloc"))]
	let jemalloc = std::future::pending::<anyhow::Result<()>>();

	tokio::select! {
		Err(err) = cluster.clone().run() => return Err(err).context("cluster failed"),
		Err(err) = web.run() => return Err(err).context("web server failed"),
		Err(err) = serve(server, cluster, auth) => return Err(err).context("server failed"),
		Err(err) = jemalloc => return Err(err).context("jemalloc profiler failed"),
		else => Ok(()),
	}
}

async fn serve(mut server: moq_native::Server, cluster: Cluster, auth: Auth) -> anyhow::Result<()> {
	let mut conn_id = 0;

	while let Some(request) = server.accept().await {
		let conn = Connection {
			id: conn_id,
			request,
			cluster: cluster.clone(),
			auth: auth.clone(),
		};

		conn_id += 1;
		tokio::spawn(async move {
			if let Err(err) = conn.run().await {
				tracing::warn!(%err, "connection closed");
			}
		});
	}

	anyhow::bail!("stopped accepting connections")
}
