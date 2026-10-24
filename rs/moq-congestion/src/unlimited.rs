use std::any::Any;
use std::sync::Arc;

use super::{Controller, ControllerFactory};
use crate::Instant;
use crate::SpaceId;

/// No congestion control: a window that never shrinks, too large to pace.
///
/// Meant for tests and controlled links, where nothing else shares the path.
#[derive(Debug, Default, Clone, Copy)]
pub struct Unlimited;

impl Controller for Unlimited {
    fn on_congestion_event(
        &mut self,
        _now: Instant,
        _sent: Instant,
        _is_persistent_congestion: bool,
        _is_ecn: bool,
        _lost_bytes: u64,
        _largest_lost: u64,
        _space: SpaceId,
    ) {
    }

    fn on_mtu_update(&mut self, _new_mtu: u16) {}

    // The pacer turns off for any window past `u32::MAX`.
    fn window(&self) -> u64 {
        u64::MAX
    }

    fn clone_box(&self) -> Box<dyn Controller> {
        Box::new(*self)
    }

    fn initial_window(&self) -> u64 {
        u64::MAX
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

impl ControllerFactory for Unlimited {
    fn build(self: Arc<Self>, _now: Instant, _current_mtu: u16) -> Box<dyn Controller> {
        Box::new(*self)
    }
}
