/*
 *  Copyright 2021 The WebRTC project authors. All rights reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE file in the root of the source
 *  tree. An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the AUTHORS file in the root of the source tree.
 */

use std::collections::HashMap;

use crate::{
    api::{
        transport::PacketResult,
        units::{DataRate, DataSize, TimeDelta, Timestamp},
    },
    remote_bitrate_estimator::CONGESTION_CONTROLLER_MIN_BITRATE,
};

// State of the loss based estimate, which can be either increasing/decreasing
// when network is loss limited, or equal to the delay based estimate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LossBasedState {
    Increasing = 0,
    // TODO(bugs.webrtc.org/12707): Remove one of the increasing states once we
    // have decided if padding is usefull for ramping up when BWE is loss
    // limited.
    IncreaseUsingPadding = 1,
    Decreasing = 2,
    DelayBasedEstimate = 3,
}
#[derive(Clone, Debug, Copy)]
pub struct LossBasedBweV2Result {
    pub bandwidth_estimate: DataRate,
    // State is used by goog_cc, which later sends probe requests to probe
    // controller if state is Increasing.
    pub state: LossBasedState,
}

impl Default for LossBasedBweV2Result {
    fn default() -> Self {
        Self {
            bandwidth_estimate: DataRate::zero(),
            state: LossBasedState::DelayBasedEstimate,
        }
    }
}

#[derive(Clone, Debug)]
struct ChannelParameters {
    pub inherent_loss: f64,
    pub loss_limited_bandwidth: DataRate,
}

impl Default for ChannelParameters {
    fn default() -> Self {
        Self {
            inherent_loss: 0.0,
            loss_limited_bandwidth: DataRate::minus_infinity(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct LossBasedBweV2Config {
    pub enabled: bool,
    pub bw_rampup_upper_bound_factor: f64,
    pub bw_rampup_upper_bound_in_hold_factor: f64,
    pub bw_rampup_upper_bound_hold_threshold: f64,
    pub bw_rampup_accel_max_factor: f64,
    pub bw_rampup_accel_maxout_time: TimeDelta,
    pub candidate_factors: Vec<f64>,
    pub higher_bw_bias_factor: f64,
    pub higher_log_bw_bias_factor: f64,
    pub inherent_loss_lower_bound: f64,
    pub loss_threshold_of_high_bandwidth_preference: f64,
    pub bandwidth_preference_smoothing_factor: f64,
    pub inherent_loss_upper_bound_bw_balance: DataRate,
    pub inherent_loss_upper_bound_offset: f64,
    pub initial_inherent_loss_estimate: f64,
    pub newton_iterations: usize,
    pub newton_step_size: f64,
    pub acked_rate_candidate: bool,
    pub delay_based_candidate: bool,
    pub upper_bound_candidate_in_alr: bool,
    pub observation_duration_lower_bound: TimeDelta,
    pub observation_window_size: usize,
    pub sending_rate_smoothing_factor: f64,
    pub instant_upper_bound_temporal_weight_factor: f64,
    pub instant_upper_bound_bw_balance: DataRate,
    pub instant_upper_bound_loss_offset: f64,
    pub temporal_weight_factor: f64,
    pub bw_backoff_lower_bound_factor: f64,
    pub max_increase_factor: f64,
    pub delayed_increase_window: TimeDelta,
    pub not_increase_if_inherent_loss_less_than_average_loss: bool,
    pub not_use_acked_rate_in_alr: bool,
    pub use_in_start_phase: bool,
    pub min_num_observations: usize,
    pub lower_bound_by_acked_rate_factor: f64,
    pub hold_duration_factor: f64,
    pub use_byte_loss_rate: bool,
    pub padding_duration: TimeDelta,
    pub bound_best_candidate: bool,
    pub pace_at_loss_based_estimate: bool,
    pub median_sending_rate_factor: f64,
}

impl LossBasedBweV2Config {
    pub fn is_valid(&self) -> bool {
        let mut valid = true;

        if self.bw_rampup_upper_bound_factor <= 1.0 {
            tracing::warn!(
                "The bandwidth rampup upper bound factor must be greater than 1: {}",
                self.bw_rampup_upper_bound_factor
            );
            valid = false;
        }
        if self.bw_rampup_upper_bound_in_hold_factor <= 1.0 {
            tracing::warn!(
                "The bandwidth rampup upper bound factor in hold must be greater than 1: {}",
                self.bw_rampup_upper_bound_in_hold_factor
            );
            valid = false;
        }
        if self.bw_rampup_upper_bound_hold_threshold < 0.0 {
            tracing::warn!(
                "The bandwidth rampup hold threshold must be non-negative: {}",
                self.bw_rampup_upper_bound_hold_threshold
            );
            valid = false;
        }
        if self.bw_rampup_accel_max_factor < 0.0 {
            tracing::warn!(
                "The rampup acceleration max factor must be non-negative: {}",
                self.bw_rampup_accel_max_factor
            );
            valid = false;
        }
        if self.bw_rampup_accel_maxout_time <= TimeDelta::zero() {
            tracing::warn!(
                "The rampup acceleration maxout time must be above zero: {:?}",
                self.bw_rampup_accel_maxout_time
            );
            valid = false;
        }
        for candidate_factor in self.candidate_factors.iter() {
            if *candidate_factor <= 0.0 {
                tracing::warn!(
                    "All candidate factors must be greater than zero: {}",
                    candidate_factor
                );
                valid = false;
            }
        }

        // Ensure that the configuration allows generation of at least one candidate
        // other than the current estimate.
        if !self.acked_rate_candidate
            && !self.delay_based_candidate
            && !self.candidate_factors.iter().any(|cf| *cf != 1.0)
        {
            tracing::warn!("The configuration does not allow generating candidates. Specify a candidate factor other than 1.0, allow the acknowledged rate to be a candidate, and/or allow the delay based estimate to be a candidate.");
            valid = false;
        }

        if self.higher_bw_bias_factor < 0.0 {
            tracing::warn!(
                "The higher bandwidth bias factor must be non-negative: {}",
                self.higher_bw_bias_factor
            );
            valid = false;
        }
        if self.inherent_loss_lower_bound < 0.0 || self.inherent_loss_lower_bound >= 1.0 {
            tracing::warn!(
                "The inherent loss lower bound must be in [0, 1): {}",
                self.inherent_loss_lower_bound
            );
            valid = false;
        }
        if self.loss_threshold_of_high_bandwidth_preference < 0.0
            || self.loss_threshold_of_high_bandwidth_preference >= 1.0
        {
            tracing::warn!(
                "The loss threshold of high bandwidth preference must be in [0, 1): {}",
                self.loss_threshold_of_high_bandwidth_preference
            );
            valid = false;
        }
        if self.bandwidth_preference_smoothing_factor <= 0.0
            || self.bandwidth_preference_smoothing_factor > 1.0
        {
            tracing::warn!(
                "The bandwidth preference smoothing factor must be in (0, 1]: {}",
                self.bandwidth_preference_smoothing_factor
            );
            valid = false;
        }
        if self.inherent_loss_upper_bound_bw_balance <= DataRate::zero() {
            tracing::warn!(
                "The inherent loss upper bound bandwidth balance must be positive: {:?}",
                self.inherent_loss_upper_bound_bw_balance
            );
            valid = false;
        }
        if self.inherent_loss_upper_bound_offset < self.inherent_loss_lower_bound
            || self.inherent_loss_upper_bound_offset >= 1.0
        {
            tracing::warn!("The inherent loss upper bound must be greater than or equal to the inherent loss lower bound, which is {}, and less than 1: {}", self.inherent_loss_lower_bound, self.inherent_loss_upper_bound_offset);
            valid = false;
        }
        if self.initial_inherent_loss_estimate < 0.0 || self.initial_inherent_loss_estimate >= 1.0 {
            tracing::warn!(
                "The initial inherent loss estimate must be in [0, 1): {}",
                self.initial_inherent_loss_estimate
            );
            valid = false;
        }
        if self.newton_iterations == 0 {
            tracing::warn!(
                "The number of Newton iterations must be positive: {}",
                self.newton_iterations
            );
            valid = false;
        }
        if self.newton_step_size <= 0.0 {
            tracing::warn!(
                "The Newton step size must be positive: {}",
                self.newton_step_size
            );
            valid = false;
        }
        if self.observation_duration_lower_bound <= TimeDelta::zero() {
            tracing::warn!(
                "The observation duration lower bound must be positive: {}",
                self.observation_duration_lower_bound.ms()
            );
            valid = false;
        }
        if self.observation_window_size < 2 {
            tracing::warn!(
                "The observation window size must be at least 2: {}",
                self.observation_window_size
            );
            valid = false;
        }
        if self.sending_rate_smoothing_factor < 0.0 || self.sending_rate_smoothing_factor >= 1.0 {
            tracing::warn!(
                "The sending rate smoothing factor must be in [0, 1): {}",
                self.sending_rate_smoothing_factor
            );
            valid = false;
        }
        if self.instant_upper_bound_temporal_weight_factor <= 0.0
            || self.instant_upper_bound_temporal_weight_factor > 1.0
        {
            tracing::warn!(
                "The instant upper bound temporal weight factor must be in (0, 1]: {}",
                self.instant_upper_bound_temporal_weight_factor
            );
            valid = false;
        }
        if self.instant_upper_bound_bw_balance <= DataRate::zero() {
            tracing::warn!(
                "The instant upper bound bandwidth balance must be positive: {:?}",
                self.instant_upper_bound_bw_balance
            );
            valid = false;
        }
        if self.instant_upper_bound_loss_offset < 0.0 || self.instant_upper_bound_loss_offset >= 1.0
        {
            tracing::warn!(
                "The instant upper bound loss offset must be in [0, 1): {}",
                self.instant_upper_bound_loss_offset
            );
            valid = false;
        }
        if self.temporal_weight_factor <= 0.0 || self.temporal_weight_factor > 1.0 {
            tracing::warn!(
                "The temporal weight factor must be in (0, 1]: {}",
                self.temporal_weight_factor
            );
            valid = false;
        }
        if self.bw_backoff_lower_bound_factor > 1.0 {
            tracing::warn!(
                "The bandwidth backoff lower bound factor must not be greater than 1: {}",
                self.bw_backoff_lower_bound_factor
            );
            valid = false;
        }
        if self.max_increase_factor <= 0.0 {
            tracing::warn!(
                "The maximum increase factor must be positive: {}",
                self.max_increase_factor
            );
            valid = false;
        }
        if self.delayed_increase_window <= TimeDelta::zero() {
            tracing::warn!(
                "The delayed increase window must be positive: {:?}",
                self.delayed_increase_window
            );
            valid = false;
        }
        if self.min_num_observations == 0 {
            tracing::warn!(
                "The min number of observations must be positive: {}",
                self.min_num_observations
            );
            valid = false;
        }
        if self.lower_bound_by_acked_rate_factor < 0.0 {
            tracing::warn!(
                "The estimate lower bound by acknowledged rate factor must be non-negative: {}",
                self.lower_bound_by_acked_rate_factor
            );
            valid = false;
        }
        valid
    }
}

impl Default for LossBasedBweV2Config {
    fn default() -> Self {
        Self {
            enabled: true,                                                             // Enabled
            bw_rampup_upper_bound_factor: 1000000.0, // BwRampupUpperBoundFactor
            bw_rampup_upper_bound_in_hold_factor: 1000000.0, // BwRampupUpperBoundInHoldFactor
            bw_rampup_upper_bound_hold_threshold: 1.3, // BwRampupUpperBoundHoldThreshold
            bw_rampup_accel_max_factor: 0.0,         // BwRampupAccelMaxFactor
            bw_rampup_accel_maxout_time: TimeDelta::from_seconds(60), // BwRampupAccelMaxoutTime
            candidate_factors: vec![1.02, 1.0, 0.95], // CandidateFactors
            higher_bw_bias_factor: 0.0002,           // HigherBwBiasFactor
            higher_log_bw_bias_factor: 0.02,         // HigherLogBwBiasFactor
            inherent_loss_lower_bound: 1.0e-3,       // InherentLossLowerBound
            loss_threshold_of_high_bandwidth_preference: 0.15, // LossThresholdOfHighBandwidthPreference
            bandwidth_preference_smoothing_factor: 0.002,      // BandwidthPreferenceSmoothingFactor
            inherent_loss_upper_bound_bw_balance: DataRate::from_kilobits_per_sec(75), // InherentLossUpperBoundBwBalance
            inherent_loss_upper_bound_offset: 0.05, // InherentLossUpperBoundOffset
            initial_inherent_loss_estimate: 0.01,   // InitialInherentLossEstimate
            newton_iterations: 1,                   // NewtonIterations
            newton_step_size: 0.75,                 // NewtonStepSize
            acked_rate_candidate: true,             // AckedRateCandidate
            delay_based_candidate: true,            // DelayBasedCandidate
            upper_bound_candidate_in_alr: false,    // UpperBoundCandidateInAlr
            observation_duration_lower_bound: TimeDelta::from_millis(250), // ObservationDurationLowerBound
            observation_window_size: 20,                                   // ObservationWindowSize
            sending_rate_smoothing_factor: 0.0, // SendingRateSmoothingFactor
            instant_upper_bound_temporal_weight_factor: 0.9, // InstantUpperBoundTemporalWeightFactor
            instant_upper_bound_bw_balance: DataRate::from_kilobits_per_sec(75), // InstantUpperBoundBwBalance
            instant_upper_bound_loss_offset: 0.05, // InstantUpperBoundLossOffset
            temporal_weight_factor: 0.9,           // TemporalWeightFactor
            bw_backoff_lower_bound_factor: 1.0,    // BwBackoffLowerBoundFactor
            max_increase_factor: 1.3,              // MaxIncreaseFactor
            delayed_increase_window: TimeDelta::from_millis(300), // DelayedIncreaseWindow
            not_increase_if_inherent_loss_less_than_average_loss: true, // NotIncreaseIfInherentLossLessThanAverageLoss
            not_use_acked_rate_in_alr: true,                            // NotUseAckedRateInAlr
            use_in_start_phase: false,                                  // UseInStartPhase
            min_num_observations: 3,                                    // MinNumObservations
            lower_bound_by_acked_rate_factor: 0.0, // LowerBoundByAckedRateFactor
            hold_duration_factor: 0.0,             // HoldDurationFactor
            use_byte_loss_rate: false,             // UseByteLossRate
            padding_duration: TimeDelta::zero(),   // PaddingDuration
            bound_best_candidate: false,           // BoundBest
            pace_at_loss_based_estimate: false,    // PaceAtLossBasedEstimate
            median_sending_rate_factor: 2.0,       // MedianSendingRateFactor
        }
    }
}

#[derive(Default)]
struct Derivatives {
    pub first: f64,
    pub second: f64,
}

#[derive(Debug, Clone)]
struct Observation {
    pub num_packets: usize,
    pub num_lost_packets: usize,
    pub num_received_packets: usize,
    pub sending_rate: DataRate,
    pub size: DataSize,
    pub lost_size: DataSize,
    pub id: i64,
}

impl Default for Observation {
    fn default() -> Self {
        Self {
            num_packets: 0,
            num_lost_packets: 0,
            num_received_packets: 0,
            sending_rate: DataRate::minus_infinity(),
            size: DataSize::zero(),
            lost_size: DataSize::zero(),
            id: -1,
        }
    }
}

impl Observation {
    pub fn is_initialized(&self) -> bool {
        self.id != -1
    }
}

struct PartialObservation {
    pub num_packets: usize,
    pub lost_packets: HashMap<i64, DataSize>,
    pub size: DataSize,
}

impl Default for PartialObservation {
    fn default() -> Self {
        Self {
            num_packets: 0,
            lost_packets: HashMap::new(),
            size: DataSize::zero(),
        }
    }
}

struct PaddingInfo {
    pub padding_rate: DataRate,
    pub padding_timestamp: Timestamp,
}

impl Default for PaddingInfo {
    fn default() -> Self {
        Self {
            padding_rate: DataRate::minus_infinity(),
            padding_timestamp: Timestamp::minus_infinity(),
        }
    }
}

struct HoldInfo {
    timestamp: Timestamp,
    duration: TimeDelta,
    rate: DataRate,
}

impl Default for HoldInfo {
    fn default() -> Self {
        Self {
            timestamp: Timestamp::minus_infinity(),
            duration: TimeDelta::zero(),
            rate: DataRate::plus_infinity(),
        }
    }
}

pub struct LossBasedBweV2 {
    acknowledged_bitrate: Option<DataRate>,
    config: LossBasedBweV2Config,
    current_best_estimate: ChannelParameters,
    num_observations: usize,
    observations: Vec<Observation>,
    partial_observation: PartialObservation,
    last_send_time_most_recent_observation: Timestamp,
    last_time_estimate_reduced: Timestamp,
    cached_instant_upper_bound: Option<DataRate>,
    cached_instant_lower_bound: Option<DataRate>,
    instant_upper_bound_temporal_weights: Vec<f64>,
    temporal_weights: Vec<f64>,
    recovering_after_loss_timestamp: Timestamp,
    bandwidth_limit_in_current_window: DataRate,
    min_bitrate: DataRate,
    max_bitrate: DataRate,
    delay_based_estimate: DataRate,
    loss_based_result: LossBasedBweV2Result,
    last_hold_info: HoldInfo,
    last_padding_info: PaddingInfo,
    average_reported_loss_ratio: f64,
}

impl LossBasedBweV2 {
    const INIT_HOLD_DURATION: TimeDelta = TimeDelta::from_millis(300);
    const MAX_HOLD_DURATION: TimeDelta = TimeDelta::from_seconds(60);

    pub fn new(config: LossBasedBweV2Config) -> Self {
        let mut this = Self {
            acknowledged_bitrate: None,
            current_best_estimate: ChannelParameters::default(),
            num_observations: 0,
            observations: Vec::new(),
            partial_observation: PartialObservation::default(),
            last_send_time_most_recent_observation: Timestamp::plus_infinity(),
            last_time_estimate_reduced: Timestamp::minus_infinity(),
            cached_instant_upper_bound: None,
            cached_instant_lower_bound: None,
            temporal_weights: Vec::new(),
            instant_upper_bound_temporal_weights: Vec::new(),
            recovering_after_loss_timestamp: Timestamp::minus_infinity(),
            bandwidth_limit_in_current_window: DataRate::plus_infinity(),
            min_bitrate: DataRate::from_kilobits_per_sec(1),
            max_bitrate: DataRate::plus_infinity(),
            delay_based_estimate: DataRate::plus_infinity(),
            loss_based_result: LossBasedBweV2Result::default(),
            last_hold_info: HoldInfo::default(),
            last_padding_info: PaddingInfo::default(),
            average_reported_loss_ratio: 0.0,
            config,
        };

        if !this.config.enabled {
            tracing::debug!("The configuration does not specify that the estimator should be enabled, disabling it.");
            return this;
        }

        if !this.config.is_valid() {
            tracing::warn!("The configuration is invalid, disabling the estimator.");
            this.config.enabled = false;
            return this;
        }

        this.current_best_estimate.inherent_loss = this.config.initial_inherent_loss_estimate;
        this.observations
            .resize_with(this.config.observation_window_size, Observation::default);

        for i in 0..this.config.observation_window_size {
            this.temporal_weights
                .push(this.config.temporal_weight_factor.powf(i as _));

            this.instant_upper_bound_temporal_weights.push(
                this.config
                    .instant_upper_bound_temporal_weight_factor
                    .powf(i as _),
            );
        }

        this.last_hold_info.duration = Self::INIT_HOLD_DURATION;

        this
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }
    // Returns true iff a BWE can be calculated, i.e., the estimator has been
    // initialized with a BWE and then has received enough `PacketResult`s.
    pub fn is_ready(&self) -> bool {
        self.is_enabled()
            && self
                .current_best_estimate
                .loss_limited_bandwidth
                .is_finite()
            && self.num_observations >= self.config.min_num_observations
    }

    // Returns true if loss based BWE is ready to be used in the start phase.
    pub fn ready_to_use_in_start_phase(&self) -> bool {
        self.is_ready() && self.config.use_in_start_phase
    }

    // Returns true if loss based BWE can be used in the start phase.
    pub fn use_in_start_phase(&self) -> bool {
        self.config.use_in_start_phase
    }

    // Returns `DataRate::plus_infinity` if no BWE can be calculated.
    pub fn get_loss_based_result(&self) -> LossBasedBweV2Result {
        if !self.is_ready() {
            if !self.is_enabled() {
                tracing::warn!("The estimator must be enabled before it can be used.");
            } else {
                if !self
                    .current_best_estimate
                    .loss_limited_bandwidth
                    .is_finite()
                {
                    tracing::warn!("The estimator must be initialized before it can be used.");
                }
                if self.num_observations <= self.config.min_num_observations {
                    tracing::warn!(
                        "The estimator must receive enough loss statistics before it can be used."
                    );
                }
            }
            return LossBasedBweV2Result {
                bandwidth_estimate: if self.delay_based_estimate.is_finite() {
                    self.delay_based_estimate
                } else {
                    DataRate::plus_infinity()
                },
                state: LossBasedState::DelayBasedEstimate,
            };
        }
        self.loss_based_result
    }

    pub fn set_acknowledged_bitrate(&mut self, acknowledged_bitrate: DataRate) {
        if acknowledged_bitrate.is_finite() {
            self.acknowledged_bitrate = Some(acknowledged_bitrate);
            self.calculate_instant_lower_bound();
        } else {
            tracing::warn!(
                "The acknowledged bitrate must be finite: {:?}",
                acknowledged_bitrate
            );
        }
    }
    pub fn set_min_max_bitrate(&mut self, min_bitrate: DataRate, max_bitrate: DataRate) {
        if min_bitrate.is_finite() {
            self.min_bitrate = min_bitrate;
            self.calculate_instant_lower_bound();
        } else {
            tracing::warn!("The min bitrate must be finite: {:?}", min_bitrate);
        }

        if max_bitrate.is_finite() {
            self.max_bitrate = max_bitrate;
        } else {
            tracing::warn!("The max bitrate must be finite: {:?}", max_bitrate);
        }
    }
    pub fn update_bandwidth_estimate(
        &mut self,
        packet_results: &[PacketResult],
        delay_based_estimate: DataRate,
        in_alr: bool,
    ) {
        self.delay_based_estimate = delay_based_estimate;
        if !self.is_enabled() {
            tracing::warn!("The estimator must be enabled before it can be used.");
            return;
        }

        if packet_results.is_empty() {
            tracing::debug!("The estimate cannot be updated without any loss statistics.");
            return;
        }

        if !self.push_back_observation(packet_results) {
            return;
        }

        if !self
            .current_best_estimate
            .loss_limited_bandwidth
            .is_finite()
        {
            if !delay_based_estimate.is_finite() {
                tracing::warn!(
                    "The delay based estimate must be finite: {:?}",
                    delay_based_estimate
                );
                return;
            }
            self.current_best_estimate.loss_limited_bandwidth = delay_based_estimate;
            self.loss_based_result = LossBasedBweV2Result {
                bandwidth_estimate: delay_based_estimate,
                state: LossBasedState::DelayBasedEstimate,
            };
        }

        let mut best_candidate: ChannelParameters = self.current_best_estimate.clone();
        let mut objective_max: f64 = f64::MIN;
        for mut candidate in self.get_candidates(in_alr) {
            self.newtons_method_update(&mut candidate);

            let candidate_objective: f64 = self.get_objective(&candidate);
            if candidate_objective > objective_max {
                objective_max = candidate_objective;
                best_candidate = candidate;
            }
        }
        if best_candidate.loss_limited_bandwidth < self.current_best_estimate.loss_limited_bandwidth
        {
            self.last_time_estimate_reduced = self.last_send_time_most_recent_observation;
        }

        // Do not increase the estimate if the average loss is greater than current
        // inherent loss.
        if self.average_reported_loss_ratio > best_candidate.inherent_loss
            && self
                .config
                .not_increase_if_inherent_loss_less_than_average_loss
            && self.current_best_estimate.loss_limited_bandwidth
                < best_candidate.loss_limited_bandwidth
        {
            best_candidate.loss_limited_bandwidth =
                self.current_best_estimate.loss_limited_bandwidth;
        }

        if self.is_in_loss_limited_state() {
            // Bound the estimate increase if:
            // 1. The estimate has been increased for less than
            // `delayed_increase_window` ago, and
            // 2. The best candidate is greater than bandwidth_limit_in_current_window.
            if self.recovering_after_loss_timestamp.is_finite()
                && self.recovering_after_loss_timestamp + self.config.delayed_increase_window
                    > self.last_send_time_most_recent_observation
                && best_candidate.loss_limited_bandwidth > self.bandwidth_limit_in_current_window
            {
                best_candidate.loss_limited_bandwidth = self.bandwidth_limit_in_current_window;
            }

            let increasing_when_loss_limited: bool = self.is_estimate_increasing_when_loss_limited(
                /*old_estimate=*/ self.current_best_estimate.loss_limited_bandwidth,
                /*new_estimate=*/ best_candidate.loss_limited_bandwidth,
            );
            // Bound the best candidate by the acked bitrate.
            if let Some(acknowledged_bitrate) = self.acknowledged_bitrate {
                if increasing_when_loss_limited && acknowledged_bitrate.is_finite() {
                    let mut rampup_factor: f64 = self.config.bw_rampup_upper_bound_factor;
                    if self.last_hold_info.rate.is_finite()
                        && acknowledged_bitrate
                            < self.config.bw_rampup_upper_bound_hold_threshold
                                * self.last_hold_info.rate
                    {
                        rampup_factor = self.config.bw_rampup_upper_bound_in_hold_factor;
                    }

                    best_candidate.loss_limited_bandwidth = std::cmp::max(
                        self.current_best_estimate.loss_limited_bandwidth,
                        std::cmp::min(
                            best_candidate.loss_limited_bandwidth,
                            rampup_factor * (acknowledged_bitrate),
                        ),
                    );
                    // Increase current estimate by at least 1kbps to make sure that the state
                    // will be switched to Increasing, thus padding is triggered.
                    if self.loss_based_result.state == LossBasedState::Decreasing
                        && best_candidate.loss_limited_bandwidth
                            == self.current_best_estimate.loss_limited_bandwidth
                    {
                        best_candidate.loss_limited_bandwidth =
                            self.current_best_estimate.loss_limited_bandwidth
                                + DataRate::from_bits_per_sec(1);
                    }
                }
            }
        }

        let bounded_bandwidth_estimate: DataRate = if self.delay_based_estimate.is_finite() {
            self.get_instant_lower_bound().max(
                [
                    best_candidate.loss_limited_bandwidth,
                    self.get_instant_upper_bound(),
                    self.delay_based_estimate,
                ]
                .iter()
                .min()
                .cloned()
                .unwrap(),
            )
        } else {
            std::cmp::max(
                self.get_instant_lower_bound(),
                std::cmp::min(
                    best_candidate.loss_limited_bandwidth,
                    self.get_instant_upper_bound(),
                ),
            )
        };
        if self.config.bound_best_candidate
            && bounded_bandwidth_estimate < best_candidate.loss_limited_bandwidth
        {
            tracing::debug!(
                "Resetting loss based BWE to {:?} due to loss. Avg loss rate: {:?}",
                bounded_bandwidth_estimate,
                self.average_reported_loss_ratio
            );
            self.current_best_estimate.loss_limited_bandwidth = bounded_bandwidth_estimate;
            self.current_best_estimate.inherent_loss = 0.0;
        } else {
            self.current_best_estimate = best_candidate;
            if self.config.lower_bound_by_acked_rate_factor > 0.0 {
                self.current_best_estimate.loss_limited_bandwidth = std::cmp::max(
                    self.current_best_estimate.loss_limited_bandwidth,
                    self.get_instant_lower_bound(),
                );
            }
        }

        if self.loss_based_result.state == LossBasedState::Decreasing
            && self.last_hold_info.timestamp > self.last_send_time_most_recent_observation
            && bounded_bandwidth_estimate < self.delay_based_estimate
        {
            // Ensure that acked rate is the lower bound of HOLD rate.
            if self.config.lower_bound_by_acked_rate_factor > 0.0 {
                self.last_hold_info.rate =
                    std::cmp::max(self.get_instant_lower_bound(), self.last_hold_info.rate);
            }
            // BWE is not allowed to increase above the HOLD rate. The purpose of
            // HOLD is to not immediately ramp up BWE to a rate that may cause loss.
            self.loss_based_result.bandwidth_estimate =
                std::cmp::min(self.last_hold_info.rate, bounded_bandwidth_estimate);
            return;
        }

        if self.is_estimate_increasing_when_loss_limited(
            /*old_estimate=*/ self.loss_based_result.bandwidth_estimate,
            /*new_estimate=*/ bounded_bandwidth_estimate,
        ) && self.can_keep_increasing_state(bounded_bandwidth_estimate)
            && bounded_bandwidth_estimate < self.delay_based_estimate
            && bounded_bandwidth_estimate < self.max_bitrate
        {
            if self.config.padding_duration > TimeDelta::zero()
                && bounded_bandwidth_estimate > self.last_padding_info.padding_rate
            {
                // Start a new padding duration.
                self.last_padding_info.padding_rate = bounded_bandwidth_estimate;
                self.last_padding_info.padding_timestamp =
                    self.last_send_time_most_recent_observation;
            }
            self.loss_based_result.state = if self.config.padding_duration > TimeDelta::zero() {
                LossBasedState::IncreaseUsingPadding
            } else {
                LossBasedState::Increasing
            };
        } else if bounded_bandwidth_estimate < self.delay_based_estimate
            && bounded_bandwidth_estimate < self.max_bitrate
        {
            if self.loss_based_result.state != LossBasedState::Decreasing
                && self.config.hold_duration_factor > 0.0
            {
                tracing::debug!(
                    "Switch to HOLD. Bounded BWE: {:?}, duration: {:?}",
                    bounded_bandwidth_estimate,
                    self.last_hold_info.duration
                );
                self.last_hold_info = HoldInfo {
                    timestamp: self.last_send_time_most_recent_observation
                        + self.last_hold_info.duration,
                    duration: std::cmp::min(
                        Self::MAX_HOLD_DURATION,
                        self.last_hold_info.duration * self.config.hold_duration_factor,
                    ),
                    rate: bounded_bandwidth_estimate,
                };
            }

            self.last_padding_info = PaddingInfo::default();
            self.loss_based_result.state = LossBasedState::Decreasing;
        } else {
            // Reset the HOLD info if delay based estimate works to avoid getting
            // stuck in low bitrate.
            self.last_hold_info = HoldInfo {
                timestamp: Timestamp::minus_infinity(),
                duration: Self::INIT_HOLD_DURATION,
                rate: DataRate::plus_infinity(),
            };
            self.last_padding_info = PaddingInfo::default();
            self.loss_based_result.state = LossBasedState::DelayBasedEstimate;
        }
        self.loss_based_result.bandwidth_estimate = bounded_bandwidth_estimate;

        if self.is_in_loss_limited_state()
            && (self.recovering_after_loss_timestamp.is_infinite()
                || self.recovering_after_loss_timestamp + self.config.delayed_increase_window
                    < self.last_send_time_most_recent_observation)
        {
            self.bandwidth_limit_in_current_window = std::cmp::max(
                CONGESTION_CONTROLLER_MIN_BITRATE,
                self.current_best_estimate.loss_limited_bandwidth * self.config.max_increase_factor,
            );
            self.recovering_after_loss_timestamp = self.last_send_time_most_recent_observation;
        }
    }
    pub fn pace_at_loss_based_estimate(&self) -> bool {
        self.config.pace_at_loss_based_estimate
            && self.loss_based_result.state != LossBasedState::DelayBasedEstimate
    }

    // For unit testing only.
    #[cfg(test)]
    pub fn set_bandwidth_estimate(&mut self, bandwidth_estimate: DataRate) {
        if bandwidth_estimate.is_finite() {
            self.current_best_estimate.loss_limited_bandwidth = bandwidth_estimate;
            self.loss_based_result = LossBasedBweV2Result {
                bandwidth_estimate,
                state: LossBasedState::DelayBasedEstimate,
            };
        } else {
            tracing::warn!(
                "The bandwidth estimate must be finite: {:?}",
                bandwidth_estimate
            );
        }
    }

    // Returns `0.0` if not enough loss statistics have been received.
    fn update_average_reported_loss_ratio(&mut self) {
        self.average_reported_loss_ratio = if self.config.use_byte_loss_rate {
            self.calculate_average_reported_byte_loss_ratio()
        } else {
            self.calculate_average_reported_packet_loss_ratio()
        };
    }

    fn calculate_average_reported_packet_loss_ratio(&self) -> f64 {
        if self.num_observations == 0 {
            return 0.0;
        }

        let mut num_packets: f64 = 0.0;
        let mut num_lost_packets: f64 = 0.0;
        for observation in &self.observations {
            if !observation.is_initialized() {
                continue;
            }

            let id: usize = observation.id.try_into().unwrap();
            let instant_temporal_weight: f64 =
                self.instant_upper_bound_temporal_weights[(self.num_observations - 1) - id];
            num_packets += instant_temporal_weight * observation.num_packets as f64;
            num_lost_packets += instant_temporal_weight * observation.num_lost_packets as f64;
        }

        num_lost_packets / num_packets
    }
    // Calculates the average loss ratio over the last `observation_window_size`
    // observations but skips the observation with min and max loss ratio in order
    // to filter out loss spikes.
    fn calculate_average_reported_byte_loss_ratio(&self) -> f64 {
        if self.num_observations == 0 {
            return 0.0;
        }

        let mut total_bytes: DataSize = DataSize::zero();
        let mut lost_bytes: DataSize = DataSize::zero();
        let mut min_loss_rate: f64 = 1.0;
        let mut max_loss_rate: f64 = 0.0;
        let mut min_lost_bytes: DataSize = DataSize::zero();
        let mut max_lost_bytes: DataSize = DataSize::zero();
        let mut min_bytes_received: DataSize = DataSize::zero();
        let mut max_bytes_received: DataSize = DataSize::zero();
        let mut send_rate_of_max_loss_observation: DataRate = DataRate::zero();
        for observation in &self.observations {
            if !observation.is_initialized() {
                continue;
            }

            let id: usize = observation.id.try_into().unwrap();
            let instant_temporal_weight: f64 =
                self.instant_upper_bound_temporal_weights[(self.num_observations - 1) - id];
            total_bytes += instant_temporal_weight * observation.size;
            lost_bytes += instant_temporal_weight * observation.lost_size;

            let loss_rate: f64 = if !observation.size.is_zero() {
                observation.lost_size / observation.size
            } else {
                0.0
            };
            if self.num_observations > 3 {
                if loss_rate > max_loss_rate {
                    max_loss_rate = loss_rate;
                    max_lost_bytes = instant_temporal_weight * observation.lost_size;
                    max_bytes_received = instant_temporal_weight * observation.size;
                    send_rate_of_max_loss_observation = observation.sending_rate;
                }
                if loss_rate < min_loss_rate {
                    min_loss_rate = loss_rate;
                    min_lost_bytes = instant_temporal_weight * observation.lost_size;
                    min_bytes_received = instant_temporal_weight * observation.size;
                }
            }
        }
        if self.get_median_sending_rate() * self.config.median_sending_rate_factor
            <= send_rate_of_max_loss_observation
        {
            // If the median sending rate is less than half of the sending rate of the
            // observation with max loss rate, i.e. we suddenly send a lot of data, then
            // the loss rate might not be due to a spike.
            return lost_bytes / total_bytes;
        }
        (lost_bytes - min_lost_bytes - max_lost_bytes)
            / (total_bytes - max_bytes_received - min_bytes_received)
    }

    fn get_candidates(&self, in_alr: bool) -> Vec<ChannelParameters> {
        let best_estimate: ChannelParameters = self.current_best_estimate.clone();
        let mut bandwidths: Vec<DataRate> = Vec::new();
        for candidate_factor in &self.config.candidate_factors {
            bandwidths.push(*candidate_factor * best_estimate.loss_limited_bandwidth);
        }

        if let Some(acknowledged_bitrate) = self.acknowledged_bitrate {
            if self.config.acked_rate_candidate
                && (!(self.config.not_use_acked_rate_in_alr && in_alr)
                    || (self.config.padding_duration > TimeDelta::zero()
                        && self.last_padding_info.padding_timestamp + self.config.padding_duration
                            >= self.last_send_time_most_recent_observation))
            {
                bandwidths.push(acknowledged_bitrate * self.config.bw_backoff_lower_bound_factor);
            }
        }

        if self.delay_based_estimate.is_finite()
            && self.config.delay_based_candidate
            && self.delay_based_estimate > best_estimate.loss_limited_bandwidth
        {
            bandwidths.push(self.delay_based_estimate);
        }

        if in_alr
            && self.config.upper_bound_candidate_in_alr
            && best_estimate.loss_limited_bandwidth > self.get_instant_upper_bound()
        {
            bandwidths.push(self.get_instant_upper_bound());
        }

        let candidate_bandwidth_upper_bound: DataRate = self.get_candidate_bandwidth_upper_bound();

        let mut candidates: Vec<ChannelParameters> = Vec::new();
        for bandwidth in &bandwidths {
            let mut candidate: ChannelParameters = best_estimate.clone();
            candidate.loss_limited_bandwidth = std::cmp::min(
                *bandwidth,
                std::cmp::max(
                    best_estimate.loss_limited_bandwidth,
                    candidate_bandwidth_upper_bound,
                ),
            );
            candidate.inherent_loss = self.get_feasible_inherent_loss(&candidate);
            candidates.push(candidate);
        }
        candidates
    }

    fn get_candidate_bandwidth_upper_bound(&self) -> DataRate {
        let mut candidate_bandwidth_upper_bound: DataRate = self.max_bitrate;
        if self.is_in_loss_limited_state() && self.bandwidth_limit_in_current_window.is_finite() {
            candidate_bandwidth_upper_bound = self.bandwidth_limit_in_current_window;
        }

        let acknowledged_bitrate = match self.acknowledged_bitrate {
            Some(rate) => rate,
            None => return candidate_bandwidth_upper_bound,
        };

        if self.config.bw_rampup_accel_max_factor > 0.0 {
            let time_since_bandwidth_reduced: TimeDelta = std::cmp::min(
                self.config.bw_rampup_accel_maxout_time,
                std::cmp::max(
                    TimeDelta::zero(),
                    self.last_send_time_most_recent_observation - self.last_time_estimate_reduced,
                ),
            );
            let rampup_acceleration: f64 = self.config.bw_rampup_accel_max_factor
                * time_since_bandwidth_reduced
                / self.config.bw_rampup_accel_maxout_time;

            candidate_bandwidth_upper_bound += rampup_acceleration * (acknowledged_bitrate);
        }
        candidate_bandwidth_upper_bound
    }

    fn get_derivatives(&self, channel_parameters: &ChannelParameters) -> Derivatives {
        let mut derivatives = Derivatives::default();

        for observation in &self.observations {
            if !observation.is_initialized() {
                continue;
            }

            let loss_probability: f64 = get_loss_probability(
                channel_parameters.inherent_loss,
                channel_parameters.loss_limited_bandwidth,
                observation.sending_rate,
            );

            let id: usize = observation.id.try_into().unwrap();
            let temporal_weight: f64 = self.temporal_weights[(self.num_observations - 1) - id];
            if self.config.use_byte_loss_rate {
                derivatives.first += temporal_weight
                    * ((to_kilo_bytes(observation.lost_size) / loss_probability)
                        - (to_kilo_bytes(observation.size - observation.lost_size)
                            / (1.0 - loss_probability)));
                derivatives.second -= temporal_weight
                    * ((to_kilo_bytes(observation.lost_size) / loss_probability.powf(2.0))
                        + (to_kilo_bytes(observation.size - observation.lost_size)
                            / (1.0 - loss_probability).powf(2.0)));
            } else {
                derivatives.first += temporal_weight
                    * ((observation.num_lost_packets as f64 / loss_probability)
                        - (observation.num_received_packets as f64 / (1.0 - loss_probability)));
                derivatives.second -= temporal_weight
                    * ((observation.num_lost_packets as f64 / loss_probability.powf(2.0))
                        + (observation.num_received_packets as f64
                            / (1.0 - loss_probability).powf(2.0)));
            }
        }

        if derivatives.second >= 0.0 {
            tracing::error!(
                "The second derivative is mathematically guaranteed to be negative but is {}.",
                derivatives.second
            );
            derivatives.second = -1.0e-6;
        }

        derivatives
    }

    fn get_feasible_inherent_loss(&self, channel_parameters: &ChannelParameters) -> f64 {
        channel_parameters
            .inherent_loss
            .max(self.config.inherent_loss_lower_bound)
            .min(self.get_inherent_loss_upper_bound(channel_parameters.loss_limited_bandwidth))
    }

    fn get_inherent_loss_upper_bound(&self, bandwidth: DataRate) -> f64 {
        if bandwidth.is_zero() {
            return 1.0;
        }

        let inherent_loss_upper_bound: f64 = self.config.inherent_loss_upper_bound_offset
            + self.config.inherent_loss_upper_bound_bw_balance / bandwidth;
        inherent_loss_upper_bound.min(1.0)
    }

    fn adjust_bias_factor(&self, loss_rate: f64, bias_factor: f64) -> f64 {
        bias_factor * (self.config.loss_threshold_of_high_bandwidth_preference - loss_rate)
            / (self.config.bandwidth_preference_smoothing_factor
                + (self.config.loss_threshold_of_high_bandwidth_preference - loss_rate).abs())
    }

    fn get_high_bandwidth_bias(&self, bandwidth: DataRate) -> f64 {
        if bandwidth.is_finite() {
            return self.adjust_bias_factor(
                self.average_reported_loss_ratio,
                self.config.higher_bw_bias_factor,
            ) * bandwidth.kbps_float()
                + self.adjust_bias_factor(
                    self.average_reported_loss_ratio,
                    self.config.higher_log_bw_bias_factor,
                ) * (1.0 + bandwidth.kbps_float()).ln();
        }
        0.0
    }

    fn get_objective(&self, channel_parameters: &ChannelParameters) -> f64 {
        let mut objective: f64 = 0.0;

        let high_bandwidth_bias: f64 =
            self.get_high_bandwidth_bias(channel_parameters.loss_limited_bandwidth);

        for observation in &self.observations {
            if !observation.is_initialized() {
                continue;
            }

            let loss_probability: f64 = get_loss_probability(
                channel_parameters.inherent_loss,
                channel_parameters.loss_limited_bandwidth,
                observation.sending_rate,
            );

            let id: usize = observation.id.try_into().unwrap();
            let temporal_weight: f64 = self.temporal_weights[(self.num_observations - 1) - id];
            if self.config.use_byte_loss_rate {
                objective += temporal_weight
                    * ((to_kilo_bytes(observation.lost_size) * loss_probability.ln())
                        + (to_kilo_bytes(observation.size - observation.lost_size)
                            * (1.0 - loss_probability).ln()));
                objective +=
                    temporal_weight * high_bandwidth_bias * to_kilo_bytes(observation.size);
            } else {
                objective += temporal_weight
                    * ((observation.num_lost_packets as f64 * (loss_probability).ln())
                        + (observation.num_received_packets as f64
                            * (1.0 - loss_probability).ln()));
                objective += temporal_weight * high_bandwidth_bias * observation.num_packets as f64;
            }
        }

        objective
    }

    fn get_sending_rate(&self, instantaneous_sending_rate: DataRate) -> DataRate {
        if self.num_observations == 0 {
            return instantaneous_sending_rate;
        }

        let most_recent_observation_idx: usize =
            (self.num_observations - 1) % self.config.observation_window_size;
        let most_recent_observation = &self.observations[most_recent_observation_idx];
        let sending_rate_previous_observation: DataRate = most_recent_observation.sending_rate;

        self.config.sending_rate_smoothing_factor * sending_rate_previous_observation
            + (1.0 - self.config.sending_rate_smoothing_factor) * instantaneous_sending_rate
    }

    fn get_instant_upper_bound(&self) -> DataRate {
        self.cached_instant_upper_bound.unwrap_or(self.max_bitrate)
    }

    fn calculate_instant_upper_bound(&mut self) {
        let mut instant_limit: DataRate = self.max_bitrate;
        if self.average_reported_loss_ratio > self.config.instant_upper_bound_loss_offset {
            instant_limit = self.config.instant_upper_bound_bw_balance
                / (self.average_reported_loss_ratio - self.config.instant_upper_bound_loss_offset);
        }

        self.cached_instant_upper_bound = Some(instant_limit);
    }

    fn get_instant_lower_bound(&self) -> DataRate {
        self.cached_instant_lower_bound.unwrap_or(DataRate::zero())
    }

    fn calculate_instant_lower_bound(&mut self) {
        let mut instance_lower_bound: DataRate = DataRate::zero();

        if let Some(acknowledged_bitrate) = self.acknowledged_bitrate {
            if self.config.lower_bound_by_acked_rate_factor > 0.0 {
                instance_lower_bound =
                    self.config.lower_bound_by_acked_rate_factor * acknowledged_bitrate;
            }
        }

        if self.min_bitrate.is_finite() {
            instance_lower_bound = std::cmp::max(instance_lower_bound, self.min_bitrate);
        }
        self.cached_instant_lower_bound = Some(instance_lower_bound);
    }

    fn newtons_method_update(&self, channel_parameters: &mut ChannelParameters) {
        if self.num_observations == 0 {
            return;
        }

        for _ in 0..self.config.newton_iterations {
            let derivatives: Derivatives = self.get_derivatives(channel_parameters);
            channel_parameters.inherent_loss -=
                self.config.newton_step_size * derivatives.first / derivatives.second;
            channel_parameters.inherent_loss = self.get_feasible_inherent_loss(channel_parameters);
        }
    }

    // Returns false if no observation was created.
    fn push_back_observation(&mut self, packet_results: &[PacketResult]) -> bool {
        if packet_results.is_empty() {
            return false;
        }

        self.partial_observation.num_packets += packet_results.len();
        let mut last_send_time: Timestamp = Timestamp::minus_infinity();
        let mut first_send_time: Timestamp = Timestamp::plus_infinity();
        for packet in packet_results {
            if packet.is_received() {
                self.partial_observation
                    .lost_packets
                    .remove(&packet.sent_packet.sequence_number);
            } else {
                self.partial_observation
                    .lost_packets
                    .insert(packet.sent_packet.sequence_number, packet.sent_packet.size);
            }
            self.partial_observation.size += packet.sent_packet.size;
            last_send_time = std::cmp::max(last_send_time, packet.sent_packet.send_time);
            first_send_time = std::cmp::min(first_send_time, packet.sent_packet.send_time);
        }

        // This is the first packet report we have received.
        if !self.last_send_time_most_recent_observation.is_finite() {
            self.last_send_time_most_recent_observation = first_send_time;
        }

        let observation_duration: TimeDelta =
            last_send_time - self.last_send_time_most_recent_observation;
        // Too small to be meaningful.
        // To consider: what if it is too long?, i.e. we did not receive any packets
        // for a long time, then all the packets we received are too old.
        if observation_duration <= TimeDelta::zero()
            || observation_duration < self.config.observation_duration_lower_bound
        {
            return false;
        }

        self.last_send_time_most_recent_observation = last_send_time;

        let mut observation = Observation::default();
        observation.num_packets = self.partial_observation.num_packets;
        observation.num_lost_packets = self.partial_observation.lost_packets.len();
        observation.num_received_packets = observation.num_packets - observation.num_lost_packets;
        observation.sending_rate =
            self.get_sending_rate(self.partial_observation.size / observation_duration);
        for packet_size in self.partial_observation.lost_packets.values() {
            observation.lost_size += *packet_size;
        }
        observation.size = self.partial_observation.size;
        observation.id = self.num_observations as _;
        self.observations[self.num_observations % self.config.observation_window_size] =
            observation;
        self.num_observations += 1;

        self.partial_observation = PartialObservation::default();
        self.update_average_reported_loss_ratio();
        self.calculate_instant_upper_bound();
        true
    }

    fn is_estimate_increasing_when_loss_limited(
        &mut self,
        old_estimate: DataRate,
        new_estimate: DataRate,
    ) -> bool {
        (old_estimate < new_estimate
            || (old_estimate == new_estimate
                && (self.loss_based_result.state == LossBasedState::Increasing
                    || self.loss_based_result.state == LossBasedState::IncreaseUsingPadding)))
            && self.is_in_loss_limited_state()
    }

    fn is_in_loss_limited_state(&self) -> bool {
        self.loss_based_result.state != LossBasedState::DelayBasedEstimate
    }

    fn can_keep_increasing_state(&self, estimate: DataRate) -> bool {
        if self.config.padding_duration == TimeDelta::zero()
            || self.loss_based_result.state != LossBasedState::IncreaseUsingPadding
        {
            return true;
        }

        // Keep using the IncreaseUsingPadding if either the state has been
        // IncreaseUsingPadding for less than PaddingDuration or the estimate
        // increases.
        self.last_padding_info.padding_timestamp + self.config.padding_duration
            >= self.last_send_time_most_recent_observation
            || self.last_padding_info.padding_rate < estimate
    }

    fn get_median_sending_rate(&self) -> DataRate {
        let mut sending_rates: Vec<DataRate> = Vec::new();
        for observation in &self.observations {
            if !observation.is_initialized()
                || !observation.sending_rate.is_finite()
                || observation.sending_rate.is_zero()
            {
                continue;
            }
            sending_rates.push(observation.sending_rate);
        }
        if sending_rates.is_empty() {
            return DataRate::zero();
        }
        sending_rates.sort();
        if sending_rates.len() % 2 == 0 {
            return (sending_rates[sending_rates.len() / 2 - 1]
                + sending_rates[sending_rates.len() / 2])
                / 2;
        }
        sending_rates[sending_rates.len() / 2]
    }
}

fn to_kilo_bytes(datasize: DataSize) -> f64 {
    datasize.bytes_float() / 1000.0
}

fn get_loss_probability(
    mut inherent_loss: f64,
    loss_limited_bandwidth: DataRate,
    sending_rate: DataRate,
) -> f64 {
    if !(0.0..=1.0).contains(&inherent_loss) {
        tracing::warn!("The inherent loss must be in [0,1]: {}", inherent_loss);
        inherent_loss = inherent_loss.clamp(0.0, 1.0)
    }
    if !sending_rate.is_finite() {
        tracing::warn!("The sending rate must be finite: {:?}", sending_rate);
    }
    if !loss_limited_bandwidth.is_finite() {
        tracing::warn!(
            "The loss limited bandwidth must be finite: {:?}",
            loss_limited_bandwidth
        );
    }

    let mut loss_probability: f64 = inherent_loss;
    if sending_rate.is_finite()
        && loss_limited_bandwidth.is_finite()
        && (sending_rate > loss_limited_bandwidth)
    {
        loss_probability +=
            (1.0 - inherent_loss) * (sending_rate - loss_limited_bandwidth) / sending_rate;
    }
    loss_probability.clamp(1.0e-6, 1.0 - 1.0e-6)
}

#[cfg(test)]
mod test {
    use approx::assert_relative_eq;
    use test_trace::test;

    use crate::api::transport::SentPacket;

    use super::*;

    const OBSERVATION_DURATION_LOWER_BOUND: TimeDelta = TimeDelta::from_millis(250);
    const DELAYED_INCREASE_WINDOW: TimeDelta = TimeDelta::from_millis(300);
    const MAX_INCREASE_FACTOR: f64 = 1.5;
    const PACKET_SIZE: i64 = 15_000;

    fn config(enabled: bool, valid: bool) -> LossBasedBweV2Config {
        LossBasedBweV2Config {
            enabled,
            bw_rampup_upper_bound_factor: if valid { 1.2 } else { 0.0 },
            candidate_factors: vec![1.1, 1.0, 0.95],
            higher_bw_bias_factor: 0.01,
            inherent_loss_lower_bound: 0.001,
            inherent_loss_upper_bound_bw_balance: DataRate::from_kilobits_per_sec(14),
            inherent_loss_upper_bound_offset: 0.9,
            initial_inherent_loss_estimate: 0.01,
            newton_iterations: 2,
            newton_step_size: 0.4,
            observation_window_size: 15,
            sending_rate_smoothing_factor: 0.01,
            instant_upper_bound_temporal_weight_factor: 0.97,
            instant_upper_bound_bw_balance: DataRate::from_kilobits_per_sec(90),
            instant_upper_bound_loss_offset: 0.1,
            temporal_weight_factor: 0.98,
            min_num_observations: 1,
            observation_duration_lower_bound: OBSERVATION_DURATION_LOWER_BOUND,
            max_increase_factor: MAX_INCREASE_FACTOR,
            delayed_increase_window: DELAYED_INCREASE_WINDOW,
            ..Default::default()
        }
    }

    fn short_observation_config() -> LossBasedBweV2Config {
        LossBasedBweV2Config {
            min_num_observations: 1,
            observation_window_size: 2,
            ..Default::default()
        }
    }

    #[derive(Default)]
    struct LossBasedBweV2Test {
        transport_sequence_number: i64,
    }

    impl LossBasedBweV2Test {
        fn create_packet_results_with_received_packets(
            &mut self,
            first_packet_timestamp: Timestamp,
        ) -> Vec<PacketResult> {
            let mut enough_feedback = vec![PacketResult::default(); 2];
            self.transport_sequence_number += 1;
            enough_feedback[0].sent_packet.sequence_number = self.transport_sequence_number;
            self.transport_sequence_number += 1;
            enough_feedback[1].sent_packet.sequence_number = self.transport_sequence_number;
            enough_feedback[0].sent_packet.size = DataSize::from_bytes(PACKET_SIZE);
            enough_feedback[1].sent_packet.size = DataSize::from_bytes(PACKET_SIZE);
            enough_feedback[0].sent_packet.send_time = first_packet_timestamp;
            enough_feedback[1].sent_packet.send_time =
                first_packet_timestamp + OBSERVATION_DURATION_LOWER_BOUND;
            enough_feedback[0].receive_time =
                first_packet_timestamp + OBSERVATION_DURATION_LOWER_BOUND;
            enough_feedback[1].receive_time =
                first_packet_timestamp + 2 * OBSERVATION_DURATION_LOWER_BOUND;
            enough_feedback
        }

        fn create_packet_results_with_10p_packet_loss_rate(
            &mut self,
            first_packet_timestamp: Timestamp,
            lost_packet_size: DataSize,
        ) -> Vec<PacketResult> {
            let mut enough_feedback = vec![PacketResult::default(); 10];
            for (i, packet) in enough_feedback.iter_mut().enumerate() {
                self.transport_sequence_number += 1;
                packet.sent_packet.sequence_number = self.transport_sequence_number;
                packet.sent_packet.size = DataSize::from_bytes(PACKET_SIZE);
                packet.sent_packet.send_time =
                    first_packet_timestamp + (i) as i64 * OBSERVATION_DURATION_LOWER_BOUND;
                packet.receive_time =
                    first_packet_timestamp + (i + 1) as i64 * OBSERVATION_DURATION_LOWER_BOUND;
            }
            enough_feedback[9].receive_time = Timestamp::plus_infinity();
            enough_feedback[9].sent_packet.size = lost_packet_size;
            enough_feedback
        }

        fn create_packet_results_with_50p_packet_loss_rate(
            &mut self,
            first_packet_timestamp: Timestamp,
        ) -> Vec<PacketResult> {
            let mut enough_feedback = vec![PacketResult::default(); 2];
            self.transport_sequence_number += 1;
            enough_feedback[0].sent_packet.sequence_number = self.transport_sequence_number;
            self.transport_sequence_number += 1;
            enough_feedback[1].sent_packet.sequence_number = self.transport_sequence_number;
            enough_feedback[0].sent_packet.size = DataSize::from_bytes(PACKET_SIZE);
            enough_feedback[1].sent_packet.size = DataSize::from_bytes(PACKET_SIZE);
            enough_feedback[0].sent_packet.send_time = first_packet_timestamp;
            enough_feedback[1].sent_packet.send_time =
                first_packet_timestamp + OBSERVATION_DURATION_LOWER_BOUND;
            enough_feedback[0].receive_time =
                first_packet_timestamp + OBSERVATION_DURATION_LOWER_BOUND;
            enough_feedback[1].receive_time = Timestamp::plus_infinity();
            enough_feedback
        }

        fn create_packet_results_with_100p_loss_rate(
            &mut self,
            first_packet_timestamp: Timestamp,
            num_packets: usize, /* =2 */
        ) -> Vec<PacketResult> {
            let mut enough_feedback = Vec::with_capacity(num_packets);

            for i in 0..(num_packets - 1) {
                self.transport_sequence_number += 1;

                enough_feedback.push(PacketResult {
                    sent_packet: SentPacket {
                        sequence_number: self.transport_sequence_number,
                        size: DataSize::from_bytes(PACKET_SIZE),
                        send_time: first_packet_timestamp + TimeDelta::from_millis(i as i64 * 10),
                        ..Default::default()
                    },
                    receive_time: Timestamp::plus_infinity(),
                    ..Default::default()
                });
            }
            self.transport_sequence_number += 1;

            enough_feedback.push(PacketResult {
                sent_packet: SentPacket {
                    sequence_number: self.transport_sequence_number,
                    size: DataSize::from_bytes(PACKET_SIZE),
                    send_time: first_packet_timestamp + OBSERVATION_DURATION_LOWER_BOUND,
                    ..Default::default()
                },
                receive_time: Timestamp::plus_infinity(),
                ..Default::default()
            });

            enough_feedback
        }
    }

    #[test]
    fn enabled_when_given_valid_configuration_values() {
        let config = config(true, true);
        let loss_based_bandwidth_estimator = LossBasedBweV2::new(config);

        assert!(loss_based_bandwidth_estimator.is_enabled());
    }

    #[test]
    fn disabled_when_given_disabled_configuration() {
        let config = config(false, true);
        let loss_based_bandwidth_estimator = LossBasedBweV2::new(config);

        assert!(!loss_based_bandwidth_estimator.is_enabled());
    }

    #[test]
    fn disabled_when_given_non_valid_configuration_values() {
        let config = config(true, false);
        let loss_based_bandwidth_estimator = LossBasedBweV2::new(config);

        assert!(!loss_based_bandwidth_estimator.is_enabled());
    }

    #[test]
    fn disabled_when_given_non_positive_candidate_factor() {
        let config = LossBasedBweV2Config {
            candidate_factors: vec![-1.3, 1.1],
            ..Default::default()
        };
        let loss_based_bandwidth_estimator_1 = LossBasedBweV2::new(config);
        assert!(!loss_based_bandwidth_estimator_1.is_enabled());

        let config = LossBasedBweV2Config {
            candidate_factors: vec![0.0, 1.1],
            ..Default::default()
        };
        let loss_based_bandwidth_estimator_2 = LossBasedBweV2::new(config);
        assert!(!loss_based_bandwidth_estimator_2.is_enabled());
    }

    #[test]
    fn disabled_when_given_configuration_that_does_not_allow_generating_candidates() {
        let config = LossBasedBweV2Config {
            candidate_factors: vec![1.0],
            acked_rate_candidate: false,
            delay_based_candidate: false,
            ..Default::default()
        };
        let loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        assert!(!loss_based_bandwidth_estimator.is_enabled());
    }

    #[test]
    fn returns_delay_based_estimate_when_disabled() {
        let config = config(false, true);
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            /*packet_results=*/ &[],
            /*delay_based_estimate=*/ DataRate::from_kilobits_per_sec(100),
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate,
            DataRate::from_kilobits_per_sec(100)
        );
    }

    #[test]
    fn returns_delay_based_estimate_when_when_given_non_valid_configuration_values() {
        let config = config(true, false);
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            /*packet_results=*/ &[],
            /*delay_based_estimate=*/ DataRate::from_kilobits_per_sec(100),
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate,
            DataRate::from_kilobits_per_sec(100)
        );
    }

    #[test]
    fn bandwidth_estimate_given_initialization_and_then_feedback() {
        let mut test = LossBasedBweV2Test::default();
        let enough_feedback: Vec<PacketResult> = test.create_packet_results_with_received_packets(
            /*first_packet_timestamp=*/ Timestamp::zero(),
        );

        let config = config(true, true);
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);

        loss_based_bandwidth_estimator.set_bandwidth_estimate(DataRate::from_kilobits_per_sec(600));
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );

        assert!(loss_based_bandwidth_estimator.is_ready());
        assert!(loss_based_bandwidth_estimator
            .get_loss_based_result()
            .bandwidth_estimate
            .is_finite());
    }

    #[test]
    fn no_bandwidth_estimate_given_no_initialization() {
        let mut test = LossBasedBweV2Test::default();
        let enough_feedback: Vec<PacketResult> = test.create_packet_results_with_received_packets(
            /*first_packet_timestamp=*/ Timestamp::zero(),
        );
        let config = config(true, true);
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);

        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );

        assert!(!loss_based_bandwidth_estimator.is_ready());
        assert!(loss_based_bandwidth_estimator
            .get_loss_based_result()
            .bandwidth_estimate
            .is_plus_infinity());
    }

    #[test]
    fn no_bandwidth_estimate_given_not_enough_feedback() {
        // Create packet results where the observation duration is less than the lower
        // bound.
        let mut not_enough_feedback = [PacketResult::default(); 2];
        not_enough_feedback[0].sent_packet.size = DataSize::from_bytes(15_000);
        not_enough_feedback[1].sent_packet.size = DataSize::from_bytes(15_000);
        not_enough_feedback[0].sent_packet.send_time = Timestamp::zero();
        not_enough_feedback[1].sent_packet.send_time =
            Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND / 2;
        not_enough_feedback[0].receive_time =
            Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND / 2;
        not_enough_feedback[1].receive_time = Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND;

        let config = config(true, true);
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);

        loss_based_bandwidth_estimator.set_bandwidth_estimate(DataRate::from_kilobits_per_sec(600));

        assert!(!loss_based_bandwidth_estimator.is_ready());
        assert!(loss_based_bandwidth_estimator
            .get_loss_based_result()
            .bandwidth_estimate
            .is_plus_infinity());

        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &not_enough_feedback,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );

        assert!(!loss_based_bandwidth_estimator.is_ready());
        assert!(loss_based_bandwidth_estimator
            .get_loss_based_result()
            .bandwidth_estimate
            .is_plus_infinity());
    }

    #[test]
    fn set_value_is_the_estimate_until_additional_feedback_has_been_received() {
        let mut test = LossBasedBweV2Test::default();
        let enough_feedback_1: Vec<PacketResult> = test
            .create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/ Timestamp::zero(),
            );
        let enough_feedback_2: Vec<PacketResult> = test
            .create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/
                Timestamp::zero() + 2 * OBSERVATION_DURATION_LOWER_BOUND,
            );

        let config = config(true, true);
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);

        loss_based_bandwidth_estimator.set_bandwidth_estimate(DataRate::from_kilobits_per_sec(600));
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_1,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );

        assert_ne!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate,
            DataRate::from_kilobits_per_sec(600)
        );

        loss_based_bandwidth_estimator.set_bandwidth_estimate(DataRate::from_kilobits_per_sec(600));

        assert_eq!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate,
            DataRate::from_kilobits_per_sec(600)
        );

        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_2,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );

        assert_ne!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate,
            DataRate::from_kilobits_per_sec(600)
        );
    }

    #[test]
    fn set_acknowledged_bitrate_only_affects_the_bwe_when_additional_feedback_is_given() {
        let mut test = LossBasedBweV2Test::default();
        let enough_feedback_1: Vec<PacketResult> = test
            .create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/ Timestamp::zero(),
            );
        let enough_feedback_2: Vec<PacketResult> = test
            .create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/
                Timestamp::zero() + 2 * OBSERVATION_DURATION_LOWER_BOUND,
            );

        let config = config(true, true);
        let mut loss_based_bandwidth_estimator_1 = LossBasedBweV2::new(config.clone());
        let mut loss_based_bandwidth_estimator_2 = LossBasedBweV2::new(config);

        loss_based_bandwidth_estimator_1
            .set_bandwidth_estimate(DataRate::from_kilobits_per_sec(600));
        loss_based_bandwidth_estimator_2
            .set_bandwidth_estimate(DataRate::from_kilobits_per_sec(600));
        loss_based_bandwidth_estimator_1.update_bandwidth_estimate(
            &enough_feedback_1,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        loss_based_bandwidth_estimator_2.update_bandwidth_estimate(
            &enough_feedback_1,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );

        assert_eq!(
            loss_based_bandwidth_estimator_1
                .get_loss_based_result()
                .bandwidth_estimate,
            DataRate::from_kilobits_per_sec(660)
        );

        loss_based_bandwidth_estimator_1
            .set_acknowledged_bitrate(DataRate::from_kilobits_per_sec(900));

        assert_eq!(
            loss_based_bandwidth_estimator_1
                .get_loss_based_result()
                .bandwidth_estimate,
            DataRate::from_kilobits_per_sec(660)
        );

        loss_based_bandwidth_estimator_1.update_bandwidth_estimate(
            &enough_feedback_2,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        loss_based_bandwidth_estimator_2.update_bandwidth_estimate(
            &enough_feedback_2,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );

        assert_ne!(
            loss_based_bandwidth_estimator_1
                .get_loss_based_result()
                .bandwidth_estimate,
            loss_based_bandwidth_estimator_2
                .get_loss_based_result()
                .bandwidth_estimate
        );
    }

    #[test]
    fn bandwidth_estimate_is_capped_to_be_tcp_fair_given_too_high_loss_rate() {
        let mut test = LossBasedBweV2Test::default();
        let enough_feedback_no_received_packets: Vec<PacketResult> = test
            .create_packet_results_with_100p_loss_rate(
                /*first_packet_timestamp=*/ Timestamp::zero(),
                2,
            );

        let config = config(true, true);
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);

        loss_based_bandwidth_estimator.set_bandwidth_estimate(DataRate::from_kilobits_per_sec(600));
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_no_received_packets,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );

        assert_eq!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate,
            DataRate::from_kilobits_per_sec(100)
        );
    }

    // When network is normal, estimate can increase but never be higher than
    // the delay based estimate.
    #[test]
    fn bandwidth_estimate_capped_by_delay_based_estimate_when_network_normal() {
        let mut test = LossBasedBweV2Test::default();
        // Create two packet results, network is in normal state, 100% packets are
        // received, and no delay increase.
        let enough_feedback_1: Vec<PacketResult> = test
            .create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/ Timestamp::zero(),
            );
        let enough_feedback_2: Vec<PacketResult> = test
            .create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/
                Timestamp::zero() + 2 * OBSERVATION_DURATION_LOWER_BOUND,
            );
        let config = config(true, true);
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);

        loss_based_bandwidth_estimator.set_bandwidth_estimate(DataRate::from_kilobits_per_sec(600));
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_1,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        // If the delay based estimate is infinity, then loss based estimate increases
        // and not bounded by delay based estimate.
        assert!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate
                > DataRate::from_kilobits_per_sec(600)
        );
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_2,
            /*delay_based_estimate=*/ DataRate::from_kilobits_per_sec(500),
            /*in_alr=*/ false,
        );
        // If the delay based estimate is not infinity, then loss based estimate is
        // bounded by delay based estimate.
        assert_eq!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate,
            DataRate::from_kilobits_per_sec(500)
        );
    }

    // When loss based bwe receives a strong signal of overusing and an increase in
    // loss rate, it should acked bitrate for emegency backoff.
    #[test]
    fn use_acked_bitrate_for_emegency_back_off() {
        let mut test = LossBasedBweV2Test::default();
        // Create two packet results, first packet has 50% loss rate, second packet
        // has 100% loss rate.
        let enough_feedback_1: Vec<PacketResult> = test
            .create_packet_results_with_50p_packet_loss_rate(
                /*first_packet_timestamp=*/ Timestamp::zero(),
            );
        let enough_feedback_2: Vec<PacketResult> = test.create_packet_results_with_100p_loss_rate(
            /*first_packet_timestamp=*/
            Timestamp::zero() + 2 * OBSERVATION_DURATION_LOWER_BOUND,
            2,
        );

        let config = config(true, true);
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);

        loss_based_bandwidth_estimator.set_bandwidth_estimate(DataRate::from_kilobits_per_sec(600));
        let acked_bitrate: DataRate = DataRate::from_kilobits_per_sec(300);
        loss_based_bandwidth_estimator.set_acknowledged_bitrate(acked_bitrate);
        // Update estimate when network is overusing, and 50% loss rate.
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_1,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        // Update estimate again when network is continuously overusing, and 100%
        // loss rate.
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_2,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        // The estimate bitrate now is backed off based on acked bitrate.
        assert!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate
                <= acked_bitrate
        );
    }

    // When receiving the same packet feedback, loss based bwe ignores the feedback
    // and returns the current estimate.
    #[test]
    fn no_bwe_change_if_observation_duration_unchanged() {
        let mut test = LossBasedBweV2Test::default();
        let enough_feedback_1: Vec<PacketResult> = test
            .create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/ Timestamp::zero(),
            );
        let config = config(true, true);
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        loss_based_bandwidth_estimator.set_bandwidth_estimate(DataRate::from_kilobits_per_sec(600));
        loss_based_bandwidth_estimator
            .set_acknowledged_bitrate(DataRate::from_kilobits_per_sec(300));

        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_1,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        let estimate_1: DataRate = loss_based_bandwidth_estimator
            .get_loss_based_result()
            .bandwidth_estimate;

        // Use the same feedback and check if the estimate is unchanged.
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_1,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        let estimate_2: DataRate = loss_based_bandwidth_estimator
            .get_loss_based_result()
            .bandwidth_estimate;
        assert_eq!(estimate_2, estimate_1);
    }

    // When receiving feedback of packets that were sent within an observation
    // duration, and network is in the normal state, loss based bwe returns the
    // current estimate.
    #[test]
    fn no_bwe_change_if_observation_duration_is_small_and_network_normal() {
        let mut test = LossBasedBweV2Test::default();
        let enough_feedback_1: Vec<PacketResult> = test
            .create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/ Timestamp::zero(),
            );
        let enough_feedback_2: Vec<PacketResult> = test
            .create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/
                Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND - TimeDelta::from_millis(1),
            );
        let config = config(true, true);
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        loss_based_bandwidth_estimator.set_bandwidth_estimate(DataRate::from_kilobits_per_sec(600));
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_1,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        let estimate_1: DataRate = loss_based_bandwidth_estimator
            .get_loss_based_result()
            .bandwidth_estimate;

        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_2,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        let estimate_2: DataRate = loss_based_bandwidth_estimator
            .get_loss_based_result()
            .bandwidth_estimate;
        assert_eq!(estimate_2, estimate_1);
    }

    // When receiving feedback of packets that were sent within an observation
    // duration, and network is in the underusing state, loss based bwe returns the
    // current estimate.
    #[test]
    fn no_bwe_increase_if_observation_duration_is_small_and_network_underusing() {
        let mut test = LossBasedBweV2Test::default();
        let enough_feedback_1: Vec<PacketResult> = test
            .create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/ Timestamp::zero(),
            );
        let enough_feedback_2: Vec<PacketResult> = test
            .create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/
                Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND - TimeDelta::from_millis(1),
            );
        let config = config(true, true);
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        loss_based_bandwidth_estimator.set_bandwidth_estimate(DataRate::from_kilobits_per_sec(600));
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_1,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        let estimate_1: DataRate = loss_based_bandwidth_estimator
            .get_loss_based_result()
            .bandwidth_estimate;

        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_2,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        let estimate_2: DataRate = loss_based_bandwidth_estimator
            .get_loss_based_result()
            .bandwidth_estimate;
        assert!(estimate_2 <= estimate_1);
    }

    #[test]
    fn increase_to_delay_based_estimate_if_no_loss_or_delay_increase() {
        let mut test = LossBasedBweV2Test::default();
        let enough_feedback_1: Vec<PacketResult> = test
            .create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/ Timestamp::zero(),
            );
        let enough_feedback_2: Vec<PacketResult> = test
            .create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/
                Timestamp::zero() + 2 * OBSERVATION_DURATION_LOWER_BOUND,
            );
        let config = config(true, true);
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        let delay_based_estimate: DataRate = DataRate::from_kilobits_per_sec(5000);
        loss_based_bandwidth_estimator.set_bandwidth_estimate(DataRate::from_kilobits_per_sec(600));

        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_1,
            delay_based_estimate,
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate,
            delay_based_estimate
        );
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_2,
            delay_based_estimate,
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate,
            delay_based_estimate
        );
    }

    #[test]
    fn increase_by_max_increase_factor_after_loss_based_bwe_backs_off() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            candidate_factors: vec![1.2, 1.0, 0.5],
            instant_upper_bound_bw_balance: DataRate::from_kilobits_per_sec(10000),
            max_increase_factor: 1.5,
            not_increase_if_inherent_loss_less_than_average_loss: false,
            ..short_observation_config()
        };

        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        let delay_based_estimate: DataRate = DataRate::from_kilobits_per_sec(5000);
        let acked_rate: DataRate = DataRate::from_kilobits_per_sec(300);
        loss_based_bandwidth_estimator.set_bandwidth_estimate(DataRate::from_kilobits_per_sec(600));
        loss_based_bandwidth_estimator.set_acknowledged_bitrate(acked_rate);

        // Create some loss to create the loss limited scenario.
        let enough_feedback_1: Vec<PacketResult> = test.create_packet_results_with_100p_loss_rate(
            /*first_packet_timestamp=*/ Timestamp::zero(),
            2,
        );
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_1,
            delay_based_estimate,
            /*in_alr=*/ false,
        );
        let result_at_loss: LossBasedBweV2Result =
            loss_based_bandwidth_estimator.get_loss_based_result();

        // Network recovers after loss.
        let enough_feedback_2: Vec<PacketResult> = test
            .create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/
                Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND,
            );
        loss_based_bandwidth_estimator
            .set_acknowledged_bitrate(DataRate::from_kilobits_per_sec(600));
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_2,
            delay_based_estimate,
            /*in_alr=*/ false,
        );

        let result_after_recovery: LossBasedBweV2Result =
            loss_based_bandwidth_estimator.get_loss_based_result();
        assert_eq!(
            result_after_recovery.bandwidth_estimate,
            result_at_loss.bandwidth_estimate * 1.5
        );
    }

    #[test]
    fn loss_based_state_is_delay_based_estimate_after_network_recovering() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            candidate_factors: vec![1.2, 1.0, 0.5],
            instant_upper_bound_bw_balance: DataRate::from_kilobits_per_sec(10000),
            max_increase_factor: 100.0,
            not_increase_if_inherent_loss_less_than_average_loss: false,
            ..short_observation_config()
        };
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        let delay_based_estimate: DataRate = DataRate::from_kilobits_per_sec(600);
        let acked_rate: DataRate = DataRate::from_kilobits_per_sec(300);
        loss_based_bandwidth_estimator.set_bandwidth_estimate(DataRate::from_kilobits_per_sec(600));
        loss_based_bandwidth_estimator.set_acknowledged_bitrate(acked_rate);

        // Create some loss to create the loss limited scenario.
        let enough_feedback_1: Vec<PacketResult> = test.create_packet_results_with_100p_loss_rate(
            /*first_packet_timestamp=*/ Timestamp::zero(),
            2,
        );
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_1,
            delay_based_estimate,
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::Decreasing
        );

        // Network recovers after loss.
        let enough_feedback_2: Vec<PacketResult> = test
            .create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/
                Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND,
            );
        loss_based_bandwidth_estimator
            .set_acknowledged_bitrate(DataRate::from_kilobits_per_sec(600));
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_2,
            delay_based_estimate,
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::DelayBasedEstimate
        );

        // Network recovers continuing.
        let enough_feedback_3: Vec<PacketResult> = test
            .create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/
                Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND * 2,
            );
        loss_based_bandwidth_estimator
            .set_acknowledged_bitrate(DataRate::from_kilobits_per_sec(600));
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_3,
            delay_based_estimate,
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::DelayBasedEstimate
        );
    }

    #[test]
    fn loss_based_state_is_not_delay_based_estimate_if_delay_based_estimate_infinite() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            candidate_factors: vec![100.0, 1.0, 0.5],
            instant_upper_bound_bw_balance: DataRate::from_kilobits_per_sec(10000),
            max_increase_factor: 100.0,
            ..short_observation_config()
        };
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        loss_based_bandwidth_estimator.set_bandwidth_estimate(DataRate::from_kilobits_per_sec(600));

        // Create some loss to create the loss limited scenario.
        let enough_feedback_1: Vec<PacketResult> = test.create_packet_results_with_100p_loss_rate(
            /*first_packet_timestamp=*/ Timestamp::zero(),
            2,
        );
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_1,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::Decreasing
        );

        // Network recovers after loss.
        let enough_feedback_2: Vec<PacketResult> = test
            .create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/
                Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND,
            );
        loss_based_bandwidth_estimator
            .set_acknowledged_bitrate(DataRate::from_kilobits_per_sec(600));
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_2,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        assert_ne!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::DelayBasedEstimate
        );
    }

    // After loss based bwe backs off, the next estimate is capped by
    // a factor of acked bitrate.
    #[test]
    fn increase_by_factor_of_acked_bitrate_after_loss_based_bwe_backs_off() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            loss_threshold_of_high_bandwidth_preference: 0.99,
            bw_rampup_upper_bound_factor: 1.2,
            instant_upper_bound_bw_balance: DataRate::from_kilobits_per_sec(10000),
            ..short_observation_config()
        };
        let enough_feedback_1: Vec<PacketResult> = test.create_packet_results_with_100p_loss_rate(
            /*first_packet_timestamp=*/ Timestamp::zero(),
            2,
        );
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        let delay_based_estimate: DataRate = DataRate::from_kilobits_per_sec(5000);

        loss_based_bandwidth_estimator.set_bandwidth_estimate(DataRate::from_kilobits_per_sec(600));
        loss_based_bandwidth_estimator
            .set_acknowledged_bitrate(DataRate::from_kilobits_per_sec(300));
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_1,
            delay_based_estimate,
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::Decreasing
        );
        let mut result: LossBasedBweV2Result =
            loss_based_bandwidth_estimator.get_loss_based_result();
        let estimate_1: DataRate = result.bandwidth_estimate;
        assert!(estimate_1.kbps() < 600);

        loss_based_bandwidth_estimator.set_acknowledged_bitrate(estimate_1 * 0.9);

        let mut feedback_count: i64 = 1;
        while feedback_count < 5 && result.state != LossBasedState::Increasing {
            loss_based_bandwidth_estimator.update_bandwidth_estimate(
                &test.create_packet_results_with_received_packets(
                    /*first_packet_timestamp=*/
                    Timestamp::zero() + feedback_count * OBSERVATION_DURATION_LOWER_BOUND,
                ),
                delay_based_estimate,
                /*in_alr=*/ false,
            );
            feedback_count += 1;
            result = loss_based_bandwidth_estimator.get_loss_based_result();
        }
        assert_eq!(result.state, LossBasedState::Increasing);

        // The estimate is capped by acked_bitrate * BwRampupUpperBoundFactor.
        assert_eq!(result.bandwidth_estimate, estimate_1 * 0.9 * 1.2);

        // But if acked bitrate decreases, BWE does not decrease when there is no
        // loss.
        loss_based_bandwidth_estimator.set_acknowledged_bitrate(estimate_1 * 0.9);
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/
                Timestamp::zero() + feedback_count * OBSERVATION_DURATION_LOWER_BOUND,
            ),
            delay_based_estimate,
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate,
            result.bandwidth_estimate
        );
    }

    // Ensure that the state can switch to Increase even when the bandwidth is
    // bounded by acked bitrate.
    #[test]
    fn ensure_increase_even_if_acked_bitrate_bound() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            loss_threshold_of_high_bandwidth_preference: 0.99,
            bw_rampup_upper_bound_factor: 1.2,
            // Set InstantUpperBoundBwBalance high to disable InstantUpperBound cap.
            instant_upper_bound_bw_balance: DataRate::from_kilobits_per_sec(10000),
            ..short_observation_config()
        };
        let enough_feedback_1: Vec<PacketResult> = test.create_packet_results_with_100p_loss_rate(
            /*first_packet_timestamp=*/ Timestamp::zero(),
            2,
        );
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        let delay_based_estimate: DataRate = DataRate::from_kilobits_per_sec(5000);

        loss_based_bandwidth_estimator.set_bandwidth_estimate(DataRate::from_kilobits_per_sec(600));
        loss_based_bandwidth_estimator
            .set_acknowledged_bitrate(DataRate::from_kilobits_per_sec(300));
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_1,
            delay_based_estimate,
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::Decreasing
        );
        let mut result: LossBasedBweV2Result =
            loss_based_bandwidth_estimator.get_loss_based_result();
        let estimate_1: DataRate = result.bandwidth_estimate;
        assert!(estimate_1.kbps() < 600);

        // Set a low acked bitrate.
        loss_based_bandwidth_estimator.set_acknowledged_bitrate(estimate_1 / 2);

        let mut feedback_count: i64 = 1;
        while feedback_count < 5 && result.state != LossBasedState::Increasing {
            loss_based_bandwidth_estimator.update_bandwidth_estimate(
                &test.create_packet_results_with_received_packets(
                    /*first_packet_timestamp=*/
                    Timestamp::zero() + feedback_count * OBSERVATION_DURATION_LOWER_BOUND,
                ),
                delay_based_estimate,
                /*in_alr=*/ false,
            );
            feedback_count += 1;
            result = loss_based_bandwidth_estimator.get_loss_based_result();
        }

        assert_eq!(result.state, LossBasedState::Increasing);
        // The estimate increases by 1kbps.
        assert_eq!(
            result.bandwidth_estimate,
            estimate_1 + DataRate::from_bits_per_sec(1)
        );
    }

    // After loss based bwe backs off, the estimate is bounded during the delayed
    // window.
    #[test]
    fn estimate_bitrate_is_bounded_during_delayed_window_after_loss_based_bwe_backs_off() {
        let mut test = LossBasedBweV2Test::default();
        let enough_feedback_1: Vec<PacketResult> = test
            .create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/ Timestamp::zero(),
            );
        let enough_feedback_2: Vec<PacketResult> = test
            .create_packet_results_with_50p_packet_loss_rate(
                /*first_packet_timestamp=*/
                Timestamp::zero() + DELAYED_INCREASE_WINDOW - TimeDelta::from_millis(2),
            );
        let enough_feedback_3: Vec<PacketResult> = test
            .create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/
                Timestamp::zero() + DELAYED_INCREASE_WINDOW - TimeDelta::from_millis(1),
            );
        let config = config(true, true);
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        let delay_based_estimate: DataRate = DataRate::from_kilobits_per_sec(5000);

        loss_based_bandwidth_estimator.set_bandwidth_estimate(DataRate::from_kilobits_per_sec(600));
        loss_based_bandwidth_estimator
            .set_acknowledged_bitrate(DataRate::from_kilobits_per_sec(300));
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_1,
            delay_based_estimate,
            /*in_alr=*/ false,
        );
        // Increase the acknowledged bitrate to make sure that the estimate is not
        // capped too low.
        loss_based_bandwidth_estimator
            .set_acknowledged_bitrate(DataRate::from_kilobits_per_sec(5000));
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_2,
            delay_based_estimate,
            /*in_alr=*/ false,
        );

        // The estimate is capped by current_estimate * MaxIncreaseFactor because
        // it recently backed off.
        let estimate_2: DataRate = loss_based_bandwidth_estimator
            .get_loss_based_result()
            .bandwidth_estimate;

        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_3,
            delay_based_estimate,
            /*in_alr=*/ false,
        );
        // The latest estimate is the same as the previous estimate since the sent
        // packets were sent within the DelayedIncreaseWindow.
        assert_eq!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate,
            estimate_2
        );
    }

    // The estimate is not bounded after the delayed increase window.
    #[test]
    fn keep_increasing_estimate_after_delayed_increase_window() {
        let mut test = LossBasedBweV2Test::default();
        let enough_feedback_1: Vec<PacketResult> = test
            .create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/ Timestamp::zero(),
            );
        let enough_feedback_2: Vec<PacketResult> = test
            .create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/
                Timestamp::zero() + DELAYED_INCREASE_WINDOW - TimeDelta::from_millis(1),
            );
        let enough_feedback_3: Vec<PacketResult> = test
            .create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/
                Timestamp::zero() + DELAYED_INCREASE_WINDOW + TimeDelta::from_millis(1),
            );
        let config = config(true, true);
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        let delay_based_estimate: DataRate = DataRate::from_kilobits_per_sec(5000);

        loss_based_bandwidth_estimator.set_bandwidth_estimate(DataRate::from_kilobits_per_sec(600));
        loss_based_bandwidth_estimator
            .set_acknowledged_bitrate(DataRate::from_kilobits_per_sec(300));
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_1,
            delay_based_estimate,
            /*in_alr=*/ false,
        );
        // Increase the acknowledged bitrate to make sure that the estimate is not
        // capped too low.
        loss_based_bandwidth_estimator
            .set_acknowledged_bitrate(DataRate::from_kilobits_per_sec(5000));
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_2,
            delay_based_estimate,
            /*in_alr=*/ false,
        );

        // The estimate is capped by current_estimate * MaxIncreaseFactor because it
        // recently backed off.
        let estimate_2: DataRate = loss_based_bandwidth_estimator
            .get_loss_based_result()
            .bandwidth_estimate;

        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_3,
            delay_based_estimate,
            /*in_alr=*/ false,
        );
        // The estimate can continue increasing after the DelayedIncreaseWindow.
        assert!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate
                >= estimate_2
        );
    }

    #[test]
    fn not_increase_if_inherent_loss_less_than_average_loss() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            candidate_factors: vec![1.25],
            not_increase_if_inherent_loss_less_than_average_loss: true,
            ..short_observation_config()
        };
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);

        loss_based_bandwidth_estimator.set_bandwidth_estimate(DataRate::from_kilobits_per_sec(600));

        let enough_feedback_10p_loss_1: Vec<PacketResult> = test
            .create_packet_results_with_10p_packet_loss_rate(
                /*first_packet_timestamp=*/ Timestamp::zero(),
                DataSize::from_bytes(PACKET_SIZE),
            );
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_10p_loss_1,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );

        let enough_feedback_10p_loss_2: Vec<PacketResult> = test
            .create_packet_results_with_10p_packet_loss_rate(
                /*first_packet_timestamp=*/
                Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND,
                DataSize::from_bytes(PACKET_SIZE),
            );
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_10p_loss_2,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );

        // Do not increase the bitrate because inherent loss is less than average loss
        assert_eq!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate,
            DataRate::from_kilobits_per_sec(600)
        );
    }

    #[test]
    fn select_high_bandwidth_candidate_if_loss_rate_is_less_than_threshold() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            loss_threshold_of_high_bandwidth_preference: 0.20,
            not_increase_if_inherent_loss_less_than_average_loss: false,
            ..short_observation_config()
        };
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        let delay_based_estimate: DataRate = DataRate::from_kilobits_per_sec(5000);

        loss_based_bandwidth_estimator.set_bandwidth_estimate(DataRate::from_kilobits_per_sec(600));

        let enough_feedback_10p_loss_1: Vec<PacketResult> = test
            .create_packet_results_with_10p_packet_loss_rate(
                /*first_packet_timestamp=*/ Timestamp::zero(),
                DataSize::from_bytes(PACKET_SIZE),
            );
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_10p_loss_1,
            delay_based_estimate,
            /*in_alr=*/ false,
        );

        let enough_feedback_10p_loss_2: Vec<PacketResult> = test
            .create_packet_results_with_10p_packet_loss_rate(
                /*first_packet_timestamp=*/
                Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND,
                DataSize::from_bytes(PACKET_SIZE),
            );
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_10p_loss_2,
            delay_based_estimate,
            /*in_alr=*/ false,
        );

        // Because LossThresholdOfHighBandwidthPreference is 20%, the average loss is
        // 10%, bandwidth estimate should increase.
        assert!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate
                > DataRate::from_kilobits_per_sec(600)
        );
    }

    #[test]
    fn select_low_bandwidth_candidate_if_loss_rate_is_is_higher_than_threshold() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            loss_threshold_of_high_bandwidth_preference: 0.05,
            ..short_observation_config()
        };
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        let delay_based_estimate: DataRate = DataRate::from_kilobits_per_sec(5000);

        loss_based_bandwidth_estimator.set_bandwidth_estimate(DataRate::from_kilobits_per_sec(600));

        let enough_feedback_10p_loss_1: Vec<PacketResult> = test
            .create_packet_results_with_10p_packet_loss_rate(
                /*first_packet_timestamp=*/ Timestamp::zero(),
                DataSize::from_bytes(PACKET_SIZE),
            );
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_10p_loss_1,
            delay_based_estimate,
            /*in_alr=*/ false,
        );

        let enough_feedback_10p_loss_2: Vec<PacketResult> = test
            .create_packet_results_with_10p_packet_loss_rate(
                /*first_packet_timestamp=*/
                Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND,
                DataSize::from_bytes(PACKET_SIZE),
            );
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_10p_loss_2,
            delay_based_estimate,
            /*in_alr=*/ false,
        );

        // Because LossThresholdOfHighBandwidthPreference is 5%, the average loss is
        // 10%, bandwidth estimate should decrease.
        assert!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate
                < DataRate::from_kilobits_per_sec(600)
        );
    }

    #[test]
    fn estimate_is_not_higher_than_max_bitrate() {
        let mut test = LossBasedBweV2Test::default();
        let config = config(true, true);
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        loss_based_bandwidth_estimator.set_min_max_bitrate(
            /*min_bitrate=*/ DataRate::from_kilobits_per_sec(10),
            /*max_bitrate=*/ DataRate::from_kilobits_per_sec(1000),
        );
        loss_based_bandwidth_estimator
            .set_bandwidth_estimate(DataRate::from_kilobits_per_sec(1000));
        let enough_feedback: Vec<PacketResult> = test.create_packet_results_with_received_packets(
            /*first_packet_timestamp=*/ Timestamp::zero(),
        );
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );

        assert!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate
                <= DataRate::from_kilobits_per_sec(1000)
        );
    }

    #[test]
    fn not_back_off_to_acked_rate_in_alr() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            instant_upper_bound_bw_balance: DataRate::from_kilobits_per_sec(100),
            ..short_observation_config()
        };
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        loss_based_bandwidth_estimator.set_min_max_bitrate(
            /*min_bitrate=*/ DataRate::from_kilobits_per_sec(10),
            /*max_bitrate=*/ DataRate::from_kilobits_per_sec(1000000),
        );
        let delay_based_estimate: DataRate = DataRate::from_kilobits_per_sec(5000);
        loss_based_bandwidth_estimator.set_bandwidth_estimate(DataRate::from_kilobits_per_sec(600));

        let acked_rate: DataRate = DataRate::from_kilobits_per_sec(100);
        loss_based_bandwidth_estimator.set_acknowledged_bitrate(acked_rate);
        let enough_feedback_100p_loss_1: Vec<PacketResult> = test
            .create_packet_results_with_100p_loss_rate(
                /*first_packet_timestamp=*/ Timestamp::zero(),
                2,
            );
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_100p_loss_1,
            delay_based_estimate,
            /*in_alr=*/ true,
        );

        // Make sure that the estimate decreases but higher than acked rate.
        assert!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate
                > acked_rate
        );

        assert!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate
                < DataRate::from_kilobits_per_sec(600)
        );
    }

    #[test]
    fn back_off_to_acked_rate_if_not_in_alr() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            instant_upper_bound_bw_balance: DataRate::from_kilobits_per_sec(100),
            ..short_observation_config()
        };
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        loss_based_bandwidth_estimator.set_min_max_bitrate(
            /*min_bitrate=*/ DataRate::from_kilobits_per_sec(10),
            /*max_bitrate=*/ DataRate::from_kilobits_per_sec(1000000),
        );
        let delay_based_estimate: DataRate = DataRate::from_kilobits_per_sec(5000);
        loss_based_bandwidth_estimator.set_bandwidth_estimate(DataRate::from_kilobits_per_sec(600));

        let acked_rate: DataRate = DataRate::from_kilobits_per_sec(100);
        loss_based_bandwidth_estimator.set_acknowledged_bitrate(acked_rate);
        let enough_feedback_100p_loss_1: Vec<PacketResult> = test
            .create_packet_results_with_100p_loss_rate(
                /*first_packet_timestamp=*/ Timestamp::zero(),
                2,
            );
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_100p_loss_1,
            delay_based_estimate,
            /*in_alr=*/ false,
        );

        // Make sure that the estimate decreases but higher than acked rate.
        assert_eq!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate,
            acked_rate
        );
    }

    #[test]
    fn not_ready_to_use_in_start_phase() {
        let config = LossBasedBweV2Config {
            use_in_start_phase: true,
            ..short_observation_config()
        };
        let loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        // Make sure that the estimator is not ready to use in start phase because of
        // lacking TWCC feedback.
        assert!(!loss_based_bandwidth_estimator.ready_to_use_in_start_phase());
    }

    #[test]
    fn ready_to_use_in_start_phase() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            use_in_start_phase: true,
            ..short_observation_config()
        };
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        let enough_feedback: Vec<PacketResult> = test.create_packet_results_with_received_packets(
            /*first_packet_timestamp=*/ Timestamp::zero(),
        );

        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback,
            /*delay_based_estimate=*/ DataRate::from_kilobits_per_sec(600),
            /*in_alr=*/ false,
        );
        assert!(loss_based_bandwidth_estimator.ready_to_use_in_start_phase());
    }

    #[test]
    fn bound_estimate_by_acked_rate() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            lower_bound_by_acked_rate_factor: 1.0,
            ..short_observation_config()
        };
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        loss_based_bandwidth_estimator.set_min_max_bitrate(
            /*min_bitrate=*/ DataRate::from_kilobits_per_sec(10),
            /*max_bitrate=*/ DataRate::from_kilobits_per_sec(1000000),
        );
        loss_based_bandwidth_estimator.set_bandwidth_estimate(DataRate::from_kilobits_per_sec(600));
        loss_based_bandwidth_estimator
            .set_acknowledged_bitrate(DataRate::from_kilobits_per_sec(500));

        let enough_feedback_100p_loss_1: Vec<PacketResult> = test
            .create_packet_results_with_100p_loss_rate(
                /*first_packet_timestamp=*/ Timestamp::zero(),
                2,
            );
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_100p_loss_1,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );

        assert_eq!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate,
            DataRate::from_kilobits_per_sec(500)
        );
    }

    #[test]
    fn not_bound_estimate_by_acked_rate() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            lower_bound_by_acked_rate_factor: 0.0,
            ..short_observation_config()
        };
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        loss_based_bandwidth_estimator.set_min_max_bitrate(
            /*min_bitrate=*/ DataRate::from_kilobits_per_sec(10),
            /*max_bitrate=*/ DataRate::from_kilobits_per_sec(1000000),
        );
        loss_based_bandwidth_estimator.set_bandwidth_estimate(DataRate::from_kilobits_per_sec(600));
        loss_based_bandwidth_estimator
            .set_acknowledged_bitrate(DataRate::from_kilobits_per_sec(500));

        let enough_feedback_100p_loss_1: Vec<PacketResult> = test
            .create_packet_results_with_100p_loss_rate(
                /*first_packet_timestamp=*/ Timestamp::zero(),
                2,
            );
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_100p_loss_1,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );

        assert!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate
                < DataRate::from_kilobits_per_sec(500)
        );
    }

    #[test]
    fn has_decrease_state_because_of_upper_bound() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            candidate_factors: vec![1.0],
            instant_upper_bound_bw_balance: DataRate::from_kilobits_per_sec(10),
            ..short_observation_config()
        };
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        loss_based_bandwidth_estimator.set_min_max_bitrate(
            /*min_bitrate=*/ DataRate::from_kilobits_per_sec(10),
            /*max_bitrate=*/ DataRate::from_kilobits_per_sec(1000000),
        );
        loss_based_bandwidth_estimator.set_bandwidth_estimate(DataRate::from_kilobits_per_sec(500));
        loss_based_bandwidth_estimator
            .set_acknowledged_bitrate(DataRate::from_kilobits_per_sec(500));

        let enough_feedback_10p_loss_1: Vec<PacketResult> = test
            .create_packet_results_with_10p_packet_loss_rate(
                /*first_packet_timestamp=*/ Timestamp::zero(),
                DataSize::from_bytes(PACKET_SIZE),
            );
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_10p_loss_1,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );

        // Verify that the instant upper bound decreases the estimate, and state is
        // updated to Decreasing.
        assert_eq!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate,
            DataRate::from_kilobits_per_sec(200)
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::Decreasing
        );
    }

    #[test]
    fn has_increase_state_because_of_lower_bound() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            candidate_factors: vec![1.0],
            lower_bound_by_acked_rate_factor: 10.0,
            ..short_observation_config()
        };
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        loss_based_bandwidth_estimator.set_min_max_bitrate(
            /*min_bitrate=*/ DataRate::from_kilobits_per_sec(10),
            /*max_bitrate=*/ DataRate::from_kilobits_per_sec(1000000),
        );
        loss_based_bandwidth_estimator.set_bandwidth_estimate(DataRate::from_kilobits_per_sec(500));
        loss_based_bandwidth_estimator.set_acknowledged_bitrate(DataRate::from_kilobits_per_sec(1));

        // Network has a high loss to create a loss scenario.
        let enough_feedback_50p_loss_1: Vec<PacketResult> = test
            .create_packet_results_with_50p_packet_loss_rate(
                /*first_packet_timestamp=*/ Timestamp::zero(),
            );
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_50p_loss_1,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );

        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::Decreasing
        );

        // Network still has a high loss, but better acked rate.
        loss_based_bandwidth_estimator
            .set_acknowledged_bitrate(DataRate::from_kilobits_per_sec(200));
        let enough_feedback_50p_loss_2: Vec<PacketResult> = test
            .create_packet_results_with_50p_packet_loss_rate(
                /*first_packet_timestamp=*/
                Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND,
            );
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &enough_feedback_50p_loss_2,
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );

        // Verify that the instant lower bound increases the estimate, and state is
        // updated to Increasing.
        assert_eq!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate,
            DataRate::from_kilobits_per_sec(200) * 10
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::Increasing
        );
    }

    #[test]
    fn estimate_increase_slowly_from_instant_upper_bound_in_alr_if_field_trial() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            upper_bound_candidate_in_alr: true,
            ..short_observation_config()
        };
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        loss_based_bandwidth_estimator
            .set_bandwidth_estimate(DataRate::from_kilobits_per_sec(1000));
        loss_based_bandwidth_estimator
            .set_acknowledged_bitrate(DataRate::from_kilobits_per_sec(150));
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_50p_packet_loss_rate(
                /*first_packet_timestamp=*/ Timestamp::zero(),
            ),
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ true,
        );
        let result_after_loss: LossBasedBweV2Result =
            loss_based_bandwidth_estimator.get_loss_based_result();
        assert_eq!(result_after_loss.state, LossBasedState::Decreasing);

        for feedback_count in 1..=3 {
            loss_based_bandwidth_estimator.update_bandwidth_estimate(
                &test.create_packet_results_with_received_packets(
                    /*first_packet_timestamp=*/
                    Timestamp::zero() + feedback_count * OBSERVATION_DURATION_LOWER_BOUND,
                ),
                /*delay_based_estimate=*/ DataRate::plus_infinity(),
                /*in_alr=*/ true,
            );
        }
        // Expect less than 100% increase.
        assert!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate
                < 2 * result_after_loss.bandwidth_estimate
        );
    }

    #[test]
    fn has_delay_based_state_if_loss_based_bwe_is_max() {
        let mut test = LossBasedBweV2Test::default();
        let config = short_observation_config();
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        loss_based_bandwidth_estimator.set_min_max_bitrate(
            /*min_bitrate=*/ DataRate::from_kilobits_per_sec(10),
            /*max_bitrate=*/ DataRate::from_kilobits_per_sec(1000),
        );

        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            /*feedback = */
            &test.create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/ Timestamp::zero(),
            ),
            /*delay_based_estimate=*/ DataRate::from_kilobits_per_sec(2000),
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::DelayBasedEstimate
        );
        assert_eq!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate,
            DataRate::from_kilobits_per_sec(1000)
        );

        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            /*feedback=*/
            &test.create_packet_results_with_50p_packet_loss_rate(
                /*first_packet_timestamp=*/
                Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND,
            ),
            /*delay_based_estimate=*/ DataRate::from_kilobits_per_sec(2000),
            /*in_alr=*/ false,
        );
        let mut result: LossBasedBweV2Result =
            loss_based_bandwidth_estimator.get_loss_based_result();
        assert_eq!(result.state, LossBasedState::Decreasing);
        assert!(result.bandwidth_estimate < DataRate::from_kilobits_per_sec(1000));

        // Eventually  the estimator recovers to delay based state.
        let mut feedback_count: i64 = 2;
        while feedback_count < 5 && result.state != LossBasedState::DelayBasedEstimate {
            loss_based_bandwidth_estimator.update_bandwidth_estimate(
                /*feedback = */
                &test.create_packet_results_with_received_packets(
                    /*first_packet_timestamp=*/
                    Timestamp::zero() + feedback_count * OBSERVATION_DURATION_LOWER_BOUND,
                ),
                /*delay_based_estimate=*/ DataRate::from_kilobits_per_sec(2000),
                /*in_alr=*/ false,
            );
            feedback_count += 1;
            result = loss_based_bandwidth_estimator.get_loss_based_result();
        }
        assert_eq!(result.state, LossBasedState::DelayBasedEstimate);
        assert_eq!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate,
            DataRate::from_kilobits_per_sec(1000)
        );
    }

    #[test]
    fn increase_using_padding_state_if_field_trial() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            padding_duration: TimeDelta::from_millis(1000),
            ..short_observation_config()
        };
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        loss_based_bandwidth_estimator
            .set_bandwidth_estimate(DataRate::from_kilobits_per_sec(2500));
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_50p_packet_loss_rate(
                /*first_packet_timestamp=*/ Timestamp::zero(),
            ),
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::Decreasing
        );

        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/
                Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND,
            ),
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::IncreaseUsingPadding
        );
    }

    #[test]
    fn best_candidate_resets_to_upper_bound_in_field_trial() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            padding_duration: TimeDelta::from_millis(1000),
            bound_best_candidate: true,
            ..short_observation_config()
        };
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        loss_based_bandwidth_estimator
            .set_bandwidth_estimate(DataRate::from_kilobits_per_sec(2500));
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_50p_packet_loss_rate(
                /*first_packet_timestamp=*/ Timestamp::zero(),
            ),
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ true,
        );
        let result_after_loss: LossBasedBweV2Result =
            loss_based_bandwidth_estimator.get_loss_based_result();
        assert_eq!(result_after_loss.state, LossBasedState::Decreasing);

        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/
                Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND,
            ),
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ true,
        );
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/
                Timestamp::zero() + 2 * OBSERVATION_DURATION_LOWER_BOUND,
            ),
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ true,
        );
        // After a BWE decrease due to large loss, BWE is expected to ramp up slowly
        // and follow the acked bitrate.
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::IncreaseUsingPadding
        );
        assert_relative_eq!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate
                .kbps_float(),
            result_after_loss.bandwidth_estimate.kbps_float(),
            epsilon = 100.0
        );
    }

    #[test]
    fn decrease_to_acked_candidate_if_padding_in_alr() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            padding_duration: TimeDelta::from_millis(1000),
            // Set InstantUpperBoundBwBalance high to disable InstantUpperBound cap.
            instant_upper_bound_bw_balance: DataRate::from_kilobits_per_sec(10000),
            ..short_observation_config()
        };
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        loss_based_bandwidth_estimator
            .set_bandwidth_estimate(DataRate::from_kilobits_per_sec(1000));
        let mut feedback_id: i64 = 0;
        while loss_based_bandwidth_estimator.get_loss_based_result().state
            != LossBasedState::Decreasing
        {
            loss_based_bandwidth_estimator.update_bandwidth_estimate(
                &test.create_packet_results_with_100p_loss_rate(
                    /*first_packet_timestamp=*/
                    Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND * feedback_id,
                    2,
                ),
                /*delay_based_estimate=*/ DataRate::plus_infinity(),
                /*in_alr=*/ true,
            );
            feedback_id += 1;
        }

        while loss_based_bandwidth_estimator.get_loss_based_result().state
            != LossBasedState::IncreaseUsingPadding
        {
            loss_based_bandwidth_estimator.update_bandwidth_estimate(
                &test.create_packet_results_with_received_packets(
                    /*first_packet_timestamp=*/
                    Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND * feedback_id,
                ),
                /*delay_based_estimate=*/ DataRate::plus_infinity(),
                /*in_alr=*/ true,
            );
            feedback_id += 1;
        }
        assert!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate
                > DataRate::from_kilobits_per_sec(900)
        );

        loss_based_bandwidth_estimator
            .set_acknowledged_bitrate(DataRate::from_kilobits_per_sec(100));
        // Padding is sent now, create some lost packets.
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_100p_loss_rate(
                /*first_packet_timestamp=*/
                Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND * feedback_id,
                2,
            ),
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ true,
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::Decreasing
        );
        assert_eq!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate,
            DataRate::from_kilobits_per_sec(100)
        );
    }

    #[test]
    fn decrease_after_padding() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            padding_duration: TimeDelta::from_millis(1000),
            bw_rampup_upper_bound_factor: 2.0,
            ..short_observation_config()
        };
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        loss_based_bandwidth_estimator
            .set_bandwidth_estimate(DataRate::from_kilobits_per_sec(2500));
        let mut acknowledged_bitrate: DataRate = DataRate::from_kilobits_per_sec(51);
        loss_based_bandwidth_estimator.set_acknowledged_bitrate(acknowledged_bitrate);
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_50p_packet_loss_rate(
                /*first_packet_timestamp=*/ Timestamp::zero(),
            ),
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::Decreasing
        );
        assert_eq!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate,
            acknowledged_bitrate
        );

        acknowledged_bitrate = DataRate::from_kilobits_per_sec(26);
        loss_based_bandwidth_estimator.set_acknowledged_bitrate(acknowledged_bitrate);
        let mut feedback_id: i64 = 1;
        while loss_based_bandwidth_estimator.get_loss_based_result().state
            != LossBasedState::IncreaseUsingPadding
        {
            loss_based_bandwidth_estimator.update_bandwidth_estimate(
                &test.create_packet_results_with_received_packets(
                    /*first_packet_timestamp=*/
                    Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND * feedback_id,
                ),
                /*delay_based_estimate=*/ DataRate::plus_infinity(),
                /*in_alr=*/ false,
            );
            feedback_id += 1;
        }

        let estimate_increased: Timestamp =
            Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND * feedback_id;
        // The state is IncreaseUsingPadding for a while without changing the
        // estimate, which is limited by 2 * acked rate.
        while loss_based_bandwidth_estimator.get_loss_based_result().state
            == LossBasedState::IncreaseUsingPadding
        {
            loss_based_bandwidth_estimator.update_bandwidth_estimate(
                &test.create_packet_results_with_received_packets(
                    /*first_packet_timestamp=*/
                    Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND * feedback_id,
                ),
                /*delay_based_estimate=*/ DataRate::plus_infinity(),
                /*in_alr=*/ false,
            );
            feedback_id += 1;
        }

        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::Decreasing
        );
        let start_decreasing: Timestamp =
            Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND * (feedback_id - 1);
        assert_eq!(
            start_decreasing - estimate_increased,
            TimeDelta::from_seconds(1)
        );
    }

    #[test]
    fn increase_estimate_if_not_hold() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            hold_duration_factor: 0.0,
            ..short_observation_config()
        };
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        loss_based_bandwidth_estimator
            .set_bandwidth_estimate(DataRate::from_kilobits_per_sec(2500));
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_50p_packet_loss_rate(
                /*first_packet_timestamp=*/ Timestamp::zero(),
            ),
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::Decreasing
        );
        let estimate: DataRate = loss_based_bandwidth_estimator
            .get_loss_based_result()
            .bandwidth_estimate;

        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/
                Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND,
            ),
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::Increasing
        );
        assert!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate
                > estimate
        );
    }

    #[test]
    fn increase_estimate_after_hold_duration() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            hold_duration_factor: 10.0,
            ..short_observation_config()
        };
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        loss_based_bandwidth_estimator
            .set_bandwidth_estimate(DataRate::from_kilobits_per_sec(2500));
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_50p_packet_loss_rate(
                /*first_packet_timestamp=*/ Timestamp::zero(),
            ),
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::Decreasing
        );
        let estimate: DataRate = loss_based_bandwidth_estimator
            .get_loss_based_result()
            .bandwidth_estimate;

        // During the hold duration, e.g. first 300ms, the estimate cannot increase.
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/
                Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND,
            ),
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::Decreasing
        );
        assert_eq!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate,
            estimate
        );

        // After the hold duration, the estimate can increase.
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/
                Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND * 2,
            ),
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::Increasing
        );
        assert!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate
                >= estimate
        );

        // Get another 50p packet loss.
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_50p_packet_loss_rate(
                /*first_packet_timestamp=*/
                Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND * 3,
            ),
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::Decreasing
        );
        let estimate_at_hold: DataRate = loss_based_bandwidth_estimator
            .get_loss_based_result()
            .bandwidth_estimate;

        // In the hold duration, e.g. next 3s, the estimate cannot increase above the
        // hold rate. Get some lost packets to get lower estimate than the HOLD rate.
        for i in 4..=6 {
            loss_based_bandwidth_estimator.update_bandwidth_estimate(
                &test.create_packet_results_with_100p_loss_rate(
                    /*first_packet_timestamp=*/
                    Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND * i,
                    2,
                ),
                /*delay_based_estimate=*/ DataRate::plus_infinity(),
                /*in_alr=*/ false,
            );
            assert_eq!(
                loss_based_bandwidth_estimator.get_loss_based_result().state,
                LossBasedState::Decreasing
            );
            assert!(
                loss_based_bandwidth_estimator
                    .get_loss_based_result()
                    .bandwidth_estimate
                    < estimate_at_hold
            );
        }

        let mut feedback_id: i64 = 7;
        while loss_based_bandwidth_estimator.get_loss_based_result().state
            != LossBasedState::Increasing
        {
            loss_based_bandwidth_estimator.update_bandwidth_estimate(
                &test.create_packet_results_with_received_packets(
                    /*first_packet_timestamp=*/
                    Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND * feedback_id,
                ),
                /*delay_based_estimate=*/ DataRate::plus_infinity(),
                /*in_alr=*/ false,
            );
            if loss_based_bandwidth_estimator.get_loss_based_result().state
                == LossBasedState::Decreasing
            {
                // In the hold duration, the estimate can not go higher than estimate at
                // hold.
                assert!(
                    loss_based_bandwidth_estimator
                        .get_loss_based_result()
                        .bandwidth_estimate
                        <= estimate_at_hold
                );
            } else if loss_based_bandwidth_estimator.get_loss_based_result().state
                == LossBasedState::Increasing
            {
                // After the hold duration, the estimate can increase again.
                assert!(
                    loss_based_bandwidth_estimator
                        .get_loss_based_result()
                        .bandwidth_estimate
                        > estimate_at_hold
                );
            }
            feedback_id += 1;
        }
    }

    #[test]
    fn hold_rate_not_lower_than_acked_rate() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            hold_duration_factor: 10.0,
            lower_bound_by_acked_rate_factor: 1.0,
            ..short_observation_config()
        };
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        loss_based_bandwidth_estimator
            .set_bandwidth_estimate(DataRate::from_kilobits_per_sec(2500));
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_50p_packet_loss_rate(
                /*first_packet_timestamp=*/ Timestamp::zero(),
            ),
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::Decreasing
        );

        // During the hold duration, hold rate is not lower than the acked rate.
        loss_based_bandwidth_estimator
            .set_acknowledged_bitrate(DataRate::from_kilobits_per_sec(1000));
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_50p_packet_loss_rate(
                /*first_packet_timestamp=*/
                Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND,
            ),
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::Decreasing
        );
        assert_eq!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate,
            DataRate::from_kilobits_per_sec(1000)
        );
    }

    #[test]
    fn estimate_not_lower_than_acked_rate() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            lower_bound_by_acked_rate_factor: 1.0,
            ..short_observation_config()
        };
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        loss_based_bandwidth_estimator
            .set_bandwidth_estimate(DataRate::from_kilobits_per_sec(2500));
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_100p_loss_rate(
                /*first_packet_timestamp=*/ Timestamp::zero(),
                2,
            ),
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        assert!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate
                < DataRate::from_kilobits_per_sec(1000)
        );

        loss_based_bandwidth_estimator
            .set_acknowledged_bitrate(DataRate::from_kilobits_per_sec(1000));
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_100p_loss_rate(
                /*first_packet_timestamp=*/
                Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND,
                2,
            ),
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate,
            DataRate::from_kilobits_per_sec(1000)
        );

        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/
                Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND * 2,
            ),
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/
                Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND * 3,
            ),
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );

        // Verify that the estimate recovers from the acked rate.
        assert!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate
                > DataRate::from_kilobits_per_sec(1000)
        );
    }

    #[test]
    fn end_hold_duration_if_delay_based_estimate_works() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            hold_duration_factor: 3.0,
            ..short_observation_config()
        };
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        loss_based_bandwidth_estimator
            .set_bandwidth_estimate(DataRate::from_kilobits_per_sec(2500));
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_50p_packet_loss_rate(
                /*first_packet_timestamp=*/ Timestamp::zero(),
            ),
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::Decreasing
        );
        let estimate: DataRate = loss_based_bandwidth_estimator
            .get_loss_based_result()
            .bandwidth_estimate;

        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/
                Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND,
            ),
            /*delay_based_estimate=*/ estimate + DataRate::from_kilobits_per_sec(10),
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::DelayBasedEstimate
        );
        assert_eq!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate,
            estimate + DataRate::from_kilobits_per_sec(10)
        );
    }

    #[test]
    fn use_byte_loss_rate() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            use_byte_loss_rate: true,
            ..short_observation_config()
        };
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        loss_based_bandwidth_estimator.set_bandwidth_estimate(DataRate::from_kilobits_per_sec(500));
        // Create packet feedback having 10% packet loss but more than 50% byte loss.
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_10p_packet_loss_rate(
                /*first_packet_timestamp=*/ Timestamp::zero(),
                /*lost_packet_size=*/ DataSize::from_bytes(PACKET_SIZE * 20),
            ),
            /*delay_based_estimate=*/ DataRate::plus_infinity(),
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::Decreasing
        );
        // The estimate is bounded by the instant upper bound due to high loss.
        assert!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate
                < DataRate::from_kilobits_per_sec(150)
        );
    }

    #[test]
    fn use_byte_loss_rate_ignore_loss_spike() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            use_byte_loss_rate: true,
            observation_window_size: 5,
            ..short_observation_config()
        };
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        const DELAY_BASED_ESTIMATE: DataRate = DataRate::from_kilobits_per_sec(500);
        loss_based_bandwidth_estimator.set_bandwidth_estimate(DELAY_BASED_ESTIMATE);

        // Fill the observation window.
        for i in 0..5 {
            loss_based_bandwidth_estimator.update_bandwidth_estimate(
                &test.create_packet_results_with_received_packets(
                    /*first_packet_timestamp=*/
                    Timestamp::zero() + i * OBSERVATION_DURATION_LOWER_BOUND,
                ),
                DELAY_BASED_ESTIMATE,
                /*in_alr=*/ false,
            );
        }
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_100p_loss_rate(
                /*first_packet_timestamp=*/
                Timestamp::zero() + 5 * OBSERVATION_DURATION_LOWER_BOUND,
                2,
            ),
            DELAY_BASED_ESTIMATE,
            /*in_alr=*/ false,
        );
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/
                Timestamp::zero() + 6 * OBSERVATION_DURATION_LOWER_BOUND,
            ),
            DELAY_BASED_ESTIMATE,
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::DelayBasedEstimate
        );
        assert_eq!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate,
            DELAY_BASED_ESTIMATE
        );

        // But if more loss happen in a new observation, BWE back down.
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_100p_loss_rate(
                /*first_packet_timestamp=*/
                Timestamp::zero() + 7 * OBSERVATION_DURATION_LOWER_BOUND,
                2,
            ),
            DELAY_BASED_ESTIMATE,
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::Decreasing
        );
        assert!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate
                < DELAY_BASED_ESTIMATE
        );
    }

    #[test]
    fn use_byte_loss_rate_does_not_ignore_loss_spike_on_send_burst() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            use_byte_loss_rate: true,
            observation_window_size: 5,
            ..short_observation_config()
        };
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        const DELAY_BASED_ESTIMATE: DataRate = DataRate::from_kilobits_per_sec(500);
        loss_based_bandwidth_estimator.set_bandwidth_estimate(DELAY_BASED_ESTIMATE);

        // Fill the observation window.
        for i in 0..5 {
            loss_based_bandwidth_estimator.update_bandwidth_estimate(
                &test.create_packet_results_with_received_packets(
                    /*first_packet_timestamp=*/
                    Timestamp::zero() + i * OBSERVATION_DURATION_LOWER_BOUND,
                ),
                DELAY_BASED_ESTIMATE,
                /*in_alr=*/ false,
            );
        }

        // If the loss happens when increasing sending rate, then
        // the BWE should back down.
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_100p_loss_rate(
                /*first_packet_timestamp=*/
                Timestamp::zero() + 5 * OBSERVATION_DURATION_LOWER_BOUND,
                /*num_packets=*/ 5,
            ),
            DELAY_BASED_ESTIMATE,
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::Decreasing
        );
        assert!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate
                <= DELAY_BASED_ESTIMATE
        );
    }

    #[test]
    fn pace_at_loss_based_estimate() {
        let mut test = LossBasedBweV2Test::default();
        let config = LossBasedBweV2Config {
            pace_at_loss_based_estimate: true,
            padding_duration: TimeDelta::from_millis(1000),
            ..short_observation_config()
        };
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        loss_based_bandwidth_estimator
            .set_bandwidth_estimate(DataRate::from_kilobits_per_sec(1000));
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/ Timestamp::zero(),
            ),
            /*delay_based_estimate=*/ DataRate::from_kilobits_per_sec(1000),
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::DelayBasedEstimate
        );
        assert!(!loss_based_bandwidth_estimator.pace_at_loss_based_estimate());

        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_100p_loss_rate(
                /*first_packet_timestamp=*/
                Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND,
                2,
            ),
            /*delay_based_estimate=*/ DataRate::from_kilobits_per_sec(1000),
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::Decreasing
        );
        assert!(loss_based_bandwidth_estimator.pace_at_loss_based_estimate());

        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &test.create_packet_results_with_received_packets(
                /*first_packet_timestamp=*/
                Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND * 2,
            ),
            /*delay_based_estimate=*/ DataRate::from_kilobits_per_sec(1000),
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator.get_loss_based_result().state,
            LossBasedState::IncreaseUsingPadding
        );
        assert!(loss_based_bandwidth_estimator.pace_at_loss_based_estimate());
    }

    #[test]
    fn estimate_does_not_back_off_due_to_packet_reordering_between_feedback() {
        let config = short_observation_config();
        let mut loss_based_bandwidth_estimator = LossBasedBweV2::new(config);
        const START_BITRATE: DataRate = DataRate::from_kilobits_per_sec(2500);
        loss_based_bandwidth_estimator.set_bandwidth_estimate(START_BITRATE);

        let mut feedback_1 = vec![PacketResult::default(); 3];
        feedback_1[0].sent_packet.sequence_number = 1;
        feedback_1[0].sent_packet.size = DataSize::from_bytes(PACKET_SIZE);
        feedback_1[0].sent_packet.send_time = Timestamp::zero();
        feedback_1[0].receive_time =
            feedback_1[0].sent_packet.send_time + TimeDelta::from_millis(10);
        feedback_1[1].sent_packet.sequence_number = 2;
        feedback_1[1].sent_packet.size = DataSize::from_bytes(PACKET_SIZE);
        feedback_1[1].sent_packet.send_time = Timestamp::zero();
        // Lost or reordered
        feedback_1[1].receive_time = Timestamp::plus_infinity();

        feedback_1[2].sent_packet.sequence_number = 3;
        feedback_1[2].sent_packet.size = DataSize::from_bytes(PACKET_SIZE);
        feedback_1[2].sent_packet.send_time = Timestamp::zero();
        feedback_1[2].receive_time =
            feedback_1[2].sent_packet.send_time + TimeDelta::from_millis(10);

        let mut feedback_2 = vec![PacketResult::default(); 3];
        feedback_2[0].sent_packet.sequence_number = 2;
        feedback_2[0].sent_packet.size = DataSize::from_bytes(PACKET_SIZE);
        feedback_2[0].sent_packet.send_time = Timestamp::zero();
        feedback_2[0].receive_time =
            feedback_1[0].sent_packet.send_time + TimeDelta::from_millis(10);
        feedback_2[1].sent_packet.sequence_number = 4;
        feedback_2[1].sent_packet.size = DataSize::from_bytes(PACKET_SIZE);
        feedback_2[1].sent_packet.send_time = Timestamp::zero() + OBSERVATION_DURATION_LOWER_BOUND;
        feedback_2[1].receive_time =
            feedback_2[1].sent_packet.send_time + TimeDelta::from_millis(10);
        feedback_2[2].sent_packet.sequence_number = 5;
        feedback_2[2].sent_packet.size = DataSize::from_bytes(PACKET_SIZE);
        feedback_2[2].sent_packet.send_time = Timestamp::zero();
        feedback_2[2].receive_time =
            feedback_2[2].sent_packet.send_time + TimeDelta::from_millis(10);

        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &feedback_1,
            /*delay_based_estimate=*/ START_BITRATE,
            /*in_alr=*/ false,
        );
        loss_based_bandwidth_estimator.update_bandwidth_estimate(
            &feedback_2,
            /*delay_based_estimate=*/ START_BITRATE,
            /*in_alr=*/ false,
        );
        assert_eq!(
            loss_based_bandwidth_estimator
                .get_loss_based_result()
                .bandwidth_estimate,
            START_BITRATE
        );
    }
} // namespace
