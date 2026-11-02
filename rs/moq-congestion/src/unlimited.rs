use std::sync::Arc;

use super::{Ack, Ce, Controller, Factory, Init, Loss, Packet, Sent};
use crate::Instant;

/// No congestion control: a window that never shrinks, and no pacing.
///
/// Meant for tests and controlled links, where nothing else shares the path.
#[derive(Debug, Default, Clone, Copy)]
pub struct Unlimited;

impl Controller for Unlimited {
    fn on_sent(&mut self, _sent: Sent) -> Packet {
        Packet::default()
    }

    fn on_ack(&mut self, _ack: Ack<'_>) {}

    fn on_loss(&mut self, _loss: Loss<'_>) {}

    fn on_ce(&mut self, _ce: Ce) {}

    fn on_mtu_update(&mut self, _new_mtu: u16) {}

    fn window(&self) -> u64 {
        u64::MAX
    }

    fn pace(&mut self, _now: Instant, _bytes: u64) -> Option<Instant> {
        None
    }

    fn clone_box(&self) -> Box<dyn Controller> {
        Box::new(*self)
    }
}

impl Factory for Unlimited {
    fn build(self: Arc<Self>, _init: Init) -> Box<dyn Controller> {
        Box::new(*self)
    }
}
