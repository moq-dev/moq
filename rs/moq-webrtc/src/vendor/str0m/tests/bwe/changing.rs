//! BWE test with changing network bandwidth conditions.

use std::time::Duration;

use netem::{DataSize, NetemConfig};
use str0m::RtcError;
use str0m::bwe::Bitrate;

use crate::common::{BweTestContext, Step, connect_with_bwe, init_crypto_default, init_log};

#[test]
pub fn bwe_changing_bandwidth() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let plan = vec![
        // Phase 1: High bandwidth (10 Mbps) - above send rate, no constraint
        Step::Conditions {
            description: "High bandwidth conditions",
            config: NetemConfig::new()
                .latency(Duration::from_millis(10))
                .jitter(Duration::from_millis(2))
                .link(Bitrate::mbps(10), DataSize::kbytes(200))
                .seed(42),
        },
        Step::Media {
            description: "Probe for high bandwidth",
            desired_bitrate: Bitrate::mbps(5),
            media_send_rate: Bitrate::mbps(1),
        },
        Step::Run {
            description: "Wait for high bandwidth",
            duration: Duration::from_secs(20),
        },
        Step::Check {
            // BWE should discover high bandwidth (10 Mbps link)
            // Should be well above the send rate
            description: "Ensure we have high bandwidth",
            at_least: Bitrate::mbps(5),
        },
        // Phase 2: Low bandwidth (1 Mbps) - below send rate, constrains it
        Step::Conditions {
            description: "Bad network conditions",
            config: NetemConfig::new()
                .latency(Duration::from_millis(10))
                .jitter(Duration::from_millis(20))
                .link(Bitrate::kbps(800), DataSize::kbytes(50))
                .seed(42),
        },
        Step::Run {
            description: "Run bad conditions",
            duration: Duration::from_secs(5),
        },
        Step::Check {
            // BWE should detect the reduced bandwidth and lower estimate
            // Link is 1 Mbps, so estimate should be constrained to ~1 Mbps
            description: "Lower estimate for bad conditions",
            at_least: Bitrate::kbps(500),
        },
        // Drop the send rate.
        Step::Media {
            description: "Drop the send rate for bad conditions",
            desired_bitrate: Bitrate::mbps(5),
            media_send_rate: Bitrate::kbps(600),
        },
        // Should stabilize
        Step::Run {
            description: "Wait for bad conditions stabilization",
            duration: Duration::from_secs(10),
        },
        Step::Check {
            // BWE should detect the reduced bandwidth and lower estimate
            // Link is 1 Mbps, so estimate should be constrained to ~1 Mbps
            description: "Bad conditions stabilized",
            at_least: Bitrate::kbps(400),
        },
        // Phase 3: Switch back to high bandwidth (10 Mbps)
        Step::Conditions {
            description: "High bandwidth conditions",
            config: NetemConfig::new()
                .latency(Duration::from_millis(10))
                .jitter(Duration::from_millis(2))
                .link(Bitrate::mbps(10), DataSize::kbytes(200))
                .seed(42),
        },
        Step::Run {
            description: "Wait for high bandwidth to recover",
            duration: Duration::from_secs(40),
        },
        Step::Check {
            // BWE should recover and discover high bandwidth again
            // Should be higher than the constrained phase 2
            description: "High bandwidth should recover",
            at_least: Bitrate::mbps(3),
        },
    ];

    let (mut l, mut r) = connect_with_bwe(Bitrate::mbps(1), Bitrate::mbps(5));

    let mut ctx = BweTestContext::new(&mut l, &mut r);

    // Run the plan that changes bandwidth conditions
    ctx.run_plan(&mut l, &mut r, &plan)?;

    Ok(())
}
