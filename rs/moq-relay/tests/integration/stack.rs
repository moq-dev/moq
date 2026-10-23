//! The library's startup and serving futures fit a Windows main-thread stack.

#![cfg(all(debug_assertions, feature = "_quic"))]

use moq_relay::{Config, Relay};

async fn load_and_run() {
	// Process-wide: another test in this binary may have installed it first.
	let _ = moq_tokio::crypto::install_default();
	let mut config = Config::default();
	config.listen.bind = Some("127.0.0.1:0".parse().unwrap());
	config.listen.tls.generate = vec!["localhost".into()];
	config.auth.public = vec![moq_auth::Pattern::all()];
	config.drain_timeout = std::time::Duration::ZERO;
	let relay = Relay::load(config).await.expect("load relay").with_signals(false);
	let ready = relay.ready();
	let trigger = relay.shutdown_trigger().clone();
	let (result, ()) = tokio::join!(relay.run(), async {
		ready.wait().await.expect("relay ready");
		trigger.start();
	});
	result.expect("run relay");
}

#[test]
fn debug_relay_load_and_run_fit_windows_stack() {
	std::thread::Builder::new()
		.stack_size(1024 * 1024)
		.spawn(|| {
			tokio::runtime::Builder::new_current_thread()
				.enable_all()
				.build()
				.expect("runtime")
				.block_on(load_and_run());
		})
		.expect("spawn small-stack thread")
		.join()
		.expect("relay panicked");
}
