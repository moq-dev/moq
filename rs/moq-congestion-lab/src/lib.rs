//! Congestion controllers under test, outside moq-congestion's published API.
//!
//! Each candidate implements [`moq_congestion::Controller`], so the simulated and real-socket
//! comparisons in moq-quic and moq-tokio drive it like any other. A winner graduates into
//! moq-congestion and joins `moq_tokio::quic::CongestionControl` as a new variant.

pub mod gcc;
pub mod scream;

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use kio::clock::Instant;
    use moq_congestion::{Factory, Init};

    use super::*;

    /// The largest UDP payload (RFC 9000), whose two datagrams outgrow every 1200-based default
    const MTU: u16 = 65527;

    fn init(mtu: u16) -> Init {
        Init {
            now: Instant::ZERO,
            mtu,
            rtt: Duration::from_millis(100),
            seed: 0,
        }
    }

    /// The window holds two datagrams at the path MTU (RFC 9002 section 7.2's minimum), or the
    /// transport could never send its first one.
    #[test]
    fn window_holds_two_datagrams_at_large_mtu() {
        let factories: [(&str, Arc<dyn Factory>); 2] = [
            ("gcc", Arc::new(gcc::Config::default())),
            ("scream", Arc::new(scream::Config::default())),
        ];
        let floor = 2 * u64::from(MTU);
        for (name, factory) in factories {
            let window = factory.clone().build(init(MTU)).window();
            assert!(window >= floor, "{name} starts at {window}");

            let mut controller = factory.build(init(1200));
            controller.on_mtu_update(MTU);
            let window = controller.window();
            assert!(window >= floor, "{name} grows its MTU to {window}");
        }
    }
}
