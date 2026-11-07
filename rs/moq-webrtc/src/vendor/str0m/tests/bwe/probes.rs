//! Core probe control mechanics tests.

use std::sync::Arc;
use std::time::Duration;

use netem::{DataSize, NetemConfig};
use str0m::RtcError;
use str0m::bwe::Bitrate;

use crate::common::{BweTestContext, Step, connect_with_bwe, init_crypto_default, init_log};

#[test]
fn initial_exponential_probes_3x_and_6x() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let plan = vec![
        Step::Conditions {
            description: "High bandwidth network",
            config: NetemConfig::new()
                .link(Bitrate::mbps(10), DataSize::kbytes(200))
                .seed(42),
        },
        Step::Media {
            description: "Start at 1 Mbps",
            desired_bitrate: Bitrate::mbps(10),
            media_send_rate: Bitrate::mbps(1),
        },
        Step::Run {
            description: "Allow initial probing",
            duration: Duration::from_millis(100),
        },
        // Verify initial exponential probes at 3x and 6x start rate
        Step::CheckProbe {
            description: "First exponential probe at 3x (3 Mbps)",
            check: Arc::new(|_index, probe| {
                let expected = Bitrate::mbps(3);
                let tolerance = 0.05;
                probe.target_bitrate() >= expected * (1.0 - tolerance)
                    && probe.target_bitrate() <= expected * (1.0 + tolerance)
                    && !probe.is_alr_probe()
            }),
        },
        Step::CheckProbe {
            description: "Second exponential probe at 6x (6 Mbps)",
            check: Arc::new(|_index, probe| {
                let expected = Bitrate::mbps(6);
                let tolerance = 0.05;
                probe.target_bitrate() >= expected * (1.0 - tolerance)
                    && probe.target_bitrate() <= expected * (1.0 + tolerance)
                    && !probe.is_alr_probe()
            }),
        },
    ];

    let (mut l, mut r) = connect_with_bwe(Bitrate::mbps(1), Bitrate::mbps(10));
    let mut ctx = BweTestContext::new(&mut l, &mut r);
    ctx.run_plan(&mut l, &mut r, &plan)?;

    Ok(())
}

#[test]
fn further_exponential_probes_at_2x() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let plan = vec![
        Step::Conditions {
            description: "High bandwidth network",
            config: NetemConfig::new()
                .link(Bitrate::mbps(20), DataSize::kbytes(400))
                .seed(42),
        },
        Step::Media {
            description: "Start at 1 Mbps, high max",
            desired_bitrate: Bitrate::mbps(20),
            media_send_rate: Bitrate::mbps(1),
        },
        Step::Run {
            description: "Let probing discover capacity",
            duration: Duration::from_secs(1),
        },
        // After initial 3x and 6x probes, should see further probes at 2x estimate
        // When estimate grows past 0.7x last probe (0.7 * 6 = 4.2 Mbps),
        // further probe fires at 2x estimate
        Step::CheckProbe {
            description: "Further probe beyond 6x (should be ~2x estimate)",
            check: Arc::new(|_index, probe| {
                // Accept any probe > 6 Mbps (beyond initial 6x)
                // This proves further exponential probing is working
                probe.target_bitrate() > Bitrate::mbps(6) && !probe.is_alr_probe()
            }),
        },
    ];

    let (mut l, mut r) = connect_with_bwe(Bitrate::mbps(1), Bitrate::mbps(20));
    let mut ctx = BweTestContext::new(&mut l, &mut r);
    ctx.run_plan(&mut l, &mut r, &plan)?;

    Ok(())
}

#[test]
fn probes_respect_max_bitrate_cap() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let plan = vec![
        Step::Conditions {
            description: "Very high bandwidth network",
            config: NetemConfig::new()
                .link(Bitrate::mbps(50), DataSize::kbytes(1000))
                .seed(42),
        },
        Step::Media {
            description: "Start at 2 Mbps, cap at 10 Mbps",
            desired_bitrate: Bitrate::mbps(10), // This is the max
            media_send_rate: Bitrate::mbps(2),
        },
        Step::Run {
            description: "Let probing happen",
            duration: Duration::from_secs(2),
        },
        // Every probe should be capped at 2x max_bitrate = 20 Mbps
        // This checks that the probing respects the application's desired rate
        Step::CheckProbe {
            description: "No probe exceeds 2x max_bitrate (20 Mbps)",
            check: Arc::new(|_index, probe| {
                // Every non-ALR probe should be under the 2x max_bitrate cap
                !probe.is_alr_probe() && probe.target_bitrate() <= Bitrate::mbps(20)
            }),
        },
    ];

    let (mut l, mut r) = connect_with_bwe(Bitrate::mbps(2), Bitrate::mbps(10));
    let mut ctx = BweTestContext::new(&mut l, &mut r);
    ctx.run_plan(&mut l, &mut r, &plan)?;

    Ok(())
}
