// Exercise the transport adapter on native hosts without a browser or ring.
#[path = "../../src/transport/adapter.rs"]
mod adapter;

// The adapter's hooks for moq-quic's reliable reset, which these backends lack.
fn reliable_reset(_: &dyn std::any::Any) -> bool {
	false
}

fn checkpoint(_: &mut dyn std::any::Any) {}
