//! The ladder a transcoder publishes for one source, and the rungs serving it.
//!
//! The ladder is sized against the source picture, so it has to follow it.
//! `moq_video::encode::publish_capture` opens its source twice by design (once
//! to probe the mode, once when the first subscriber arrives), and a window
//! capture derives its geometry from the window on every open, so the picture a
//! ladder was resolved against is routinely not the one the source ends up
//! carrying. A reconnecting publisher and a renegotiated screen share do the
//! same thing later in the stream.
//!
//! So every source catalog snapshot re-resolves the ladder and diffs it: rungs
//! the new picture has no room for retire, rungs it makes room for are added,
//! and the rest carry on untouched.

use std::collections::HashMap;

use hang::catalog::{Video, VideoConfig};

use crate::catalog::{self, Published};
use crate::feed::Feed;
use crate::{Config, Error, active, rung};

/// Everything a transcoder publishes for one source rendition.
pub(crate) struct Ladder {
	source: moq_net::broadcast::Consumer,
	config: Config,
	active: active::Producer,

	/// The source rendition the rungs are sized against.
	name: String,
	rendition: VideoConfig,
	/// The shared live decode of that rendition: one subscription and one
	/// decoder for every rung serving off it.
	feed: Feed,

	/// The rungs published for it, each with its catalog entry.
	rungs: Vec<Published>,
	/// The retirement signal for each rung name being served. Keyed by name
	/// rather than reaped when a task exits, since the entry a name maps to is
	/// always the newest task for it and a signal sent to one that already ended
	/// goes nowhere.
	serving: HashMap<String, tokio::sync::watch::Sender<bool>>,
}

impl Ladder {
	/// Resolve the ladder against the chosen source rendition, probing a catalog
	/// entry per rung.
	pub(crate) async fn new(
		source: moq_net::broadcast::Consumer,
		config: Config,
		active: active::Producer,
		name: String,
		rendition: VideoConfig,
	) -> Result<Self, Error> {
		let rungs = catalog::publish_rungs(&config.rungs, &name, &rendition, &config.encoder, &[]).await?;
		tracing::info!(source = %name, rungs = rungs.len(), "transcoding");
		// Publish the ladder before any rung can be asked for, so a cursor holds
		// every handle and can bill a pipeline too short to show up as an edge.
		active.declare(rungs.iter().map(|published| &published.rung));

		// One shared live decode for every rung of this source: N active rungs
		// share one subscription and one decoder instead of N.
		let feed = Feed::new(source.track(&name)?, rendition.clone(), config.decoder.clone());

		Ok(Self {
			source,
			config,
			active,
			name,
			rendition,
			feed,
			rungs,
			serving: HashMap::new(),
		})
	}

	/// The rungs currently published, to fill the derivative catalog with.
	pub(crate) fn rungs(&self) -> &[Published] {
		&self.rungs
	}

	/// The rung to serve a requested track with, or `None` if the ladder has no
	/// such rung right now.
	pub(crate) fn rung(&mut self, name: &str) -> Result<Option<rung::Rung>, Error> {
		let Some(published) = self.rungs.iter().find(|published| published.rung.name == name) else {
			return Ok(None);
		};
		let (retired, retire) = rung::Retire::channel();
		self.serving.insert(published.rung.name.clone(), retired);

		Ok(Some(rung::Rung {
			source: self.source.track(&self.name)?,
			feed: self.feed.clone(),
			broadcast: self.source.clone(),
			config: self.rendition.clone(),
			encoder: self.config.encoder.clone(),
			decoder: self.config.decoder.clone(),
			resize: self.config.resize,
			active: self.active.clone(),
			info: published.rung.clone(),
			retire,
		}))
	}

	/// Resolve the ladder again against a new source catalog snapshot.
	pub(crate) async fn follow(&mut self, video: &Video) -> Result<(), Error> {
		let (name, rendition) = match catalog::follow_source(video, &self.name) {
			Ok(chosen) => chosen,
			// Nothing transcodable in this snapshot: keep serving the ladder we
			// have rather than tearing it down over an edit the source may undo.
			Err(err) => {
				tracing::debug!(%err, "no transcodable rendition in the catalog update");
				return Ok(());
			}
		};
		if name == self.name && rendition == self.rendition {
			return Ok(());
		}

		// Resolved before anything is committed, so a failure leaves the ladder
		// exactly as it was and the next snapshot tries again. Probing opens a
		// real encoder, and a picture this machine cannot encode at is a reason to
		// keep serving the ladder that works, not to end the broadcast.
		let rungs = match catalog::publish_rungs(
			&self.config.rungs,
			&name,
			&rendition,
			&self.config.encoder,
			&self.rungs,
		)
		.await
		{
			Ok(rungs) => rungs,
			Err(err) => {
				tracing::warn!(%err, source = %name, "could not resolve a ladder for the new source");
				return Ok(());
			}
		};

		if name != self.name || !catalog::same_stream(&self.rendition, &rendition) {
			// A different track, or a different codec on the same one: every rung
			// is decoding the wrong thing, so retire them all and rebuild the
			// shared decode against the new source.
			for retired in self.serving.values() {
				let _ = retired.send(true);
			}
			self.serving.clear();
			self.feed = Feed::new(
				self.source.track(&name)?,
				rendition.clone(),
				self.config.decoder.clone(),
			);
		}
		self.name = name;
		self.rendition = rendition;

		// Retire whatever the new picture no longer fits, a rung of the same name
		// at a size it no longer is included. Its track ends, so a subscriber
		// reselects the way it would on any other rendition change.
		for published in &self.rungs {
			if rungs.iter().any(|other| other.rung == published.rung) {
				continue;
			}
			if let Some(retired) = self.serving.remove(&published.rung.name) {
				let _ = retired.send(true);
			}
		}

		tracing::info!(source = %self.name, rungs = rungs.len(), "source changed; ladder resolved again");
		self.active.declare(rungs.iter().map(|published| &published.rung));
		self.rungs = rungs;
		Ok(())
	}
}
