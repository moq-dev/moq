//! RTP-out path stub.
//!
//! Both `server subscribe` (WHEP server) and `client publish` (WHIP client)
//! pull frames from a [`moq_net::BroadcastConsumer`] and re-packetize per
//! codec back into RTP via str0m. The codec-side work (Opus passthrough,
//! H.264 AVCC -> Annex-B, etc.) is the actual blocker; the surface here
//! holds the type shape so both entry points can construct an `EgressSource`
//! the same way once the re-packetizers land.

use crate::session::MediaSource;

/// Per-broadcast [`MediaSource`] for the egress paths. Skeleton; constructors
/// stay private until the per-codec packetizers exist.
pub struct EgressSource {
	#[allow(dead_code)]
	consumer: moq_net::BroadcastConsumer,
}

impl MediaSource for EgressSource {}
