pub(crate) enum TrackProvider {
	Unique {
		broadcast: moq_net::BroadcastProducer,
		suffix: &'static str,
	},
	Fixed(moq_net::TrackProducer),
}

impl TrackProvider {
	pub(crate) fn unique(broadcast: moq_net::BroadcastProducer, suffix: &'static str) -> Self {
		Self::Unique { broadcast, suffix }
	}

	pub(crate) fn fixed(track: moq_net::TrackProducer) -> Self {
		Self::Fixed(track)
	}

	pub(crate) fn is_fixed(&self) -> bool {
		matches!(self, Self::Fixed(_))
	}

	pub(crate) fn set_suffix(&mut self, next: &'static str) {
		if let Self::Unique { suffix, .. } = self {
			*suffix = next;
		}
	}

	pub(crate) fn create(&mut self) -> anyhow::Result<moq_net::TrackProducer> {
		match self {
			Self::Unique { broadcast, suffix } => Ok(broadcast.unique_track(suffix)?),
			Self::Fixed(track) => Ok(track.clone()),
		}
	}
}
