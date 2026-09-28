use super::hang::CatalogExt;
use super::{Estimator, Rendition, RenditionConfig};

/// The config a data producer ([`Producer::json_stream`](super::Producer::json_stream) and the
/// like) publishes as its catalog entry.
///
/// Implemented for the [`json::Config`](crate::json::Config) and
/// [`binary::Config`](crate::binary::Config) builders, which list the track in the `json` or
/// `binary` section. Also implemented for any [`RenditionConfig`] that embeds the data config `D`
/// ([`JsonConfig`](hang::catalog::JsonConfig) or [`BinaryConfig`](hang::catalog::BinaryConfig))
/// through [`AsMut`], which is how an application lists a data track in its own section beside its
/// own per-track fields; see the [`RenditionConfig`] example.
///
/// The producer sets the embedded config's `mode`, encodes the track with its `compression`, and
/// owns the entry: it is written when the producer is created and removed when it drops.
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
/// bitrate, jitter, and delay its writes measure.
///
/// Erases the entry's type, so a data producer's own type doesn't depend on which section lists it.
pub(crate) struct Listing {
	rendition: Box<dyn Owned>,
	/// Measures `delay` against the catalog's other renditions, media included.
	estimator: Estimator,
}

/// The parts of a [`Rendition`] a [`Listing`] uses, without its config type.
trait Owned: Send + Sync {
	fn name(&self) -> &str;
	fn timestamp(&self) -> crate::Result<moq_net::Timestamp>;
	fn estimate(&mut self, estimate: super::Estimate) -> crate::Result<()>;
}

impl<E: CatalogExt, C: RenditionConfig<E>> Owned for Rendition<E, C> {
	fn name(&self) -> &str {
		Rendition::name(self)
	}
	fn timestamp(&self) -> crate::Result<moq_net::Timestamp> {
		Rendition::timestamp(self, None)
	}
	fn estimate(&mut self, estimate: super::Estimate) -> crate::Result<()> {
		Rendition::estimate(self, estimate)
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
		let now = self.rendition.timestamp()?;
		let flush = captured.map(|captured| (captured, std::time::Instant::now()));
		self.record_at(now, bytes, flush)
	}

	/// [`record`](Self::record) at a chosen time, which a test needs since the broadcast clock only
	/// moves in real time. `flush` is the capture time and the instant the frame was written.
	pub(crate) fn record_at(
		&mut self,
		now: moq_net::Timestamp,
		bytes: usize,
		flush: Option<(moq_net::Timestamp, std::time::Instant)>,
	) -> crate::Result<()> {
		// Each write is its own span, closed by the next one.
		self.estimator.cut(Some(now));
		self.estimator.write(now, bytes);

		// The spacing between writes is the application's cadence, not a flush delay, so only a
		// capture time measures jitter and delay. Without one neither is advertised.
		if let Some((captured, written)) = flush {
			self.estimator.flush(captured, written);
		}
		self.rendition.estimate(self.estimator.estimate())
	}
}
