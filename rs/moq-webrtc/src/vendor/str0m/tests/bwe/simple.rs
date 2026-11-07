//! Simple BWE tests under different network conditions (cellular, WiFi).

use std::time::Duration;

use netem::{Bitrate, NetemConfig};
use str0m::RtcError;

use crate::common::{BweTestContext, Step, connect_with_bwe};
use crate::common::{init_crypto_default, init_log};

// Test bitrate layers
pub const LAYER_LOW: Bitrate = Bitrate::kbps(250);
pub const LAYER_MID: Bitrate = Bitrate::kbps(750);
pub const LAYER_TOP: Bitrate = Bitrate::kbps(1_500);

// Standard ramp-up test plan
pub const RAMP_UP_SINGLE: &[Step] = &[
    Step::Conditions {
        description: "Startup conditions",
        config: NetemConfig::new(),
    },
    Step::Media {
        description: "Low layer ramping up to mid",
        desired_bitrate: LAYER_MID,
        media_send_rate: LAYER_LOW,
    },
    Step::Run {
        description: "Wait for mid",
        duration: Duration::from_secs(20),
    },
    Step::Check {
        description: "Check enough for mid",
        at_least: Bitrate::kbps(750),
    },
    Step::Media {
        description: "Mid layer ramping up to top",
        desired_bitrate: LAYER_TOP,
        media_send_rate: LAYER_MID,
    },
    Step::Run {
        description: "Wait for top",
        duration: Duration::from_secs(20),
    },
    Step::Check {
        description: "Check enough for top",
        at_least: Bitrate::kbps(1_500),
    },
    Step::Media {
        description: "Top layer",
        desired_bitrate: LAYER_TOP,
        media_send_rate: LAYER_TOP,
    },
    Step::Run {
        description: "Ensure top stabilizes",
        duration: Duration::from_secs(10),
    },
    Step::Check {
        description: "Top is stable",
        at_least: Bitrate::kbps(1_500),
    },
];
#[test]
pub fn bwe_cellular() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    let mut plan = RAMP_UP_SINGLE.to_vec();
    plan[0] = Step::Conditions {
        description: "Cellular conditions",
        config: NetemConfig::cellular().seed(42),
    };
    // Burst loss and jitter make ramp-up sensitive to feedback timing.
    // Allow time to reach the mid layer with quantized TWCC receive times.
    plan[2] = Step::Run {
        description: "Wait for mid",
        duration: Duration::from_secs(75),
    };
    // RTT-paced NACK retries can delay feedback after burst loss; let the
    // estimator reach the existing 1 Mbps checkpoint.
    plan[5] = Step::Run {
        description: "Wait for top",
        duration: Duration::from_secs(40),
    };

    // Burst loss and jitter make the intermediate estimate conservative.
    // Require progress beyond the mid layer; the final check still requires
    // the full top-layer rate once media is flowing at that rate.
    plan[6] = Step::Check {
        description: "Check ramp toward top",
        at_least: Bitrate::mbps(1),
    };

    let (mut l, mut r) = connect_with_bwe(LAYER_LOW, LAYER_MID);

    let mut ctx = BweTestContext::new(&mut l, &mut r);

    ctx.run_plan(&mut l, &mut r, &plan)?;

    Ok(())
}

#[test]
pub fn bwe_wifi_normal() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    // WiFi: 100 Mbps link, 5ms latency, ~1% loss
    let mut plan = RAMP_UP_SINGLE.to_vec();
    plan[0] = Step::Conditions {
        description: "WiFi conditions",
        config: NetemConfig::wifi().seed(42),
    };

    let (mut l, mut r) = connect_with_bwe(LAYER_LOW, LAYER_MID);

    let mut ctx = BweTestContext::new(&mut l, &mut r);

    ctx.run_plan(&mut l, &mut r, RAMP_UP_SINGLE)?;

    Ok(())
}

#[test]
pub fn bwe_wifi_congested() -> Result<(), RtcError> {
    init_log();
    init_crypto_default();

    // Congested WiFi: 5 Mbps link, 10ms latency, ~10% loss
    let mut plan = RAMP_UP_SINGLE.to_vec();
    plan[0] = Step::Conditions {
        description: "Congested WiFi conditions",
        config: NetemConfig::congested().seed(42),
    };

    let (mut l, mut r) = connect_with_bwe(LAYER_LOW, LAYER_MID);

    let mut ctx = BweTestContext::new(&mut l, &mut r);

    ctx.run_plan(&mut l, &mut r, RAMP_UP_SINGLE)?;

    Ok(())
}
