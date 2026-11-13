use moq_relay::{Config, Relay};

#[cfg(feature = "jemalloc")]
#[global_allocator]
static ALLOC: moq_tokio::jemalloc::Allocator = moq_tokio::jemalloc::Allocator;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	moq_tokio::crypto::install_default()?;

	// The whole startup sequence lives in `Relay::load` rather than here, so an
	// embedder gets it by calling one function instead of copying this file.
	// Extra routes and cloned handles go on the returned `Relay`; `run` keeps
	// the listeners and workers.
	let config = Config::load()?;
	if config.handoff.supervise {
		let code = moq_relay::handoff::supervise(&config.handoff).await?;
		std::process::exit(code);
	}
	Relay::load(config).await?.run().await
}
