//! Delay-based bandwidth estimation tests.

use std::time::Duration;

use netem::{DataSize, NetemConfig};
use str0m::RtcError;
use str0m::bwe::Bitrate;

use crate::common::{BweTestContext, Step, connect_with_bwe, init_crypto_default, init_log};

#[test]
fn overuse_triggers_aimd_decrease() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let plan = vec![
        Step::Conditions {
            description: "Start with stable network",
            config: NetemConfig::new()
                .latency(Duration::from_millis(20))
                .link(Bitrate::mbps(3), DataSize::kbytes(100))
                .seed(42),
        },
        Step::Media {
            description: "Send at 2 Mbps",
            desired_bitrate: Bitrate::mbps(5),
            media_send_rate: Bitrate::mbps(2),
        },
        Step::Run {
            description: "Establish stable estimate",
            duration: Duration::from_secs(5),
        },
        Step::Check {
            description: "Estimate stabilizes around 2 Mbps",
            at_least: Bitrate::mbps(2),
        },
        // Introduce queuing delay to trigger overuse
        Step::Conditions {
            description: "Increase latency to cause queuing",
            config: NetemConfig::new()
                .latency(Duration::from_millis(100)) // Significant increase
                .link(Bitrate::mbps(3), DataSize::kbytes(100))
                .seed(42),
        },
        Step::Run {
            description: "Wait for overuse detection",
            duration: Duration::from_secs(2),
        },
        Step::Check {
            description: "Estimate drops due to overuse (AIMD decrease)",
            at_least: Bitrate::mbps(1),
        },
    ];

    let (mut l, mut r) = connect_with_bwe(Bitrate::mbps(2), Bitrate::mbps(5));
    let mut ctx = BweTestContext::new(&mut l, &mut r);
    ctx.run_plan(&mut l, &mut r, &plan)?;

    Ok(())
}

#[test]
fn underuse_allows_estimate_growth() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let plan = vec![
        Step::Conditions {
            description: "High capacity network",
            config: NetemConfig::new()
                .latency(Duration::from_millis(10))
                .link(Bitrate::mbps(10), DataSize::kbytes(200))
                .seed(42),
        },
        Step::Media {
            description: "Start low, send consistently",
            desired_bitrate: Bitrate::mbps(10),
            media_send_rate: Bitrate::mbps(1),
        },
        Step::Run {
            description: "Initial estimate",
            duration: Duration::from_secs(2),
        },
        Step::Check {
            description: "Baseline estimate established",
            at_least: Bitrate::mbps(1),
        },
        Step::Run {
            description: "Allow estimate to grow",
            duration: Duration::from_secs(10),
        },
        Step::Check {
            description: "Estimate grows under good conditions",
            at_least: Bitrate::mbps(5),
        },
    ];

    let (mut l, mut r) = connect_with_bwe(Bitrate::mbps(1), Bitrate::mbps(10));
    let mut ctx = BweTestContext::new(&mut l, &mut r);
    ctx.run_plan(&mut l, &mut r, &plan)?;

    Ok(())
}
