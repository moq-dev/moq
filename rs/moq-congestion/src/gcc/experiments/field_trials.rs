use super::{AlrExperimentSettings, CongestionWindowConfig, VideoRateControlConfig};

use crate::{
    AlrDetectorConfig, BitrateEstimatorConfig, BweLossExperiment,
    BweSeparateAudioPacketsSettings, LossBasedBweV2Config, LossBasedControlConfig,
    ProbeControllerConfig, RobustThroughputEstimatorSettings, RttBasedBackoffConfig,
    SafeResetOnRouteChange, TrendlineEstimatorSettings,
    remote_bitrate_estimator::{BweBackOffFactor, EstimateBoundedIncrease},
};

/// Current field trials for WebRTC that impact GoogCC.
///
/// These have been painstakingly extracted from the WebRTC source code.
/// Normally these field trials are enabled via bespoke string parsing, but that is very difficult to port correctly.
/// I decided to make everything type safe which doubles as documentation; ur welcome.
#[derive(Clone, Debug)]
pub struct FieldTrials {
    /// WebRTC-Bwe-RobustThroughputEstimatorSettings
    pub robust_throughput_estimator_settings: RobustThroughputEstimatorSettings,

    // Probing settings.
    /// WebRTC-Bwe-ProbingConfiguration
    pub probing_configuration: ProbeControllerConfig,

    // Use probing to recover faster after large bitrate estimate drops.
    /// WebRTC-BweRapidRecoveryExperiment
    pub rapid_recovery_experiment: bool,

    /// WebRTC-AlrDetectorParameters
    pub alr_detector_parameters: AlrDetectorConfig,

    /// WebRTC-ProbingScreenshareBwe or WebRTC-StrictPacingAndProbing (legacy?)
    pub alr_experiment_settings: AlrExperimentSettings,

    /// WebRTC-AddPacingToCongestionWindowPushback
    pub add_pacing_to_congestion_window_pushback: bool,

    /// WebRTC-CongestionWindow
    pub congestion_window: CongestionWindowConfig,

    /// WebRTC-VideoRateControl
    pub video_rate_control: VideoRateControlConfig,

    /// WebRTC-UseBaseHeavyVP8TL3RateAllocation
    pub vp8_base_heavy_tl3_alloc: bool,

    /// WebRTC-Bwe-SeparateAudioPackets
    pub separate_audio_packets: BweSeparateAudioPacketsSettings,

    /// WebRTC-Bwe-LossBasedControl
    pub loss_based_control: LossBasedControlConfig,

    /// LossBasedBweV2Config
    pub loss_based_bwe_v2: LossBasedBweV2Config,

    /// WebRTC-Bwe-MaxRttLimit
    pub max_rtt_limit: RttBasedBackoffConfig,

    /// WebRTC-Bwe-TrendlineEstimatorSettings
    pub trendline_estimator_settings: TrendlineEstimatorSettings,

    /// WebRTC-BweBackOffFactor
    pub bwe_back_off_factor: BweBackOffFactor,

    /// WebRTC-DontIncreaseDelayBasedBweInAlr
    pub no_bitrate_increase_in_alr: bool,

    /// WebRTC-Bwe-EstimateBoundedIncrease
    pub estimate_bounded_increase: EstimateBoundedIncrease,

    /// WebRTC-Bwe-MinAllocAsLowerBound
    pub min_alloc_as_lower_bound: bool,

    /// WebRTC-Bwe-IgnoreProbesLowerThanNetworkStateEstimate
    pub ignore_probes_lower_than_network_state_estimate: bool,

    /// WebRTC-Bwe-LimitProbesLowerThanThroughputEstimate
    pub limit_probes_lower_than_throughput_estimate: bool,

    /// WebRTC-Bwe-PaceAtMaxOfBweAndLowerLinkCapacity
    pub pace_at_max_of_bwe_and_lower_link_capacity: bool,

    /// WebRTC-Bwe-LimitPacingFactorByUpperLinkCapacityEstimate
    pub limit_pacing_factor_by_upper_link_capacity_estimate: bool,

    /// WebRTC-Bwe-SafeResetOnRouteChange
    pub safe_reset_on_route_change: SafeResetOnRouteChange,

    /// WebRTC-BweThroughputWindowConfig
    pub bwe_throughput_window_config: BitrateEstimatorConfig,

    /// WebRTC-BweLossExperiment
    pub loss_experiment: BweLossExperiment,

    /// WebRTC-Bwe-ReceiverLimitCapsOnly
    pub receiver_limit_caps_only: bool,
}

impl Default for FieldTrials {
    fn default() -> Self {
        Self {
            robust_throughput_estimator_settings: RobustThroughputEstimatorSettings::default(),
            probing_configuration: ProbeControllerConfig::default(),
            rapid_recovery_experiment: false,
            alr_detector_parameters: AlrDetectorConfig::default(),
            alr_experiment_settings: AlrExperimentSettings::default(),
            add_pacing_to_congestion_window_pushback: false,
            congestion_window: CongestionWindowConfig::default(),
            video_rate_control: VideoRateControlConfig::default(),
            vp8_base_heavy_tl3_alloc: false,
            separate_audio_packets: BweSeparateAudioPacketsSettings::default(),
            loss_based_control: LossBasedControlConfig::default(),
            loss_based_bwe_v2: LossBasedBweV2Config::default(),
            max_rtt_limit: RttBasedBackoffConfig::default(),
            trendline_estimator_settings: TrendlineEstimatorSettings::default(),
            bwe_back_off_factor: BweBackOffFactor::default(),
            no_bitrate_increase_in_alr: false,
            estimate_bounded_increase: EstimateBoundedIncrease::default(),
            min_alloc_as_lower_bound: true,
            ignore_probes_lower_than_network_state_estimate: true,
            limit_probes_lower_than_throughput_estimate: true,
            pace_at_max_of_bwe_and_lower_link_capacity: false,
            limit_pacing_factor_by_upper_link_capacity_estimate: false,
            safe_reset_on_route_change: SafeResetOnRouteChange::default(),
            bwe_throughput_window_config: BitrateEstimatorConfig::default(),
            loss_experiment: BweLossExperiment::default(),
            receiver_limit_caps_only: false,
        }
    }
}
