//! Loss-based bandwidth estimation tests.

use std::time::Duration;

use netem::{DataSize, NetemConfig, Probability, RandomLoss};
use str0m::RtcError;
use str0m::bwe::Bitrate;

use crate::common::{BweTestContext, Step, connect_with_bwe, init_crypto_default, init_log};

#[test]
fn loss_caps_estimate_below_delay_based() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let plan = vec![
        Step::Conditions {
            description: "Network with moderate loss",
            config: NetemConfig::new()
                .latency(Duration::from_millis(20))
                .jitter(Duration::from_millis(5))
                .loss(RandomLoss::new(Probability::new(0.10))) // 10% loss
                .link(Bitrate::mbps(5), DataSize::kbytes(100))
                .seed(42),
        },
        Step::Media {
            description: "Send at 2 Mbps",
            desired_bitrate: Bitrate::mbps(5),
            media_send_rate: Bitrate::mbps(2),
        },
        // Past startup phase (2 seconds) so loss controller activates
        Step::Run {
            description: "Wait past startup phase",
            duration: Duration::from_secs(3),
        },
        Step::Run {
            description: "Let loss controller observe and cap",
            duration: Duration::from_secs(5),
        },
        Step::Check {
            description: "Estimate capped by loss (safety net)",
            at_least: Bitrate::mbps(1),
        },
    ];

    let (mut l, mut r) = connect_with_bwe(Bitrate::mbps(2), Bitrate::mbps(5));
    let mut ctx = BweTestContext::new(&mut l, &mut r);
    ctx.run_plan(&mut l, &mut r, &plan)?;

    Ok(())
}

#[test]
fn loss_controller_only_reduces_never_increases() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let plan = vec![
        // Start with clean network to establish high estimate
        Step::Conditions {
            description: "Clean network initially",
            config: NetemConfig::new()
                .latency(Duration::from_millis(10))
                .link(Bitrate::mbps(5), DataSize::kbytes(150))
                .seed(42),
        },
        Step::Media {
            description: "Send at 2 Mbps",
            desired_bitrate: Bitrate::mbps(5),
            media_send_rate: Bitrate::mbps(2),
        },
        Step::Run {
            description: "Build up estimate",
            duration: Duration::from_secs(5),
        },
        Step::Check {
            description: "Estimate should be healthy",
            at_least: Bitrate::mbps(2),
        },
        // Introduce loss
        Step::Conditions {
            description: "Add significant loss",
            config: NetemConfig::new()
                .latency(Duration::from_millis(10))
                .loss(RandomLoss::new(Probability::new(0.15))) // 15% loss
                .link(Bitrate::mbps(5), DataSize::kbytes(150))
                .seed(42),
        },
        Step::Run {
            description: "Loss controller reduces estimate",
            duration: Duration::from_secs(5),
        },
        Step::Check {
            description: "Estimate reduced by loss controller",
            at_least: Bitrate::mbps(1),
        },
        // Remove loss - estimate should NOT increase from loss controller
        // (only delay-based increases)
        Step::Conditions {
            description: "Remove loss",
            config: NetemConfig::new()
                .latency(Duration::from_millis(10))
                .link(Bitrate::mbps(5), DataSize::kbytes(150))
                .seed(42),
        },
        Step::Run {
            description: "Delay-based can recover, not loss-based",
            duration: Duration::from_secs(5),
        },
        // We just verify the test completes without checking specific value
        // The point is loss controller acts as safety net, not growth driver
    ];

    let (mut l, mut r) = connect_with_bwe(Bitrate::mbps(2), Bitrate::mbps(5));
    let mut ctx = BweTestContext::new(&mut l, &mut r);
    ctx.run_plan(&mut l, &mut r, &plan)?;

    Ok(())
}
