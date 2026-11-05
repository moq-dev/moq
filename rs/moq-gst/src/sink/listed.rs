//! The track names a run's published catalog lists, so a pad reports demand only once a
//! catalog-driven viewer can find its track.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

use super::session::{CAT, RUNTIME};

/// The renditions in the latest catalog this run put on the wire.
#[derive(Default)]
pub(super) struct Listed(Mutex<HashSet<String>>);

impl Listed {
	/// Whether the latest published catalog lists `track`.
	pub(super) fn contains(&self, track: &str) -> bool {
		self.0.lock().unwrap().contains(track)
	}

	fn replace(&self, catalog: &moq_mux::catalog::hang::Catalog) {
		let names = catalog
			.video
			.renditions
			.keys()
			.chain(catalog.audio.renditions.keys())
			.chain(catalog.text.renditions.keys())
			.cloned()
			.collect();
		*self.0.lock().unwrap() = names;
	}
}

/// Follows the run's own catalog track, aborted on drop.
///
/// The catalog is the authority: the first one waits for every reserved pad and the wall clock, and
/// any pad's streaming thread (or a sibling's EOS) can be what publishes it. Reading what went out,
/// rather than predicting when it will, keeps the gate exact for later renditions and replacements too.
pub(super) struct Watch {
	listed: Arc<Listed>,
	task: tokio::task::AbortHandle,
}

impl Watch {
	/// Start following `catalog`, calling `published` after each catalog it reads has been recorded.
	pub(super) fn start(
		catalog: &moq_mux::catalog::Producer,
		published: impl Fn(&Arc<Listed>) + Send + 'static,
	) -> Result<Self> {
		let mut consumer = catalog.consume().context("follow the published catalog")?;
		let listed = Arc::new(Listed::default());
		let task = {
			let listed = listed.clone();
			RUNTIME.spawn(async move {
				loop {
					match consumer.next().await {
						Ok(Some(catalog)) => {
							listed.replace(&catalog);
							published(&listed);
						}
						// The run finished its catalog: nothing further gets listed.
						Ok(None) => return,
						// An aborted catalog ends the run anyway; a pad left unlisted keeps null demand.
						Err(err) => {
							gst::debug!(CAT, "stopped following the catalog: {err:#}");
							return;
						}
					}
				}
			})
		};
		Ok(Self {
			listed,
			task: task.abort_handle(),
		})
	}

	pub(super) fn listed(&self) -> &Arc<Listed> {
		&self.listed
	}
}

impl Drop for Watch {
	fn drop(&mut self) {
		self.task.abort();
	}
}
