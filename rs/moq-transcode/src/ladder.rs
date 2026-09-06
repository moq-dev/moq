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
//! and the rest carry on untouched. A rung whose picture changed retires and its
//! replacement is published under a fresh name, because a retired track ends for
//! good (see [`catalog::Names`]).

use std::collections::HashMap;

use hang::catalog::{Video, VideoConfig};

use crate::catalog::{self, Names, Published};
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
	/// The track names handed out so far, so a re-resolved rung never reuses one.
	names: Names,
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
		// One shared live decode for every rung of this source: N active rungs
		// share one subscription and one decoder instead of N.
		let feed = Feed::new(source.track(&name)?, rendition.clone(), config.decoder.clone());

		let mut ladder = Self {
			source,
			config,
			active,
			name,
			rendition,
			feed,
			rungs: Vec::new(),
			names: Names::default(),
			serving: HashMap::new(),
		};

		// `resolve` takes the source it resolves against, since `follow` calls it
		// with one the ladder has not adopted yet. Here it is the ladder's own.
		let (name, rendition) = (ladder.name.clone(), ladder.rendition.clone());
		let rungs = ladder.resolve(&name, &rendition, &[]).await?;
		tracing::info!(source = %ladder.name, rungs = rungs.len(), "transcoding");
		// Publish the ladder before any rung can be asked for, so a cursor holds
		// every handle and can bill a pipeline too short to show up as an edge.
		ladder.active.declare(rungs.iter().map(|published| &published.rung));
		ladder.rungs = rungs;
		Ok(ladder)
	}

	/// Resolve the configured rungs against a source rendition.
	///
	/// `carry` is the published ladder a rung may carry forward from: one that
	/// comes out identical to a rung in it keeps that rung's name and catalog
	/// entry, so it serves on untouched and pays for no second probe. Anything
	/// else is a fresh incarnation and takes a name the ladder has never handed
	/// out. Pass an empty slice when every rung is retiring regardless of shape,
	/// since a name one of them was serving can never come back.
	///
	/// Nothing is committed here: a probe that fails leaves the ladder exactly as
	/// it was.
	async fn resolve(
		&mut self,
		source_name: &str,
		source: &VideoConfig,
		carry: &[Published],
	) -> Result<Vec<Published>, Error> {
		let resolved = catalog::resolve_rungs(&self.config.rungs, source_name, source)?;

		let mut published = Vec::new();
		for mut rung in resolved {
			let reused = carry.iter().find(|other| other.rung.same_shape(&rung)).cloned();
			let entry = match reused {
				Some(reused) => {
					rung.name = reused.rung.name;
					let mut entry = reused.entry;
					// Belongs to the source rather than the ladder, so it tracks the
					// source even on a rung that skipped the probe.
					entry.optimize_for_latency = source.optimize_for_latency;
					entry
				}
				None => {
					rung.name = self.names.mint(rung.height);
					catalog::rung_entry(&rung, source, &self.config.encoder).await?
				}
			};
			published.push(Published { rung, entry });
		}
		Ok(published)
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

		// A different track, or a different codec on the same one: every rung is
		// decoding the wrong thing, so all of them retire whatever they resolve to
		// and none of their names carries forward.
		let rebuilt = name != self.name || !catalog::same_stream(&self.rendition, &rendition);
		let carry = match rebuilt {
			true => Vec::new(),
			false => self.rungs.clone(),
		};

		// Resolve against the new source before committing to it, so a failure
		// leaves the ladder exactly as it was and the next snapshot tries again.
		// Probing opens a real encoder, and a picture this machine cannot encode at
		// is a reason to keep serving the ladder that works, not to end the
		// broadcast.
		let rungs = match self.resolve(&name, &rendition, &carry).await {
			Ok(rungs) => rungs,
			Err(err) => {
				tracing::warn!(%err, source = %name, "could not resolve a ladder for the new source");
				return Ok(());
			}
		};

		if rebuilt {
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

		// Retire whatever the new ladder does not carry forward: a height it has no
		// room for, and a height it kept at a picture it is no longer serving,
		// whose replacement is a fresh name. Either way the track ends, so a
		// subscriber reselects the way it would on any other rendition change.
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
