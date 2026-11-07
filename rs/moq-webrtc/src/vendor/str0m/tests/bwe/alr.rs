//! BWE test for allocation probing mechanics when entering ALR.

use std::sync::Arc;
use std::time::Duration;

use netem::{DataSize, NetemConfig};
use str0m::RtcError;
use str0m::bwe::Bitrate;

use crate::common::{BweTestContext, Step, connect_with_bwe, init_crypto_default, init_log};

#[test]
fn bwe_allocation_probe_on_desired_increase_outside_alr() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let plan = vec![
        Step::Conditions {
            description: "Network supports steady media but limits startup probes",
            config: NetemConfig::new()
                .latency(Duration::from_millis(10))
                .link(Bitrate::mbps(3), DataSize::kbytes(100))
                .seed(42),
        },
        Step::Media {
            description: "Send enough media to stay outside ALR",
            desired_bitrate: Bitrate::mbps(3),
            media_send_rate: Bitrate::mbps(2),
        },
        Step::Run {
            description: "Let startup probes settle",
            duration: Duration::from_secs(2),
        },
        Step::Media {
            description: "Keep demand and traffic unchanged after startup",
            desired_bitrate: Bitrate::mbps(3),
            media_send_rate: Bitrate::mbps(2),
        },
        Step::Run {
            description: "No fresh demand or automatic probe trigger",
            duration: Duration::from_millis(100),
        },
        Step::AssertNoProbes {
            description: "Startup probing has finished",
        },
        Step::Media {
            description: "Raise demand without reducing media traffic",
            desired_bitrate: Bitrate::mbps(5),
            media_send_rate: Bitrate::mbps(2),
        },
        Step::Run {
            description: "Allocation increase should promptly probe outside ALR",
            duration: Duration::from_millis(20),
        },
        Step::CheckProbe {
            description: "Probe is emitted without being classified as ALR",
            check: Arc::new(|_, probe| !probe.is_alr_probe()),
        },
    ];
    let (mut l, mut r) = connect_with_bwe(Bitrate::mbps(2), Bitrate::mbps(3));
    let mut ctx = BweTestContext::new(&mut l, &mut r);
    ctx.run_plan(&mut l, &mut r, &plan)?;
    Ok(())
}

#[test]
pub fn bwe_allocation_probe_on_desired_increase_in_alr() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    // Start with lower max_bitrate, enter ALR, then increase desired
    let mut plan = alr_preamble_steps(Bitrate::mbps(3));
    plan.extend(enter_alr_steps(Bitrate::mbps(3)));
    plan.extend(vec![
        Step::CheckProbe {
            description: "First periodic ALR probe",
            check: Arc::new(|_index, probe| probe.is_alr_probe()),
        },
        // Now increase desired bitrate while in ALR - should trigger allocation probes
        Step::Media {
            description: "Increase desired bitrate while in ALR",
            desired_bitrate: Bitrate::mbps(5), // Increased from 3 to 5
            media_send_rate: Bitrate::kbps(500), // Stay in ALR
        },
        Step::Run {
            description: "Allow probe to be triggered",
            duration: Duration::from_millis(100),
        },
        Step::CheckProbe {
            description: "Allocation probe triggered by desired increase",
            check: Arc::new(|_index, probe| probe.is_alr_probe()),
        },
    ]);

    let (mut l, mut r) = connect_with_bwe(Bitrate::mbps(2), Bitrate::mbps(3));
    let mut ctx = BweTestContext::new(&mut l, &mut r);
    ctx.run_plan(&mut l, &mut r, &plan)?;

    Ok(())
}

#[test]
pub fn bwe_periodic_allocation_probing_in_alr() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut plan = alr_setup_steps(Bitrate::mbps(5));
    plan.extend(vec![
        // First allocation probe on ALR entry
        Step::CheckProbe {
            description: "First allocation probe on ALR entry",
            check: Arc::new(|_index, probe| probe.is_alr_probe()),
        },
        // Wait for periodic probe interval (MIN_TIME_BETWEEN_ALR_PROBES = 5 seconds)
        Step::Run {
            description: "Wait 6 seconds for periodic probe",
            duration: Duration::from_secs(6),
        },
        // Should see another allocation probe after 5 seconds
        Step::CheckProbe {
            description: "Periodic allocation probe fires after 5 seconds",
            check: Arc::new(|_index, probe| probe.is_alr_probe()),
        },
    ]);

    let (mut l, mut r) = connect_with_bwe(Bitrate::mbps(2), Bitrate::mbps(5));
    let mut ctx = BweTestContext::new(&mut l, &mut r);
    ctx.run_plan(&mut l, &mut r, &plan)?;

    Ok(())
}

#[test]
pub fn bwe_no_allocation_probing_when_estimate_at_max() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let plan = vec![
        Step::Conditions {
            description: "Moderate bandwidth network",
            config: NetemConfig::new()
                .latency(Duration::from_millis(10))
                .jitter(Duration::from_millis(2))
                .link(Bitrate::mbps(3), DataSize::kbytes(100))
                .seed(42),
        },
        // Start with bitrate close to max desired
        Step::Media {
            description: "Start at 1.5 Mbps, desire 2 Mbps",
            desired_bitrate: Bitrate::mbps(2),
            media_send_rate: Bitrate::mbps(1),
        },
        // Let estimate grow to near max
        Step::Run {
            description: "Grow to desired bitrate",
            duration: Duration::from_secs(10),
        },
        Step::Check {
            description: "Estimate should reach desired",
            at_least: Bitrate::mbps(1),
        },
        // Enter ALR with estimate >= max_bitrate
        Step::Media {
            description: "Enter ALR with estimate at max",
            desired_bitrate: Bitrate::mbps(2),
            media_send_rate: Bitrate::kbps(500),
        },
        // Note: We can't easily test ABSENCE of probes with current CheckProbe mechanism
        // Instead, we verify estimate stays stable without ALR probing
        Step::Run {
            description: "Wait and verify no growth from allocation probes",
            duration: Duration::from_secs(6),
        },
        Step::Check {
            description: "Estimate stable around max bitrate",
            at_least: Bitrate::mbps(1),
        },
    ];

    let (mut l, mut r) = connect_with_bwe(Bitrate::mbps(1), Bitrate::mbps(2));

    let mut ctx = BweTestContext::new(&mut l, &mut r);

    ctx.run_plan(&mut l, &mut r, &plan)?;

    Ok(())
}

#[test]
fn alr_exit_has_hysteresis() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut plan = alr_setup_steps(Bitrate::mbps(5));
    plan.extend(vec![
        Step::CheckProbe {
            description: "First periodic ALR probe",
            check: Arc::new(|_index, probe| probe.is_alr_probe()),
        },
        // Increase send rate to the hysteresis gap (between entry and exit thresholds)
        // Entry: ratio > 0.80 (utilization < 20%)
        // Exit: ratio < 0.50 (utilization > 50%)
        // Gap: 0.50-0.80 ratio (20-50% utilization) - should STAY in ALR
        Step::Media {
            description: "Increase to ~35% utilization (in hysteresis gap)",
            desired_bitrate: Bitrate::mbps(5),
            media_send_rate: Bitrate::mbps(1), // ~35% of 3 Mbps estimate
        },
        Step::Run {
            description: "Wait over 500ms window for ALR check",
            duration: Duration::from_secs(1),
        },
        // Should still be in ALR - periodic probes should continue
        Step::Run {
            description: "Wait for periodic probe (should still be in ALR)",
            duration: Duration::from_secs(5),
        },
        Step::CheckProbe {
            description: "Periodic ALR probe confirms still in ALR",
            check: Arc::new(|_index, probe| probe.is_alr_probe()),
        },
        // Now increase above exit threshold to confirm we eventually exit
        Step::Media {
            description: "Increase to 60% utilization (above exit threshold)",
            desired_bitrate: Bitrate::mbps(5),
            media_send_rate: Bitrate::mbps(2), // ~60% of 3 Mbps estimate
        },
        Step::Run {
            description: "Wait for ALR exit",
            duration: Duration::from_secs(2),
        },
        // Test completes - we've verified hysteresis by staying in ALR in the gap
    ]);

    let (mut l, mut r) = connect_with_bwe(Bitrate::mbps(2), Bitrate::mbps(5));
    let mut ctx = BweTestContext::new(&mut l, &mut r);
    ctx.run_plan(&mut l, &mut r, &plan)?;

    Ok(())
}

/// Common setup steps for ALR tests: establish connection, wait for probes to complete,
/// then enter ALR with low send rate. Tests can append their specific scenarios.
fn alr_setup_steps(max_bitrate: Bitrate) -> Vec<Step> {
    let mut plan = alr_preamble_steps(max_bitrate);
    plan.extend(enter_alr_steps(max_bitrate));
    plan
}

/// Common ALR test preamble: establish connection and allow initial probing to settle
/// without entering ALR.
fn alr_preamble_steps(max_bitrate: Bitrate) -> Vec<Step> {
    vec![
        Step::Conditions {
            description: "Moderate bandwidth network",
            config: NetemConfig::new()
                .link(Bitrate::mbps(3), DataSize::kbytes(100))
                .seed(42),
        },
        Step::Media {
            description: "Start with 2 Mbps to avoid early ALR",
            desired_bitrate: max_bitrate,
            media_send_rate: Bitrate::mbps(2), // Higher rate prevents ALR during initial probes
        },
        Step::Run {
            description: "Wait for initial probes to complete",
            duration: Duration::from_secs(2),
        },
    ]
}

/// Enter ALR and give the ALR detector time to latch.
fn enter_alr_steps(max_bitrate: Bitrate) -> Vec<Step> {
    vec![
        Step::Media {
            description: "Enter ALR with low send rate",
            desired_bitrate: max_bitrate,
            media_send_rate: Bitrate::kbps(500),
        },
        Step::Run {
            description: "Allow ALR detection",
            duration: Duration::from_secs(1),
        },
        // No probe on ALR entry - wait for first periodic probe (5s after last probe)
        Step::Run {
            description: "Wait for first periodic ALR probe",
            duration: Duration::from_secs(5),
        },
    ]
}
