use super::hang::CatalogExt;
use super::{Estimator, Rendition, RenditionConfig};

/// The config a data producer ([`Producer::json_stream`](super::Producer::json_stream) and the
/// like) publishes as its catalog entry.
///
/// Implemented for the [`json::Config`](crate::json::Config) builder, which lists the track in the
/// `json` section. Also implemented for any [`RenditionConfig`] that embeds the data config `D`
/// ([`JsonConfig`](hang::catalog::JsonConfig)) through [`AsMut`], which is how an application lists
/// a data track in its own section beside its own per-track fields; see the [`RenditionConfig`]
/// example.
///
/// The producer sets the embedded config's `mode` and owns the entry: it is written when the
/// producer is created and removed when it drops.
pub trait IntoRendition<E: CatalogExt, D> {
	/// The catalog entry, embedding `D`.
	type Config: RenditionConfig<E> + AsMut<D>;

	/// Build the catalog entry.
	fn into_rendition(self) -> Self::Config;
}

impl<E: CatalogExt, D, C: RenditionConfig<E> + AsMut<D>> IntoRendition<E, D> for C {
	type Config = C;

	fn into_rendition(self) -> C {
		self
	}
}

/// A data track's catalog entry, owned for the life of its producer and kept current with the
/// bitrate and delay its writes measure.
///
/// Erases the entry's type, so a data producer's own type doesn't depend on which section lists it.
pub(crate) struct Listing {
	rendition: Box<dyn Owned>,
	/// Measures `delay` against the catalog's other renditions, media included.
	estimator: Estimator,
	/// The catalog clock as of the last write. Read again on every write, since an importer's first
	/// frame still places the clock if it lands while the catalog is withheld.
	clock: crate::Clock,
}

/// The parts of a [`Rendition`] a [`Listing`] uses, without its config type.
trait Owned: Send + Sync {
	fn name(&self) -> &str;
	fn clock(&self) -> crate::Clock;
	fn estimate(&mut self, reading: super::Reading) -> crate::Result<()>;
}

impl<E: CatalogExt, C: RenditionConfig<E>> Owned for Rendition<E, C> {
	fn name(&self) -> &str {
		Rendition::name(self)
	}
	fn clock(&self) -> crate::Clock {
		Rendition::clock(self)
	}
	fn estimate(&mut self, reading: super::Reading) -> crate::Result<()> {
		Rendition::estimate(self, reading)
	}
}

impl Listing {
	/// Publish `config` as the entry `rendition` reserved.
	pub(crate) fn new<E: CatalogExt, C: RenditionConfig<E>>(
		mut rendition: Rendition<E, C>,
		config: C,
	) -> crate::Result<Self> {
		rendition.set(config)?;
		Ok(Self {
			estimator: rendition.estimator(),
			clock: rendition.clock(),
			rendition: Box::new(rendition),
		})
	}

	/// The track name, which is also the catalog key.
	pub(crate) fn name(&self) -> &str {
		self.rendition.name()
	}

	/// Measure a frame of `bytes` encoded bytes, just written, captured at `captured` on the
	/// broadcast clock if known.
	pub(crate) fn record(&mut self, bytes: usize, captured: Option<moq_net::Timestamp>) -> crate::Result<()> {
		let clock = self.rendition.clock();
		// A re-anchor moves this track's timeline, which measured across would read as jitter.
		// Compare the whole clock, since an unset host wall clock has no mapping to compare.
		if clock != self.clock {
			self.estimator.discontinuity();
		}
		self.clock = clock;
		self.record_at(clock.now(), bytes, captured)
	}

	/// [`record`](Self::record) at a chosen time, which a test needs since the broadcast clock only
	/// moves in real time.
	pub(crate) fn record_at(
		&mut self,
		now: moq_net::Timestamp,
		bytes: usize,
		captured: Option<moq_net::Timestamp>,
	) -> crate::Result<()> {
		// Each write is its own span, closed by the next one.
		self.estimator.cut(Some(now));
		self.estimator.rate(now, bytes);

		// The write time is the application's cadence, not media time, so only a capture time takes
		// a place in the broadcast's write order. Without one no delay is advertised.
		if let Some(captured) = captured {
			// A source clock running fast stamps ahead of now. Ordered as given, it would put every
			// media track behind it by however fast it runs. It was written no later than now.
			self.estimator.order(captured.min(now));
		}
		self.rendition.estimate(self.estimator.reading())
	}
}
