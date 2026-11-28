/// Where a [`Container`](super::Container) writes a group's frames: the group, or the track whose
/// next group the first frame creates, keyed by that frame's timestamp.
pub struct Writer<'a> {
	track: &'a moq_net::track::Producer,
	group: &'a mut Option<moq_net::group::Producer>,
	/// How the first frame creates the group, for tests.
	#[cfg(test)]
	open: Open,
}

impl<'a> Writer<'a> {
	/// Write into `group`, appending one to `track` with the first frame while it is `None`.
	pub fn new(track: &'a moq_net::track::Producer, group: &'a mut Option<moq_net::group::Producer>) -> Self {
		Self {
			track,
			group,
			#[cfg(test)]
			open: Open::Append,
		}
	}

	/// [`Self::new`], opening the group as `open` says so a test can write groups out of order.
	#[cfg(test)]
	pub(crate) fn opening(
		track: &'a moq_net::track::Producer,
		group: &'a mut Option<moq_net::group::Producer>,
		open: Open,
	) -> Self {
		Self { track, group, open }
	}

	/// Write a frame, appending the group to the track with it when it is the first.
	pub fn write_frame<B: moq_net::IntoBytes>(
		&mut self,
		timestamp: moq_net::Timestamp,
		data: B,
	) -> moq_net::Result<()> {
		match self.group {
			Some(group) => group.write_frame(timestamp, data),
			None => {
				#[cfg(test)]
				match self.open {
					Open::Append => {}
					Open::Create => {
						*self.group = Some(self.track.create_group(timestamp, data)?);
						return Ok(());
					}
					Open::Follow => {
						let key = self.track.info().rescale(timestamp)?.value();
						let from = self.track.largest().map_or(0, |largest| largest.group + 1);
						if from < key {
							self.track.skip_groups(from..key);
						}
						*self.group = Some(self.track.create_group(timestamp, data)?);
						return Ok(());
					}
				}
				*self.group = Some(self.track.append_group(timestamp, data)?);
				Ok(())
			}
		}
	}

	/// The newest timestamp the group holds, `None` before its first frame.
	pub fn latest(&self) -> Option<moq_net::Timestamp> {
		self.group.as_ref().and_then(|group| group.latest())
	}

	/// How many frames the group holds.
	pub fn frame_count(&self) -> usize {
		self.group.as_ref().map_or(0, |group| group.frame_count())
	}

	/// The track's timescale, which every frame timestamp is converted into.
	pub fn timescale(&self) -> Option<moq_net::Timescale> {
		self.track.info().timescale
	}
}

/// How a test's first frame creates its group.
#[cfg(test)]
#[derive(Clone, Copy)]
pub(crate) enum Open {
	/// `append_group`, skipping every start it passes.
	Append,
	/// `create_group`, leaving the starts around it unproduced.
	Create,
	/// `create_group`, skipping the starts between the newest group and it.
	Follow,
}
