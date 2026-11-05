/*
 *  Copyright (c) 2018 The WebRTC project authors. All Rights Reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE file in the root of the source
 *  tree. An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the AUTHORS file in the root of the source tree.
 */

//! Upstream's `NetworkControllerInterface` also takes route changes, REMB, RTCP loss reports,
//! received packets, runtime constraint changes, and a network state estimate. A QUIC sender has
//! none of them: a path change rebuilds the controller, and the constraints come once, at
//! construction. What remains is the network becoming available, the process timer, the RTT,
//! each sent packet, each ACK's feedback, and stream settings, which carry the session's probe
//! target as the allocation GoogCC probes toward.

use std::collections::VecDeque;

use crate::gcc::{
    AlrDetector, BandwidthLimitedCause, CongestionWindowPushbackController, DelayBasedBwe,
    DelayBasedBweResult, LossBasedState, ProbeBitrateEstimator, ProbeController,
    RobustThroughputEstimator, SendSideBandwidthEstimation,
    api::{
        transport::{
            BandwidthUsage, NetworkAvailability, NetworkControlUpdate, PacedPacketInfo,
            PacerConfig, ProbeClusterConfig, ProcessInterval, RoundTripTimeUpdate, SentPacket,
            StreamsConfig, TargetRateConstraints, TargetTransferRate, TransportPacketsFeedback,
        },
        units::{DataRate, DataSize, TimeDelta, Timestamp},
    },
    experiments::{FieldTrials, RateControlSettings},
    remote_bitrate_estimator::CONGESTION_CONTROLLER_MIN_BITRATE,
};

/// Configuration sent to factory create function. The parameters here are
/// optional to use for a network controller implementation.
#[derive(Default, Debug, Clone)]
pub struct NetworkControllerConfig {
    pub field_trials: FieldTrials,

    /// The initial constraints to start with. Note that the starting rate
    /// has to be set initially to provide a starting state for the network
    /// controller, even though the field is marked as optional.
    pub constraints: TargetRateConstraints,
    /// Initial stream specific configuration.
    pub stream_based_config: StreamsConfig,
    pub default_pacing_time_window: TimeDelta,
}

/// Google Congestion Control, as WebRTC's `GoogCcNetworkController`.
#[derive(Clone)]
pub struct GoogCcNetworkController {
    use_min_allocatable_as_lower_bound: bool,
    limit_probes_lower_than_throughput_estimate: bool,
    rate_control_settings: RateControlSettings,

    probe_controller: ProbeController,
    congestion_window_pushback_controller: Option<CongestionWindowPushbackController>,

    bandwidth_estimation: SendSideBandwidthEstimation,
    alr_detector: AlrDetector,
    probe_bitrate_estimator: ProbeBitrateEstimator,
    delay_based_bwe: DelayBasedBwe,
    acknowledged_bitrate_estimator: RobustThroughputEstimator,

    initial_config: Option<NetworkControllerConfig>,

    min_target_rate: DataRate,
    min_data_rate: DataRate,
    max_data_rate: DataRate,
    starting_rate: Option<DataRate>,

    first_packet_sent: bool,
    first_transport_feedback_received: bool,

    feedback_max_rtts: VecDeque<i64>,

    last_loss_based_target_rate: DataRate,
    last_pushback_target_rate: DataRate,
    last_loss_base_state: LossBasedState,

    last_estimated_round_trip_time: TimeDelta,

    pacing_factor: Option<f64>,
    pacing_time_window: TimeDelta,
    min_total_allocated_bitrate: DataRate,

    previously_in_alr: bool,
    last_is_bandwidth_limited: bool,

    current_data_window: Option<DataSize>,
}

impl GoogCcNetworkController {
    // Pacing-rate relative to our target send rate.
    // Multiplicative factor that is applied to the target bitrate to calculate
    // the number of bytes that can be transmitted per interval.
    // Increasing this factor will result in lower delays in cases of bitrate
    // overshoots from the encoder.
    const DEFAULT_PACE_MULTIPLIER: f64 = 2.5;
    // If send side bandwidth estimation is used, this pacing multiplier is used per
    // default.
    const DEFAULT_PACE_MULTIPLIER_WITH_SEND_SIDE_BWE: f64 = 1.1;

    // If the probe result is far below the current throughput estimate
    // it's unlikely that the probe is accurate, so we don't want to drop too far.
    // However, if we actually are overusing, we want to drop to something slightly
    // below the current throughput estimate to drain the network queues.
    const PROBE_DROP_THROUGHPUT_FRACTION: f64 = 0.85;

    pub fn new(config: NetworkControllerConfig) -> Self {
        let field_trials = &config.field_trials;
        let rate_control_settings = RateControlSettings::new(field_trials);
        let starting_rate = config.constraints.starting_rate.expect("a starting rate");

        let mut delay_based_bwe = DelayBasedBwe::new(field_trials);
        delay_based_bwe.set_min_bitrate(CONGESTION_CONTROLLER_MIN_BITRATE);

        Self {
            use_min_allocatable_as_lower_bound: field_trials.min_alloc_as_lower_bound,
            limit_probes_lower_than_throughput_estimate: field_trials
                .limit_probes_lower_than_throughput_estimate,

            probe_controller: ProbeController::new(field_trials.probing_configuration.clone()),
            congestion_window_pushback_controller: rate_control_settings
                .use_congestion_window_pushback()
                .then(|| CongestionWindowPushbackController::new(field_trials)),
            rate_control_settings,
            bandwidth_estimation: SendSideBandwidthEstimation::new(field_trials),
            alr_detector: AlrDetector::new(field_trials.alr_detector_parameters.clone()),
            probe_bitrate_estimator: ProbeBitrateEstimator::default(),
            delay_based_bwe,
            acknowledged_bitrate_estimator: RobustThroughputEstimator::new(
                field_trials.robust_throughput_estimator_settings.clone(),
            ),

            min_target_rate: DataRate::zero(),
            min_data_rate: DataRate::zero(),
            max_data_rate: DataRate::plus_infinity(),
            starting_rate: config.constraints.starting_rate,
            first_packet_sent: false,
            first_transport_feedback_received: false,
            feedback_max_rtts: VecDeque::new(),
            last_loss_based_target_rate: starting_rate,
            last_pushback_target_rate: starting_rate,
            last_loss_base_state: LossBasedState::DelayBasedEstimate,
            last_estimated_round_trip_time: TimeDelta::plus_infinity(),
            pacing_factor: config.stream_based_config.pacing_factor,
            pacing_time_window: config.default_pacing_time_window,
            min_total_allocated_bitrate: config
                .stream_based_config
                .min_total_allocated_bitrate
                .unwrap_or(DataRate::zero()),
            previously_in_alr: false,
            last_is_bandwidth_limited: true,
            current_data_window: None,

            initial_config: Some(config),
        }
    }

    pub fn on_network_availability(&mut self, msg: NetworkAvailability) -> NetworkControlUpdate {
        NetworkControlUpdate {
            probe_cluster_configs: self.probe_controller.on_network_availability(msg),
            ..Default::default()
        }
    }

    pub fn on_streams_config(&mut self, msg: StreamsConfig) -> NetworkControlUpdate {
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

        let mut pacing_changed = false;
        if let Some(pacing_factor) = msg.pacing_factor
            && Some(pacing_factor) != self.pacing_factor
        {
            self.pacing_factor = Some(pacing_factor);
            pacing_changed = true;
        }
        if let Some(min_total_allocated_bitrate) = msg.min_total_allocated_bitrate
            && min_total_allocated_bitrate != self.min_total_allocated_bitrate
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
        if pacing_changed {
            update.pacer_config = Some(self.get_pacing_rates());
        }
        update
    }

    pub fn on_process_interval(&mut self, msg: ProcessInterval) -> NetworkControlUpdate {
        let mut update = NetworkControlUpdate::default();
        if let Some(initial_config) = self.initial_config.take() {
            update.probe_cluster_configs = self.reset_constraints(initial_config.constraints);
            update.pacer_config = Some(self.get_pacing_rates());

            if let Some(requests_alr_probing) =
                initial_config.stream_based_config.requests_alr_probing
            {
                self.probe_controller
                    .enable_periodic_alr_probing(requests_alr_probing);
            }
            self.probe_controller.enable_repeated_initial_probing(
                initial_config
                    .stream_based_config
                    .enable_repeated_initial_probing,
            );
            if let Some(total_bitrate) = initial_config
                .stream_based_config
                .max_total_allocated_bitrate
            {
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
        // Avoid ALR probing before the first transport feedback is received in
        // order to avoid ALR probing if receive side BWE is used.
        self.probe_controller
            .set_alr_start_time(if self.first_transport_feedback_received {
                self.alr_detector
                    .get_application_limited_region_start_time()
            } else {
                None
            });

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
                congestion_window_pushback_controller.set_data_window(current_data_window)
            }
            _ => update.congestion_window = self.current_data_window,
        };

        self.maybe_trigger_on_network_changed(&mut update, msg.at_time);
        update
    }

    pub fn on_round_trip_time_update(&mut self, msg: RoundTripTimeUpdate) {
        debug_assert!(!msg.round_trip_time.is_zero());
        self.delay_based_bwe.on_rtt_update(msg.round_trip_time);
        self.bandwidth_estimation.update_rtt(msg.round_trip_time);
    }

    pub fn on_sent_packet(&mut self, sent_packet: SentPacket) -> NetworkControlUpdate {
        self.alr_detector
            .on_bytes_sent(sent_packet.size, sent_packet.send_time);

        if !self.first_packet_sent {
            self.first_packet_sent = true;
            // Initialize feedback time to send time to allow estimation of RTT until
            // first feedback is received.
            self.bandwidth_estimation
                .update_propagation_rtt(sent_packet.send_time, TimeDelta::zero());
        }
        self.bandwidth_estimation.on_sent_packet(&sent_packet);

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

    pub fn on_transport_packets_feedback(
        &mut self,
        report: &TransportPacketsFeedback,
    ) -> NetworkControlUpdate {
        if report.packet_feedbacks.is_empty() {
            // TODO(bugs.webrtc.org/10125): Design a better mechanism to safe-guard
            // against building very large network queues.
            return NetworkControlUpdate::default();
        }
        // Send side BWE is used.
        self.first_transport_feedback_received = true;

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
            .unwrap_or(Timestamp::minus_infinity());

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

        let alr_start_time: Option<Timestamp> = self
            .alr_detector
            .get_application_limited_region_start_time();
        if self.previously_in_alr && alr_start_time.is_none() {
            self.probe_controller
                .set_alr_ended_time(report.feedback_time);
        }
        self.previously_in_alr = alr_start_time.is_some();
        let sorted = report.sorted_by_receive_time();
        self.acknowledged_bitrate_estimator
            .incoming_packet_feedback(&sorted);
        let acknowledged_bitrate = self.acknowledged_bitrate_estimator.bitrate();
        self.bandwidth_estimation
            .set_acknowledged_rate(acknowledged_bitrate);
        for feedback in &sorted {
            if feedback.sent_packet.pacing_info.probe_cluster_id != PacedPacketInfo::NOT_APROBE {
                self.probe_bitrate_estimator
                    .handle_probe_and_estimate_bitrate(feedback);
            }
        }

        let mut probe_bitrate: Option<DataRate> = self
            .probe_bitrate_estimator
            .fetch_and_reset_last_estimated_bitrate();

        if let (Some(probe), Some(acknowledged)) = (probe_bitrate, acknowledged_bitrate)
            && self.limit_probes_lower_than_throughput_estimate
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

        let mut update = NetworkControlUpdate::default();

        let result: DelayBasedBweResult = self.delay_based_bwe.incoming_packet_feedback_vector(
            report,
            &sorted,
            acknowledged_bitrate,
            probe_bitrate,
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
            report,
            result.delay_detector_state,
            probe_bitrate,
            alr_start_time.is_some(),
        );
        if result.updated {
            // Update the estimate in the ProbeController, in case we want to probe.
            self.maybe_trigger_on_network_changed(&mut update, report.feedback_time);
        }

        if result.recovered_from_overuse {
            self.probe_controller.set_alr_start_time(alr_start_time);
            let mut probes = self.probe_controller.request_probe(report.feedback_time);
            update.probe_cluster_configs.append(&mut probes);
        }

        // No valid RTT could be because send-side BWE isn't used, in which case
        // we don't try to limit the outstanding packets.
        if self.rate_control_settings.use_congestion_window() && max_feedback_rtt.is_finite() {
            self.update_congestion_window_size();
        }
        match (
            &mut self.congestion_window_pushback_controller,
            self.current_data_window,
        ) {
            (Some(congestion_window_pushback_controller), Some(current_data_window)) => {
                congestion_window_pushback_controller.set_data_window(current_data_window)
            }
            _ => update.congestion_window = self.current_data_window,
        };

        update
    }

    /// The data window the congestion window settings derive from the target rate and the RTT,
    /// whether or not the pushback controller is the one enforcing it.
    pub fn congestion_window(&self) -> Option<DataSize> {
        self.current_data_window
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
            tracing::warn!(
                max = ?self.max_data_rate,
                min = ?self.min_data_rate,
                "max bitrate smaller than min bitrate"
            );
            self.max_data_rate = self.min_data_rate;
        }
        if let Some(starting_rate) = self.starting_rate
            && starting_rate < self.min_data_rate
        {
            tracing::warn!(
                start = ?starting_rate,
                min = ?self.min_data_rate,
                "start bitrate smaller than min bitrate"
            );
            self.starting_rate = Some(self.min_data_rate);
        }
    }

    fn maybe_trigger_on_network_changed(
        &mut self,
        update: &mut NetworkControlUpdate,
        at_time: Timestamp,
    ) {
        let round_trip_time: TimeDelta = self.bandwidth_estimation.round_trip_time();
        let loss_based_target_rate: DataRate = self.bandwidth_estimation.target_rate();
        let loss_based_state: LossBasedState = self.bandwidth_estimation.loss_based_state();
        let mut pushback_target_rate: DataRate = loss_based_target_rate;

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
        }

        let is_bandwidth_limited = self
            .alr_detector
            .get_application_limited_region_start_time()
            .is_none();

        if (loss_based_target_rate != self.last_loss_based_target_rate)
            || (loss_based_state != self.last_loss_base_state)
            || (round_trip_time != self.last_estimated_round_trip_time)
            || (pushback_target_rate != self.last_pushback_target_rate)
            || (is_bandwidth_limited != self.last_is_bandwidth_limited)
        {
            self.last_loss_based_target_rate = loss_based_target_rate;
            self.last_pushback_target_rate = pushback_target_rate;
            self.last_estimated_round_trip_time = round_trip_time;
            self.last_loss_base_state = loss_based_state;
            self.last_is_bandwidth_limited = is_bandwidth_limited;

            self.alr_detector
                .set_estimated_bitrate(loss_based_target_rate);

            // With DropFrame, the target stays at the loss-based rate and the pushback reaches
            // the encoder as `cwnd_reduce_ratio`, a frame drop ratio, which no consumer reads.
            let target_rate = if self
                .rate_control_settings
                .use_congestion_window_drop_frame_only()
            {
                loss_based_target_rate
            } else {
                pushback_target_rate
            };
            update.target_rate = Some(TargetTransferRate { target_rate });

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
            update.pacer_config = Some(self.get_pacing_rates());
            tracing::trace!(
                at = at_time.ms(),
                pushback_target_bps = self.last_pushback_target_rate.bps(),
                estimate_bps = loss_based_target_rate.bps(),
                "bwe"
            );
        }
    }

    fn get_pacing_rates(&self) -> PacerConfig {
        // Pacing rate is based on target rate before congestion window pushback,
        // because we don't want to build queues in the pacer when pushback occurs.
        let pacing_rate = self
            .min_total_allocated_bitrate
            .max(self.last_loss_based_target_rate)
            * self
                .pacing_factor
                .unwrap_or(if self.first_transport_feedback_received {
                    Self::DEFAULT_PACE_MULTIPLIER_WITH_SEND_SIDE_BWE
                } else {
                    Self::DEFAULT_PACE_MULTIPLIER
                });
        PacerConfig::create(pacing_rate, self.pacing_time_window)
    }

    fn update_congestion_window_size(&mut self) {
        let min_feedback_max_rtt: TimeDelta =
            TimeDelta::from_millis(*self.feedback_max_rtts.iter().min().expect("a feedback RTT"));

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
        // Probes may not be sent in this state.
        LossBasedState::IncreaseUsingPadding => BandwidthLimitedCause::LossLimitedBwe,
        // Probes may be sent in this state.
        LossBasedState::Increasing => BandwidthLimitedCause::LossLimitedBweIncreasing,
        LossBasedState::DelayBasedEstimate => BandwidthLimitedCause::DelayBasedLimited,
    }
}

#[cfg(test)]
mod test {
    use crate::gcc::api::transport::PacketResult;

    use super::*;

    const INITIAL_BITRATE_KBPS: i64 = 60;
    const INITIAL_BITRATE: DataRate = DataRate::from_kilobits_per_sec(INITIAL_BITRATE_KBPS);
    const DEFAULT_PACING_RATE: f64 = 2.5;
    const DEFAULT_PACING_RATE_SEND_SIDE_BWE: f64 = 1.1;

    fn create_packet_result(
        arrival_time: Timestamp,
        send_time: Timestamp,
        payload_size: i64,
        pacing_info: PacedPacketInfo,
    ) -> PacketResult {
        PacketResult {
            sent_packet: SentPacket {
                send_time,
                size: DataSize::from_bytes(payload_size),
                pacing_info,
                ..Default::default()
            },
            receive_time: arrival_time,
            ambiguous_receive_time: false,
        }
    }

    // Simulate sending packets and receiving transport feedback during
    // `runtime_ms`, then return the final target birate.
    fn packet_transmission_and_feedback_block(
        controller: &mut GoogCcNetworkController,
        runtime_ms: i64,
        delay: i64,
        current_time: &mut Timestamp,
    ) -> Option<DataRate> {
        let mut target_bitrate: Option<DataRate> = None;
        let mut delay_buildup: i64 = 0;
        let start_time_ms: i64 = current_time.ms();
        while current_time.ms() - start_time_ms < runtime_ms {
            const PAYLOAD_SIZE: i64 = 1000;
            let packet: PacketResult = create_packet_result(
                *current_time + TimeDelta::from_millis(delay_buildup),
                *current_time,
                PAYLOAD_SIZE,
                PacedPacketInfo::default(),
            );
            delay_buildup += delay;
            let update = controller.on_sent_packet(packet.sent_packet);
            if let Some(target_rate) = update.target_rate {
                target_bitrate = Some(target_rate.target_rate);
            }
            let feedback = TransportPacketsFeedback {
                feedback_time: packet.receive_time,
                packet_feedbacks: vec![packet],
                ..Default::default()
            };
            let update = controller.on_transport_packets_feedback(&feedback);
            if let Some(target_rate) = update.target_rate {
                target_bitrate = Some(target_rate.target_rate);
            }
            *current_time += TimeDelta::from_millis(50);
            let update = controller.on_process_interval(ProcessInterval {
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
        const PAYLOAD_SIZE: i64 = 1000;
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

    fn create_controller(stream_based_config: StreamsConfig) -> GoogCcNetworkController {
        GoogCcNetworkController::new(NetworkControllerConfig {
            constraints: TargetRateConstraints {
                at_time: Timestamp::zero(),
                min_data_rate: Some(DataRate::from_kilobits_per_sec(0)),
                max_data_rate: Some(DataRate::from_kilobits_per_sec(5 * INITIAL_BITRATE_KBPS)),
                starting_rate: Some(DataRate::from_kilobits_per_sec(INITIAL_BITRATE_KBPS)),
            },
            stream_based_config,
            default_pacing_time_window: PacerConfig::DEFAULT_TIME_INTERVAL,
            ..Default::default()
        })
    }

    fn available(controller: &mut GoogCcNetworkController, at_time: Timestamp) {
        let _ = controller.on_network_availability(NetworkAvailability {
            at_time,
            network_available: true,
        });
    }

    #[test]
    fn initialize_target_rate_on_first_process_interval_after_network_available() {
        let mut controller = create_controller(StreamsConfig::default());
        available(&mut controller, Timestamp::from_millis(123456));
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
    fn change_default_pacing_factor_after_first_feedback() {
        let mut controller = create_controller(StreamsConfig::default());
        available(&mut controller, Timestamp::from_millis(123456));
        let update = controller.on_process_interval(ProcessInterval {
            at_time: Timestamp::from_millis(123456),
            ..Default::default()
        });
        assert_eq!(
            update.pacer_config.unwrap().data_rate(),
            INITIAL_BITRATE * DEFAULT_PACING_RATE
        );

        let feedback_time = Timestamp::from_millis(123457);
        let feedback = TransportPacketsFeedback {
            feedback_time,
            packet_feedbacks: vec![create_packet_result(
                feedback_time,
                feedback_time,
                100,
                PacedPacketInfo::default(),
            )],
            ..Default::default()
        };
        let update = controller.on_transport_packets_feedback(&feedback);
        let pacer_config = update.pacer_config.expect("a pacer config");
        assert!(pacer_config.data_rate() >= INITIAL_BITRATE * DEFAULT_PACING_RATE_SEND_SIDE_BWE);
        assert!(
            pacer_config.data_rate()
                < (INITIAL_BITRATE + DataRate::from_kilobits_per_sec(5))
                    * DEFAULT_PACING_RATE_SEND_SIDE_BWE
        );
    }

    #[test]
    fn respects_configured_pacing_factor() {
        let mut controller = create_controller(StreamsConfig {
            pacing_factor: Some(10.0),
            ..Default::default()
        });
        available(&mut controller, Timestamp::from_millis(123456));
        let update = controller.on_process_interval(ProcessInterval {
            at_time: Timestamp::from_millis(123456),
            ..Default::default()
        });
        assert_eq!(
            update.pacer_config.unwrap().data_rate(),
            INITIAL_BITRATE * 10.0
        );

        let feedback_time = Timestamp::from_millis(123457);
        let feedback = TransportPacketsFeedback {
            feedback_time,
            packet_feedbacks: vec![create_packet_result(
                feedback_time,
                feedback_time,
                100,
                PacedPacketInfo::default(),
            )],
            ..Default::default()
        };
        let update = controller.on_transport_packets_feedback(&feedback);
        let pacer_config = update.pacer_config.expect("a pacer config");
        assert!(pacer_config.data_rate() >= INITIAL_BITRATE * 10.0);
        assert!(
            pacer_config.data_rate()
                < (INITIAL_BITRATE + DataRate::from_kilobits_per_sec(5)) * 10.0
        );
    }

    #[test]
    fn updates_delay_based_estimate() {
        let mut controller = create_controller(StreamsConfig::default());
        const RUN_TIME_MS: i64 = 6000;
        let mut current_time: Timestamp = Timestamp::from_millis(123);
        available(&mut controller, current_time);

        // The test must run and insert packets/feedback long enough that the
        // BWE computes a valid estimate. This is first done in an environment which
        // simulates no bandwidth limitation, and therefore not built-up delay.
        let target_bitrate_before_delay = packet_transmission_and_feedback_block(
            &mut controller,
            RUN_TIME_MS,
            0,
            &mut current_time,
        )
        .expect("an estimate");

        // Repeat, but this time with a building delay, and make sure that the
        // estimation is adjusted downwards.
        let target_bitrate_after_delay = packet_transmission_and_feedback_block(
            &mut controller,
            RUN_TIME_MS,
            50,
            &mut current_time,
        )
        .expect("an estimate");
        assert!(target_bitrate_after_delay < target_bitrate_before_delay);
    }

    #[test]
    fn calculates_rtt_from_transport_feedback() {
        let mut controller = create_controller(StreamsConfig::default());
        let current_time: Timestamp = Timestamp::from_millis(123);
        let one_way_delay: TimeDelta = TimeDelta::from_millis(10);

        let feedback: TransportPacketsFeedback = create_transport_packets_feedback(
            /*per_packet_network_delay=*/ TimeDelta::from_millis(50),
            one_way_delay,
            /*send_time=*/ current_time,
        );
        controller.on_transport_packets_feedback(&feedback);
        // The propagation RTT discounts each packet's wait behind the newest one received.
        assert!(!controller.bandwidth_estimation.is_rtt_above_limit());
        assert_eq!(
            controller.feedback_max_rtts.back(),
            Some(&(2 * one_way_delay.ms() + 100))
        );
    }

    #[test]
    fn triggers_target_rate_update_when_bandwidth_limited_changes() {
        let mut controller = create_controller(StreamsConfig::default());
        let mut current_time = Timestamp::from_millis(123456);
        available(&mut controller, current_time);
        let update = controller.on_process_interval(ProcessInterval {
            at_time: current_time,
            ..Default::default()
        });
        assert!(update.target_rate.is_some());
        assert!(controller.last_is_bandwidth_limited);

        // Send packet to initialize ALR detector
        let mut sent_packet = SentPacket {
            send_time: current_time,
            size: DataSize::from_bytes(1000),
            ..Default::default()
        };
        let _ = controller.on_sent_packet(sent_packet);

        // Wait 10 seconds without sending any packets (this will make budget ratio
        // accumulate and enter ALR)
        current_time += TimeDelta::from_seconds(10);
        // Send a tiny packet to trigger AlrDetector state change evaluation
        sent_packet.send_time = current_time;
        sent_packet.size = DataSize::from_bytes(1);
        let update = controller.on_sent_packet(sent_packet);
        let alr_detected = update.target_rate.is_some()
            || controller
                .on_process_interval(ProcessInterval {
                    at_time: current_time,
                    ..Default::default()
                })
                .target_rate
                .is_some();
        assert!(alr_detected);
        assert!(!controller.last_is_bandwidth_limited);

        // Now send a large burst of packets to leave ALR.
        let mut bandwidth_limited_detected = false;
        for _ in 0..50 {
            current_time += TimeDelta::from_millis(1);
            sent_packet.send_time = current_time;
            sent_packet.size = DataSize::from_bytes(1000);
            let update = controller.on_sent_packet(sent_packet);
            bandwidth_limited_detected |= update.target_rate.is_some();
        }
        if !bandwidth_limited_detected {
            bandwidth_limited_detected = controller
                .on_process_interval(ProcessInterval {
                    at_time: current_time,
                    ..Default::default()
                })
                .target_rate
                .is_some();
        }
        assert!(bandwidth_limited_detected);
        assert!(controller.last_is_bandwidth_limited);
    }
}
