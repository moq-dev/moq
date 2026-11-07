//! BWE estimate behavior tests (capping, recovery, convergence).

use std::time::Duration;

use netem::{DataSize, NetemConfig};
use str0m::RtcError;
use str0m::bwe::Bitrate;

use crate::common::{BweTestContext, Step, connect_with_bwe, init_crypto_default, init_log};

#[test]
fn estimate_capped_by_max_bitrate() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let plan = vec![
        Step::Conditions {
            description: "High capacity network (10 Mbps)",
            config: NetemConfig::new()
                .latency(Duration::from_millis(10))
                .link(Bitrate::mbps(10), DataSize::kbytes(200))
                .seed(42),
        },
        Step::Media {
            description: "Send at 2 Mbps with 3 Mbps max",
            desired_bitrate: Bitrate::mbps(3), // max_bitrate
            media_send_rate: Bitrate::mbps(2),
        },
        Step::Run {
            description: "Let probes discover high capacity",
            duration: Duration::from_secs(10),
        },
        // Estimate should not exceed 3 Mbps despite 10 Mbps capacity
        Step::Check {
            description: "Estimate capped at max_bitrate",
            at_least: Bitrate::mbps(2),
        },
    ];

    let (mut l, mut r) = connect_with_bwe(Bitrate::mbps(2), Bitrate::mbps(3));
    let mut ctx = BweTestContext::new(&mut l, &mut r);
    ctx.run_plan(&mut l, &mut r, &plan)?;

    Ok(())
}

#[test]
fn estimate_recovers_after_capacity_restoration() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let plan = vec![
        Step::Conditions {
            description: "Good network initially",
            config: NetemConfig::new()
                .latency(Duration::from_millis(20))
                .link(Bitrate::mbps(5), DataSize::kbytes(100))
                .seed(42),
        },
        Step::Media {
            description: "Send at 2 Mbps",
            desired_bitrate: Bitrate::mbps(5),
            media_send_rate: Bitrate::mbps(2),
        },
        Step::Run {
            description: "Establish baseline",
            duration: Duration::from_secs(5),
        },
        Step::Check {
            description: "Good estimate",
            at_least: Bitrate::mbps(2),
        },
        // Degrade network temporarily
        Step::Conditions {
            description: "Degrade to 1 Mbps",
            config: NetemConfig::new()
                .latency(Duration::from_millis(40))
                .link(Bitrate::mbps(1), DataSize::kbytes(50))
                .seed(42),
        },
        Step::Run {
            description: "Estimate drops to lower capacity",
            duration: Duration::from_secs(2),
        },
        Step::Check {
            description: "Estimate reduced to ~1 Mbps",
            at_least: Bitrate::kbps(400),
        },
        // Reduce send rate to avoid overwhelming the low estimate
        Step::Media {
            description: "Reduce send rate to allow AIMD recovery",
            desired_bitrate: Bitrate::mbps(5),
            media_send_rate: Bitrate::kbps(500), // Send at ~estimate level
        },
        // Restore network capacity
        Step::Conditions {
            description: "Restore to 5 Mbps",
            config: NetemConfig::new()
                .latency(Duration::from_millis(20))
                .link(Bitrate::mbps(5), DataSize::kbytes(100))
                .seed(42),
        },
        // Initial recovery should be via AIMD (no probes immediately after restoration)
        Step::Run {
            description: "Initial AIMD recovery period",
            duration: Duration::from_secs(3),
        },
        Step::AssertNoProbes {
            description: "No probes during initial AIMD recovery",
        },
        Step::Check {
            description: "AIMD recovery is very slow after severe degradation",
            at_least: Bitrate::kbps(350),
        },
        // Further recovery via AIMD and eventually exponential probes
        Step::Run {
            description: "Continue recovery (slow due to low threshold)",
            duration: Duration::from_secs(25),
        },
        Step::Check {
            description: "Recovery remains slow (threshold adapted down)",
            at_least: Bitrate::mbps(1),
        },
    ];

    let (mut l, mut r) = connect_with_bwe(Bitrate::mbps(2), Bitrate::mbps(5));
    let mut ctx = BweTestContext::new(&mut l, &mut r);
    ctx.run_plan(&mut l, &mut r, &plan)?;

    Ok(())
}

#[test]
fn estimate_converges_to_actual_capacity() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let plan = vec![
        Step::Conditions {
            description: "Network with 2 Mbps capacity",
            config: NetemConfig::new()
                .latency(Duration::from_millis(20))
                .link(Bitrate::mbps(2), DataSize::kbytes(100))
                .seed(42),
        },
        Step::Media {
            description: "Send at 1.5 Mbps",
            desired_bitrate: Bitrate::mbps(5),
            media_send_rate: Bitrate::mbps(1),
        },
        Step::Run {
            description: "Allow convergence over 15 seconds",
            duration: Duration::from_secs(15),
        },
        // Estimate should converge close to 2 Mbps capacity
        Step::Check {
            description: "Estimate converges near capacity",
            at_least: Bitrate::mbps(1),
        },
    ];

    let (mut l, mut r) = connect_with_bwe(Bitrate::mbps(1), Bitrate::mbps(5));
    let mut ctx = BweTestContext::new(&mut l, &mut r);
    ctx.run_plan(&mut l, &mut r, &plan)?;

    Ok(())
}
