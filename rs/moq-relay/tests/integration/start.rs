//! Start relay fixtures before clients connect, preserving any startup error.

pub(crate) async fn start_relay(relay: crate::Relay) -> anyhow::Result<tokio::task::JoinHandle<anyhow::Result<()>>> {
	let ready = relay.ready();
	let mut run = Box::pin(relay.run());
	tokio::select! {
		Ok(()) = ready.wait() => Ok(tokio::spawn(run)),
		result = &mut run => {
			result?;
			anyhow::bail!("relay stopped before becoming ready");
		}
	}
}
