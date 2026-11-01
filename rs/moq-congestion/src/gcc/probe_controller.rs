/*
 *  Copyright (c) 2016 The WebRTC project authors. All Rights Reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE file in the root of the source
 *  tree. An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the AUTHORS file in the root of the source tree.
 */

use crate::api::{
    transport::{NetworkAvailability, NetworkStateEstimate, ProbeClusterConfig},
    units::{DataRate, TimeDelta, Timestamp},
};

// NOTE: These have been renamed to match the field trials.
// WebRTC-Bwe-ProbingConfiguration
#[derive(Debug, Clone)]
pub struct ProbeControllerConfig {
    // These parameters configure the initial probes. First we send one or two
    // probes of sizes p1 * self.start_bitrate and p2 * self.start_bitrate.
    // Then whenever we get a bitrate estimate of at least further_probe_threshold
    // times the size of the last sent probe we'll send another one of size
    // step_size times the new estimate.
    pub p1: f64,
    pub p2: f64,
    pub step_size: f64,
    pub further_probe_threshold: f64,
    pub abort_further: bool,
    // Duration of time from the first initial probe where repeated initial probes
    // are sent if repeated initial probing is enabled.
    pub initial_probing: TimeDelta,
    // The minimum probing duration of an individual probe during
    // the repeated_initial_probing_time_period.
    pub initial_probe_duration: TimeDelta,
    // Delta time between sent bursts of packets in a probe during
    // the repeated_initial_probing_time_period.
    pub initial_min_probe_delta: TimeDelta,
    // Configures how often we send ALR probes and how big they are.
    pub alr_interval: TimeDelta,
    pub alr_scale: f64,
    // Configures how often we send probes if NetworkStateEstimate is available.
    pub network_state_interval: TimeDelta,
    // Periodically probe as long as the ratio between current estimate and
    // NetworkStateEstimate is lower then this.
    pub est_lower_than_network_ratio: f64,
    pub est_lower_than_network_interval: TimeDelta,
    pub network_state_scale: f64,
    // Overrides min_probe_duration if network_state_estimate_probing_interval
    // is set and a network state estimate is known and equal or higher than the
    // probe target.
    pub network_state_probe_duration: TimeDelta,
    // Overrides min_probe_delta if network_state_estimate_probing_interval
    // is set and a network state estimate is known and equal or higher than the
    // probe target.
    pub network_state_min_probe_delta: TimeDelta,

    // Configures the probes emitted by changed to the allocated bitrate.
    pub probe_max_allocation: bool,
    pub alloc_p1: f64,
    pub alloc_p2: f64,
    pub alloc_current_bwe_limit: f64,

    // The minimum number probing packets used.
    pub min_probe_packets_sent: i32,
    // The minimum probing duration.
    pub min_probe_duration: TimeDelta,
    // Delta time between sent bursts of packets in a probe.
    pub min_probe_delta: TimeDelta,
    pub loss_limited_scale: f64,
    // Don't send a probe if min(estimate, network state estimate) is larger than
    // this fraction of the set max or max allocated bitrate.
    pub skip_if_est_larger_than_fraction_of_max: f64,
    // Scale factor of the max allocated bitrate. Used when deciding if a probe
    // can be skiped due to that the estimate is already high enough.
    pub skip_max_allocated_scale: f64,
}

impl Default for ProbeControllerConfig {
    fn default() -> Self {
        Self {
            p1: 3.0,
            p2: 6.0,
            step_size: 2.0,
            further_probe_threshold: 0.7,
            abort_further: false,
            initial_probing: TimeDelta::from_seconds(5),
            initial_probe_duration: TimeDelta::from_millis(100),
            initial_min_probe_delta: TimeDelta::from_millis(20),
            alr_interval: TimeDelta::from_seconds(5),
            alr_scale: 2.0,
            network_state_interval: TimeDelta::plus_infinity(),
            est_lower_than_network_ratio: 0.0,
            est_lower_than_network_interval: TimeDelta::from_seconds(3),
            network_state_scale: 1.0,
            network_state_probe_duration: TimeDelta::from_millis(15),
            network_state_min_probe_delta: TimeDelta::from_millis(20),
            probe_max_allocation: true,
            alloc_p1: 1.0,
            alloc_p2: 2.0,
            alloc_current_bwe_limit: 2.0,
            min_probe_packets_sent: 5,
            min_probe_duration: TimeDelta::from_millis(15),
            min_probe_delta: TimeDelta::from_millis(2),
            loss_limited_scale: 1.5,
            skip_if_est_larger_than_fraction_of_max: 0.0,
            skip_max_allocated_scale: 1.0,
        }
    }
}

// Reason that bandwidth estimate is limited. Bandwidth estimate can be limited
// by either delay based bwe, or loss based bwe when it increases/decreases the
// estimate.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum BandwidthLimitedCause {
    LossLimitedBweIncreasing = 0,
    LossLimitedBwe = 1,
    DelayBasedLimited = 2,
    DelayBasedLimitedDelayIncreased = 3,
    RttBasedBackOffHighRtt = 4,
}

enum State {
    // Initial state where no probing has been triggered yet.
    Init,
    // Waiting for probing results to continue further probing.
    WaitingForProbingResult,
    // Probing is complete.
    ProbingComplete,
}

// This class controls initiation of probing to estimate initial channel
// capacity. There is also support for probing during a session when max
// bitrate is adjusted by an application.
pub struct ProbeController {
    network_available: bool,
    repeated_initial_probing_enabled: bool,
    last_allowed_repeated_initial_probe: Timestamp,
    bandwidth_limited_cause: BandwidthLimitedCause,
    state: State,
    min_bitrate_to_probe_further: DataRate,
    time_last_probing_initiated: Timestamp,
    estimated_bitrate: DataRate,
    network_estimate: Option<NetworkStateEstimate>,
    start_bitrate: DataRate,
    max_bitrate: DataRate,
    last_bwe_drop_probing_time: Timestamp,
    alr_start_time: Option<Timestamp>,
    alr_end_time: Option<Timestamp>,
    enable_periodic_alr_probing: bool,
    time_of_last_large_drop: Timestamp,
    bitrate_before_last_large_drop: DataRate,
    max_total_allocated_bitrate: DataRate,

    in_rapid_recovery_experiment: bool,
    next_probe_cluster_id: i32,

    config: ProbeControllerConfig,
}

impl Default for ProbeController {
    fn default() -> Self {
        let mut this = Self {
            network_available: false,
            state: State::Init,
            min_bitrate_to_probe_further: DataRate::plus_infinity(),
            time_last_probing_initiated: Timestamp::minus_infinity(),
            estimated_bitrate: DataRate::zero(),
            network_estimate: None,
            start_bitrate: DataRate::zero(),
            max_bitrate: DataRate::plus_infinity(),
            last_bwe_drop_probing_time: Timestamp::zero(),
            alr_start_time: None,
            alr_end_time: None,
            enable_periodic_alr_probing: false,
            time_of_last_large_drop: Timestamp::minus_infinity(),
            bitrate_before_last_large_drop: DataRate::zero(),
            max_total_allocated_bitrate: DataRate::zero(),
            in_rapid_recovery_experiment: false,
            next_probe_cluster_id: 1,
            config: ProbeControllerConfig::default(),
            repeated_initial_probing_enabled: false,
            last_allowed_repeated_initial_probe: Timestamp::minus_infinity(),
            bandwidth_limited_cause: BandwidthLimitedCause::DelayBasedLimited,
        };
        this.reset(Timestamp::zero());
        this
    }
}

impl ProbeController {
    // the measured results back.
    const MAX_WAITING_TIME_FOR_PROBING_RESULT: TimeDelta = TimeDelta::from_seconds(1);

    // Default probing bitrate limit. Applied only when the application didn't
    // specify max bitrate.
    const DEFAULT_MAX_PROBING_BITRATE: DataRate = DataRate::from_kilobits_per_sec(5000);

    // If the bitrate drops to a factor `kBitrateDropThreshold` or lower
    // and we recover within `kBitrateDropTimeoutMs`, then we'll send
    // a probe at a fraction `kProbeFractionAfterDrop` of the original bitrate.
    const BITRATE_DROP_THRESHOLD: f64 = 0.66;
    const BITRATE_DROP_TIMEOUT: TimeDelta = TimeDelta::from_seconds(5);
    const PROBE_FRACTION_AFTER_DROP: f64 = 0.85;

    // Timeout for probing after leaving ALR. If the bitrate drops significantly,
    // (as determined by the delay based estimator) and we leave ALR, then we will
    // send a probe if we recover within `kLeftAlrTimeoutMs` ms.
    const ALR_ENDED_TIMEOUT: TimeDelta = TimeDelta::from_seconds(3);

    // The expected uncertainty of probe result (as a fraction of the target probe
    // This is a limit on how often probing can be done when there is a BW
    // drop detected in ALR.
    const MIN_TIME_BETWEEN_ALR_PROBES: TimeDelta = TimeDelta::from_seconds(5);

    // bitrate). Used to avoid probing if the probe bitrate is close to our current
    // estimate.
    const PROBE_UNCERTAINTY: f64 = 0.05;

    pub fn new(config: ProbeControllerConfig) -> Self {
        Self {
            config,
            ..Default::default()
        }
    }

    pub fn set_bitrates(
        &mut self,
        min_bitrate: DataRate,
        start_bitrate: DataRate,
        max_bitrate: DataRate,
        at_time: Timestamp,
    ) -> Vec<ProbeClusterConfig> {
        if start_bitrate > DataRate::zero() {
            self.start_bitrate = start_bitrate;
            self.estimated_bitrate = start_bitrate;
        } else if self.start_bitrate.is_zero() {
            self.start_bitrate = min_bitrate;
        }

        // The reason we use the variable `old_max_bitrate_pbs` is because we
        // need to set `max_bitrate_` before we call InitiateProbing.
        let old_max_bitrate: DataRate = self.max_bitrate;
        self.max_bitrate = if max_bitrate.is_finite() {
            max_bitrate
        } else {
            Self::DEFAULT_MAX_PROBING_BITRATE
        };

        match self.state {
            State::Init => {
                if self.network_available {
                    return self.initiate_exponential_probing(at_time);
                }
            }
            State::WaitingForProbingResult => (),
            State::ProbingComplete => {
                // If the new max bitrate is higher than both the old max bitrate and the
                // estimate then initiate probing.
                if !self.estimated_bitrate.is_zero()
                    && old_max_bitrate < self.max_bitrate
                    && self.estimated_bitrate < self.max_bitrate
                {
                    return self.initiate_probing(at_time, &[self.max_bitrate], false);
                }
            }
        }
        vec![]
    }

    // The total bitrate, as opposed to the max bitrate, is the sum of the
    // configured bitrates for all active streams.

    pub fn on_max_total_allocated_bitrate(
        &mut self,
        max_total_allocated_bitrate: DataRate,
        at_time: Timestamp,
    ) -> Vec<ProbeClusterConfig> {
        let in_alr: bool = self.alr_start_time.is_some();
        let allow_allocation_probe: bool = in_alr;
        if self.config.probe_max_allocation
            && matches!(self.state, State::ProbingComplete)
            && max_total_allocated_bitrate != self.max_total_allocated_bitrate
            && self.estimated_bitrate < self.max_bitrate
            && self.estimated_bitrate < max_total_allocated_bitrate
            && allow_allocation_probe
        {
            self.max_total_allocated_bitrate = max_total_allocated_bitrate;

            if self.config.alloc_p1 == 0.0 {
                return vec![];
            }

            let mut first_probe_rate: DataRate = max_total_allocated_bitrate * self.config.alloc_p1;
            let current_bwe_limit: DataRate =
                self.config.alloc_current_bwe_limit * self.estimated_bitrate;
            let mut limited_by_current_bwe: bool = current_bwe_limit < first_probe_rate;
            if limited_by_current_bwe {
                first_probe_rate = current_bwe_limit;
            }

            let mut probes: Vec<DataRate> = vec![first_probe_rate];
            if self.config.alloc_p2 > 0.0 && !limited_by_current_bwe {
                let mut second_probe_rate: DataRate =
                    max_total_allocated_bitrate * self.config.alloc_p2;
                limited_by_current_bwe = current_bwe_limit < second_probe_rate;
                if limited_by_current_bwe {
                    second_probe_rate = current_bwe_limit;
                }
                if second_probe_rate > first_probe_rate {
                    probes.push(second_probe_rate);
                }
            }

            let allow_further_probing: bool = limited_by_current_bwe;

            return self.initiate_probing(at_time, &probes, allow_further_probing);
        }
        if !max_total_allocated_bitrate.is_zero() {
            self.last_allowed_repeated_initial_probe = at_time;
        }

        self.max_total_allocated_bitrate = max_total_allocated_bitrate;
        vec![]
    }

    pub fn on_network_availability(&mut self, msg: NetworkAvailability) -> Vec<ProbeClusterConfig> {
        self.network_available = msg.network_available;

        if !self.network_available && matches!(self.state, State::WaitingForProbingResult) {
            self.state = State::ProbingComplete;
            self.min_bitrate_to_probe_further = DataRate::plus_infinity();
        }

        if self.network_available
            && matches!(self.state, State::Init)
            && !self.start_bitrate.is_zero()
        {
            return self.initiate_exponential_probing(msg.at_time);
        }
        vec![]
    }

    pub fn set_estimated_bitrate(
        &mut self,
        bitrate: DataRate,
        bandwidth_limited_cause: BandwidthLimitedCause,
        at_time: Timestamp,
    ) -> Vec<ProbeClusterConfig> {
        self.bandwidth_limited_cause = bandwidth_limited_cause;
        if bitrate < Self::BITRATE_DROP_THRESHOLD * self.estimated_bitrate {
            self.time_of_last_large_drop = at_time;
            self.bitrate_before_last_large_drop = self.estimated_bitrate;
        }
        self.estimated_bitrate = bitrate;

        if matches!(self.state, State::WaitingForProbingResult) {
            // Continue probing if probing results indicate channel has greater
            // capacity unless we already reached the needed bitrate.
            if self.config.abort_further
                && (bitrate > self.max_bitrate
                    || (!self.max_total_allocated_bitrate.is_zero()
                        && bitrate > 2 * self.max_total_allocated_bitrate))
            {
                // No need to continue probing.
                self.min_bitrate_to_probe_further = DataRate::plus_infinity();
            }
            let network_state_estimate_probe_further_limit: DataRate = match self.network_estimate {
                Some(network_estimate)
                    if self.config.network_state_interval.is_finite()
                        && network_estimate.link_capacity_upper.is_finite() =>
                {
                    network_estimate.link_capacity_upper * self.config.further_probe_threshold
                }
                _ => DataRate::plus_infinity(),
            };
            tracing::debug!(
                "Measured bitrate: {:?} Minimum to probe further: {:?} upper limit: {:?}",
                bitrate,
                self.min_bitrate_to_probe_further,
                network_state_estimate_probe_further_limit
            );

            if bitrate > self.min_bitrate_to_probe_further
                && bitrate <= network_state_estimate_probe_further_limit
            {
                return self.initiate_probing(at_time, &[self.config.step_size * bitrate], true);
            }
        }
        vec![]
    }

    pub fn enable_periodic_alr_probing(&mut self, enable: bool) {
        self.enable_periodic_alr_probing = enable;
    }

    // Probes are sent periodically every 1s during the first 5s after the network
    // becomes available or until OnMaxTotalAllocatedBitrate is invoked with a
    // none zero max_total_allocated_bitrate (there are active streams being
    // sent.) Probe rate is up to max configured bitrate configured via
    // SetBitrates.
    pub fn enable_repeated_initial_probing(&mut self, enable: bool) {
        self.repeated_initial_probing_enabled = enable;
    }

    pub fn set_alr_start_time_ms(&mut self, alr_start_time: Option<i64>) {
        self.alr_start_time = alr_start_time.map(Timestamp::from_millis);
    }
    pub fn set_alr_ended_time_ms(&mut self, alr_end_time: i64) {
        self.alr_end_time = Some(Timestamp::from_millis(alr_end_time));
    }

    pub fn request_probe(&mut self, at_time: Timestamp) -> Vec<ProbeClusterConfig> {
        // Called once we have returned to normal state after a large drop in
        // estimated bandwidth. The current response is to initiate a single probe
        // session (if not already probing) at the previous bitrate.
        //
        // If the probe session fails, the assumption is that this drop was a
        // real one from a competing flow or a network change.
        let in_alr: bool = self.alr_start_time.is_some();
        let alr_ended_recently: bool = match self.alr_end_time {
            Some(alr_end_time) => at_time - alr_end_time < Self::ALR_ENDED_TIMEOUT,
            _ => false,
        };

        if (in_alr || alr_ended_recently || self.in_rapid_recovery_experiment)
            && matches!(self.state, State::ProbingComplete)
        {
            let suggested_probe: DataRate =
                Self::PROBE_FRACTION_AFTER_DROP * self.bitrate_before_last_large_drop;
            let min_expected_probe_result: DataRate =
                (1.0 - Self::PROBE_UNCERTAINTY) * suggested_probe;
            let time_since_drop: TimeDelta = at_time - self.time_of_last_large_drop;
            let time_since_probe: TimeDelta = at_time - self.last_bwe_drop_probing_time;
            if min_expected_probe_result > self.estimated_bitrate
                && time_since_drop < Self::BITRATE_DROP_TIMEOUT
                && time_since_probe > Self::MIN_TIME_BETWEEN_ALR_PROBES
            {
                tracing::debug!("Detected big bandwidth drop, start probing.");
                // Track how often we probe in response to bandwidth drop in ALR.
                self.last_bwe_drop_probing_time = at_time;
                return self.initiate_probing(at_time, &[suggested_probe], false);
            }
        }
        vec![]
    }

    pub fn set_network_state_estimate(&mut self, estimate: NetworkStateEstimate) {
        self.network_estimate = Some(estimate);
    }

    // Resets the ProbeController to a state equivalent to as if it was just
    // created EXCEPT for configuration settings like
    // `enable_periodic_alr_probing_` `network_available_` and
    // `max_total_allocated_bitrate_`.
    pub fn reset(&mut self, at_time: Timestamp) {
        self.bandwidth_limited_cause = BandwidthLimitedCause::DelayBasedLimited;
        self.state = State::Init;
        self.min_bitrate_to_probe_further = DataRate::plus_infinity();
        self.time_last_probing_initiated = Timestamp::zero();
        self.estimated_bitrate = DataRate::zero();
        self.network_estimate = None;
        self.start_bitrate = DataRate::zero();
        self.max_bitrate = Self::DEFAULT_MAX_PROBING_BITRATE;
        let now: Timestamp = at_time;
        self.last_bwe_drop_probing_time = now;
        self.alr_end_time.take();
        self.time_of_last_large_drop = now;
        self.bitrate_before_last_large_drop = DataRate::zero();
    }

    pub fn process(&mut self, at_time: Timestamp) -> Vec<ProbeClusterConfig> {
        if at_time - self.time_last_probing_initiated > Self::MAX_WAITING_TIME_FOR_PROBING_RESULT
            && matches!(self.state, State::WaitingForProbingResult)
        {
            tracing::debug!("WaitingForProbingResult: timeout");
            self.update_state(State::ProbingComplete);
        }
        if self.estimated_bitrate.is_zero() || !matches!(self.state, State::ProbingComplete) {
            return vec![];
        }
        if self.time_for_next_repeated_initial_probe(at_time) {
            return self.initiate_probing(
                at_time,
                &[self.estimated_bitrate * self.config.p1],
                true,
            );
        }
        if self.time_for_alr_probe(at_time) || self.time_for_network_state_probe(at_time) {
            return self.initiate_probing(
                at_time,
                &[self.estimated_bitrate * self.config.alr_scale],
                true,
            );
        }
        vec![]
    }

    fn update_state(&mut self, new_state: State) {
        match new_state {
            State::Init => {
                self.state = State::Init;
            }
            State::WaitingForProbingResult => {
                self.state = State::WaitingForProbingResult;
            }
            State::ProbingComplete => {
                self.state = State::ProbingComplete;
                self.min_bitrate_to_probe_further = DataRate::plus_infinity();
            }
        }
    }
    fn initiate_exponential_probing(&mut self, at_time: Timestamp) -> Vec<ProbeClusterConfig> {
        assert!(self.network_available);
        assert!(matches!(self.state, State::Init));
        assert!(self.start_bitrate > DataRate::zero());

        // When probing at 1.8 Mbps ( 6x 300), this represents a threshold of
        // 1.2 Mbps to continue probing.
        let mut probes: Vec<DataRate> = vec![self.config.p1 * self.start_bitrate];
        if self.config.p2 > 0.0 {
            probes.push(self.config.p2 * self.start_bitrate);
        }

        if self.repeated_initial_probing_enabled && self.max_total_allocated_bitrate.is_zero() {
            self.last_allowed_repeated_initial_probe = at_time + self.config.initial_probing;
            tracing::debug!(
                "Repeated initial probing enabled, last allowed probe: {:?} now: {:?}",
                self.last_allowed_repeated_initial_probe,
                at_time
            );
        }

        self.initiate_probing(at_time, &probes, true)
    }
    fn initiate_probing(
        &mut self,
        now: Timestamp,
        bitrates_to_probe: &[DataRate],
        mut probe_further: bool,
    ) -> Vec<ProbeClusterConfig> {
        if self.config.skip_if_est_larger_than_fraction_of_max > 0.0 {
            let network_estimate: DataRate = self
                .network_estimate
                .map(|x| x.link_capacity_upper)
                .unwrap_or(DataRate::plus_infinity());
            let max_probe_rate: DataRate = if self.max_total_allocated_bitrate.is_zero() {
                self.max_bitrate
            } else {
                std::cmp::min(
                    self.config.skip_max_allocated_scale * self.max_total_allocated_bitrate,
                    self.max_bitrate,
                )
            };
            if std::cmp::min(network_estimate, self.estimated_bitrate)
                > self.config.skip_if_est_larger_than_fraction_of_max * max_probe_rate
            {
                self.update_state(State::ProbingComplete);
                return vec![];
            }
        }

        let mut max_probe_bitrate: DataRate = self.max_bitrate;
        if self.max_total_allocated_bitrate > DataRate::zero() {
            // If a max allocated bitrate has been configured, allow probing up to 2x
            // that rate. This allows some overhead to account for bursty streams,
            // which otherwise would have to ramp up when the overshoot is already in
            // progress.
            // It also avoids minor quality reduction caused by probes often being
            // received at slightly less than the target probe bitrate.
            max_probe_bitrate =
                std::cmp::min(max_probe_bitrate, self.max_total_allocated_bitrate * 2);
        }

        match self.bandwidth_limited_cause {
            BandwidthLimitedCause::RttBasedBackOffHighRtt
            | BandwidthLimitedCause::DelayBasedLimitedDelayIncreased
            | BandwidthLimitedCause::LossLimitedBwe => {
                tracing::debug!(
                    "Not sending probe in bandwidth limited state. {:?}",
                    self.bandwidth_limited_cause
                );
                return vec![];
            }
            BandwidthLimitedCause::LossLimitedBweIncreasing => {
                max_probe_bitrate = std::cmp::min(
                    max_probe_bitrate,
                    self.estimated_bitrate * self.config.loss_limited_scale,
                );
            }
            _ => (),
        };

        match self.network_estimate {
            Some(network_estimate) if network_estimate.link_capacity_upper.is_finite() => {
                if self.config.network_state_interval.is_finite() {
                    if network_estimate.link_capacity_upper.is_zero() {
                        tracing::debug!("Not sending probe, Network state estimate is zero");
                        return vec![];
                    }
                    max_probe_bitrate = std::cmp::min(
                        max_probe_bitrate,
                        std::cmp::max(
                            self.estimated_bitrate,
                            network_estimate.link_capacity_upper * self.config.network_state_scale,
                        ),
                    );
                }
            }
            _ => (),
        };

        let mut pending_probes: Vec<ProbeClusterConfig> = vec![];
        for mut bitrate in bitrates_to_probe.iter().cloned() {
            assert!(!bitrate.is_zero());
            if bitrate >= max_probe_bitrate {
                bitrate = max_probe_bitrate;
                probe_further = false;
            }
            pending_probes.push(self.create_probe_cluster_config(now, bitrate));
        }
        self.time_last_probing_initiated = now;
        if probe_further {
            self.update_state(State::WaitingForProbingResult);
            // Dont expect probe results to be larger than a fraction of the actual
            // probe rate.
            self.min_bitrate_to_probe_further = pending_probes.last().unwrap().target_data_rate
                * self.config.further_probe_threshold;
        } else {
            self.update_state(State::ProbingComplete);
        }
        pending_probes
    }
    fn time_for_alr_probe(&self, at_time: Timestamp) -> bool {
        if let Some(alr_start_time) = self.alr_start_time {
            if self.enable_periodic_alr_probing {
                let next_probe_time: Timestamp =
                    std::cmp::max(alr_start_time, self.time_last_probing_initiated)
                        + self.config.alr_interval;
                return at_time >= next_probe_time;
            }
        }
        false
    }
    fn time_for_network_state_probe(&self, at_time: Timestamp) -> bool {
        let network_estimate = match self.network_estimate.as_ref() {
            Some(network_estimate) if !network_estimate.link_capacity_upper.is_infinite() => {
                network_estimate
            }
            _ => return false,
        };

        let probe_due_to_low_estimate: bool = self.bandwidth_limited_cause
            == BandwidthLimitedCause::DelayBasedLimited
            && self.estimated_bitrate
                < self.config.est_lower_than_network_ratio * network_estimate.link_capacity_upper;
        if probe_due_to_low_estimate && self.config.est_lower_than_network_interval.is_finite() {
            let next_probe_time: Timestamp =
                self.time_last_probing_initiated + self.config.est_lower_than_network_interval;
            return at_time >= next_probe_time;
        }

        let periodic_probe: bool = self.estimated_bitrate < network_estimate.link_capacity_upper;
        if periodic_probe && self.config.network_state_interval.is_finite() {
            let next_probe_time: Timestamp =
                self.time_last_probing_initiated + self.config.network_state_interval;
            return at_time >= next_probe_time;
        }

        false
    }
    fn time_for_next_repeated_initial_probe(&self, at_time: Timestamp) -> bool {
        if !matches!(self.state, State::WaitingForProbingResult)
            && self.last_allowed_repeated_initial_probe > at_time
        {
            let next_probe_time: Timestamp =
                self.time_last_probing_initiated + Self::MAX_WAITING_TIME_FOR_PROBING_RESULT;
            if at_time >= next_probe_time {
                return true;
            }
        }
        false
    }
    fn create_probe_cluster_config(
        &mut self,
        at_time: Timestamp,
        bitrate: DataRate,
    ) -> ProbeClusterConfig {
        let mut config = ProbeClusterConfig {
            id: self.next_probe_cluster_id,
            at_time,
            target_data_rate: bitrate,
            target_probe_count: self.config.min_probe_packets_sent,
            ..Default::default()
        };

        self.next_probe_cluster_id += 1;

        match self.network_estimate {
            Some(network_estimate)
                if network_estimate.link_capacity_upper.is_finite()
                    && network_estimate.link_capacity_upper >= bitrate =>
            {
                config.target_duration = self.config.network_state_probe_duration;
                config.min_probe_delta = self.config.network_state_min_probe_delta;
            }
            _ => {
                if at_time < self.last_allowed_repeated_initial_probe {
                    config.target_duration = self.config.initial_probe_duration;
                    config.min_probe_delta = self.config.initial_min_probe_delta;
                } else {
                    config.target_duration = self.config.min_probe_duration;
                    config.min_probe_delta = self.config.min_probe_delta;
                }
            }
        };

        config
    }
}

#[cfg(test)]
mod test {
    use super::*;
    const MIN_BITRATE: DataRate = DataRate::from_bits_per_sec(100);
    const START_BITRATE: DataRate = DataRate::from_bits_per_sec(300);
    const MAX_BITRATE: DataRate = DataRate::from_bits_per_sec(10000);

    const EXPONENTIAL_PROBING_TIMEOUT: TimeDelta = TimeDelta::from_seconds(5);

    const ALR_PROBE_INTERVAL: TimeDelta = TimeDelta::from_seconds(5);
    const ALR_ENDED_TIMEOUT: TimeDelta = TimeDelta::from_seconds(3);
    const BITRATE_DROP_TIMEOUT: TimeDelta = TimeDelta::from_seconds(5);

    #[test]
    fn initiates_probing_after_set_bitrates() {
        let clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());

        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert!(probes.len() >= 2);
    }

    #[test]
    fn initiates_probing_when_network_available() {
        let clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });

        let probes: Vec<ProbeClusterConfig> =
            probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert!(probes.is_empty());
        let probes = probe_controller.on_network_availability(NetworkAvailability {
            network_available: true,
            ..Default::default()
        });
        assert!(probes.len() >= 2);
    }

    #[test]
    fn sets_default_target_duration_and_target_probe_count() {
        let clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        let probes: Vec<ProbeClusterConfig> =
            probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert!(probes.len() >= 2);

        assert_eq!(probes[0].target_duration, TimeDelta::from_millis(15));
        assert_eq!(probes[0].target_probe_count, 5);
    }

    #[test]
    fn field_trials_override_default_target_duration_and_target_probe_count() {
        let clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            min_probe_packets_sent: 2,
            min_probe_duration: TimeDelta::from_millis(123),
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        let probes: Vec<ProbeClusterConfig> =
            probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert!(probes.len() >= 2);

        assert_eq!(probes[0].target_duration, TimeDelta::from_millis(123));
        assert_eq!(probes[0].target_probe_count, 2);
    }

    #[test]
    fn probe_only_when_network_is_up() {
        let clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });
        probe_controller.on_network_availability(NetworkAvailability {
            at_time: clock,
            network_available: false,
        });
        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert!(probes.is_empty());
        let probes = probe_controller.on_network_availability(NetworkAvailability {
            at_time: clock,
            network_available: true,
        });
        assert!(probes.len() >= 2);
    }

    #[test]
    fn can_configure_initial_probe_rate_factor() {
        let clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            p1: 2.0,
            p2: 3.0,
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert_eq!(probes.len(), 2);
        assert_eq!(probes[0].target_data_rate, START_BITRATE * 2);
        assert_eq!(probes[1].target_data_rate, START_BITRATE * 3);
    }

    #[test]
    fn disable_second_initial_probe_if_rate_factorzero() {
        let clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            p1: 2.0,
            p2: 0.0,
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].target_data_rate, START_BITRATE * 2);
    }

    #[test]
    fn initiates_probing_on_max_bitrate_increase() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        // Long enough to time out exponential probing.
        clock += EXPONENTIAL_PROBING_TIMEOUT;
        probe_controller.set_estimated_bitrate(
            START_BITRATE,
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        probe_controller.process(clock);
        let probes = probe_controller.set_bitrates(
            MIN_BITRATE,
            START_BITRATE,
            MAX_BITRATE + DataRate::from_bits_per_sec(100),
            clock,
        );
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].target_data_rate.bps(), MAX_BITRATE.bps() + 100);
    }

    #[test]
    fn probes_on_max_allocated_bitrate_increase_only_when_in_alr() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        probe_controller.set_estimated_bitrate(
            MAX_BITRATE - DataRate::from_bits_per_sec(1),
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );

        // Wait long enough to time out exponential probing.
        clock += EXPONENTIAL_PROBING_TIMEOUT;
        let probes = probe_controller.process(clock);
        assert!(probes.is_empty());

        // Probe when in alr.
        probe_controller.set_alr_start_time_ms(Some(clock.ms()));
        let probes = probe_controller
            .on_max_total_allocated_bitrate(MAX_BITRATE + DataRate::from_bits_per_sec(1), clock);
        assert_eq!(probes.len(), 2);
        assert_eq!(probes[0].target_data_rate, MAX_BITRATE);

        // Do not probe when not in alr.
        probe_controller.set_alr_start_time_ms(None);
        let probes = probe_controller
            .on_max_total_allocated_bitrate(MAX_BITRATE + DataRate::from_bits_per_sec(2), clock);
        assert!(probes.is_empty());
    }

    #[test]
    fn probes_on_max_allocated_bitrate_limited_by_current_bwe() {
        let mut clock: Timestamp = Timestamp::zero();

        assert!(MAX_BITRATE > 1.5 * START_BITRATE);
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        probe_controller.set_estimated_bitrate(
            START_BITRATE,
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );

        // Wait long enough to time out exponential probing.
        clock += EXPONENTIAL_PROBING_TIMEOUT;
        let probes = probe_controller.process(clock);
        assert!(probes.is_empty());

        // Probe when in alr.
        probe_controller.set_alr_start_time_ms(Some(clock.ms()));
        let probes = probe_controller.on_max_total_allocated_bitrate(MAX_BITRATE, clock);
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].target_data_rate, 2.0 * START_BITRATE);

        // Continue probing if probe succeeds.
        let probes = probe_controller.set_estimated_bitrate(
            1.5 * START_BITRATE,
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        assert_eq!(probes.len(), 1);
        assert!(probes[0].target_data_rate > 1.5 * START_BITRATE);
    }

    #[test]
    fn can_disable_probing_on_max_total_allocated_bitrate_increase() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            probe_max_allocation: false,
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        probe_controller.set_estimated_bitrate(
            MAX_BITRATE - DataRate::from_bits_per_sec(1),
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        clock += EXPONENTIAL_PROBING_TIMEOUT;
        let probes = probe_controller.process(clock);
        assert!(probes.is_empty());
        probe_controller.set_alr_start_time_ms(Some(clock.ms()));

        // Do no probe, since probe_max_allocation:false.
        probe_controller.set_alr_start_time_ms(Some(clock.ms()));
        let probes = probe_controller
            .on_max_total_allocated_bitrate(MAX_BITRATE + DataRate::from_bits_per_sec(1), clock);
        assert!(probes.is_empty());
    }

    #[test]
    fn initiates_probing_on_max_bitrate_increase_at_max_bitrate() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        // Long enough to time out exponential probing.
        clock += EXPONENTIAL_PROBING_TIMEOUT;
        probe_controller.set_estimated_bitrate(
            START_BITRATE,
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        probe_controller.process(clock);
        probe_controller.set_estimated_bitrate(
            MAX_BITRATE,
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        let probes = probe_controller.set_bitrates(
            MIN_BITRATE,
            START_BITRATE,
            MAX_BITRATE + DataRate::from_bits_per_sec(100),
            clock,
        );
        assert_eq!(probes.len(), 1);
        assert_eq!(
            probes[0].target_data_rate,
            MAX_BITRATE + DataRate::from_bits_per_sec(100)
        );
    }

    #[test]
    fn test_exponential_probing() {
        let clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);

        // Repeated probe should only be sent when estimated bitrate climbs above
        // 0.7 * 6 * StartBitrate = 1260.
        let probes = probe_controller.set_estimated_bitrate(
            DataRate::from_bits_per_sec(1000),
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        assert!(probes.is_empty());

        let probes = probe_controller.set_estimated_bitrate(
            DataRate::from_bits_per_sec(1800),
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].target_data_rate.bps(), 2 * 1800);
    }

    #[test]
    fn exponential_probing_stop_if_max_bitrate_low() {
        let clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            abort_further: true,
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert!(!probes.is_empty());

        // Repeated probe normally is sent when estimated bitrate climbs above
        // 0.7 * 6 * StartBitrate = 1260. But since max bitrate is low, expect
        // exponential probing to stop.
        let probes = probe_controller.set_bitrates(
            MIN_BITRATE,
            START_BITRATE,
            /*max_bitrate=*/ START_BITRATE,
            clock,
        );
        assert!(probes.is_empty());

        let probes = probe_controller.set_estimated_bitrate(
            DataRate::from_bits_per_sec(1800),
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        assert!(probes.is_empty());
    }

    #[test]
    fn exponential_probing_stop_if_max_allocated_bitrate_low() {
        let clock = Timestamp::zero();
        let mut probe_controller: ProbeController = ProbeController::new(ProbeControllerConfig {
            abort_further: true,
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert!(!probes.is_empty());

        // Repeated probe normally is sent when estimated bitrate climbs above
        // 0.7 * 6 * StartBitrate = 1260. But since allocated bitrate i slow, expect
        // exponential probing to stop.
        let probes = probe_controller.on_max_total_allocated_bitrate(START_BITRATE, clock);
        assert!(probes.is_empty());

        let probes = probe_controller.set_estimated_bitrate(
            DataRate::from_bits_per_sec(1800),
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        assert!(probes.is_empty());
    }

    #[test]
    fn initial_probing_to_low_max_allocatedbitrate() {
        let clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert!(!probes.is_empty());

        // Repeated probe is sent when estimated bitrate climbs above
        // 0.7 * 6 * StartBitrate = 1260.
        let probes = probe_controller.on_max_total_allocated_bitrate(START_BITRATE, clock);
        assert!(probes.is_empty());

        // If the inital probe result is received, a new probe is sent at 2x the
        // needed max bitrate.
        let probes = probe_controller.set_estimated_bitrate(
            DataRate::from_bits_per_sec(1800),
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].target_data_rate.bps(), 2 * START_BITRATE.bps());
    }

    #[test]
    fn initial_probing_timeout() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert!(!probes.is_empty());
        // Advance far enough to cause a time out in waiting for probing result.
        clock += EXPONENTIAL_PROBING_TIMEOUT;
        let probes = probe_controller.process(clock);
        assert!(probes.is_empty());
        let probes = probe_controller.set_estimated_bitrate(
            DataRate::from_bits_per_sec(1800),
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        assert!(probes.is_empty());
    }

    #[test]
    fn repeated_initial_probing_sends_new_probe_after_timeout() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });
        probe_controller.enable_repeated_initial_probing(true);
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert!(!probes.is_empty());
        let start_time: Timestamp = clock;
        let mut last_probe_time: Timestamp = clock;
        while clock < start_time + TimeDelta::from_seconds(5) {
            clock += TimeDelta::from_millis(100);
            let probes = probe_controller.process(clock);
            if !probes.is_empty() {
                // Expect a probe every second.
                assert_eq!(clock - last_probe_time, TimeDelta::from_seconds_float(1.1));
                assert_eq!(probes[0].min_probe_delta, TimeDelta::from_millis(20));
                assert_eq!(probes[0].target_duration, TimeDelta::from_millis(100));
                last_probe_time = clock;
            } else {
                assert!(clock - last_probe_time < TimeDelta::from_seconds_float(1.1));
            }
        }
        clock += TimeDelta::from_seconds(1);
        // After 5s, repeated initial probing stops.
        assert!(probe_controller.process(clock).is_empty());
    }

    #[test]
    fn repeated_initial_probing_stop_if_max_allocated_bitrate_set() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });
        probe_controller.enable_repeated_initial_probing(true);
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert!(!probes.is_empty());

        clock += TimeDelta::from_millis(1100);
        let probes = probe_controller.process(clock);
        assert_eq!(probes.len(), 1);
        probe_controller.on_max_total_allocated_bitrate(MIN_BITRATE, clock);
        clock += TimeDelta::from_millis(1100);
        let probes = probe_controller.process(clock);
        assert!(probes.is_empty());
    }

    #[test]
    fn request_probe_in_alr() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert!(probes.len() >= 2);
        probe_controller.set_estimated_bitrate(
            DataRate::from_bits_per_sec(500),
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );

        probe_controller.set_alr_start_time_ms(Some(clock.ms()));
        clock += ALR_PROBE_INTERVAL + TimeDelta::from_millis(1);
        probe_controller.process(clock);
        probe_controller.set_estimated_bitrate(
            DataRate::from_bits_per_sec(250),
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        let probes = probe_controller.request_probe(clock);

        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].target_data_rate.bps_float(), 0.85 * 500.0);
    }

    #[test]
    fn request_probe_when_alr_ended_recently() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert_eq!(probes.len(), 2);
        probe_controller.set_estimated_bitrate(
            DataRate::from_bits_per_sec(500),
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );

        probe_controller.set_alr_start_time_ms(None);
        clock += ALR_PROBE_INTERVAL + TimeDelta::from_millis(1);
        probe_controller.process(clock);
        probe_controller.set_estimated_bitrate(
            DataRate::from_bits_per_sec(250),
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        probe_controller.set_alr_ended_time_ms(clock.ms());
        clock += ALR_ENDED_TIMEOUT - TimeDelta::from_millis(1);
        let probes = probe_controller.request_probe(clock);

        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].target_data_rate.bps_float(), 0.85 * 500.0);
    }

    #[test]
    fn request_probe_when_alr_not_ended_recently() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert_eq!(probes.len(), 2);
        probe_controller.set_estimated_bitrate(
            DataRate::from_bits_per_sec(500),
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );

        probe_controller.set_alr_start_time_ms(None);
        clock += ALR_PROBE_INTERVAL + TimeDelta::from_millis(1);
        probe_controller.process(clock);
        probe_controller.set_estimated_bitrate(
            DataRate::from_bits_per_sec(250),
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        probe_controller.set_alr_ended_time_ms(clock.ms());
        clock += ALR_ENDED_TIMEOUT + TimeDelta::from_millis(1);
        let probes = probe_controller.request_probe(clock);
        assert!(probes.is_empty());
    }

    #[test]
    fn request_probe_when_bwe_drop_not_recent() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert_eq!(probes.len(), 2);
        probe_controller.set_estimated_bitrate(
            DataRate::from_bits_per_sec(500),
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );

        probe_controller.set_alr_start_time_ms(Some(clock.ms()));
        clock += ALR_PROBE_INTERVAL + TimeDelta::from_millis(1);
        probe_controller.process(clock);
        probe_controller.set_estimated_bitrate(
            DataRate::from_bits_per_sec(250),
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        clock += BITRATE_DROP_TIMEOUT + TimeDelta::from_millis(1);
        let probes = probe_controller.request_probe(clock);
        assert!(probes.is_empty());
    }

    #[test]
    fn periodic_probing() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        probe_controller.enable_periodic_alr_probing(true);
        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert_eq!(probes.len(), 2);
        probe_controller.set_estimated_bitrate(
            DataRate::from_bits_per_sec(500),
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );

        let start_time: Timestamp = clock;

        // Expect the controller to send a new probe after 5s has passed.
        probe_controller.set_alr_start_time_ms(Some(start_time.ms()));
        clock += TimeDelta::from_seconds(5);
        let probes = probe_controller.process(clock);
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].target_data_rate.bps(), 1000);

        probe_controller.set_estimated_bitrate(
            DataRate::from_bits_per_sec(500),
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );

        // The following probe should be sent at 10s into ALR.
        probe_controller.set_alr_start_time_ms(Some(start_time.ms()));
        clock += TimeDelta::from_seconds(4);
        probe_controller.process(clock);
        let probes = probe_controller.set_estimated_bitrate(
            DataRate::from_bits_per_sec(500),
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        assert!(probes.is_empty());

        probe_controller.set_alr_start_time_ms(Some(start_time.ms()));
        clock += TimeDelta::from_seconds(1);
        let probes = probe_controller.process(clock);
        assert_eq!(probes.len(), 1);
        let probes = probe_controller.set_estimated_bitrate(
            DataRate::from_bits_per_sec(500),
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        assert!(probes.is_empty());
    }

    #[test]
    fn periodic_probing_after_reset() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        let alr_start_time: Timestamp = clock;

        probe_controller.set_alr_start_time_ms(Some(alr_start_time.ms()));
        probe_controller.enable_periodic_alr_probing(true);
        probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        probe_controller.reset(clock);

        clock += TimeDelta::from_seconds(10);
        let probes = probe_controller.process(clock);
        // Since bitrates are not yet set, no probe is sent event though we are in ALR
        // mode.
        assert!(probes.is_empty());

        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert_eq!(probes.len(), 2);

        // Make sure we use `kStartBitrateBps` as the estimated bitrate
        // until SetEstimatedBitrate is called with an updated estimate.
        clock += TimeDelta::from_seconds(10);
        let probes = probe_controller.process(clock);
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].target_data_rate, START_BITRATE * 2);
    }

    #[test]
    fn no_probes_when_transport_is_not_writable() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });
        probe_controller.enable_periodic_alr_probing(true);

        let probes: Vec<ProbeClusterConfig> =
            probe_controller.on_network_availability(NetworkAvailability {
                network_available: false,
                ..Default::default()
            });
        assert!(probes.is_empty());
        assert!(probe_controller
            .set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock)
            .is_empty());
        clock += TimeDelta::from_seconds(10);
        assert!(probe_controller.process(clock).is_empty());

        // Controller is reset after a network route change.
        // But, a probe should not be sent since the transport is not writable.
        // Transport is not writable until after DTLS negotiation completes.
        // However, the bitrate constraints may change.
        probe_controller.reset(clock);
        assert!(probe_controller
            .set_bitrates(2 * MIN_BITRATE, 2 * START_BITRATE, 2 * MAX_BITRATE, clock)
            .is_empty());
        clock += TimeDelta::from_seconds(10);
        assert!(probe_controller.process(clock).is_empty());
    }

    #[test]
    fn test_exponential_probing_overflow() {
        let clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        const MBPS_MULTIPLIER: DataRate = DataRate::from_kilobits_per_sec(1000);
        probe_controller.set_bitrates(
            MIN_BITRATE,
            10 * MBPS_MULTIPLIER,
            100 * MBPS_MULTIPLIER,
            clock,
        );
        // Verify that probe bitrate is capped at the specified max bitrate.
        let probes = probe_controller.set_estimated_bitrate(
            60 * MBPS_MULTIPLIER,
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].target_data_rate, 100 * MBPS_MULTIPLIER);
        // Verify that repeated probes aren't sent.
        let probes = probe_controller.set_estimated_bitrate(
            100 * MBPS_MULTIPLIER,
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        assert!(probes.is_empty());
    }

    #[test]
    fn test_allocated_bitrate_cap() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        const MBPS_MULTIPLIER: DataRate = DataRate::from_kilobits_per_sec(1000);
        const MAX_BITRATE: DataRate = DataRate::from_kilobits_per_sec(100 * 1000);
        probe_controller.set_bitrates(MIN_BITRATE, 10 * MBPS_MULTIPLIER, MAX_BITRATE, clock);

        // Configure ALR for periodic probing.
        probe_controller.enable_periodic_alr_probing(true);
        let alr_start_time: Timestamp = clock;
        probe_controller.set_alr_start_time_ms(Some(alr_start_time.ms()));

        let estimated_bitrate: DataRate = MAX_BITRATE / 10;
        probe_controller.set_estimated_bitrate(
            estimated_bitrate,
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );

        // Set a max allocated bitrate below the current estimate.
        let max_allocated: DataRate = estimated_bitrate - 1 * MBPS_MULTIPLIER;
        let probes = probe_controller.on_max_total_allocated_bitrate(max_allocated, clock);
        assert!(probes.is_empty()); // No probe since lower than current max.

        // Probes such as ALR capped at 2x the max allocation limit.
        clock += TimeDelta::from_seconds(5);
        let probes = probe_controller.process(clock);
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].target_data_rate, 2 * max_allocated);

        // Remove allocation limit.
        assert!(probe_controller
            .on_max_total_allocated_bitrate(DataRate::zero(), clock)
            .is_empty());
        clock += TimeDelta::from_seconds(5);
        let probes = probe_controller.process(clock);
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].target_data_rate, estimated_bitrate * 2);
    }

    #[test]
    fn configurable_probing_field_trial() {
        let mut clock = Timestamp::zero();

        // "WebRTC-Bwe-ProbingConfiguration/p1:2,p2:5,step_size:3,further_probe_threshold:0.8,alloc_p1:2,alloc_current_bwe_limit:1000.0,alloc_p2,min_probe_packets_sent:2/"
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            p1: 2.0,
            p2: 5.0,
            step_size: 3.0,
            further_probe_threshold: 0.8,
            alloc_p1: 2.0,
            alloc_current_bwe_limit: 1000.0,
            alloc_p2: 0.0,
            min_probe_packets_sent: 2,
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());

        let probes = probe_controller.set_bitrates(
            MIN_BITRATE,
            START_BITRATE,
            DataRate::from_kilobits_per_sec(5000),
            clock,
        );
        assert_eq!(probes.len(), 2);
        assert_eq!(probes[0].target_data_rate.bps(), 600);
        assert_eq!(probes[0].target_probe_count, 2);
        assert_eq!(probes[1].target_data_rate.bps(), 1500);
        assert_eq!(probes[1].target_probe_count, 2);

        // Repeated probe should only be sent when estimated bitrate climbs above
        // 0.8 * 5 * StartBitrateBps = 1200.
        let probes = probe_controller.set_estimated_bitrate(
            DataRate::from_bits_per_sec(1100),
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        assert_eq!(probes.len(), 0);

        let probes = probe_controller.set_estimated_bitrate(
            DataRate::from_bits_per_sec(1250),
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].target_data_rate.bps(), 3 * 1250);

        clock += TimeDelta::from_seconds(5);
        probe_controller.process(clock);

        probe_controller.set_alr_start_time_ms(Some(clock.ms()));
        let probes = probe_controller
            .on_max_total_allocated_bitrate(DataRate::from_kilobits_per_sec(200), clock);
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].target_data_rate.bps(), 400_000);
    }

    #[test]
    fn limit_alr_probe_when_loss_based_bwe_limited() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        probe_controller.enable_periodic_alr_probing(true);
        probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        probe_controller.set_estimated_bitrate(
            DataRate::from_bits_per_sec(500),
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        // Expect the controller to send a new probe after 5s has passed.
        probe_controller.set_alr_start_time_ms(Some(clock.ms()));
        clock += TimeDelta::from_seconds(5);
        let probes = probe_controller.process(clock);
        assert_eq!(probes.len(), 1);

        probe_controller.set_estimated_bitrate(
            DataRate::from_bits_per_sec(500),
            BandwidthLimitedCause::LossLimitedBweIncreasing,
            clock,
        );
        clock += TimeDelta::from_seconds(6);
        let probes = probe_controller.process(clock);
        assert_eq!(probes.len(), 1);
        assert_eq!(
            probes[0].target_data_rate,
            1.5 * DataRate::from_bits_per_sec(500)
        );

        probe_controller.set_estimated_bitrate(
            1.5 * DataRate::from_bits_per_sec(500),
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        clock += TimeDelta::from_seconds(6);
        let probes = probe_controller.process(clock);
        assert!(!probes.is_empty());
        assert!(probes[0].target_data_rate > 1.5 * 1.5 * DataRate::from_bits_per_sec(500));
    }

    #[test]
    fn periodic_probe_at_upper_network_state_estimate() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            network_state_interval: TimeDelta::from_seconds(5),
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());

        probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        probe_controller.set_estimated_bitrate(
            DataRate::from_bits_per_sec(5000),
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        // Expect the controller to send a new probe after 5s has passed.
        let state_estimate = NetworkStateEstimate {
            link_capacity_upper: DataRate::from_kilobits_per_sec(6),
            ..Default::default()
        };
        probe_controller.set_network_state_estimate(state_estimate);

        clock += TimeDelta::from_seconds(5);
        let probes = probe_controller.process(clock);
        assert_eq!(probes.len(), 1);
        assert_eq!(
            probes[0].target_data_rate,
            state_estimate.link_capacity_upper
        );
        clock += TimeDelta::from_seconds(5);
        let probes = probe_controller.process(clock);
        assert_eq!(probes.len(), 1);
        assert_eq!(
            probes[0].target_data_rate,
            state_estimate.link_capacity_upper
        );
    }

    #[test]
    fn limit_probe_at_upper_network_state_estimate_if_loss_based_limited() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            network_state_interval: TimeDelta::from_seconds(5),
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());

        probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        probe_controller.set_estimated_bitrate(
            DataRate::from_bits_per_sec(500),
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        // Expect the controller to send a new probe after 5s has passed.
        let state_estimate = NetworkStateEstimate {
            link_capacity_upper: DataRate::from_bits_per_sec(700),
            ..Default::default()
        };
        probe_controller.set_network_state_estimate(state_estimate);
        clock += TimeDelta::from_seconds(5);
        let probes = probe_controller.process(clock);
        assert_eq!(probes.len(), 1);

        probe_controller.set_estimated_bitrate(
            DataRate::from_bits_per_sec(500),
            BandwidthLimitedCause::LossLimitedBweIncreasing,
            clock,
        );
        // Expect the controller to send a new probe after 5s has passed.
        clock += TimeDelta::from_seconds(5);
        let probes = probe_controller.process(clock);
        assert!(!probes.is_empty());
        assert_eq!(probes[0].target_data_rate, DataRate::from_bits_per_sec(700));
    }

    #[test]
    fn alr_probes_limited_by_network_state_estimate() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            network_state_interval: TimeDelta::from_seconds(5),
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        probe_controller.enable_periodic_alr_probing(true);
        probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        probe_controller.set_estimated_bitrate(
            DataRate::from_kilobits_per_sec(6),
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        probe_controller.set_alr_start_time_ms(Some(clock.ms()));

        clock += TimeDelta::from_seconds(5);
        let probes = probe_controller.process(clock);
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].target_data_rate, MAX_BITRATE);

        let state_estimate = NetworkStateEstimate {
            link_capacity_upper: DataRate::from_bits_per_sec(8000),
            ..Default::default()
        };
        probe_controller.set_network_state_estimate(state_estimate);
        clock += TimeDelta::from_seconds(5);
        let probes = probe_controller.process(clock);
        assert_eq!(probes.len(), 1);
        assert_eq!(
            probes[0].target_data_rate,
            state_estimate.link_capacity_upper
        );
    }

    #[test]
    fn can_set_longer_probe_duration_after_network_state_estimate() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            network_state_interval: TimeDelta::from_seconds(5),
            network_state_probe_duration: TimeDelta::from_millis(100),
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());

        probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        let probes = probe_controller.set_estimated_bitrate(
            DataRate::from_kilobits_per_sec(5),
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        assert!(!probes.is_empty());
        assert!(probes[0].target_duration < TimeDelta::from_millis(100));

        let state_estimate = NetworkStateEstimate {
            link_capacity_upper: DataRate::from_kilobits_per_sec(6),
            ..Default::default()
        };
        probe_controller.set_network_state_estimate(state_estimate);
        clock += TimeDelta::from_seconds(5);
        let probes = probe_controller.process(clock);
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].target_duration, TimeDelta::from_millis(100));
    }

    #[test]
    fn probe_in_alr_if_loss_based_increasing() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        probe_controller.enable_periodic_alr_probing(true);
        probe_controller.set_estimated_bitrate(
            START_BITRATE,
            BandwidthLimitedCause::LossLimitedBweIncreasing,
            clock,
        );

        // Wait long enough to time out exponential probing.
        clock += EXPONENTIAL_PROBING_TIMEOUT;
        let probes = probe_controller.process(clock);
        assert!(probes.is_empty());

        // Probe when in alr.
        probe_controller.set_alr_start_time_ms(Some(clock.ms()));
        clock += ALR_PROBE_INTERVAL + TimeDelta::from_millis(1);
        let probes = probe_controller.process(clock);
        assert_eq!(probes.len(), 1);
        assert_eq!(probes[0].target_data_rate, 1.5 * START_BITRATE);
    }

    #[test]
    fn not_probe_when_in_alr_if_loss_based_decreases() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        probe_controller.enable_periodic_alr_probing(true);
        probe_controller.set_estimated_bitrate(
            START_BITRATE,
            BandwidthLimitedCause::LossLimitedBwe,
            clock,
        );

        // Wait long enough to time out exponential probing.
        clock += EXPONENTIAL_PROBING_TIMEOUT;
        let probes = probe_controller.process(clock);
        assert!(probes.is_empty());

        // Not probe in alr when loss based estimate decreases.
        probe_controller.set_alr_start_time_ms(Some(clock.ms()));
        clock += ALR_PROBE_INTERVAL + TimeDelta::from_millis(1);
        let probes = probe_controller.process(clock);
        assert!(probes.is_empty());
    }

    #[test]
    fn not_probe_if_loss_based_increasing_outside_alr() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        probe_controller.enable_periodic_alr_probing(true);
        probe_controller.set_estimated_bitrate(
            START_BITRATE,
            BandwidthLimitedCause::LossLimitedBweIncreasing,
            clock,
        );

        // Wait long enough to time out exponential probing.
        clock += EXPONENTIAL_PROBING_TIMEOUT;
        let probes = probe_controller.process(clock);
        assert!(probes.is_empty());

        probe_controller.set_alr_start_time_ms(None);
        clock += ALR_PROBE_INTERVAL + TimeDelta::from_millis(1);
        let probes = probe_controller.process(clock);
        assert!(probes.is_empty());
    }

    #[test]
    fn probe_further_when_loss_based_is_same_as_delay_based_estimate() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            network_state_interval: TimeDelta::from_seconds(5),
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());

        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert!(!probes.is_empty());

        // Need to wait at least one second before process can trigger a new probe.
        clock += TimeDelta::from_millis(1100);
        let probes = probe_controller.process(clock);
        assert!(probes.is_empty());

        let state_estimate = NetworkStateEstimate {
            link_capacity_upper: 5 * START_BITRATE,
            ..Default::default()
        };
        probe_controller.set_network_state_estimate(state_estimate);
        clock += TimeDelta::from_seconds(5);
        let probes = probe_controller.process(clock);
        assert!(!probes.is_empty());

        let probe_target_rate: DataRate = probes[0].target_data_rate;
        assert!(probe_target_rate < state_estimate.link_capacity_upper);
        // Expect that more probes are sent if BWE is the same as delay based
        // estimate.
        let probes = probe_controller.set_estimated_bitrate(
            probe_target_rate,
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        assert!(!probes.is_empty());
        assert_eq!(probes[0].target_data_rate, 2 * probe_target_rate);
    }

    #[test]
    fn probe_if_estimate_lower_than_network_state_estimate() {
        // Periodic probe every 1 second if estimate is lower than 50% of the
        // NetworkStateEstimate.
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            est_lower_than_network_interval: TimeDelta::from_seconds(1),
            est_lower_than_network_ratio: 0.5,
            // limit_probe_target_rate_to_loss_bwe: true,
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());

        probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        probe_controller.set_estimated_bitrate(
            START_BITRATE,
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        // Need to wait at least one second before process can trigger a new probe.
        clock += TimeDelta::from_millis(1100);
        let probes = probe_controller.process(clock);
        assert!(probes.is_empty());

        let mut state_estimate = NetworkStateEstimate {
            link_capacity_upper: START_BITRATE,
            ..Default::default()
        };
        probe_controller.set_network_state_estimate(state_estimate);
        let probes = probe_controller.process(clock);
        assert!(probes.is_empty());

        state_estimate.link_capacity_upper = START_BITRATE * 3;
        probe_controller.set_network_state_estimate(state_estimate);
        let probes = probe_controller.process(clock);
        assert_eq!(probes.len(), 1);
        assert!(probes[0].target_data_rate > START_BITRATE);

        // If network state not increased, send another probe.
        clock += TimeDelta::from_millis(1100);
        let probes = probe_controller.process(clock);
        assert!(!probes.is_empty());

        // Stop probing if estimate increase. We might probe further here though.
        probe_controller.set_estimated_bitrate(
            2 * START_BITRATE,
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        // No more periodic probes.
        clock += TimeDelta::from_millis(1100);
        let probes = probe_controller.process(clock);
        assert!(probes.is_empty());
    }

    #[test]
    fn dont_probe_further_when_loss_limited() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            network_state_interval: TimeDelta::from_seconds(5),
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());

        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert!(!probes.is_empty());

        // Need to wait at least one second before process can trigger a new probe.
        clock += TimeDelta::from_millis(1100);
        let probes = probe_controller.process(clock);
        assert!(probes.is_empty());

        let state_estimate = NetworkStateEstimate {
            link_capacity_upper: 3 * START_BITRATE,
            ..Default::default()
        };
        probe_controller.set_network_state_estimate(state_estimate);
        clock += TimeDelta::from_seconds(5);
        let probes = probe_controller.process(clock);
        assert!(!probes.is_empty());
        assert!(probes[0].target_data_rate < state_estimate.link_capacity_upper);
        // Expect that no more probes are sent immediately if BWE is loss limited.
        let probes = probe_controller.set_estimated_bitrate(
            probes[0].target_data_rate,
            BandwidthLimitedCause::LossLimitedBwe,
            clock,
        );
        assert!(probes.is_empty());
    }

    #[test]
    fn probe_further_when_delay_based_limited() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            network_state_interval: TimeDelta::from_seconds(5),
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());

        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert!(!probes.is_empty());

        // Need to wait at least one second before process can trigger a new probe.
        clock += TimeDelta::from_millis(1100);
        let probes = probe_controller.process(clock);
        assert!(probes.is_empty());

        let state_estimate = NetworkStateEstimate {
            link_capacity_upper: 3 * START_BITRATE,
            ..Default::default()
        };
        probe_controller.set_network_state_estimate(state_estimate);
        clock += TimeDelta::from_seconds(5);
        let probes = probe_controller.process(clock);
        assert!(!probes.is_empty());
        assert!(probes[0].target_data_rate < state_estimate.link_capacity_upper);
        // Since the probe was successfull, expect to continue probing.
        let probes = probe_controller.set_estimated_bitrate(
            probes[0].target_data_rate,
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        assert!(!probes.is_empty());
        assert_eq!(
            probes[0].target_data_rate,
            state_estimate.link_capacity_upper
        );
    }

    #[test]
    fn probe_after_timeout_if_network_state_estimate_increase_after_probe_sent() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            network_state_interval: TimeDelta::from_seconds(5),
            est_lower_than_network_interval: TimeDelta::from_seconds(3),
            est_lower_than_network_ratio: 0.7,
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert!(!probes.is_empty());
        let mut state_estimate = NetworkStateEstimate {
            link_capacity_upper: 1.2 * probes[0].target_data_rate / 2.0,
            ..Default::default()
        };
        probe_controller.set_network_state_estimate(state_estimate);
        // No immediate further probing since probe result is low.
        let probes = probe_controller.set_estimated_bitrate(
            probes[0].target_data_rate / 2,
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        assert!(probes.is_empty());

        clock += TimeDelta::from_seconds(5);
        let probes = probe_controller.process(clock);
        assert!(!probes.is_empty());
        assert!(probes[0].target_data_rate <= state_estimate.link_capacity_upper);
        // If the network state estimate increase, even before the probe result,
        // expect a new probe after `est_lower_than_network_interval` timeout.
        state_estimate.link_capacity_upper = 3 * START_BITRATE;
        probe_controller.set_network_state_estimate(state_estimate);
        let probes = probe_controller.set_estimated_bitrate(
            probes[0].target_data_rate,
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        assert!(probes.is_empty());

        clock += TimeDelta::from_seconds(3);
        let probes = probe_controller.process(clock);
        assert!(!probes.is_empty());

        // But no more probes if estimate is close to the link capacity.
        let probes = probe_controller.set_estimated_bitrate(
            state_estimate.link_capacity_upper * 0.9,
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        assert!(probes.is_empty());
    }

    #[test]
    fn skip_probe_further_if_already_probed_to_max_rate() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            network_state_interval: TimeDelta::from_seconds(2),
            skip_if_est_larger_than_fraction_of_max: 0.9,
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert!(!probes.is_empty());

        probe_controller.set_network_state_estimate(NetworkStateEstimate {
            link_capacity_upper: 2 * MAX_BITRATE,
            ..Default::default()
        });

        // Attempt to probe up to max rate.
        let probes = probe_controller.set_estimated_bitrate(
            MAX_BITRATE * 0.8,
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        assert!(!probes.is_empty());
        assert_eq!(probes[0].target_data_rate, MAX_BITRATE);

        // If the probe result arrives, dont expect a new probe immediately since we
        // already tried to probe at the max rate.
        let probes = probe_controller.set_estimated_bitrate(
            MAX_BITRATE * 0.8,
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        assert!(probes.is_empty());

        clock += TimeDelta::from_millis(1000);
        let probes = probe_controller.process(clock);
        assert!(probes.is_empty());
        // But when enough time has passed, expect a new probe.
        clock += TimeDelta::from_millis(1000);
        let probes = probe_controller.process(clock);
        assert!(!probes.is_empty());
    }

    #[test]
    fn max_allocated_bitrate_not_reset() {
        let clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert!(!probes.is_empty());

        probe_controller.on_max_total_allocated_bitrate(START_BITRATE / 4, clock);
        probe_controller.reset(clock);

        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert!(!probes.is_empty());
        assert_eq!(probes[0].target_data_rate, START_BITRATE / 4 * 2);
    }

    #[test]
    fn skip_alr_probe_if_estimate_larger_than_max_probe() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            skip_if_est_larger_than_fraction_of_max: 0.9,
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        probe_controller.enable_periodic_alr_probing(true);
        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert!(!probes.is_empty());

        let probes = probe_controller.set_estimated_bitrate(
            MAX_BITRATE,
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        assert!(probes.is_empty());

        probe_controller.set_alr_start_time_ms(Some(clock.ms()));
        clock += TimeDelta::from_seconds(10);
        let probes = probe_controller.process(clock);
        assert!(probes.is_empty());

        // But if the max rate increase, A new probe is sent.
        let probes =
            probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, 2 * MAX_BITRATE, clock);
        assert!(!probes.is_empty());
    }

    #[test]
    fn skip_alr_probe_if_estimate_larger_than_fraction_of_max_allocated() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            skip_if_est_larger_than_fraction_of_max: 1.0,
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        probe_controller.enable_periodic_alr_probing(true);
        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert!(!probes.is_empty());
        probe_controller.set_estimated_bitrate(
            MAX_BITRATE / 2,
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );

        clock += TimeDelta::from_seconds(10);
        probe_controller.set_alr_start_time_ms(Some(clock.ms()));
        let probes = probe_controller.on_max_total_allocated_bitrate(MAX_BITRATE / 2, clock);
        // No probes since total allocated is not higher than the current estimate.
        assert!(probes.is_empty());
        clock += TimeDelta::from_seconds(2);
        let probes = probe_controller.process(clock);
        assert!(probes.is_empty());

        // But if the max allocated increase, A new probe is sent.
        let probes = probe_controller.on_max_total_allocated_bitrate(
            MAX_BITRATE / 2 + DataRate::from_bits_per_sec(1),
            clock,
        );
        assert!(!probes.is_empty());
    }

    #[test]
    fn skip_network_state_probe_if_estimate_larger_than_max_probe() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            network_state_interval: TimeDelta::from_seconds(2),
            skip_if_est_larger_than_fraction_of_max: 0.9,
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert!(!probes.is_empty());

        probe_controller.set_network_state_estimate(NetworkStateEstimate {
            link_capacity_upper: 2 * MAX_BITRATE,
            ..Default::default()
        });
        let probes = probe_controller.set_estimated_bitrate(
            MAX_BITRATE,
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        assert!(probes.is_empty());

        clock += TimeDelta::from_seconds(10);
        let probes = probe_controller.process(clock);
        assert!(probes.is_empty());
    }

    #[test]
    fn sends_probe_if_network_state_estimate_lower_than_max_probe() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            network_state_interval: TimeDelta::from_seconds(2),
            skip_if_est_larger_than_fraction_of_max: 0.9,
            network_state_probe_duration: TimeDelta::from_millis(100),
            network_state_min_probe_delta: TimeDelta::from_millis(20),
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert!(!probes.is_empty());
        probe_controller.set_network_state_estimate(NetworkStateEstimate {
            link_capacity_upper: 2 * MAX_BITRATE,
            ..Default::default()
        });
        let probes = probe_controller.set_estimated_bitrate(
            MAX_BITRATE,
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        assert!(probes.is_empty());

        // Need to wait at least two seconds before process can trigger a new probe.
        clock += TimeDelta::from_millis(2100);

        let probes = probe_controller.set_estimated_bitrate(
            START_BITRATE,
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        assert!(probes.is_empty());
        probe_controller.set_network_state_estimate(NetworkStateEstimate {
            link_capacity_upper: 2 * START_BITRATE,
            ..Default::default()
        });
        let probes = probe_controller.process(clock);
        assert!(!probes.is_empty());
        assert!(probes[0].target_data_rate <= 2 * START_BITRATE);
        // Expect probe durations to be picked from field trial probe target is lower
        // or equal to the network state estimate.
        assert_eq!(probes[0].min_probe_delta, TimeDelta::from_millis(20));
        assert_eq!(probes[0].target_duration, TimeDelta::from_millis(100));
    }

    #[test]
    fn probe_not_limited_by_network_state_esimate_if_lower_thant_current() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            network_state_interval: TimeDelta::from_seconds(5),
            network_state_probe_duration: TimeDelta::from_millis(100),
            network_state_min_probe_delta: TimeDelta::from_millis(20),
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());
        probe_controller.enable_periodic_alr_probing(true);
        probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        probe_controller.set_estimated_bitrate(
            START_BITRATE,
            BandwidthLimitedCause::DelayBasedLimited,
            clock,
        );
        probe_controller.set_network_state_estimate(NetworkStateEstimate {
            link_capacity_upper: START_BITRATE,
            ..Default::default()
        });
        // Need to wait at least one second before process can trigger a new probe.
        clock += TimeDelta::from_millis(1100);
        let probes = probe_controller.process(clock);
        assert!(probes.is_empty());

        probe_controller.set_alr_start_time_ms(Some(clock.ms()));
        probe_controller.set_network_state_estimate(NetworkStateEstimate {
            link_capacity_upper: START_BITRATE / 2,
            ..Default::default()
        });
        clock += TimeDelta::from_seconds(6);
        let probes = probe_controller.process(clock);
        assert!(!probes.is_empty());
        assert_eq!(probes[0].target_data_rate, START_BITRATE);
        // Expect probe durations to be default since network state estimate is lower
        // than the probe rate.
        assert_eq!(probes[0].min_probe_delta, TimeDelta::from_millis(2));
        assert_eq!(probes[0].target_duration, TimeDelta::from_millis(15));
    }

    #[test]
    fn dont_probe_if_delay_increased() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());

        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert!(!probes.is_empty());

        // Need to wait at least one second before process can trigger a new probe.
        clock += TimeDelta::from_millis(1100);
        let probes = probe_controller.process(clock);
        assert!(probes.is_empty());

        let state_estimate = NetworkStateEstimate {
            link_capacity_upper: 3 * START_BITRATE,
            ..Default::default()
        };
        probe_controller.set_network_state_estimate(state_estimate);
        let probes = probe_controller.set_estimated_bitrate(
            START_BITRATE,
            BandwidthLimitedCause::DelayBasedLimitedDelayIncreased,
            clock,
        );
        assert!(probes.is_empty());

        clock += TimeDelta::from_seconds(5);
        let probes = probe_controller.process(clock);
        assert!(probes.is_empty());
    }

    #[test]
    fn dont_probe_if_high_rtt() {
        let mut clock = Timestamp::zero();
        let mut probe_controller = ProbeController::new(ProbeControllerConfig {
            ..Default::default()
        });
        assert!(probe_controller
            .on_network_availability(NetworkAvailability {
                network_available: true,
                ..Default::default()
            })
            .is_empty());

        let probes = probe_controller.set_bitrates(MIN_BITRATE, START_BITRATE, MAX_BITRATE, clock);
        assert!(!probes.is_empty());

        // Need to wait at least one second before process can trigger a new probe.
        clock += TimeDelta::from_millis(1100);
        let probes = probe_controller.process(clock);
        assert!(probes.is_empty());

        let state_estimate = NetworkStateEstimate {
            link_capacity_upper: 3 * START_BITRATE,
            ..Default::default()
        };
        probe_controller.set_network_state_estimate(state_estimate);
        let probes = probe_controller.set_estimated_bitrate(
            START_BITRATE,
            BandwidthLimitedCause::RttBasedBackOffHighRtt,
            clock,
        );
        assert!(probes.is_empty());

        clock += TimeDelta::from_seconds(5);
        let probes = probe_controller.process(clock);
        assert!(probes.is_empty());
    }
}
