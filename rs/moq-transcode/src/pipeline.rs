//! The ladder a transcoder publishes for one source, and the rungs serving it.
//!
//! The ladder is sized against the source picture, so it has to follow it.
//! An on-demand `moq_video::encode::publish_capture` opens its source twice
//! (once to probe the mode, once when the first subscriber arrives), and a window
//! capture derives its geometry from the window on every open, so the picture a
//! ladder was resolved against is routinely not the one the source ends up
//! carrying. A reconnecting publisher and a renegotiated screen share do the
//! same thing later in the stream.
//!
//! So every source catalog snapshot re-resolves the ladder and diffs it: rungs
//! the new picture has no room for retire, rungs it makes room for are added,
//! and the rest carry on untouched. A rung whose picture changed retires and its
//! replacement is published under a fresh name, because a name always means the
//! same content (see [`catalog::Names`]).

use std::collections::{HashMap, VecDeque};

use hang::catalog::{Reference, Video, VideoConfig};

/// Picture identity for ladder follow: enabling, disabling, or re-ranking a rendition is not a
/// source change.
fn same_picture(a: &VideoConfig, b: &VideoConfig) -> bool {
	let mut a = a.clone();
	let mut b = b.clone();
	a.enabled = true;
	b.enabled = true;
	a.preference = 0;
	b.preference = 0;
	a == b
}

use crate::catalog::{self, Names, Published};
use crate::feed::Feed;
use crate::{Config, Error, active, rung};

/// How many source pictures' worth of probed rung entries a ladder remembers.
///
/// A source flapping between a few sizes (a window capture being dragged, a
/// publisher renegotiating back and forth) resolves the same rung shapes over
/// and over, and each probe opens a real encoder serially on the transcoder's
/// one loop. Bounded so a source cycling through endless sizes cannot grow it.
const PROBED_PICTURES: usize = 4;

/// Everything a transcoder publishes for one source rendition.
pub(crate) struct Pipeline {
	source: moq_net::broadcast::Consumer,
	config: Config,
	active: active::Producer,
	/// Which codecs this host decodes, so a catalog edit never probes again.
	decoders: catalog::Decoders,

	/// The catalog ID of the source rendition the rungs are sized against.
	name: String,
	rendition: VideoConfig,
	/// The shared live decode of that rendition: one subscription and one
	/// decoder for every rung serving off it.
	feed: Feed,

	/// The rungs published for it, each with its catalog entry.
	rungs: Vec<Published>,
	/// The track names handed out so far, so a re-resolved rung never reuses one.
	names: Names,
	/// The catalog entries probed so far, most recent last, so a rung shape seen
	/// before takes its entry without opening an encoder again. Only the probe is
	/// reused: the rung still takes a fresh name.
	probed: VecDeque<Published>,
	/// The retirement signal for each rung name being served. Keyed by name
	/// rather than reaped when a task exits, since the entry a name maps to is
	/// always the newest task for it and a signal sent to one that already ended
	/// goes nowhere.
	serving: HashMap<String, tokio::sync::watch::Sender<bool>>,
}

impl Pipeline {
	/// Resolve the ladder against the chosen source rendition, probing a catalog
	/// entry per rung.
	pub(crate) async fn new(
		source: moq_net::broadcast::Consumer,
		config: Config,
		active: active::Producer,
		decoders: catalog::Decoders,
		name: String,
		rendition: VideoConfig,
	) -> Result<Self, Error> {
		// One shared live decode for every rung of this source: N active rungs
		// share one subscription and one decoder instead of N.
		let feed = Feed::new(
			source.clone(),
			rendition.track(&name).to_string(),
			rendition.clone(),
			config.feed_decoder(),
		);

		let mut ladder = Self {
			source,
			config,
			active,
			decoders,
			name,
			rendition,
			feed,
			rungs: Vec::new(),
			names: Names::default(),
			probed: VecDeque::new(),
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
	/// it was. The probes that succeeded before it are remembered regardless,
	/// since they describe the encoder rather than the ladder.
	async fn resolve(
		&mut self,
		source_name: &str,
		source: &VideoConfig,
		carry: &[Published],
	) -> Result<Vec<Published>, Error> {
		let resolved = catalog::resolve_rungs(&self.config.ladder, source_name, source)?;

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
					rung.name = self.names.mint(rung.short_side);
					self.probe(&rung, source).await?
				}
			};
			published.push(Published { rung, entry });
		}
		catalog::inherit(&mut published, source);
		Ok(published)
	}

	/// The catalog entry for a fresh rung, probing an encoder only for a shape
	/// this ladder has not probed recently.
	async fn probe(&mut self, rung: &catalog::Resolved, source: &VideoConfig) -> Result<VideoConfig, Error> {
		let mut entry = match self.probed.iter().find(|probed| probed.rung.same_shape(rung)) {
			Some(probed) => probed.entry.clone(),
			None => {
				let entry = catalog::rung_entry(rung, source, &self.config.encoder).await?;
				let capacity = (self.config.ladder.rungs().len() * PROBED_PICTURES).max(1);
				while self.probed.len() >= capacity {
					self.probed.pop_front();
				}
				self.probed.push_back(Published {
					rung: rung.clone(),
					entry: entry.clone(),
				});
				entry
			}
		};
		// Belongs to the source rather than the probe, like on a carried rung.
		entry.optimize_for_latency = source.optimize_for_latency;
		Ok(entry)
	}

	/// The rungs currently published, to fill the derivative catalog with.
	pub(crate) fn rungs(&self) -> &[Published] {
		&self.rungs
	}

	/// The rung to serve a requested track with, or `None` if the ladder has no
	/// such rung right now.
	pub(crate) fn rung(&mut self, name: &str) -> Option<rung::Rung> {
		let published = self.rungs.iter().find(|published| published.rung.name == name)?;
		let (retired, retire) = rung::Retire::channel();
		self.serving.insert(published.rung.name.clone(), retired);

		Some(rung::Rung {
			source: self.rendition.track(&self.name).to_string(),
			feed: self.feed.clone(),
			broadcast: self.source.clone(),
			config: self.rendition.clone(),
			encoder: self.config.encoder.clone(),
			decoder: self.config.decoder.clone(),
			resize: self.config.resize,
			active: self.active.clone(),
			rendition: self.active.rendition(&published.rung),
			info: published.rung.clone(),
			retire,
		})
	}

	/// Resolve the ladder again against a new source catalog snapshot.
	pub(crate) async fn follow(&mut self, video: &Video) -> Result<(), Error> {
		let (name, rendition) = match catalog::follow_source(video, &self.name, &mut self.decoders).await {
			Ok(chosen) => chosen,
			// Nothing transcodable in this snapshot: keep serving the ladder we
			// have rather than tearing it down over an edit the source may undo.
			Err(err) => {
				tracing::debug!(%err, "no transcodable rendition in the catalog update");
				return Ok(());
			}
		};
		if name == self.name && same_picture(&rendition, &self.rendition) {
			catalog::inherit(&mut self.rungs, &rendition);
			self.rendition = rendition;
			return Ok(());
		}

		// A different track, or a different codec on the same one: every rung is
		// decoding the wrong thing, so all of them retire whatever they resolve to
		// and none of their names carries forward.
		let rebuilt = rendition.track(&name) != self.rendition.track(&self.name)
			|| !catalog::same_stream(&self.rendition, &rendition);
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
				self.source.clone(),
				rendition.track(&name).to_string(),
				rendition.clone(),
				self.config.feed_decoder(),
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
			self.active.retire(&published.rung.name);
		}

		tracing::info!(source = %self.name, rungs = rungs.len(), "source changed; ladder resolved again");
		self.active.declare(rungs.iter().map(|published| &published.rung));
		self.rungs = rungs;
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{Ladder, Rung};

	fn video(width: u32, height: u32) -> VideoConfig {
		let mut video = VideoConfig::new(hang::catalog::H264 {
			inline: true,
			profile: 0x42,
			constraints: 0,
			level: 30,
		});
		video.coded_width = Some(width);
		video.coded_height = Some(height);
		video.bitrate = Some(1_000_000);
		video.framerate = Some(30.0);
		video
	}

	/// A source flapping between two sizes resolves the same two rung shapes
	/// over and over. Each shape is probed once, and every name retired before
	/// it encoded leaves the meters, so neither grows with the flapping.
	#[tokio::test]
	async fn a_flapping_source_probes_each_shape_once() {
		let source = moq_net::broadcast::Info::default().produce();
		let config = Config {
			ladder: Ladder::new([Rung::new(120, moq_net::bandwidth::Rate::from_bps(100_000))]).unwrap(),
			encoder: moq_video::encode::Kind::Software,
			decoder: moq_video::decode::Kind::Software,
			source: None,
			..Default::default()
		};
		let active = active::Producer::default();
		let decoders = catalog::Decoders::new(config.feed_decoder());
		let mut pipeline = Pipeline::new(
			source.consume(),
			config,
			active.clone(),
			decoders,
			"video".into(),
			video(640, 360),
		)
		.await
		.unwrap();

		for (width, height) in [(480, 360), (640, 360), (480, 360), (640, 360)] {
			let mut snapshot = Video::default();
			snapshot.insert("video", video(width, height)).unwrap();
			pipeline.follow(&snapshot).await.unwrap();
		}

		// A fresh name per incarnation, but only one probe per picture.
		assert_eq!(pipeline.rungs()[0].rung.name, "video/120p.5");
		assert_eq!(pipeline.probed.len(), 2);

		// Only the rung still published is reported; the retired ones never encoded.
		let mut cursor = active.consume();
		assert_eq!(cursor.try_next().unwrap().rendition.name(), "video/120p.5");
		assert!(cursor.try_next().is_none());
	}
}
