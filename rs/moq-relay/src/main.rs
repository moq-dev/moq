use moq_relay::{Config, Relay};

#[cfg(feature = "jemalloc")]
#[global_allocator]
static ALLOC: moq_native::jemalloc::tikv_jemallocator::Jemalloc = moq_native::jemalloc::tikv_jemallocator::Jemalloc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	// TODO: It would be nice to remove this and rely on feature flags only.
	// However, some dependency is pulling in `ring` and I don't know why, so meh for now.
	rustls::crypto::aws_lc_rs::default_provider()
		.install_default()
		.expect("failed to install default crypto provider");

	// Build the relay stack from CLI/env/TOML config and run it. Embedders that
	// want in-process workers build a `Relay` the same way, spawn tasks against
	// `relay.origin()`, then call `relay.run()` (see `Relay` docs).
	let config = Config::load()?;
	Relay::new(config).await?.run().await
}
