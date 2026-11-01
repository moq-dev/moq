//! libwebrtc's Google Congestion Control, vendored from kixelated/goog_cc.

/// `approx::assert_relative_eq!`: within `epsilon`, or within a relative `f64::EPSILON`.
#[cfg(test)]
macro_rules! assert_relative_eq {
    ($a:expr, $b:expr $(,)?) => {
        assert_relative_eq!($a, $b, epsilon = f64::EPSILON)
    };
    ($a:expr, $b:expr, epsilon = $e:expr $(,)?) => {{
        let (a, b, e): (f64, f64, f64) = ($a.into(), $b.into(), $e.into());
        assert!(
            (a - b).abs() <= e || (a - b).abs() <= a.abs().max(b.abs()) * f64::EPSILON,
            "{a} is not within {e} of {b}"
        );
    }};
}

mod api;

/// [FieldTrials](experiments::FieldTrials) and dynamic configuration.
pub mod experiments;

// We inline goog_cc modules to avoid goog_cc::goog_cc logging prefix.
mod pacing;

mod remote_bitrate_estimator;
mod acknowledged_bitrate_estimator;
mod acknowledged_bitrate_estimator_interface;
mod alr_detector;
mod bitrate_estimator;
mod congestion_window_pushback_controller;
mod delay_based_bwe;
mod delay_increase_detector_interface;
mod goog_cc_network_control;
mod inter_arrival_delta;
mod link_capacity_estimator;
mod loss_based_bandwidth_estimation;
mod loss_based_bwe_v2;
mod probe_bitrate_estimator;
mod probe_controller;
mod robust_throughput_estimator;
mod send_side_bandwidth_estimation;
mod trendline_estimator;

use acknowledged_bitrate_estimator::*;
use acknowledged_bitrate_estimator_interface::*;
use alr_detector::*;
use bitrate_estimator::*;
use congestion_window_pushback_controller::*;
use delay_based_bwe::*;
use delay_increase_detector_interface::*;
use inter_arrival_delta::*;
use link_capacity_estimator::*;
use loss_based_bandwidth_estimation::*;
use loss_based_bwe_v2::*;
use probe_bitrate_estimator::*;
use probe_controller::*;
use robust_throughput_estimator::*;
use send_side_bandwidth_estimation::*;
use trendline_estimator::*;

// Selectively export the public API.
pub use api::{network_control, transport, units};
pub use goog_cc_network_control::{GoogCcConfig, GoogCcNetworkController};
pub use {
    alr_detector::AlrDetectorConfig, bitrate_estimator::BitrateEstimatorConfig,
    send_side_bandwidth_estimation::BweLossExperiment,
    delay_based_bwe::BweSeparateAudioPacketsSettings,
    loss_based_bwe_v2::LossBasedBweV2Config,
    loss_based_bandwidth_estimation::LossBasedControlConfig,
    probe_controller::ProbeControllerConfig,
    robust_throughput_estimator::RobustThroughputEstimatorSettings,
    send_side_bandwidth_estimation::RttBasedBackoffConfig, goog_cc_network_control::SafeResetOnRouteChange, trendline_estimator::TrendlineEstimatorSettings,
};
