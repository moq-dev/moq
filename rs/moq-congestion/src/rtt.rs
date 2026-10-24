use std::time::Duration;

/// The transport's current RTT estimate supplied to a controller.
#[derive(Copy, Clone)]
pub struct RttEstimator(Duration);

impl RttEstimator {
    /// Wrap the transport's current best RTT estimate.
    pub fn new(rtt: Duration) -> Self {
        Self(rtt)
    }

    /// The transport's current best RTT estimate.
    pub fn get(&self) -> Duration {
        self.0
    }
}
