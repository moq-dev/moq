use super::CongestionWindowConfig;

use crate::gcc::{
    AlrDetectorConfig, LossBasedBweV2Config, ProbeControllerConfig,
    RobustThroughputEstimatorSettings, RttBasedBackoffConfig, TrendlineEstimatorSettings,
    remote_bitrate_estimator::BweBackOffFactor,
};

/// The WebRTC field trials that still reach a code path here, as typed settings.
///
/// Upstream parses each from a string keyed by the trial's name, given in each field's doc. The
/// defaults are upstream's when no trial string is set.
#[derive(Clone, Debug)]
pub struct FieldTrials {
    /// WebRTC-Bwe-RobustThroughputEstimatorSettings
    pub robust_throughput_estimator_settings: RobustThroughputEstimatorSettings,

    /// WebRTC-Bwe-ProbingConfiguration
    pub probing_configuration: ProbeControllerConfig,

    /// WebRTC-AlrDetectorParameters
    pub alr_detector_parameters: AlrDetectorConfig,

    /// WebRTC-AddPacingToCongestionWindowPushback
    pub add_pacing_to_congestion_window_pushback: bool,

    /// WebRTC-CongestionWindow
    pub congestion_window: CongestionWindowConfig,

    /// WebRTC-Bwe-LossBasedBweV2
    pub loss_based_bwe_v2: LossBasedBweV2Config,

    /// WebRTC-Bwe-MaxRttLimit
    pub max_rtt_limit: RttBasedBackoffConfig,

    /// WebRTC-Bwe-TrendlineEstimatorSettings
    pub trendline_estimator_settings: TrendlineEstimatorSettings,

    /// WebRTC-BweBackOffFactor
    pub bwe_back_off_factor: BweBackOffFactor,

    /// WebRTC-DontIncreaseDelayBasedBweInAlr
    pub no_bitrate_increase_in_alr: bool,

    /// WebRTC-Bwe-MinAllocAsLowerBound
    pub min_alloc_as_lower_bound: bool,

    /// WebRTC-Bwe-LimitProbesLowerThanThroughputEstimate
    pub limit_probes_lower_than_throughput_estimate: bool,
}

impl Default for FieldTrials {
    fn default() -> Self {
        Self {
            robust_throughput_estimator_settings: RobustThroughputEstimatorSettings::default(),
            probing_configuration: ProbeControllerConfig::default(),
            alr_detector_parameters: AlrDetectorConfig::default(),
            add_pacing_to_congestion_window_pushback: false,
            congestion_window: CongestionWindowConfig::default(),
            loss_based_bwe_v2: LossBasedBweV2Config::default(),
            max_rtt_limit: RttBasedBackoffConfig::default(),
            trendline_estimator_settings: TrendlineEstimatorSettings::default(),
            bwe_back_off_factor: BweBackOffFactor::default(),
            no_bitrate_increase_in_alr: false,
            min_alloc_as_lower_bound: true,
            limit_probes_lower_than_throughput_estimate: true,
        }
    }
}
