#![allow(unused)]

//! Common utilities for Bandwidth Estimation (BWE) integration tests.

use std::sync::Arc;
use std::time::{Duration, Instant};

use netem::NetemConfig;
use str0m::_internal_test_exports::ProbeClusterConfig;
use str0m::bwe::{Bitrate, BweKind};
use str0m::format::Codec;
use str0m::media::MediaKind;
use str0m::media::Pt;
use str0m::rtp::{RtpWrite, Ssrc};
use str0m::{Event, Rtc, RtcError};
use tracing::info;

#[path = "../common.rs"]
mod test_common;
pub use test_common::*;

/// Helper to create two connected peers with BWE enabled on the sender.
pub fn connect_with_bwe(initial_bitrate: Bitrate, desired_bitrate: Bitrate) -> (TestRtc, TestRtc) {
    let start = Instant::now();

    // Only sender (L) needs BWE enabled
    let rtc1 = Rtc::builder()
        .set_rtp_mode(true)
        .enable_bwe(Some(initial_bitrate))
        .build(start);

    let rtc2 = Rtc::builder().set_rtp_mode(true).build(start);

    let (mut l, mut r) = connect_l_r_with_rtc(rtc1, rtc2);

    l.bwe().set_bitrate(Bitrate::ZERO, desired_bitrate);

    // The resolution must be smaller than the fastest send rate we want to test.
    l.set_forced_time_advance(Duration::from_micros(100));
    r.set_forced_time_advance(Duration::from_micros(100));

    // Normalize time after DTLS connection to make tests deterministic across backends
    // This ensures all crypto backends start from the same simulated time
    let normalized_time = l.start + Duration::from_millis(100);

    l.last = normalized_time;
    r.last = normalized_time;

    (l, r)
}

/// Extract the last BWE estimate from events.
pub fn get_last_bwe_estimate(rtc: &TestRtc) -> Option<Bitrate> {
    rtc.events
        .iter()
        .filter_map(|(_, e)| {
            let Event::EgressBitrateEstimate(kind) = e else {
                return None;
            };
            if let BweKind::Twcc { estimate, .. } = kind {
                Some(estimate)
            } else {
                None
            }
        })
        .next_back()
        .copied()
}

/// BweTestContext holds state for running BWE tests.
pub struct BweTestContext {
    ssrc: Ssrc,
    pt: Pt,
    seq_no: u64,
    /// Current media send rate (excluding padding/probes)
    media_send_rate: Bitrate,
    /// Accumulated byte budget for sending (smooths out timing variations)
    byte_budget: f64,
}

type ProbeCheck = Arc<dyn Fn(usize, &ProbeClusterConfig) -> bool>;

#[derive(Clone)]
pub enum Step {
    /// Network conditions
    Conditions {
        description: &'static str,
        config: NetemConfig,
    },
    /// Send media
    Media {
        description: &'static str,
        desired_bitrate: Bitrate,
        media_send_rate: Bitrate,
    },
    /// Run simulation for duration
    Run {
        description: &'static str,
        duration: Duration,
    },
    /// Check the latest BWE estimate
    Check {
        description: &'static str,
        at_least: Bitrate,
    },
    CheckProbe {
        description: &'static str,
        check: ProbeCheck,
    },
    /// Assert no probes fired since last event_offset update
    AssertNoProbes { description: &'static str },
}

impl BweTestContext {
    pub fn new(l: &mut TestRtc, r: &mut TestRtc) -> Self {
        let mid = "vid".into();
        let ssrc_tx: Ssrc = 42.into();
        let ssrc_rtx: Ssrc = 44.into();

        l.direct_api().declare_media(mid, MediaKind::Video);
        l.direct_api()
            .declare_stream_tx(ssrc_tx, Some(ssrc_rtx), mid, None);

        r.direct_api().declare_media(mid, MediaKind::Video);
        r.direct_api()
            .expect_stream_rx(ssrc_tx, Some(ssrc_rtx), mid, None);

        // Enable TWCC feedback on the receiver so it sends feedback to the sender
        r.direct_api().enable_twcc_feedback();

        // Sync time
        let max = l.last.max(r.last);
        l.last = max;
        r.last = max;

        let params = l.params_vp8();
        assert_eq!(params.spec().codec, Codec::Vp8);
        let pt = params.pt();

        let ssrc = l.direct_api().stream_tx_by_mid(mid, None).unwrap().ssrc();

        Self {
            ssrc,
            pt,
            seq_no: 47_000,
            media_send_rate: Bitrate::mbps(2), // Default to 2 Mbps
            byte_budget: 0.0,
        }
    }

    pub fn run_plan(
        &mut self,
        l: &mut TestRtc,
        r: &mut TestRtc,
        plan: &[Step],
    ) -> Result<(), RtcError> {
        let total = plan.len();

        let mut event_offset = 0;

        for (no, step) in plan.iter().enumerate() {
            match step {
                Step::Conditions {
                    description,
                    config,
                } => {
                    info!("{}/{}: Set conditions: {}", no + 1, total, description);
                    l.set_netem(*config);
                    r.set_netem(*config);
                    event_offset = l.events.len();
                }
                Step::Media {
                    description,
                    desired_bitrate,
                    media_send_rate,
                } => {
                    info!("{}/{}: Media rates: {}", no + 1, total, description);
                    l.bwe().set_bitrate(Bitrate::ZERO, *desired_bitrate);
                    self.set_media_send_rate(*media_send_rate);
                    event_offset = l.events.len();
                }
                Step::Run {
                    description,
                    duration,
                } => {
                    info!("{}/{}: Run: {}", no + 1, total, description);
                    self.run_for_duration(l, r, *duration)?;
                    // Don't update event_offset here - let CheckProbe see events from this Run
                }
                Step::Check {
                    description,
                    at_least,
                } => {
                    let estimate = get_last_bwe_estimate(l).expect("a BWE estimate");
                    info!(
                        "{}/{}: Check estimate: {} ({} >= {})",
                        no + 1,
                        total,
                        description,
                        estimate,
                        *at_least
                    );

                    let is_ok = estimate >= *at_least;

                    assert!(
                        is_ok,
                        "Step {} estimate {} should be at least {}",
                        no + 1,
                        estimate,
                        *at_least
                    );
                    // Check does not reset event offset.
                }
                Step::CheckProbe { description, check } => {
                    info!("{}/{}: Check probe: {}", no + 1, total, description);

                    // All probes since previous step started.
                    let probes = l.events[event_offset..]
                        .iter()
                        .filter_map(|e| match e {
                            (_, Event::Probe(probe)) => Some(probe),
                            _ => None,
                        })
                        .enumerate();

                    let mut any_ok = false;

                    for (index, probe) in probes {
                        let is_ok = (check)(index, probe);
                        if is_ok {
                            any_ok = true;
                            break;
                        }
                    }

                    assert!(any_ok, "No probe check passed");

                    // Check does not reset event offset.
                }
                Step::AssertNoProbes { description } => {
                    info!("{}/{}: Assert no probes: {}", no + 1, total, description);

                    // Count probes since previous step started.
                    let probe_count = l.events[event_offset..]
                        .iter()
                        .filter(|e| matches!(e, (_, Event::Probe(_))))
                        .count();

                    assert_eq!(
                        probe_count, 0,
                        "Expected no probes, but {} probe(s) fired",
                        probe_count
                    );

                    // AssertNoProbes does not reset event offset.
                }
            }
        }

        Ok(())
    }

    pub fn set_media_send_rate(&mut self, media_send_rate: Bitrate) {
        self.media_send_rate = media_send_rate;
    }

    /// Run traffic for specified duration
    pub fn run_for_duration(
        &mut self,
        l: &mut TestRtc,
        r: &mut TestRtc,
        duration: Duration,
    ) -> Result<Option<Bitrate>, RtcError> {
        let start_duration = l.duration();
        let end_time = start_duration + duration;

        let mut last_send_time = l.duration();

        while l.duration() < end_time {
            let current_time = l.duration();

            // Calculate elapsed time since last send
            let elapsed = current_time.saturating_sub(last_send_time);
            last_send_time = current_time;

            // Accumulate byte budget based on elapsed time and target bitrate
            // This smooths out timing variations by allowing budget to carry over
            let elapsed_secs = elapsed.as_secs_f64();
            let bytes_earned = (self.media_send_rate.as_u64() as f64 / 8.0) * elapsed_secs;
            self.byte_budget += bytes_earned;

            // Use packet size of ~1150 bytes (allows RTX probe reuse)
            let packet_size = 1150;

            let mut did_progress = false;

            // Send packets while we have budget
            // IMPORTANT: Must call progress() after EACH write_rtp to consume outputs
            while self.byte_budget >= packet_size as f64 {
                // Calculate wallclock for THIS packet at current simulated time
                let wallclock = l.start + l.duration();
                let time = (self.seq_no * 1000 + 47_000_000) as u32;

                let mut direct = l.direct_api();
                let stream = direct.stream_tx(&self.ssrc).unwrap();

                // Send a video packet
                let payload = vec![0u8; packet_size];
                stream.write_rtp(
                    RtpWrite::new(self.pt, self.seq_no.into(), time, wallclock, payload)
                        .nackable(true),
                );

                self.seq_no += 1;
                self.byte_budget -= packet_size as f64;

                // Must progress after each write_rtp to consume outputs
                progress(l, r)?;
                did_progress = true;
            }

            if !did_progress {
                progress(l, r)?;
            }
        }

        Ok(get_last_bwe_estimate(l))
    }
}
