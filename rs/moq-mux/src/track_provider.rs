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

	pub(crate) fn create(&mut self) -> crate::Result<moq_net::TrackProducer> {
		match self {
			Self::Unique { broadcast, suffix } => {
				// Newly created tracks publish at the native container timescale so the
				// relay gets timing without parsing the payload.
				let info = moq_net::TrackInfo::default().with_timescale(hang::container::TIMESCALE);
				Ok(broadcast.unique_track(suffix, info)?)
			}
			Self::Fixed(track) => Ok(track.clone()),
		}
	}
}
