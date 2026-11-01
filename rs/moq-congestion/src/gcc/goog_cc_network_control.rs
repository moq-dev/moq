/*
 *  Copyright (c) 2018 The WebRTC project authors. All Rights Reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE file in the root of the source
 *  tree. An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the AUTHORS file in the root of the source tree.
 */

use std::collections::VecDeque;

use crate::{
    api::{
        network_control::{NetworkControllerConfig, NetworkControllerInterface},
        transport::{
            BandwidthUsage, NetworkAvailability, NetworkControlUpdate, NetworkEstimate,
            NetworkRouteChange, NetworkStateEstimate, PacedPacketInfo, PacerConfig,
            ProbeClusterConfig, ProcessInterval, ReceivedPacket, RemoteBitrateReport,
            RoundTripTimeUpdate, SentPacket, StreamsConfig, TargetRateConstraints,
            TargetTransferRate, TransportLossReport, TransportPacketsFeedback,
        },
        units::{DataRate, DataSize, TimeDelta, Timestamp},
    },
    experiments::{FieldTrials, RateControlSettings},
    AcknowledgedBitrateEstimator,
    remote_bitrate_estimator::CONGESTION_CONTROLLER_MIN_BITRATE,
};

use super::{
    AcknowledgedBitrateEstimatorInterface, AlrDetector, BandwidthLimitedCause,
    CongestionWindowPushbackController, DelayBasedBwe, DelayBasedBweResult, LossBasedState,
    ProbeBitrateEstimator, ProbeController, SendSideBandwidthEstimation,
};

#[derive(Clone, Debug)]
pub struct SafeResetOnRouteChange {
    pub enabled: bool, // Enabled
    pub ack: bool,     // ack
}

impl Default for SafeResetOnRouteChange {
    fn default() -> Self {
        Self {
            enabled: true,
            ack: true,
        }
    }
}

/// Google Congestion Control specific configuration.
///
/// NOTE: NetworkStateEstimator and NetworkStatePredictor have not been ported.
/// These are optional traits and do not seem to be implemented, except perhaps by Google internally.
#[derive(Clone, Debug, Default)]
pub struct GoogCcConfig {
    // network_state_estimator: Box<dyn NetworkStateEstimator>,
    // network_state_predictor: Box<dyn NetworkStatePredictor>,
    pub feedback_only: bool,
}

/// Google Congestion Control.
///
/// This is the congestion controller used within WebRTC.
/// It has been extracted from the libwebrtc source code and ported to Rust to avoid linking headaches.
/// Use the [NetworkControllerInterface] to interact with this controller.
pub struct GoogCcNetworkController {
    field_trials: FieldTrials,

    packet_feedback_only: bool,
    safe_reset_on_route_change: bool,
    safe_reset_acknowledged_rate: bool,
    use_min_allocatable_as_lower_bound: bool,
    ignore_probes_lower_than_network_estimate: bool,
    limit_probes_lower_than_throughput_estimate: bool,
    rate_control_settings: RateControlSettings,
    pace_at_max_of_bwe_and_lower_link_capacity: bool,
    limit_pacingfactor_by_upper_link_capacity_estimate: bool,

    probe_controller: ProbeController,
    congestion_window_pushback_controller: Option<CongestionWindowPushbackController>,

    bandwidth_estimation: SendSideBandwidthEstimation,
    alr_detector: AlrDetector,
    probe_bitrate_estimator: ProbeBitrateEstimator,
    delay_based_bwe: DelayBasedBwe,
    acknowledged_bitrate_estimator: Box<dyn AcknowledgedBitrateEstimatorInterface>,

    initial_config: Option<NetworkControllerConfig>,

    min_target_rate: DataRate,
    min_data_rate: DataRate,
    max_data_rate: DataRate, // = DataRate::plus_infinity();
    starting_rate: Option<DataRate>,

    first_packet_sent: bool,

    estimate: Option<NetworkStateEstimate>,

    next_loss_update: Timestamp, // = Timestamp::minus_infinity();
    lost_packets_since_last_loss_update: i64,
    expected_packets_since_last_loss_update: i64,

    feedback_max_rtts: VecDeque<i64>,

    last_loss_based_target_rate: DataRate,
    last_pushback_target_rate: DataRate,
    last_stable_target_rate: DataRate,
    last_loss_base_state: LossBasedState,

    last_estimated_fraction_loss: u8,
    last_estimated_round_trip_time: TimeDelta, //= TimeDelta::plus_infinity();

    pacing_factor: f64,
    min_total_allocated_bitrate: DataRate,
    max_padding_rate: DataRate,

    previously_in_alr: bool,

    current_data_window: Option<DataSize>,
}

impl GoogCcNetworkController {
    // From RTCPSender video report interval.
    const LOSS_UPDATE_INTERVAL: TimeDelta = TimeDelta::from_millis(1000);

    // Pacing-rate relative to our target send rate.
    // Multiplicative factor that is applied to the target bitrate to calculate
    // the number of bytes that can be transmitted per interval.
    // Increasing this factor will result in lower delays in cases of bitrate
    // overshoots from the encoder.
    const DEFAULT_PACE_MULTIPLIER: f64 = 2.5;

    // If the probe result is far below the current throughput estimate
    // it's unlikely that the probe is accurate, so we don't want to drop too far.
    // However, if we actually are overusing, we want to drop to something slightly
    // below the current throughput estimate to drain the network queues.
    const PROBE_DROP_THROUGHPUT_FRACTION: f64 = 0.85;

    pub fn new(config: NetworkControllerConfig, goog_cc_config: GoogCcConfig) -> Self {
        let field_trials = config.field_trials.clone();
        let rate_control_settings = RateControlSettings::new(&field_trials);

        assert!(config.constraints.at_time.is_finite());
        let mut delay_based_bwe = DelayBasedBwe::new(&field_trials);
        delay_based_bwe.set_min_bitrate(CONGESTION_CONTROLLER_MIN_BITRATE);

        Self {
            packet_feedback_only: goog_cc_config.feedback_only,
            safe_reset_on_route_change: field_trials.safe_reset_on_route_change.enabled,
            safe_reset_acknowledged_rate: field_trials.safe_reset_on_route_change.ack,
            use_min_allocatable_as_lower_bound: field_trials.min_alloc_as_lower_bound,
            ignore_probes_lower_than_network_estimate: field_trials
                .ignore_probes_lower_than_network_state_estimate,
            limit_probes_lower_than_throughput_estimate: field_trials
                .limit_probes_lower_than_throughput_estimate,
            pace_at_max_of_bwe_and_lower_link_capacity: field_trials
                .pace_at_max_of_bwe_and_lower_link_capacity,
            limit_pacingfactor_by_upper_link_capacity_estimate: field_trials
                .limit_pacing_factor_by_upper_link_capacity_estimate,

            probe_controller: ProbeController::new(field_trials.probing_configuration.clone()),
            congestion_window_pushback_controller: rate_control_settings
                .use_congestion_window_pushback()
                .then(|| CongestionWindowPushbackController::new(&field_trials)),
            rate_control_settings,
            bandwidth_estimation: SendSideBandwidthEstimation::new(field_trials.clone()),
            alr_detector: AlrDetector::new(field_trials.alr_detector_parameters.clone()),
            probe_bitrate_estimator: ProbeBitrateEstimator::default(),
            delay_based_bwe,
            acknowledged_bitrate_estimator: AcknowledgedBitrateEstimator::create(&field_trials),

            min_target_rate: DataRate::zero(),
            min_data_rate: DataRate::zero(),
            max_data_rate: DataRate::plus_infinity(),
            starting_rate: config.constraints.starting_rate,
            first_packet_sent: false,
            estimate: None,
            next_loss_update: Timestamp::minus_infinity(),
            lost_packets_since_last_loss_update: 0,
            expected_packets_since_last_loss_update: 0,
            feedback_max_rtts: VecDeque::new(),
            last_loss_based_target_rate: config.constraints.starting_rate.unwrap(),
            last_pushback_target_rate: config.constraints.starting_rate.unwrap(),
            last_stable_target_rate: config.constraints.starting_rate.unwrap(),
            last_loss_base_state: LossBasedState::DelayBasedEstimate,
            last_estimated_fraction_loss: 0,
            last_estimated_round_trip_time: TimeDelta::plus_infinity(),
            pacing_factor: config
                .stream_based_config
                .pacing_factor
                .unwrap_or(Self::DEFAULT_PACE_MULTIPLIER),
            min_total_allocated_bitrate: config
                .stream_based_config
                .min_total_allocated_bitrate
                .unwrap_or(DataRate::zero()),

            max_padding_rate: config
                .stream_based_config
                .max_padding_rate
                .unwrap_or(DataRate::zero()),
            previously_in_alr: false,
            current_data_window: None,

            initial_config: Some(config),
            field_trials,
        }
    }

    pub fn get_network_state(&self, at_time: Timestamp) -> NetworkControlUpdate {
        NetworkControlUpdate {
            target_rate: Some(TargetTransferRate {
                network_estimate: NetworkEstimate {
                    at_time,
                    loss_rate_ratio: self.last_estimated_fraction_loss as f32 / 255.0,
                    round_trip_time: self.last_estimated_round_trip_time,
                    bwe_period: self.delay_based_bwe.get_expected_bwe_period(),
                    ..Default::default()
                },
                at_time,
                target_rate: self.last_pushback_target_rate,
                stable_target_rate: self.bandwidth_estimation.get_estimated_link_capacity(),
                ..Default::default()
            }),
            pacer_config: Some(self.get_pacing_rates(at_time)),
            congestion_window: self.current_data_window,
            ..Default::default()
        }
    }

    fn reset_constraints(
        &mut self,
        new_constraints: TargetRateConstraints,
    ) -> Vec<ProbeClusterConfig> {
        self.min_target_rate = new_constraints.min_data_rate.unwrap_or(DataRate::zero());
        self.max_data_rate = new_constraints
            .max_data_rate
            .unwrap_or(DataRate::plus_infinity());
        self.starting_rate = new_constraints.starting_rate;
        self.clamp_constraints();

        self.bandwidth_estimation.set_bitrates(
            self.starting_rate,
            self.min_data_rate,
            self.max_data_rate,
            new_constraints.at_time,
        );

        if let Some(starting_rate) = self.starting_rate {
            self.delay_based_bwe.set_start_bitrate(starting_rate);
        }
        self.delay_based_bwe.set_min_bitrate(self.min_data_rate);

        self.probe_controller.set_bitrates(
            self.min_data_rate,
            self.starting_rate.unwrap_or(DataRate::zero()),
            self.max_data_rate,
            new_constraints.at_time,
        )
    }

    fn clamp_constraints(&mut self) {
        // TODO(holmer): We should make sure the default bitrates are set to 10 kbps,
        // and that we don't try to set the min bitrate to 0 from any applications.
        // The congestion controller should allow a min bitrate of 0.
        self.min_data_rate = std::cmp::max(self.min_target_rate, CONGESTION_CONTROLLER_MIN_BITRATE);
        if self.use_min_allocatable_as_lower_bound {
            self.min_data_rate =
                std::cmp::max(self.min_data_rate, self.min_total_allocated_bitrate);
        }
        if self.max_data_rate < self.min_data_rate {
            tracing::warn!("max bitrate smaller than min bitrate");
            self.max_data_rate = self.min_data_rate;
        }
        match self.starting_rate {
            Some(starting_rate) if starting_rate < self.min_data_rate => {
                tracing::warn!("start bitrate smaller than min bitrate");
                self.starting_rate = Some(self.min_data_rate);
            }
            _ => {}
        }
    }
    fn maybe_trigger_on_network_changed(
        &mut self,
        update: &mut NetworkControlUpdate,
        at_time: Timestamp,
    ) {
        let fraction_loss: u8 = self.bandwidth_estimation.fraction_loss();
        let round_trip_time: TimeDelta = self.bandwidth_estimation.round_trip_time();
        let loss_based_target_rate: DataRate = self.bandwidth_estimation.target_rate();
        let loss_based_state: LossBasedState = self.bandwidth_estimation.loss_based_state();
        let mut pushback_target_rate: DataRate = loss_based_target_rate;

        let mut cwnd_reduce_ratio: f64 = 0.0;
        if let Some(congestion_window_pushback_controller) =
            &mut self.congestion_window_pushback_controller
        {
            let mut pushback_rate: i64 = congestion_window_pushback_controller
                .update_target_bitrate(loss_based_target_rate.bps() as _)
                as _;
            pushback_rate = self
                .bandwidth_estimation
                .get_min_bitrate()
                .max(pushback_rate);
            pushback_target_rate = DataRate::from_bits_per_sec(pushback_rate);
            if self
                .rate_control_settings
                .use_congestion_window_drop_frame_only()
            {
                cwnd_reduce_ratio = (loss_based_target_rate.bps_float()
                    - pushback_target_rate.bps_float())
                    / loss_based_target_rate.bps_float();
            }
        }
        let mut stable_target_rate: DataRate =
            self.bandwidth_estimation.get_estimated_link_capacity();
        stable_target_rate = std::cmp::min(stable_target_rate, pushback_target_rate);

        if (loss_based_target_rate != self.last_loss_based_target_rate)
            || (loss_based_state != self.last_loss_base_state)
            || (fraction_loss != self.last_estimated_fraction_loss)
            || (round_trip_time != self.last_estimated_round_trip_time)
            || (pushback_target_rate != self.last_pushback_target_rate)
            || (stable_target_rate != self.last_stable_target_rate)
        {
            self.last_loss_based_target_rate = loss_based_target_rate;
            self.last_pushback_target_rate = pushback_target_rate;
            self.last_estimated_fraction_loss = fraction_loss;
            self.last_estimated_round_trip_time = round_trip_time;
            self.last_stable_target_rate = stable_target_rate;
            self.last_loss_base_state = loss_based_state;

            self.alr_detector
                .set_estimated_bitrate(loss_based_target_rate.bps());

            let bwe_period: TimeDelta = self.delay_based_bwe.get_expected_bwe_period();

            let mut target_rate_msg = TargetTransferRate {
                at_time,
                stable_target_rate,
                network_estimate: NetworkEstimate {
                    at_time,
                    round_trip_time,
                    loss_rate_ratio: fraction_loss as f32 / 255.0,
                    bwe_period,
                    ..Default::default()
                },
                ..Default::default()
            };

            if self
                .rate_control_settings
                .use_congestion_window_drop_frame_only()
            {
                target_rate_msg.target_rate = loss_based_target_rate;
                target_rate_msg.cwnd_reduce_ratio = cwnd_reduce_ratio;
            } else {
                target_rate_msg.target_rate = pushback_target_rate;
            }

            update.target_rate = Some(target_rate_msg);

            let mut probes = self.probe_controller.set_estimated_bitrate(
                loss_based_target_rate,
                get_bandwidth_limited_cause(
                    self.bandwidth_estimation.loss_based_state(),
                    self.bandwidth_estimation.is_rtt_above_limit(),
                    self.delay_based_bwe.last_state(),
                ),
                at_time,
            );
            update.probe_cluster_configs.append(&mut probes);
            update.pacer_config = Some(self.get_pacing_rates(at_time));
            tracing::trace!(
                "bwe {} pushback_target_bps={} estimate_bps={}",
                at_time.ms(),
                self.last_pushback_target_rate.bps(),
                loss_based_target_rate.bps()
            );
        }
    }

    fn get_pacing_rates(&self, at_time: Timestamp) -> PacerConfig {
        // Pacing rate is based on target rate before congestion window pushback,
        // because we don't want to build queues in the pacer when pushback occurs.
        let mut pacing_rate = match self.estimate {
            Some(estimate)
                if self.pace_at_max_of_bwe_and_lower_link_capacity
                    && !self.bandwidth_estimation.pace_at_loss_based_estimate() =>
            {
                *[
                    self.min_total_allocated_bitrate,
                    estimate.link_capacity_lower,
                    self.last_loss_based_target_rate,
                ]
                .iter()
                .max()
                .unwrap()
                    * self.pacing_factor
            }
            _ => {
                self.min_total_allocated_bitrate
                    .max(self.last_loss_based_target_rate)
                    * self.pacing_factor
            }
        };

        match self.estimate {
            Some(estimate)
                if self.limit_pacingfactor_by_upper_link_capacity_estimate
                    && estimate.link_capacity_upper.is_finite()
                    && pacing_rate > estimate.link_capacity_upper =>
            {
                pacing_rate = *[
                    estimate.link_capacity_upper,
                    self.min_total_allocated_bitrate,
                    self.last_loss_based_target_rate,
                ]
                .iter()
                .max()
                .unwrap()
            }
            _ => {}
        };

        let mut padding_rate: DataRate =
            if self.last_loss_base_state == LossBasedState::IncreaseUsingPadding {
                self.max_padding_rate.max(self.last_loss_based_target_rate)
            } else {
                self.max_padding_rate
            };
        padding_rate = std::cmp::min(padding_rate, self.last_pushback_target_rate);
        let mut msg = PacerConfig::default();
        msg.at_time = at_time;
        msg.time_window = TimeDelta::from_seconds(1);
        msg.data_window = pacing_rate * msg.time_window;
        msg.pad_window = padding_rate * msg.time_window;
        msg
    }

    fn set_network_state_estimate(&mut self, estimate: Option<NetworkStateEstimate>) {
        let prev_estimate = self.estimate;
        self.estimate = estimate;

        match (self.estimate, prev_estimate) {
            (Some(estimate), Some(prev_estimate))
                if estimate.update_time == prev_estimate.update_time => {}
            (Some(estimate), _) => self.probe_controller.set_network_state_estimate(estimate),
            _ => {}
        };
    }

    fn update_congestion_window_size(&mut self) {
        let min_feedback_max_rtt: TimeDelta =
            TimeDelta::from_millis(*self.feedback_max_rtts.iter().min().unwrap());

        const MIN_CWND: DataSize = DataSize::from_bytes(2 * 1500);
        let time_window: TimeDelta = min_feedback_max_rtt
            + TimeDelta::from_millis(
                self.rate_control_settings
                    .get_congestion_window_additional_time_ms(),
            );

        let mut data_window: DataSize = self.last_loss_based_target_rate * time_window;
        if let Some(current_data_window) = self.current_data_window {
            data_window = std::cmp::max(MIN_CWND, (data_window + current_data_window) / 2);
        } else {
            data_window = std::cmp::max(MIN_CWND, data_window);
        }
        self.current_data_window = Some(data_window);
    }
}

impl NetworkControllerInterface for GoogCcNetworkController {
    fn on_network_availability(&mut self, msg: NetworkAvailability) -> NetworkControlUpdate {
        NetworkControlUpdate {
            probe_cluster_configs: self.probe_controller.on_network_availability(msg),
            ..Default::default()
        }
    }

    fn on_network_route_change(&mut self, mut msg: NetworkRouteChange) -> NetworkControlUpdate {
        if self.safe_reset_on_route_change {
            let mut estimated_bitrate: Option<DataRate>;
            if self.safe_reset_acknowledged_rate {
                estimated_bitrate = self.acknowledged_bitrate_estimator.bitrate();
                if estimated_bitrate.is_none() {
                    estimated_bitrate = self.acknowledged_bitrate_estimator.peek_rate();
                }
            } else {
                estimated_bitrate = Some(self.bandwidth_estimation.target_rate());
            }
            if let Some(estimated_bitrate) = estimated_bitrate {
                msg.constraints.starting_rate = Some(match msg.constraints.starting_rate {
                    Some(starting_rate) => starting_rate.min(estimated_bitrate),
                    None => estimated_bitrate,
                });
            }
        }

        self.acknowledged_bitrate_estimator =
            AcknowledgedBitrateEstimator::create(&self.field_trials);
        self.probe_bitrate_estimator = ProbeBitrateEstimator::default();
        self.delay_based_bwe = DelayBasedBwe::new(&self.field_trials);
        self.bandwidth_estimation.on_route_change();
        self.probe_controller.reset(msg.at_time);
        let mut update = NetworkControlUpdate {
            probe_cluster_configs: self.reset_constraints(msg.constraints),
            ..Default::default()
        };
        self.maybe_trigger_on_network_changed(&mut update, msg.at_time);
        update
    }

    fn on_process_interval(&mut self, msg: ProcessInterval) -> NetworkControlUpdate {
        let mut update = NetworkControlUpdate::default();
        if let Some(initial_config) = self.initial_config.take() {
            update.probe_cluster_configs = self.reset_constraints(initial_config.constraints);
            update.pacer_config = Some(self.get_pacing_rates(msg.at_time));

            if let Some(requests_alr_probing) =
                initial_config.stream_based_config.requests_alr_probing
            {
                self.probe_controller
                    .enable_periodic_alr_probing(requests_alr_probing);
            }
            if let Some(enable_repeated_initial_probing) = initial_config
                .stream_based_config
                .enable_repeated_initial_probing
            {
                self.probe_controller
                    .enable_repeated_initial_probing(enable_repeated_initial_probing);
            }
            let total_bitrate: Option<DataRate> = initial_config
                .stream_based_config
                .max_total_allocated_bitrate;
            if let Some(total_bitrate) = total_bitrate {
                let mut probes = self
                    .probe_controller
                    .on_max_total_allocated_bitrate(total_bitrate, msg.at_time);
                update.probe_cluster_configs.append(&mut probes);
            }
        }

        if let (Some(congestion_window_pushback_controller), Some(pacer_queue)) = (
            &mut self.congestion_window_pushback_controller,
            msg.pacer_queue,
        ) {
            congestion_window_pushback_controller.update_pacing_queue(pacer_queue.bytes())
        };
        self.bandwidth_estimation.update_estimate(msg.at_time);
        let start_time_ms: Option<i64> = self
            .alr_detector
            .get_application_limited_region_start_time();
        self.probe_controller.set_alr_start_time_ms(start_time_ms);

        let mut probes = self.probe_controller.process(msg.at_time);
        update.probe_cluster_configs.append(&mut probes);

        if self.rate_control_settings.use_congestion_window() && !self.feedback_max_rtts.is_empty()
        {
            self.update_congestion_window_size();
        }

        match (
            &mut self.congestion_window_pushback_controller,
            self.current_data_window,
        ) {
            (Some(congestion_window_pushback_controller), Some(current_data_window)) => {
                congestion_window_pushback_controller
                    .update_outstanding_data(current_data_window.bytes())
            }
            _ => update.congestion_window = self.current_data_window,
        };

        self.maybe_trigger_on_network_changed(&mut update, msg.at_time);
        update
    }

    fn on_remote_bitrate_report(&mut self, msg: RemoteBitrateReport) -> NetworkControlUpdate {
        if self.packet_feedback_only {
            tracing::error!("Received REMB for packet feedback only GoogCC");
            return NetworkControlUpdate::default();
        }
        self.bandwidth_estimation
            .update_receiver_estimate(msg.receive_time, msg.bandwidth);
        NetworkControlUpdate::default()
    }

    fn on_round_trip_time_update(&mut self, msg: RoundTripTimeUpdate) -> NetworkControlUpdate {
        if self.packet_feedback_only || msg.smoothed {
            return NetworkControlUpdate::default();
        }
        assert!(!msg.round_trip_time.is_zero());
        self.delay_based_bwe.on_rtt_update(msg.round_trip_time);
        self.bandwidth_estimation
            .update_rtt(msg.round_trip_time, msg.receive_time);
        NetworkControlUpdate::default()
    }

    fn on_sent_packet(&mut self, sent_packet: SentPacket) -> NetworkControlUpdate {
        self.alr_detector
            .on_bytes_sent(sent_packet.size.bytes() as _, sent_packet.send_time.ms());
        self.acknowledged_bitrate_estimator.set_alr(
            self.alr_detector
                .get_application_limited_region_start_time()
                .is_some(),
        );

        if !self.first_packet_sent {
            self.first_packet_sent = true;
            // Initialize feedback time to send time to allow estimation of RTT until
            // first feedback is received.
            self.bandwidth_estimation
                .update_propagation_rtt(sent_packet.send_time, TimeDelta::zero());
        }
        self.bandwidth_estimation.on_sent_packet(sent_packet);

        if let Some(congestion_window_pushback_controller) =
            &mut self.congestion_window_pushback_controller
        {
            congestion_window_pushback_controller
                .update_outstanding_data(sent_packet.data_in_flight.bytes());
            let mut update = NetworkControlUpdate::default();
            self.maybe_trigger_on_network_changed(&mut update, sent_packet.send_time);
            update
        } else {
            NetworkControlUpdate::default()
        }
    }

    fn on_received_packet(&mut self, _: ReceivedPacket) -> NetworkControlUpdate {
        NetworkControlUpdate::default()
    }

    fn on_streams_config(&mut self, msg: StreamsConfig) -> NetworkControlUpdate {
        let mut update = NetworkControlUpdate::default();
        if let Some(requests_alr_probing) = msg.requests_alr_probing {
            self.probe_controller
                .enable_periodic_alr_probing(requests_alr_probing);
        }
        if let Some(max_total_allocated_bitrate) = msg.max_total_allocated_bitrate {
            update.probe_cluster_configs = self
                .probe_controller
                .on_max_total_allocated_bitrate(max_total_allocated_bitrate, msg.at_time);
        }

        let mut pacing_changed: bool = false;
        match msg.pacing_factor {
            Some(pacing_factor) if pacing_factor != self.pacing_factor => {
                self.pacing_factor = pacing_factor;
                pacing_changed = true;
            }
            _ => {}
        };
        match msg.min_total_allocated_bitrate {
            Some(min_total_allocated_bitrate)
                if min_total_allocated_bitrate != self.min_total_allocated_bitrate =>
            {
                self.min_total_allocated_bitrate = min_total_allocated_bitrate;
                pacing_changed = true;

                if self.use_min_allocatable_as_lower_bound {
                    self.clamp_constraints();
                    self.delay_based_bwe.set_min_bitrate(self.min_data_rate);
                    self.bandwidth_estimation
                        .set_min_max_bitrate(self.min_data_rate, self.max_data_rate);
                }
            }
            _ => {}
        };

        match msg.max_padding_rate {
            Some(max_padding_rate) if max_padding_rate != self.max_padding_rate => {
                self.max_padding_rate = max_padding_rate;
                pacing_changed = true;
            }
            _ => {}
        };

        if pacing_changed {
            update.pacer_config = Some(self.get_pacing_rates(msg.at_time));
        }
        update
    }

    fn on_target_rate_constraints(
        &mut self,
        constraints: TargetRateConstraints,
    ) -> NetworkControlUpdate {
        let mut update = NetworkControlUpdate {
            probe_cluster_configs: self.reset_constraints(constraints),
            ..Default::default()
        };
        self.maybe_trigger_on_network_changed(&mut update, constraints.at_time);
        update
    }

    fn on_transport_loss_report(&mut self, msg: TransportLossReport) -> NetworkControlUpdate {
        if self.packet_feedback_only {
            return NetworkControlUpdate::default();
        }
        let total_packets_delta: i64 =
            msg.packets_received_delta as i64 + msg.packets_lost_delta as i64;
        self.bandwidth_estimation.update_packets_lost(
            msg.packets_lost_delta as _,
            total_packets_delta,
            msg.receive_time,
        );
        NetworkControlUpdate::default()
    }

    fn on_transport_packets_feedback(
        &mut self,
        report: TransportPacketsFeedback,
    ) -> NetworkControlUpdate {
        if report.packet_feedbacks.is_empty() {
            // TODO(bugs.webrtc.org/10125): Design a better mechanism to safe-guard
            // against building very large network queues.
            return NetworkControlUpdate::default();
        }

        if let Some(congestion_window_pushback_controller) =
            &mut self.congestion_window_pushback_controller
        {
            congestion_window_pushback_controller
                .update_outstanding_data(report.data_in_flight.bytes());
        }
        let mut max_feedback_rtt: TimeDelta = TimeDelta::minus_infinity();
        let mut min_propagation_rtt: TimeDelta = TimeDelta::plus_infinity();
        let max_recv_time = report
            .received_with_send_info()
            .map(|x| x.receive_time)
            .max()
            .unwrap();

        for feedback in report.received_with_send_info() {
            let feedback_rtt: TimeDelta = report.feedback_time - feedback.sent_packet.send_time;
            let min_pending_time: TimeDelta = max_recv_time - feedback.receive_time;
            let propagation_rtt: TimeDelta = feedback_rtt - min_pending_time;
            max_feedback_rtt = std::cmp::max(max_feedback_rtt, feedback_rtt);
            min_propagation_rtt = std::cmp::min(min_propagation_rtt, propagation_rtt);
        }

        if max_feedback_rtt.is_finite() {
            self.feedback_max_rtts.push_back(max_feedback_rtt.ms());
            const MAX_FEEDBACK_RTT_WINDOW: usize = 32;
            if self.feedback_max_rtts.len() > MAX_FEEDBACK_RTT_WINDOW {
                self.feedback_max_rtts.pop_front();
            }
            // TODO(srte): Use time since last unacknowledged packet.
            self.bandwidth_estimation
                .update_propagation_rtt(report.feedback_time, min_propagation_rtt);
        }
        if self.packet_feedback_only {
            if !self.feedback_max_rtts.is_empty() {
                let sum_rtt_ms: i64 = self.feedback_max_rtts.iter().sum();
                let mean_rtt_ms: i64 = sum_rtt_ms / self.feedback_max_rtts.len() as i64;
                self.delay_based_bwe
                    .on_rtt_update(TimeDelta::from_millis(mean_rtt_ms));
            }

            let mut feedback_min_rtt: TimeDelta = TimeDelta::plus_infinity();
            for packet_feedback in report.received_with_send_info() {
                let pending_time: TimeDelta = max_recv_time - packet_feedback.receive_time;
                let rtt: TimeDelta =
                    report.feedback_time - packet_feedback.sent_packet.send_time - pending_time;
                // Value used for predicting NACK round trip time in FEC controller.
                feedback_min_rtt = std::cmp::min(rtt, feedback_min_rtt);
            }
            if feedback_min_rtt.is_finite() {
                self.bandwidth_estimation
                    .update_rtt(feedback_min_rtt, report.feedback_time);
            }

            self.expected_packets_since_last_loss_update +=
                report.packets_with_feedback().len() as i64;
            for packet_feedback in report.packets_with_feedback() {
                if !packet_feedback.is_received() {
                    self.lost_packets_since_last_loss_update += 1;
                }
            }
            if report.feedback_time > self.next_loss_update {
                self.next_loss_update = report.feedback_time + Self::LOSS_UPDATE_INTERVAL;
                self.bandwidth_estimation.update_packets_lost(
                    self.lost_packets_since_last_loss_update,
                    self.expected_packets_since_last_loss_update,
                    report.feedback_time,
                );
                self.expected_packets_since_last_loss_update = 0;
                self.lost_packets_since_last_loss_update = 0;
            }
        }
        let alr_start_time: Option<i64> = self
            .alr_detector
            .get_application_limited_region_start_time();

        if self.previously_in_alr && alr_start_time.is_none() {
            let now_ms: i64 = report.feedback_time.ms();
            self.acknowledged_bitrate_estimator
                .set_alr_ended_time(report.feedback_time);
            self.probe_controller.set_alr_ended_time_ms(now_ms);
        }
        self.previously_in_alr = alr_start_time.is_some();
        self.acknowledged_bitrate_estimator
            .incoming_packet_feedback(&report.sorted_by_receive_time());
        let acknowledged_bitrate = self.acknowledged_bitrate_estimator.bitrate();
        self.bandwidth_estimation
            .set_acknowledged_rate(acknowledged_bitrate, report.feedback_time);
        for feedback in report.sorted_by_receive_time() {
            if feedback.sent_packet.pacing_info.probe_cluster_id != PacedPacketInfo::NOT_APROBE {
                self.probe_bitrate_estimator
                    .handle_probe_and_estimate_bitrate(&feedback);
            }
        }

        let mut probe_bitrate: Option<DataRate> = self
            .probe_bitrate_estimator
            .fetch_and_reset_last_estimated_bitrate();
        match (probe_bitrate, self.estimate) {
            (Some(current), Some(estimate))
                if self.ignore_probes_lower_than_network_estimate
                    && current < self.delay_based_bwe.last_estimate()
                    && current < estimate.link_capacity_lower =>
            {
                probe_bitrate = None;
            }
            _ => {}
        };

        match (probe_bitrate, acknowledged_bitrate) {
            (Some(probe), Some(acknowledged))
                if self.limit_probes_lower_than_throughput_estimate =>
            {
                // Limit the backoff to something slightly below the acknowledged
                // bitrate. ("Slightly below" because we want to drain the queues
                // if we are actually overusing.)
                // The acknowledged bitrate shouldn't normally be higher than the delay
                // based estimate, but it could happen e.g. due to packet bursts or
                // encoder overshoot. We use std::cmp::min to ensure that a probe result
                // below the current BWE never causes an increase.
                let limit: DataRate = std::cmp::min(
                    self.delay_based_bwe.last_estimate(),
                    acknowledged * Self::PROBE_DROP_THROUGHPUT_FRACTION,
                );
                probe_bitrate = Some(std::cmp::max(probe, limit));
            }
            _ => {}
        };

        let mut update = NetworkControlUpdate::default();

        let result: DelayBasedBweResult = self.delay_based_bwe.incoming_packet_feedback_vector(
            &report,
            acknowledged_bitrate,
            probe_bitrate, /*self.estimate,*/
            alr_start_time.is_some(),
        );

        if result.updated {
            if result.probe {
                self.bandwidth_estimation
                    .set_send_bitrate(result.target_bitrate, report.feedback_time);
            }
            // Since SetSendBitrate now resets the delay-based estimate, we have to
            // call UpdateDelayBasedEstimate after SetSendBitrate.
            self.bandwidth_estimation
                .update_delay_based_estimate(report.feedback_time, result.target_bitrate);
        }
        self.bandwidth_estimation.update_loss_based_estimator(
            &report,
            result.delay_detector_state,
            probe_bitrate,
            alr_start_time.is_some(),
        );
        if result.updated {
            // Update the estimate in the ProbeController, in case we want to probe.
            self.maybe_trigger_on_network_changed(&mut update, report.feedback_time);
        }

        let recovered_from_overuse = result.recovered_from_overuse;

        if recovered_from_overuse {
            self.probe_controller.set_alr_start_time_ms(alr_start_time);
            let mut probes = self.probe_controller.request_probe(report.feedback_time);
            update.probe_cluster_configs.append(&mut probes);
        }

        // No valid RTT could be because send-side BWE isn't used, in which case
        // we don't try to limit the outstanding packets.
        if self.rate_control_settings.use_congestion_window() && max_feedback_rtt.is_finite() {
            self.update_congestion_window_size();
        }
        if let (Some(congestion_window_pushback_controller), Some(current_data_window)) = (
            &mut self.congestion_window_pushback_controller,
            self.current_data_window,
        ) {
            congestion_window_pushback_controller.set_data_window(current_data_window);
        } else {
            update.congestion_window = self.current_data_window;
        }

        update
    }

    fn on_network_state_estimate(
        &mut self,
        estimate: NetworkStateEstimate,
    ) -> NetworkControlUpdate {
        self.set_network_state_estimate(Some(estimate));
        NetworkControlUpdate::default()
    }

    fn get_process_interval() -> TimeDelta {
        TimeDelta::from_millis(25)
    }
}

fn get_bandwidth_limited_cause(
    loss_based_state: LossBasedState,
    is_rtt_above_limit: bool,
    bandwidth_usage: BandwidthUsage,
) -> BandwidthLimitedCause {
    if bandwidth_usage == BandwidthUsage::Overusing || bandwidth_usage == BandwidthUsage::Underusing
    {
        return BandwidthLimitedCause::DelayBasedLimitedDelayIncreased;
    } else if is_rtt_above_limit {
        return BandwidthLimitedCause::RttBasedBackOffHighRtt;
    }
    match loss_based_state {
        // Probes may not be sent in this state.
        LossBasedState::Decreasing => BandwidthLimitedCause::LossLimitedBwe,
        LossBasedState::IncreaseUsingPadding =>
        // Probes may not be sent in this state.
        {
            BandwidthLimitedCause::LossLimitedBwe
        }
        LossBasedState::Increasing =>
        // Probes may be sent in this state.
        {
            BandwidthLimitedCause::LossLimitedBweIncreasing
        }
        LossBasedState::DelayBasedEstimate => BandwidthLimitedCause::DelayBasedLimited,
    }
}

#[cfg(test)]
mod test {
    use approx::assert_relative_eq;
    use test_trace::test;

    use crate::api::transport::PacketResult;

    use super::*;

    // Count dips from a constant high bandwidth level within a short window.
    /*
    fn count_bandwidth_dips(mut bandwidth_history: VecDeque<DataRate>, threshold: DataRate) -> i64 {
        if bandwidth_history.is_empty() {
            return 1;
        }
        let first = bandwidth_history.pop_front().unwrap();

        let mut dips: i64 = 0;
        let mut state_high: bool = true;
        while let Some(front) = bandwidth_history.pop_front() {
            if front + threshold < first && state_high {
                dips += 1;
                state_high = false;
            } else if front == first {
                state_high = true;
            } else if front > first {
                // If this is toggling we will catch it later when front becomes first.
                state_high = false;
            }
        }
        dips
    }
    */

    const INITIAL_BITRATE_KBPS: i64 = 60;
    const INITIAL_BITRATE: DataRate = DataRate::from_kilobits_per_sec(INITIAL_BITRATE_KBPS);
    const DEFAULT_PACING_RATE: f64 = 2.5;

    /*
    fn CreateVideoSendingClient(
        s: &Scenerio,
        config: CallClientConfig,
        send_link: Vec<EmulatedNetworkNode>,
        return_link: Vec<EmulatedNetworkNode>) -> CallClient {
    let client = s.CreateClient("send", config);
    let route = s.CreateRoutes(client, send_link,
                                    s.CreateClient("return", CallClientConfig()),
                                    return_link);
      s.CreateVideoStream(route.forward(), VideoStreamConfig());
      return client;
    }
    */

    fn create_route_change(
        time: Timestamp,
        start_rate: Option<DataRate>,
        min_rate: Option<DataRate>,
        max_rate: Option<DataRate>,
    ) -> NetworkRouteChange {
        NetworkRouteChange {
            at_time: time,
            constraints: TargetRateConstraints {
                at_time: time,
                starting_rate: start_rate,
                min_data_rate: min_rate,
                max_data_rate: max_rate,
            },
        }
    }

    fn create_packet_result(
        arrival_time: Timestamp,
        send_time: Timestamp,
        payload_size: usize,
        pacing_info: PacedPacketInfo,
    ) -> PacketResult {
        PacketResult {
            sent_packet: SentPacket {
                send_time,
                size: DataSize::from_bytes(payload_size as _),
                pacing_info,
                ..Default::default()
            },
            receive_time: arrival_time,
            ..Default::default()
        }
    }

    // Simulate sending packets and receiving transport feedback during
    // `runtime_ms`, then return the final target birate.
    fn packet_transmission_and_feedback_block<T: NetworkControllerInterface>(
        controller: &mut T,
        runtime_ms: i64,
        delay: i64,
        current_time: &mut Timestamp,
    ) -> Option<DataRate> {
        let mut update: NetworkControlUpdate;
        let mut target_bitrate: Option<DataRate> = None;
        let mut delay_buildup: i64 = 0;
        let start_time_ms: i64 = current_time.ms();
        while current_time.ms() - start_time_ms < runtime_ms {
            const PAYLOAD_SIZE: usize = 1000;
            let packet: PacketResult = create_packet_result(
                *current_time + TimeDelta::from_millis(delay_buildup),
                *current_time,
                PAYLOAD_SIZE,
                PacedPacketInfo::default(),
            );
            delay_buildup += delay;
            update = controller.on_sent_packet(packet.sent_packet);
            if let Some(target_rate) = update.target_rate {
                target_bitrate = Some(target_rate.target_rate);
            }
            let feedback = TransportPacketsFeedback {
                feedback_time: packet.receive_time,
                packet_feedbacks: vec![packet],
                ..Default::default()
            };
            update = controller.on_transport_packets_feedback(feedback);
            if let Some(target_rate) = update.target_rate {
                target_bitrate = Some(target_rate.target_rate);
            }
            *current_time += TimeDelta::from_millis(50);
            update = controller.on_process_interval(ProcessInterval {
                at_time: *current_time,
                ..Default::default()
            });
            if let Some(target_rate) = update.target_rate {
                target_bitrate = Some(target_rate.target_rate);
            }
        }
        target_bitrate
    }

    // Create transport packets feedback with a built-up delay.
    fn create_transport_packets_feedback(
        per_packet_network_delay: TimeDelta,
        one_way_delay: TimeDelta,
        send_time: Timestamp,
    ) -> TransportPacketsFeedback {
        let mut delay_buildup: TimeDelta = one_way_delay;
        const FEEDBACK_SIZE: i64 = 3;
        const PAYLOAD_SIZE: usize = 1000;
        let mut feedback = TransportPacketsFeedback::default();
        for _ in 0..FEEDBACK_SIZE {
            let packet: PacketResult = create_packet_result(
                /*arrival_time=*/ send_time + delay_buildup,
                send_time,
                PAYLOAD_SIZE,
                PacedPacketInfo::default(),
            );
            delay_buildup += per_packet_network_delay;
            feedback.feedback_time = packet.receive_time + one_way_delay;
            feedback.packet_feedbacks.push(packet);
        }
        feedback
    }

    // Scenarios:

    /*
    fn UpdatesTargetRateBasedOnLinkCapacity(test_name: &str) {
    let factory = CreateFeedbackOnlyFactory();
      let s = Scenario::new("googcc_unit/target_capacity" + std::string(test_name), false);
      let mut config: CallClientConfig::default();
      config.transport.cc_factory = &factory;
      config.transport.rates.min_rate = DataRate::KilobitsPerSec(10);
      config.transport.rates.max_rate = DataRate::KilobitsPerSec(1500);
      config.transport.rates.start_rate = DataRate::KilobitsPerSec(300);
    let send_net = s.CreateMutableSimulationNode(|c: &mut NetworkSimulationConfig| {
        c.bandwidth = DataRate::KilobitsPerSec(500);
        c.delay = TimeDelta::Millis(100);
        c.loss_rate = 0.0;
      });
    let ret_net = s.CreateMutableSimulationNode(
          |c: &mut NetworkSimulationConfig| { c.delay = TimeDelta::Millis(100); });
    let truth: &StatesPrinter = s.CreatePrinter(
          "send.truth.txt", TimeDelta::plus_infinity(), {send_net.ConfigPrinter()});

    let client = CreateVideoSendingClient(&s, config, {send_net.node()},
                                              {ret_net.node()});

      truth.PrintRow();
      s.RunFor(TimeDelta::Seconds(25));
      truth.PrintRow();
      assert_relative_eq!(client.target_rate().kbps(), 450, 100);

      send_net.UpdateConfig(|c: &mut NetworkSimulationConfig| {
        c.bandwidth = DataRate::KilobitsPerSec(800);
        c.delay = TimeDelta::Millis(100);
      });

      truth.PrintRow();
      s.RunFor(TimeDelta::Seconds(20));
      truth.PrintRow();
      assert_relative_eq!(client.target_rate().kbps(), 750, 150);

      send_net.UpdateConfig(|c: &mut NetworkSimulationConfig| {
        c.bandwidth = DataRate::KilobitsPerSec(100);
        c.delay = TimeDelta::Millis(200);
      });
      ret_net.UpdateConfig(
          |c: &mut NetworkSimulationConfig| { c.delay = TimeDelta::Millis(200); });

      truth.PrintRow();
      s.RunFor(TimeDelta::Seconds(50));
      truth.PrintRow();
      assert_relative_eq!(client.target_rate().kbps(), 90, 25);
    }

    fn RunRembDipScenario(test_name: &str) -> DataRate {
      let s = Scenario::new(test_name);
      let net_conf = NetworkSimulationConfig::new();
      net_conf.bandwidth = DataRate::KilobitsPerSec(2000);
      net_conf.delay = TimeDelta::Millis(50);
    let client = s.CreateClient("send", |c: &mut CallClientConfig| {
        c.transport.rates.start_rate = DataRate::KilobitsPerSec(1000);
      });
    let send_net = {s.CreateSimulationNode(net_conf)};
    let ret_net = {s.CreateSimulationNode(net_conf)};
    let route = s.CreateRoutes(
          client, send_net, s.CreateClient("return", CallClientConfig()), ret_net);
      s.CreateVideoStream(route.forward(), VideoStreamConfig());

      s.RunFor(TimeDelta::Seconds(10));
      assert!(client.send_bandwidth().kbps() > 1500);

      let RembLimit: DataRate = DataRate::KilobitsPerSec(250);
      client.SetRemoteBitrate(RembLimit);
      s.RunFor(TimeDelta::Seconds(1));
      assert_eq!(client.send_bandwidth(), RembLimit);

      let RembLimitLifted: DataRate = DataRate::KilobitsPerSec(10000);
      client.SetRemoteBitrate(RembLimitLifted);
      s.RunFor(TimeDelta::Seconds(10));

      return client.send_bandwidth();
    }
    */

    fn create_controller(
        field_trials: FieldTrials,
        feedback_only: bool,
    ) -> GoogCcNetworkController {
        let config = NetworkControllerConfig {
            field_trials,
            constraints: TargetRateConstraints {
                at_time: Timestamp::zero(),
                min_data_rate: Some(DataRate::from_kilobits_per_sec(0)),
                max_data_rate: Some(DataRate::from_kilobits_per_sec(5 * INITIAL_BITRATE_KBPS)),
                starting_rate: Some(DataRate::from_kilobits_per_sec(INITIAL_BITRATE_KBPS)),
            },
            ..Default::default()
        };

        let goog_cc_config = GoogCcConfig { feedback_only };

        GoogCcNetworkController::new(config, goog_cc_config)
    }

    #[test]
    fn initialize_target_rate_on_first_process_interval_after_network_available() {
        let mut controller = create_controller(FieldTrials::default(), false);

        let _ = controller.on_network_availability(NetworkAvailability {
            at_time: Timestamp::from_millis(123456),
            network_available: true,
        });
        let update = controller.on_process_interval(ProcessInterval {
            at_time: Timestamp::from_millis(123456),
            ..Default::default()
        });

        assert_eq!(update.target_rate.unwrap().target_rate, INITIAL_BITRATE);
        assert_eq!(
            update.pacer_config.unwrap().data_rate(),
            INITIAL_BITRATE * DEFAULT_PACING_RATE
        );
        assert_eq!(
            update.probe_cluster_configs[0].target_data_rate,
            INITIAL_BITRATE * 3
        );
        assert_eq!(
            update.probe_cluster_configs[1].target_data_rate,
            INITIAL_BITRATE * 5
        );
    }

    #[test]
    fn reacts_to_changed_network_conditions() {
        let mut controller = create_controller(FieldTrials::default(), false);
        let mut current_time: Timestamp = Timestamp::from_millis(123);
        controller.on_network_availability(NetworkAvailability {
            at_time: current_time,
            network_available: true,
        });
        controller.on_process_interval(ProcessInterval {
            at_time: current_time,
            ..Default::default()
        });
        controller.on_remote_bitrate_report(RemoteBitrateReport {
            receive_time: current_time,
            bandwidth: INITIAL_BITRATE * 2,
        });

        current_time += TimeDelta::from_millis(25);
        let update = controller.on_process_interval(ProcessInterval {
            at_time: current_time,
            ..Default::default()
        });
        assert_eq!(update.target_rate.unwrap().target_rate, INITIAL_BITRATE * 2);
        assert_eq!(
            update.pacer_config.unwrap().data_rate(),
            INITIAL_BITRATE * 2 * DEFAULT_PACING_RATE
        );

        controller.on_remote_bitrate_report(RemoteBitrateReport {
            receive_time: current_time,
            bandwidth: INITIAL_BITRATE,
        });
        current_time += TimeDelta::from_millis(25);
        let update = controller.on_process_interval(ProcessInterval {
            at_time: current_time,
            ..Default::default()
        });
        assert_eq!(update.target_rate.unwrap().target_rate, INITIAL_BITRATE);
        assert_eq!(
            update.pacer_config.unwrap().data_rate(),
            INITIAL_BITRATE * DEFAULT_PACING_RATE
        );
    }

    #[test]
    fn on_network_route_changed() {
        let mut controller = create_controller(FieldTrials::default(), false);
        let current_time: Timestamp = Timestamp::from_millis(123);
        controller.on_network_availability(NetworkAvailability {
            at_time: current_time,
            network_available: true,
        });
        let new_bitrate: DataRate = DataRate::from_bits_per_sec(200000);

        let update = controller.on_network_route_change(create_route_change(
            current_time,
            Some(new_bitrate),
            None,
            None,
        ));
        assert_eq!(update.target_rate.unwrap().target_rate, new_bitrate);
        assert_eq!(
            update.pacer_config.unwrap().data_rate(),
            new_bitrate * DEFAULT_PACING_RATE
        );
        assert_eq!(update.probe_cluster_configs.len(), 2);

        // If the bitrate is reset to -1, the new starting bitrate will be
        // the minimum default bitrate.
        const DEFAULT_MIN_BITRATE: DataRate = DataRate::from_kilobits_per_sec(5);
        let update =
            controller.on_network_route_change(create_route_change(current_time, None, None, None));
        assert_eq!(update.target_rate.unwrap().target_rate, DEFAULT_MIN_BITRATE);
        assert_relative_eq!(
            update.pacer_config.unwrap().data_rate().bps_float(),
            DEFAULT_MIN_BITRATE.bps_float() * DEFAULT_PACING_RATE,
            epsilon = 10.0
        );
        assert_eq!(update.probe_cluster_configs.len(), 2);
    }

    #[test]
    fn probe_on_route_change() {
        let mut controller = create_controller(FieldTrials::default(), false);
        let mut current_time: Timestamp = Timestamp::from_millis(123);
        controller.on_network_availability(NetworkAvailability {
            at_time: current_time,
            network_available: true,
        });
        current_time += TimeDelta::from_seconds(3);

        let update = controller.on_network_route_change(create_route_change(
            current_time,
            Some(2 * INITIAL_BITRATE),
            Some(DataRate::zero()),
            Some(20 * INITIAL_BITRATE),
        ));

        assert!(update.pacer_config.is_some());
        assert_eq!(update.target_rate.unwrap().target_rate, INITIAL_BITRATE * 2);
        assert_eq!(update.probe_cluster_configs.len(), 2);
        assert_eq!(
            update.probe_cluster_configs[0].target_data_rate,
            INITIAL_BITRATE * 6
        );
        assert_eq!(
            update.probe_cluster_configs[1].target_data_rate,
            INITIAL_BITRATE * 12
        );

        controller.on_process_interval(ProcessInterval {
            at_time: current_time,
            ..Default::default()
        });
    }

    #[test]
    fn probe_after_route_change_when_transport_writable() {
        let mut controller = create_controller(FieldTrials::default(), false);
        let current_time: Timestamp = Timestamp::from_millis(123);

        let mut update: NetworkControlUpdate =
            controller.on_network_availability(NetworkAvailability {
                at_time: current_time,
                network_available: false,
            });
        assert!(update.probe_cluster_configs.is_empty());

        update = controller.on_network_route_change(create_route_change(
            current_time,
            Some(2 * INITIAL_BITRATE),
            Some(DataRate::zero()),
            Some(20 * INITIAL_BITRATE),
        ));
        // Transport is not writable. So not point in sending a probe.
        assert!(update.probe_cluster_configs.is_empty());

        // Probe is sent when transport becomes writable.
        update = controller.on_network_availability(NetworkAvailability {
            at_time: current_time,
            network_available: true,
        });
        assert!(!update.probe_cluster_configs.is_empty());
    }

    // Bandwidth estimation is updated when feedbacks are received.
    // Feedbacks which show an increasing delay cause the estimation to be reduced.
    #[test]
    fn updates_delay_based_estimate() {
        let mut controller = create_controller(FieldTrials::default(), false);
        const RUN_TIME_MS: i64 = 6000;
        let mut current_time: Timestamp = Timestamp::from_millis(123);
        controller.on_network_availability(NetworkAvailability {
            at_time: current_time,
            network_available: true,
        });

        // The test must run and insert packets/feedback long enough that the
        // BWE computes a valid estimate. This is first done in an environment which
        // simulates no bandwidth limitation, and therefore not built-up delay.
        let target_bitrate_before_delay: Option<DataRate> = packet_transmission_and_feedback_block(
            &mut controller,
            RUN_TIME_MS,
            0,
            &mut current_time,
        );
        assert!(target_bitrate_before_delay.is_some());

        // Repeat, but this time with a building delay, and make sure that the
        // estimation is adjusted downwards.
        let target_bitrate_after_delay: Option<DataRate> = packet_transmission_and_feedback_block(
            &mut controller,
            RUN_TIME_MS,
            50,
            &mut current_time,
        );
        assert!(target_bitrate_after_delay.unwrap() < target_bitrate_before_delay.unwrap());
    }

    #[test]
    fn pace_at_max_of_lower_link_capacity_and_bwe() {
        let field_trials = FieldTrials {
            pace_at_max_of_bwe_and_lower_link_capacity: true,
            ..Default::default()
        };
        let mut controller = create_controller(field_trials, false);
        let mut current_time: Timestamp = Timestamp::from_millis(123);
        controller.on_network_availability(NetworkAvailability {
            at_time: current_time,
            network_available: true,
        });
        controller.on_process_interval(ProcessInterval {
            at_time: current_time,
            ..Default::default()
        });
        current_time += TimeDelta::from_millis(100);
        let mut network_estimate: NetworkStateEstimate = NetworkStateEstimate {
            link_capacity_lower: 10 * INITIAL_BITRATE,
            ..Default::default()
        };
        controller.set_network_state_estimate(Some(network_estimate));
        // OnNetworkStateEstimate does not trigger processing a new estimate. So add a
        // dummy loss report to trigger a BWE update in the next process interval.
        let loss_report = TransportLossReport {
            start_time: current_time,
            end_time: current_time,
            receive_time: current_time,
            packets_received_delta: 50,
            packets_lost_delta: 1,
        };
        controller.on_transport_loss_report(loss_report);
        let update = controller.on_process_interval(ProcessInterval {
            at_time: current_time,
            ..Default::default()
        });
        assert!(update.pacer_config.is_some());
        assert!(update.target_rate.is_some());
        assert!(update.target_rate.unwrap().target_rate < network_estimate.link_capacity_lower);
        assert_eq!(
            update.pacer_config.unwrap().data_rate().kbps_float(),
            network_estimate.link_capacity_lower.kbps_float() * DEFAULT_PACING_RATE
        );

        current_time += TimeDelta::from_millis(100);
        // Set a low link capacity estimate and verify that pacing rate is set
        // relative to loss based/delay based estimate.
        network_estimate = NetworkStateEstimate {
            link_capacity_lower: 0.5 * INITIAL_BITRATE,
            ..Default::default()
        };
        controller.set_network_state_estimate(Some(network_estimate));
        // Again, we need to inject a dummy loss report to trigger an update of the
        // BWE in the next process interval.
        let loss_report = TransportLossReport {
            start_time: current_time,
            end_time: current_time,
            receive_time: current_time,
            packets_received_delta: 50,
            packets_lost_delta: 0,
        };
        controller.on_transport_loss_report(loss_report);
        let update = controller.on_process_interval(ProcessInterval {
            at_time: current_time,
            ..Default::default()
        });
        assert!(update.target_rate.is_some());
        assert!(
            update.target_rate.as_ref().unwrap().target_rate > network_estimate.link_capacity_lower
        );
        // NOTE: These are off by 4 bps
        assert_eq!(
            update.pacer_config.unwrap().data_rate().kbps(),
            (update.target_rate.unwrap().target_rate * DEFAULT_PACING_RATE).kbps(),
        );
    }

    #[test]
    fn limit_pacing_factor_to_upper_link_capacity() {
        let field_trials = FieldTrials {
            limit_pacing_factor_by_upper_link_capacity_estimate: true,
            ..Default::default()
        };
        let mut controller = create_controller(field_trials, false);
        let mut current_time: Timestamp = Timestamp::from_millis(123);
        controller.on_network_availability(NetworkAvailability {
            at_time: current_time,
            network_available: true,
        });
        controller.on_process_interval(ProcessInterval {
            at_time: current_time,
            ..Default::default()
        });
        current_time += TimeDelta::from_millis(100);
        let network_estimate: NetworkStateEstimate = NetworkStateEstimate {
            link_capacity_upper: (INITIAL_BITRATE * DEFAULT_PACING_RATE) / 2,
            ..Default::default()
        };
        controller.set_network_state_estimate(Some(network_estimate));
        // OnNetworkStateEstimate does not trigger processing a new estimate. So add a
        // dummy loss report to trigger a BWE update in the next process interval.
        let loss_report = TransportLossReport {
            start_time: current_time,
            end_time: current_time,
            receive_time: current_time,
            packets_received_delta: 50,
            packets_lost_delta: 1,
        };
        controller.on_transport_loss_report(loss_report);
        let update = controller.on_process_interval(ProcessInterval {
            at_time: current_time,
            ..Default::default()
        });
        assert!(update.pacer_config.is_some());
        assert!(update.target_rate.is_some());
        assert!(update.target_rate.unwrap().target_rate >= INITIAL_BITRATE);
        assert_eq!(
            update.pacer_config.unwrap().data_rate(),
            network_estimate.link_capacity_upper
        );
    }

    // Test congestion window pushback on network delay happens.
    /*
    #[test]
    fn CongestionWindowPushbackOnNetworkDelay() {
    let factory = CreateFeedbackOnlyFactory();
          let field_trials = FieldTrials {
           congestion_window: experiments::CongestionWindowConfig { queue_size_ms: 800, min_bitrate_bps: 30000, ..Default::default() }
           ..Default::default()
          };
      let s = Scenario::new("googcc_unit/cwnd_on_delay", false);
    let send_net =
          s.CreateMutableSimulationNode(|c: &mut NetworkSimulcationConfig| {
            c.bandwidth = DataRate::KilobitsPerSec(1000);
            c.delay = TimeDelta::Millis(100);
          });
    let ret_net = s.CreateSimulationNode(
          |c: &mut NetworkSimulationConfig| { c.delay = TimeDelta::Millis(100); });
      let mut config = CallClientConfig::default();
      config.transport.cc_factory = &factory;
      // Start high so bandwidth drop has max effect.
      config.transport.rates.start_rate = DataRate::KilobitsPerSec(300);
      config.transport.rates.max_rate = DataRate::KilobitsPerSec(2000);
      config.transport.rates.min_rate = DataRate::KilobitsPerSec(10);

    let client = CreateVideoSendingClient(&s, config,
                                              {send_net.node()}, {ret_net});

      s.RunFor(TimeDelta::Seconds(10));
      send_net.PauseTransmissionUntil(s.Now() + TimeDelta::Seconds(10));
      s.RunFor(TimeDelta::Seconds(3));

      // After 3 seconds without feedback from any sent packets, we expect that the
      // target rate is reduced to the minimum pushback threshold
      // DefaultMinPushbackTargetBitrateBps, which is defined as 30 kbps in
      // congestion_window_pushback_controller.
      EXPECT_LT(client.target_rate().kbps(), 40);
    }
    */

    /*
    // Test congestion window pushback on network delay happens.
    #[test]
    fn CongestionWindowPushbackDropFrameOnNetworkDelay() {
    let factory = CreateFeedbackOnlyFactory();
          let field_trials = FieldTrials {
           congestion_window: experiments::CongestionWindowConfig { queue_size_ms: 800, min_bitrate_bps: 30000, drop_frame: true, ..Default::default() }
           ..Default::default()
          };
      let s = Scenario::new("googcc_unit/cwnd_on_delay", false);
    let send_net =
          s.CreateMutableSimulationNode(|c: &mut NetworkSimulcationConfig| {
            c.bandwidth = DataRate::KilobitsPerSec(1000);
            c.delay = TimeDelta::Millis(100);
          });
    let ret_net = s.CreateSimulationNode(
          |c: &mut NetworkSimulationConfig| { c.delay = TimeDelta::Millis(100); });
      let mut config = CallClientConfig::default();
      config.transport.cc_factory = &factory;
      // Start high so bandwidth drop has max effect.
      config.transport.rates.start_rate = DataRate::KilobitsPerSec(300);
      config.transport.rates.max_rate = DataRate::KilobitsPerSec(2000);
      config.transport.rates.min_rate = DataRate::KilobitsPerSec(10);

    let client = CreateVideoSendingClient(&s, config,
                                              {send_net.node()}, {ret_net});

      s.RunFor(TimeDelta::Seconds(10));
      send_net.PauseTransmissionUntil(s.Now() + TimeDelta::Seconds(10));
      s.RunFor(TimeDelta::Seconds(3));

      // As the dropframe is set, after 3 seconds without feedback from any sent
      // packets, we expect that the target rate is not reduced by congestion
      // window.
      assert!(client.target_rate().kbps() > 300);
    }
    */

    /*
    #[test]
    fn PaddingRateLimitedByCongestionWindowInTrial() {
         let field_trials = FieldTrials {
           congestion_window: experiments::CongestionWindowConfig { queue_size_ms: 200, min_bitrate_bps: 30000, ..Default::default() }
           ..Default::default()
          };

      let s = Scenario::new("googcc_unit/padding_limited", false);
    let send_net =
          s.CreateMutableSimulationNode(|c: &mut NetworkSimulcationConfig| {
            c.bandwidth = DataRate::KilobitsPerSec(1000);
            c.delay = TimeDelta::Millis(100);
          });
    let ret_net = s.CreateSimulationNode(
          |c: &mut NetworkSimulationConfig| { c.delay = TimeDelta::Millis(100); });
      let mut config = CallClientConfig::default();
      // Start high so bandwidth drop has max effect.
      config.transport.rates.start_rate = DataRate::KilobitsPerSec(1000);
      config.transport.rates.max_rate = DataRate::KilobitsPerSec(2000);
    let client = s.CreateClient("send", config);
    let route =
          s.CreateRoutes(client, {send_net.node()},
                         s.CreateClient("return", CallClientConfig()), {ret_net});
      let mut video = VideoStreamConfig::default();
      video.stream.pad_to_rate = config.transport.rates.max_rate;
      s.CreateVideoStream(route.forward(), video);

      // Run for a few seconds to allow the controller to stabilize.
      s.RunFor(TimeDelta::Seconds(10));

      // Check that padding rate matches target rate.
      assert_relative_eq!(client.padding_rate().kbps(), client.target_rate().kbps(), 1);

      // Check this is also the case when congestion window pushback kicks in.
      send_net.PauseTransmissionUntil(s.Now() + TimeDelta::Seconds(1));
      assert_relative_eq!(client.padding_rate().kbps(), client.target_rate().kbps(), 1);
    }
    */

    /*
    #[test]
    fn LimitsToFloorIfRttIsHighInTrial() {
      // The field trial limits maximum RTT to 2 seconds, higher RTT means that the
      // controller backs off until it reaches the minimum configured bitrate. This
      // allows the RTT to recover faster than the regular control mechanism would
      // achieve.
      const BandwidthFloor: DataRate = DataRate::KilobitsPerSec(50);
             let field_trials = FieldTrials {
               max_rtt_limit: experiments::MaxRttLimitConfig { max_rtt: TimeDelta::Seconds(2), floor: BandwidthFloor, ..Default::default() },
               ..Default::default()
             };
      // In the test case, we limit the capacity and add a cross traffic packet
      // burst that blocks media from being sent. This causes the RTT to quickly
      // increase above the threshold in the trial.
      const LinkCapacity: DataRate = DataRate::KilobitsPerSec(100);
      const BufferBloatDuration: TimeDelta = TimeDelta::Seconds(10);
      let s = Scenario::new("googcc_unit/limit_trial", false);
    let send_net = s.CreateSimulationNode(|c: &mut NetworkSimulcationConfig| {
        c.bandwidth = LinkCapacity;
        c.delay = TimeDelta::Millis(100);
      });
    let ret_net = s.CreateSimulationNode(
          |c: &mut NetworkSimulationConfig| { c.delay = TimeDelta::Millis(100); });
      let mut config = CallClientConfig::default();
      config.transport.rates.start_rate = LinkCapacity;

    let client = CreateVideoSendingClient(&s, config, {send_net}, {ret_net});
      // Run for a few seconds to allow the controller to stabilize.
      s.RunFor(TimeDelta::Seconds(10));
      const BloatPacketSize: DataSize = DataSize::Bytes(1000);
      const BloatPacketCount: i64 =
          (BufferBloatDuration * LinkCapacity / BloatPacketSize) as i64;
      // This will cause the RTT to be large for a while.
      s.TriggerPacketBurst({send_net}, BloatPacketCount, BloatPacketSize.bytes());
      // Wait to allow the high RTT to be detected and acted upon.
      s.RunFor(TimeDelta::Seconds(6));
      // By now the target rate should have dropped to the minimum configured rate.
      assert_relative_eq!(client.target_rate().kbps(), BandwidthFloor.kbps(), 5);
    }
    */

    /*
    #[test]
    fn UpdatesTargetRateBasedOnLinkCapacity() {
      UpdatesTargetRateBasedOnLinkCapacity();
    }
    */

    /*
    #[test]
    fn StableEstimateDoesNotVaryInSteadyState() {
    let factory = CreateFeedbackOnlyFactory();
      let s = Scenario::new("googcc_unit/stable_target", false);
      let mut config = CallClientConfig::default();
      config.transport.cc_factory = &factory;
      let mut net_conf = NetworkSimulationConfig::default();
      net_conf.bandwidth = DataRate::KilobitsPerSec(500);
      net_conf.delay = TimeDelta::Millis(100);
    let send_net = s.CreateSimulationNode(net_conf);
    let ret_net = s.CreateSimulationNode(net_conf);

    let client = CreateVideoSendingClient(&s, config, {send_net}, {ret_net});
      // Run for a while to allow the estimate to stabilize.
      s.RunFor(TimeDelta::Seconds(30));
      let min_stable_target: DataRate = DataRate::plus_infinity();
      let max_stable_target: DataRate = DataRate::minus_infinity();
      let min_target: DataRate = DataRate::plus_infinity();
      let max_target: DataRate = DataRate::minus_infinity();

      // Measure variation in steady state.
       for i in 0..20 {
    let stable_target_rate = client.stable_target_rate();
    let target_rate = client.target_rate();
        assert!(stable_target_rate <= target_rate);

        min_stable_target = std::cmp::min(min_stable_target, stable_target_rate);
        max_stable_target = std::cmp::max(max_stable_target, stable_target_rate);
        min_target = std::cmp::min(min_target, target_rate);
        max_target = std::cmp::max(max_target, target_rate);
        s.RunFor(TimeDelta::Seconds(1));
      }
      // We should expect drops by at least 15% (default backoff.)
      assert!(min_target / max_target < 0.85);
      // We should expect the stable target to be more stable than the immediate one
      assert!(min_stable_target / max_stable_target >= min_target / max_target);
    }
    */

    /*
    #[test]
    fn LossBasedControlUpdatesTargetRateBasedOnLinkCapacity() {
      ScopedFieldTrials trial("WebRTC-Bwe-LossBasedControl/Enabled/");
      // TODO(srte): Should the behavior be unaffected at low loss rates?
      UpdatesTargetRateBasedOnLinkCapacity("_loss_based");
    }
    */

    /*
    #[test]
    fn LossBasedControlDoesModestBackoffToHighLoss() {
      ScopedFieldTrials trial("WebRTC-Bwe-LossBasedControl/Enabled/");
      let s = Scenario::new("googcc_unit/high_loss_channel", false);
      let mut config = CallClientConfig::default();
      config.transport.rates.min_rate = DataRate::KilobitsPerSec(10);
      config.transport.rates.max_rate = DataRate::KilobitsPerSec(1500);
      config.transport.rates.start_rate = DataRate::KilobitsPerSec(300);
    let send_net = s.CreateSimulationNode(|c: &mut NetworkSimulationConfig| {
        c.bandwidth = DataRate::KilobitsPerSec(2000);
        c.delay = TimeDelta::Millis(200);
        c.loss_rate = 0.1;
      });
    let ret_net = s.CreateSimulationNode(
          |c: &mut NetworkSimulationConfig| { c.delay = TimeDelta::Millis(200); });

    let client = CreateVideoSendingClient(&s, config, {send_net}, {ret_net});

      s.RunFor(TimeDelta::Seconds(120));
      // Without LossBasedControl trial, bandwidth drops to ~10 kbps.
      EXPECT_GT(client.target_rate().kbps(), 100);
    }
    */

    /*
    fn AverageBitrateAfterCrossInducedLoss(absl::string_view name) -> DataRate {
      Scenario s(name, false);
      let mut net_conf = NetworkSimulationConfig::default();
      net_conf.bandwidth = DataRate::KilobitsPerSec(1000);
      net_conf.delay = TimeDelta::Millis(100);
      // Short queue length means that we'll induce loss when sudden TCP traffic
      // spikes are induced. This corresponds to ca 200 ms for a packet size of 1000
      // bytes. Such limited buffers are common on for instance wifi routers.
      net_conf.packet_queue_length_limit = 25;

    let send_net = {s.CreateSimulationNode(net_conf)};
    let ret_net = {s.CreateSimulationNode(net_conf)};

    let client = s.CreateClient("send", CallClientConfig());
    let callee = s.CreateClient("return", CallClientConfig());
    let route = s.CreateRoutes(client, send_net, callee, ret_net);
      // TODO(srte): Make this work with RTX enabled or remove it.
    let video = s.CreateVideoStream(route.forward(), [](VideoStreamConfig* c) {
        c.stream.use_rtx = false;
      });
      s.RunFor(TimeDelta::Seconds(10));
      for i in 0..4 {
        // Sends TCP cross traffic inducing loss.
    let tcp_traffic = s.net().StartCrossTraffic(CreateFakeTcpCrossTraffic(
            s.net().CreateRoute(send_net), s.net().CreateRoute(ret_net),
            FakeTcpConfig()));
        s.RunFor(TimeDelta::Seconds(2));
        // Allow the ccongestion controller to recover.
        s.net().StopCrossTraffic(tcp_traffic);
        s.RunFor(TimeDelta::Seconds(20));
      }

      // Querying the video stats from within the expected runtime environment
      // (i.e. the TQ that belongs to the CallClient, not the Scenario TQ that
      // we're currently on).
      VideoReceiveStreamInterface::Stats video_receive_stats;
    let video_stream = video.receive();
      callee.SendTask([&video_stream, &video_receive_stats]() {
        video_receive_stats = video_stream.GetStats();
      });
      return DataSize::Bytes(
                 video_receive_stats.rtp_stats.packet_counter.TotalBytes()) /
             s.TimeSinceStart();
    }
    */

    /*
    #[test]
    fn MaintainsLowRateInSafeResetTrial() {
      const LinkCapacity: DataRate = DataRate::KilobitsPerSec(200);
      const StartRate: DataRate = DataRate::KilobitsPerSec(300);

      ScopedFieldTrials trial("WebRTC-Bwe-SafeResetOnRouteChange/Enabled/");
      Scenario s("googcc_unit/safe_reset_low");
    let send_net = s.CreateSimulationNode([&](NetworkSimulationConfig* c) {
        c.bandwidth = LinkCapacity;
        c.delay = TimeDelta::Millis(10);
      });
    let client = s.CreateClient("send", [&](CallClientConfig* c) {
        c.transport.rates.start_rate = StartRate;
      });
    let route = s.CreateRoutes(
          client, {send_net}, s.CreateClient("return", CallClientConfig()),
          {s.CreateSimulationNode(NetworkSimulationConfig())});
      s.CreateVideoStream(route.forward(), VideoStreamConfig());
      // Allow the controller to stabilize.
      s.RunFor(TimeDelta::Millis(500));
      assert_relative_eq!(client.send_bandwidth().kbps(), LinkCapacity.kbps(), 50);
      s.ChangeRoute(route.forward(), {send_net});
      // Allow new settings to propagate.
      s.RunFor(TimeDelta::Millis(100));
      // Under the trial, the target should be unchanged for low rates.
      assert_relative_eq!(client.send_bandwidth().kbps(), LinkCapacity.kbps(), 50);
    }
    */

    /*
    #[test]
    fn DoNotResetBweUnlessNetworkAdapterChangeOnRoutChange() {
      ScopedFieldTrials trial("WebRTC-Bwe-ResetOnAdapterIdChange/Enabled/");
      Scenario s("googcc_unit/do_not_reset_bwe_unless_adapter_change");

      const LinkCapacity: DataRate = DataRate::KilobitsPerSec(1000);
      const StartRate: DataRate = DataRate::KilobitsPerSec(300);

    let send_net = s.CreateSimulationNode([&](NetworkSimulationConfig* c) {
        c.bandwidth = LinkCapacity;
        c.delay = TimeDelta::Millis(50);
      });
    let client = s.CreateClient("send", [&](CallClientConfig* c) {
        c.transport.rates.start_rate = StartRate;
      });
      client.UpdateNetworkAdapterId(0);
    let route = s.CreateRoutes(
          client, {send_net}, s.CreateClient("return", CallClientConfig()),
          {s.CreateSimulationNode(NetworkSimulationConfig())});
      s.CreateVideoStream(route.forward(), VideoStreamConfig());
      // Allow the controller to stabilize.
      s.RunFor(TimeDelta::Millis(500));
      assert_relative_eq!(client.send_bandwidth().kbps(), LinkCapacity.kbps(), 300);
      s.ChangeRoute(route.forward(), {send_net});
      // Allow new settings to propagate.
      s.RunFor(TimeDelta::Millis(50));
      // Under the trial, the target should not drop.
      assert_relative_eq!(client.send_bandwidth().kbps(), LinkCapacity.kbps(), 300);

      s.RunFor(TimeDelta::Millis(500));
      // But if adapter id change, BWE should reset and start from the beginning if
      // the network route changes.
      client.UpdateNetworkAdapterId(1);
      s.ChangeRoute(route.forward(), {send_net});
      // Allow new settings to propagate.
      s.RunFor(TimeDelta::Millis(50));
      assert_relative_eq!(client.send_bandwidth().kbps(), StartRate.kbps(), 30);
    }
    */

    /*
    #[test]
    fn CutsHighRateInSafeResetTrial() {
      const LinkCapacity: DataRate = DataRate::KilobitsPerSec(1000);
      const StartRate: DataRate = DataRate::KilobitsPerSec(300);

      ScopedFieldTrials trial("WebRTC-Bwe-SafeResetOnRouteChange/Enabled/");
      Scenario s("googcc_unit/safe_reset_high_cut");
    let send_net = s.CreateSimulationNode([&](NetworkSimulationConfig* c) {
        c.bandwidth = LinkCapacity;
        c.delay = TimeDelta::Millis(50);
      });
    let client = s.CreateClient("send", [&](CallClientConfig* c) {
        c.transport.rates.start_rate = StartRate;
      });
    let route = s.CreateRoutes(
          client, {send_net}, s.CreateClient("return", CallClientConfig()),
          {s.CreateSimulationNode(NetworkSimulationConfig())});
      s.CreateVideoStream(route.forward(), VideoStreamConfig());
      // Allow the controller to stabilize.
      s.RunFor(TimeDelta::Millis(500));
      assert_relative_eq!(client.send_bandwidth().kbps(), LinkCapacity.kbps(), 300);
      client.UpdateNetworkAdapterId(1);
      s.ChangeRoute(route.forward(), {send_net});
      // Allow new settings to propagate.
      s.RunFor(TimeDelta::Millis(50));
      // Under the trial, the target should be reset from high values.
      assert_relative_eq!(client.send_bandwidth().kbps(), StartRate.kbps(), 30);
    }
    */

    /*
    #[test]
    fn DetectsHighRateInSafeResetTrial() {
      ScopedFieldTrials trial("WebRTC-Bwe-SafeResetOnRouteChange/Enabled,ack/");
      const InitialLinkCapacity: DataRate = DataRate::KilobitsPerSec(200);
      const NewLinkCapacity: DataRate = DataRate::KilobitsPerSec(800);
      const StartRate: DataRate = DataRate::KilobitsPerSec(300);

      Scenario s("googcc_unit/safe_reset_high_detect");
    let initial_net = s.CreateSimulationNode([&](NetworkSimulationConfig* c) {
        c.bandwidth = InitialLinkCapacity;
        c.delay = TimeDelta::Millis(50);
      });
    let new_net = s.CreateSimulationNode([&](NetworkSimulationConfig* c) {
        c.bandwidth = NewLinkCapacity;
        c.delay = TimeDelta::Millis(50);
      });
    let client = s.CreateClient("send", [&](CallClientConfig* c) {
        c.transport.rates.start_rate = StartRate;
      });
    let route = s.CreateRoutes(
          client, {initial_net}, s.CreateClient("return", CallClientConfig()),
          {s.CreateSimulationNode(NetworkSimulationConfig())});
      s.CreateVideoStream(route.forward(), VideoStreamConfig());
      // Allow the controller to stabilize.
      s.RunFor(TimeDelta::Millis(2000));
      assert_relative_eq!(client.send_bandwidth().kbps(), InitialLinkCapacity.kbps(), 50);
      client.UpdateNetworkAdapterId(1);
      s.ChangeRoute(route.forward(), {new_net});
      // Allow new settings to propagate, but not probes to be received.
      s.RunFor(TimeDelta::Millis(50));
      // Under the field trial, the target rate should be unchanged since it's lower
      // than the starting rate.
      assert_relative_eq!(client.send_bandwidth().kbps(), InitialLinkCapacity.kbps(), 50);
      // However, probing should have made us detect the higher rate.
      // NOTE: This test causes high loss rate, and the loss-based estimator reduces
      // the bitrate, making the test fail if we wait longer than one second here.
      s.RunFor(TimeDelta::Millis(1000));
      EXPECT_GT(client.send_bandwidth().kbps(), NewLinkCapacity.kbps() - 300);
    }
    */

    /*
    #[test]
    fn TargetRateReducedOnPacingBufferBuildupInTrial() {
      // Configure strict pacing to ensure build-up.
      ScopedFieldTrials trial(
          "WebRTC-CongestionWindow/QueueSize:100,MinBitrate:30000/"
          "WebRTC-Video-Pacing/factor:1.0/"
          "WebRTC-AddPacingToCongestionWindowPushback/Enabled/");

      const LinkCapacity: DataRate = DataRate::KilobitsPerSec(1000);
      const StartRate: DataRate = DataRate::KilobitsPerSec(1000);

      Scenario s("googcc_unit/pacing_buffer_buildup");
    let net = s.CreateSimulationNode([&](NetworkSimulationConfig* c) {
        c.bandwidth = LinkCapacity;
        c.delay = TimeDelta::Millis(50);
      });
    let client = s.CreateClient("send", [&](CallClientConfig* c) {
        c.transport.rates.start_rate = StartRate;
      });
    let route = s.CreateRoutes(
          client, {net}, s.CreateClient("return", CallClientConfig()),
          {s.CreateSimulationNode(NetworkSimulationConfig())});
      s.CreateVideoStream(route.forward(), VideoStreamConfig());
      // Allow some time for the buffer to build up.
      s.RunFor(TimeDelta::Seconds(5));

      // Without trial, pacer delay reaches ~250 ms.
      EXPECT_LT(client.GetStats().pacer_delay_ms, 150);
    }
    */

    /*
    #[test]
    fn NoBandwidthTogglingInLossControlTrial() {
      ScopedFieldTrials trial("WebRTC-Bwe-LossBasedControl/Enabled/");
      Scenario s("googcc_unit/no_toggling");
    let send_net = s.CreateSimulationNode([&](NetworkSimulationConfig* c) {
        c.bandwidth = DataRate::KilobitsPerSec(2000);
        c.loss_rate = 0.2;
        c.delay = TimeDelta::Millis(10);
      });

    let client = s.CreateClient("send", [&](CallClientConfig* c) {
        c.transport.rates.start_rate = DataRate::KilobitsPerSec(300);
      });
    let route = s.CreateRoutes(
          client, {send_net}, s.CreateClient("return", CallClientConfig()),
          {s.CreateSimulationNode(NetworkSimulationConfig())});
      s.CreateVideoStream(route.forward(), VideoStreamConfig());
      // Allow the controller to initialize.
      s.RunFor(TimeDelta::Millis(250));

      std::queue<DataRate> bandwidth_history;
      const step: TimeDelta = TimeDelta::Millis(50);
      for (TimeDelta time = TimeDelta::zero(); time < TimeDelta::Millis(2000);
           time += step) {
        s.RunFor(step);
        const window: TimeDelta = TimeDelta::Millis(500);
        if (bandwidth_history.len() >= window / step)
          bandwidth_history.pop();
        bandwidth_history.push(client.send_bandwidth());
        EXPECT_LT(
            CountBandwidthDips(bandwidth_history, DataRate::KilobitsPerSec(100)),
            2);
      }
    }

    #[test]
    fn NoRttBackoffCollapseWhenVideoStops() {
      ScopedFieldTrials trial("WebRTC-Bwe-MaxRttLimit/limit:2s/");
      Scenario s("googcc_unit/rttbackoff_video_stop");
    let send_net = s.CreateSimulationNode([&](NetworkSimulationConfig* c) {
        c.bandwidth = DataRate::KilobitsPerSec(2000);
        c.delay = TimeDelta::Millis(100);
      });

    let client = s.CreateClient("send", [&](CallClientConfig* c) {
        c.transport.rates.start_rate = DataRate::KilobitsPerSec(1000);
      });
    let route = s.CreateRoutes(
          client, {send_net}, s.CreateClient("return", CallClientConfig()),
          {s.CreateSimulationNode(NetworkSimulationConfig())});
    let video = s.CreateVideoStream(route.forward(), VideoStreamConfig());
      // Allow the controller to initialize, then stop video.
      s.RunFor(TimeDelta::Seconds(1));
      video.send().Stop();
      s.RunFor(TimeDelta::Seconds(4));
      EXPECT_GT(client.send_bandwidth().kbps(), 1000);
    }

    #[test]
    fn NoCrashOnVeryLateFeedback() {
      Scenario s;
    let ret_net = s.CreateMutableSimulationNode(NetworkSimulationConfig());
    let route = s.CreateRoutes(
          s.CreateClient("send", CallClientConfig()),
          {s.CreateSimulationNode(NetworkSimulationConfig())},
          s.CreateClient("return", CallClientConfig()), {ret_net.node()});
    let video = s.CreateVideoStream(route.forward(), VideoStreamConfig());
      s.RunFor(TimeDelta::Seconds(5));
      // Delay feedback by several minutes. This will cause removal of the send time
      // history for the packets as long as SendTimeHistoryWindow is configured for
      // a shorter time span.
      ret_net.PauseTransmissionUntil(s.Now() + TimeDelta::Seconds(300));
      // Stopping video stream while waiting to save test execution time.
      video.send().Stop();
      s.RunFor(TimeDelta::Seconds(299));
      // Starting to cause addition of new packet to history, which cause old
      // packets to be removed.
      video.send().Start();
      // Runs until the lost packets are received. We expect that this will run
      // without causing any runtime failures.
      s.RunFor(TimeDelta::Seconds(2));
    }

    #[test]
    fn IsFairToTCP() {
      Scenario s("googcc_unit/tcp_fairness");
      let mut net_conf = NetworkSimulationConfig::default();
      net_conf.bandwidth = DataRate::KilobitsPerSec(1000);
      net_conf.delay = TimeDelta::Millis(50);
    let client = s.CreateClient("send", [&](CallClientConfig* c) {
        c.transport.rates.start_rate = DataRate::KilobitsPerSec(1000);
      });
    let send_net = {s.CreateSimulationNode(net_conf)};
    let ret_net = {s.CreateSimulationNode(net_conf)};
    let route = s.CreateRoutes(
          client, send_net, s.CreateClient("return", CallClientConfig()), ret_net);
      s.CreateVideoStream(route.forward(), VideoStreamConfig());
      s.net().StartCrossTraffic(CreateFakeTcpCrossTraffic(
          s.net().CreateRoute(send_net), s.net().CreateRoute(ret_net),
          FakeTcpConfig()));
      s.RunFor(TimeDelta::Seconds(10));

      // Currently only testing for the upper limit as we in practice back out
      // quite a lot in this scenario. If this behavior is fixed, we should add a
      // lower bound to ensure it stays fixed.
      EXPECT_LT(client.send_bandwidth().kbps(), 750);
    }

    #[test]
    fn FastRampupOnRembCapLifted() {
      let final_estimate: DataRate =
          RunRembDipScenario("googcc_unit/default_fast_rampup_on_remb_cap_lifted");
      EXPECT_GT(final_estimate.kbps(), 1500);
    }

    #[test]
    fn FallbackToLossBasedBweWithoutPacketFeedback() {
      const LinkCapacity: DataRate = DataRate::KilobitsPerSec(1000);
      const StartRate: DataRate = DataRate::KilobitsPerSec(1000);

      let s = Scenario::new("googcc_unit/high_loss_channel", false);
    let net = s.CreateMutableSimulationNode([&](NetworkSimulationConfig* c) {
        c.bandwidth = LinkCapacity;
        c.delay = TimeDelta::Millis(100);
      });
    let client = s.CreateClient("send", [&](CallClientConfig* c) {
        c.transport.rates.start_rate = StartRate;
      });
    let route = s.CreateRoutes(
          client, {net.node()}, s.CreateClient("return", CallClientConfig()),
          {s.CreateSimulationNode(NetworkSimulationConfig())});

      // Create a config without packet feedback.
      VideoStreamConfig video_config;
      video_config.stream.packet_feedback = false;
      s.CreateVideoStream(route.forward(), video_config);

      s.RunFor(TimeDelta::Seconds(20));
      // Bandwith does not backoff because network is normal.
      EXPECT_GE(client.target_rate().kbps(), 500);

      // Update the network to create high loss ratio
      net.UpdateConfig(|c: &mut NetworkSimulationConfig| { c.loss_rate = 0.15; });
      s.RunFor(TimeDelta::Seconds(20));

      // Bandwidth decreases thanks to loss based bwe v0.
      EXPECT_LE(client.target_rate().kbps(), 300);
    }
    */

    #[test]
    fn calculates_rtt_from_transport_feedback() {
        let mut controller = create_controller(FieldTrials::default(), true);
        let mut current_time: Timestamp = Timestamp::from_millis(123);
        let one_way_delay: TimeDelta = TimeDelta::from_millis(10);
        let mut rtt: Option<TimeDelta> = None;

        let feedback: TransportPacketsFeedback = create_transport_packets_feedback(
            /*per_packet_network_delay=*/ TimeDelta::from_millis(50),
            one_way_delay,
            /*send_time=*/ current_time,
        );
        controller.on_transport_packets_feedback(feedback);
        current_time += TimeDelta::from_millis(50);
        let update = controller.on_process_interval(ProcessInterval {
            at_time: current_time,
            ..Default::default()
        });
        if let Some(target_rate) = update.target_rate {
            rtt = Some(target_rate.network_estimate.round_trip_time);
        }
        assert!(rtt.is_some());
        assert_eq!(rtt.unwrap().ms(), 2 * one_way_delay.ms());
    }
}
