//! Collapse a pending announce burst without changing its last-event order.

use std::collections::HashMap;

use crate::{announce, origin};

/// Keep each prefix's last active event in its original event order.
pub(super) fn initial(events: impl IntoIterator<Item = origin::RouteEvent>) -> Vec<announce::Announce> {
	let mut pending: Vec<Option<announce::Announce>> = Vec::new();
	let mut positions = HashMap::new();
	for event in events {
		let origin::RouteEvent {
			kind, announce: update, ..
		} = event;
		// A kind this match predates counts as active: the route is still advertised.
		let active = !matches!(kind, origin::RouteKind::End | origin::RouteKind::Offline);
		if let Some(index) = positions.remove(&update.prefix) {
			pending[index] = None;
		}
		if active {
			positions.insert(update.prefix.clone(), pending.len());
			pending.push(Some(update));
		}
	}
	// Tombstones preserve last-event order without shifting the remaining replay.
	pending.into_iter().flatten().collect()
}
