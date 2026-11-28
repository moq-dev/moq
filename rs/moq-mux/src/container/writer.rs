/// Where a [`Container`](super::Container) writes a group's frames: the group, or the track whose
/// next group the first frame creates, keyed by that frame's timestamp.
pub struct Writer<'a> {
	track: &'a moq_net::track::Producer,
	group: &'a mut Option<moq_net::group::Producer>,
	/// The first frame creates the group out of order, rather than appending it, for tests.
	#[cfg(test)]
	create: bool,
}

impl<'a> Writer<'a> {
	/// Write into `group`, appending one to `track` with the first frame while it is `None`.
	pub fn new(track: &'a moq_net::track::Producer, group: &'a mut Option<moq_net::group::Producer>) -> Self {
		Self {
			track,
			group,
			#[cfg(test)]
			create: false,
		}
	}

	/// [`Self::new`], creating the group with `create_group` so a test can write groups out of order.
	#[cfg(test)]
	pub(crate) fn creating(track: &'a moq_net::track::Producer, group: &'a mut Option<moq_net::group::Producer>) -> Self {
		Self { track, group, create: true }
	}

	/// Write a frame, appending the group to the track with it when it is the first.
	pub fn write_frame<B: moq_net::IntoBytes>(&mut self, timestamp: moq_net::Timestamp, data: B) -> moq_net::Result<()> {
		match self.group {
			Some(group) => group.write_frame(timestamp, data),
			None => {
				#[cfg(test)]
				if self.create {
					*self.group = Some(self.track.create_group(timestamp, data)?);
					return Ok(());
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
