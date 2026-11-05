//! Collapse a pending announce burst without changing its last-event order.

use std::collections::HashMap;

use crate::announce;

/// Keep each prefix's last active event in its original event order.
pub(super) fn initial(events: impl IntoIterator<Item = announce::Event>) -> Vec<announce::Announce> {
	let mut pending: Vec<Option<announce::Announce>> = Vec::new();
	let mut positions = HashMap::new();
	for event in events {
		let (update, active) = match event {
			announce::Event::Start(update)
			| announce::Event::Update(update)
			| announce::Event::Restart(update)
			| announce::Event::Takeover { announce: update, .. } => (update, true),
			announce::Event::End(update) => (update, false),
		};
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
