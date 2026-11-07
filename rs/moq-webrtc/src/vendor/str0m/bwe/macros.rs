macro_rules! log_delay_variation {
    ($($arg:expr),+) => {
        crate::log_stat!("DELAY_VARIATION", $($arg),+);
    }
}

macro_rules! log_trendline_estimate {
    ($($arg:expr),+) => {
        crate::log_stat!("TRENDLINE_ESTIMATE", $($arg),+);
    }
}

macro_rules! log_trendline_modified_trend {
    ($($arg:expr),+) => {
        crate::log_stat!("TRENDLINE_MODIFIED_TREND", $($arg),+);
    }
}

macro_rules! log_bitrate_estimate {
    ($($arg:expr),+) => {
        crate::log_stat!("BITRATE_ESTIMATE", $($arg),+);
    }
}

macro_rules! log_loss_based_bitrate_estimate {
    ($($arg:expr),+) => {
        crate::log_stat!("LOSS_BITRATE_ESTIMATE", $($arg),+);
    }
}

macro_rules! log_rate_control_state {
    ($($arg:expr),+) => {
        crate::log_stat!("RATE_CONTROL_STATE", $($arg),+);
    }
}

macro_rules! log_rate_control_observed_bitrate {
    ($($arg:expr),+) => {
        crate::log_stat!("RATE_CONTROL_OBSERVED_BITRATE", $($arg),+);
    }
}

macro_rules! log_rate_control_applied_change {
    ($($arg:expr),+) => {
        crate::log_stat!("RATE_CONTROL_APPLIED_CHANGE", $($arg),+);
    }
}

macro_rules! log_pacer_media_debt {
    ($($arg:expr),+) => {
        crate::log_stat!("PACER_DEBT", $($arg),+, "media");
    }
}

macro_rules! log_pacer_padding_debt {
    ($($arg:expr),+) => {
        crate::log_stat!("PACER_DEBT", $($arg),+, "padding");
    }
}

macro_rules! log_inherent_loss {
    ($($arg:expr),+) => {
        crate::log_stat!("INHERENT_LOSS", $($arg),+);
    }
}

macro_rules! log_loss {
    ($($arg:expr),+) => {
        crate::log_stat!("LOSS", $($arg),+);
    }
}

macro_rules! log_loss_bw_limit_in_window {
    ($($arg:expr),+) => {
        crate::log_stat!("LOSS_BW_LIMIT_IN_WINDOW", $($arg),+);
    }
}

macro_rules! log_probe_bitrate_estimate {
    ($($arg:expr),+) => {
        crate::log_stat!("PROBE_BITRATE_ESTIMATE", $($arg),+);
    }
}

pub(crate) use log_bitrate_estimate;
pub(crate) use log_delay_variation;
pub(crate) use log_inherent_loss;
pub(crate) use log_loss;
pub(crate) use log_loss_based_bitrate_estimate;
pub(crate) use log_loss_bw_limit_in_window;
pub(crate) use log_pacer_media_debt;
pub(crate) use log_pacer_padding_debt;
pub(crate) use log_probe_bitrate_estimate;
pub(crate) use log_rate_control_applied_change;
pub(crate) use log_rate_control_observed_bitrate;
pub(crate) use log_rate_control_state;
pub(crate) use log_trendline_estimate;
pub(crate) use log_trendline_modified_trend;
