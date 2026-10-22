use hang::catalog::{AudioLayout, Projection, Stereo};

/// How the broadcast's media fills the space around the viewer, stamped onto each video and
/// audio rendition whose own config leaves the field unset.
///
/// The bitstreams the importers read carry none of this, so the publisher declares it. Set it
/// with [`Config::with_spatial`](super::Config::with_spatial).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Spatial {
	/// How each video picture maps onto the viewer's field of view.
	pub projection: Option<Projection>,

	/// How each video picture packs the views of both eyes.
	pub stereo: Option<Stereo>,

	/// What each audio rendition's decoded channels carry.
	pub layout: Option<AudioLayout>,
}

impl Spatial {
	pub(super) fn video(&self, config: &mut hang::catalog::VideoConfig) -> crate::Result<()> {
		if config.projection.is_none() {
			config.projection.clone_from(&self.projection);
		}
		if config.stereo.is_none() {
			config.stereo.clone_from(&self.stereo);
		}
		if let Some(projection) = &config.projection {
			projection.check()?;
		}
		Ok(())
	}

	pub(super) fn audio(&self, config: &mut hang::catalog::AudioConfig) -> crate::Result<()> {
		if config.layout.is_none() {
			config.layout.clone_from(&self.layout);
		}
		if let Some(layout) = &config.layout {
			layout.check(config.channel_count)?;
		}
		Ok(())
	}
}
