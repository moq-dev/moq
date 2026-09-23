pub use root::*;

const _: () = ::planus::check_version_compatibility("planus-1.3.0");

/// The root namespace
///
/// Generated from these locations:
/// * File `stats.fbs`
#[no_implicit_prelude]
#[allow(clippy::needless_lifetimes)]
mod root {
	/// The namespace `moq`
	///
	/// Generated from these locations:
	/// * File `stats.fbs`
	pub mod moq {
		/// The namespace `moq.stats`
		///
		/// Generated from these locations:
		/// * File `stats.fbs`
		pub mod stats {
			///  Payload-volume counters for content with one delivery outcome.
			///
			/// Generated from these locations:
			/// * Table `Content` in the file `stats.fbs:19`
			#[derive(Clone, Debug, PartialEq, PartialOrd, Eq, Ord, Hash, ::serde::Serialize, ::serde::Deserialize)]
			pub struct Content {
				/// The field `bytes` in the table `Content`
				pub bytes: u64,
				/// The field `frames` in the table `Content`
				pub frames: u64,
				/// The field `groups` in the table `Content`
				pub groups: u64,
				/// The field `datagrams` in the table `Content`
				pub datagrams: u64,
			}

			#[allow(clippy::derivable_impls)]
			impl ::core::default::Default for Content {
				fn default() -> Self {
					Self {
						bytes: 0,
						frames: 0,
						groups: 0,
						datagrams: 0,
					}
				}
			}

			impl Content {
				/// Creates a [ContentBuilder] for serializing an instance of this table.
				#[inline]
				pub fn builder() -> ContentBuilder<()> {
					ContentBuilder(())
				}

				#[allow(clippy::too_many_arguments)]
				pub fn create(
					builder: &mut ::planus::Builder,
					field_bytes: impl ::planus::WriteAsDefault<u64, u64>,
					field_frames: impl ::planus::WriteAsDefault<u64, u64>,
					field_groups: impl ::planus::WriteAsDefault<u64, u64>,
					field_datagrams: impl ::planus::WriteAsDefault<u64, u64>,
				) -> ::planus::Offset<Self> {
					let prepared_bytes = field_bytes.prepare(builder, &0);
					let prepared_frames = field_frames.prepare(builder, &0);
					let prepared_groups = field_groups.prepare(builder, &0);
					let prepared_datagrams = field_datagrams.prepare(builder, &0);

					let mut table_writer: ::planus::table_writer::TableWriter<12> = ::core::default::Default::default();
					if prepared_bytes.is_some() {
						table_writer.write_entry::<u64>(0);
					}
					if prepared_frames.is_some() {
						table_writer.write_entry::<u64>(1);
					}
					if prepared_groups.is_some() {
						table_writer.write_entry::<u64>(2);
					}
					if prepared_datagrams.is_some() {
						table_writer.write_entry::<u64>(3);
					}

					unsafe {
						table_writer.finish(builder, |object_writer| {
							if let ::core::option::Option::Some(prepared_bytes) = prepared_bytes {
								object_writer.write::<_, _, 8>(&prepared_bytes);
							}
							if let ::core::option::Option::Some(prepared_frames) = prepared_frames {
								object_writer.write::<_, _, 8>(&prepared_frames);
							}
							if let ::core::option::Option::Some(prepared_groups) = prepared_groups {
								object_writer.write::<_, _, 8>(&prepared_groups);
							}
							if let ::core::option::Option::Some(prepared_datagrams) = prepared_datagrams {
								object_writer.write::<_, _, 8>(&prepared_datagrams);
							}
						});
					}
					builder.current_offset()
				}
			}

			impl ::planus::WriteAs<::planus::Offset<Content>> for Content {
				type Prepared = ::planus::Offset<Self>;

				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Content> {
					::planus::WriteAsOffset::prepare(self, builder)
				}
			}

			impl ::planus::WriteAsOptional<::planus::Offset<Content>> for Content {
				type Prepared = ::planus::Offset<Self>;

				#[inline]
				fn prepare(
					&self,
					builder: &mut ::planus::Builder,
				) -> ::core::option::Option<::planus::Offset<Content>> {
					::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
				}
			}

			impl ::planus::WriteAsOffset<Content> for Content {
				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Content> {
					Content::create(builder, self.bytes, self.frames, self.groups, self.datagrams)
				}
			}

			/// Builder for serializing an instance of the [Content] type.
			///
			/// Can be created using the [Content::builder] method.
			#[derive(Debug)]
			#[must_use]
			pub struct ContentBuilder<State>(State);

			impl ContentBuilder<()> {
				/// Setter for the [`bytes` field](Content#structfield.bytes).
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn bytes<T0>(self, value: T0) -> ContentBuilder<(T0,)>
				where
					T0: ::planus::WriteAsDefault<u64, u64>,
				{
					ContentBuilder((value,))
				}

				/// Sets the [`bytes` field](Content#structfield.bytes) to the default value.
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn bytes_as_default(self) -> ContentBuilder<(::planus::DefaultValue,)> {
					self.bytes(::planus::DefaultValue)
				}
			}

			impl<T0> ContentBuilder<(T0,)> {
				/// Setter for the [`frames` field](Content#structfield.frames).
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn frames<T1>(self, value: T1) -> ContentBuilder<(T0, T1)>
				where
					T1: ::planus::WriteAsDefault<u64, u64>,
				{
					let (v0,) = self.0;
					ContentBuilder((v0, value))
				}

				/// Sets the [`frames` field](Content#structfield.frames) to the default value.
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn frames_as_default(self) -> ContentBuilder<(T0, ::planus::DefaultValue)> {
					self.frames(::planus::DefaultValue)
				}
			}

			impl<T0, T1> ContentBuilder<(T0, T1)> {
				/// Setter for the [`groups` field](Content#structfield.groups).
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn groups<T2>(self, value: T2) -> ContentBuilder<(T0, T1, T2)>
				where
					T2: ::planus::WriteAsDefault<u64, u64>,
				{
					let (v0, v1) = self.0;
					ContentBuilder((v0, v1, value))
				}

				/// Sets the [`groups` field](Content#structfield.groups) to the default value.
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn groups_as_default(self) -> ContentBuilder<(T0, T1, ::planus::DefaultValue)> {
					self.groups(::planus::DefaultValue)
				}
			}

			impl<T0, T1, T2> ContentBuilder<(T0, T1, T2)> {
				/// Setter for the [`datagrams` field](Content#structfield.datagrams).
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn datagrams<T3>(self, value: T3) -> ContentBuilder<(T0, T1, T2, T3)>
				where
					T3: ::planus::WriteAsDefault<u64, u64>,
				{
					let (v0, v1, v2) = self.0;
					ContentBuilder((v0, v1, v2, value))
				}

				/// Sets the [`datagrams` field](Content#structfield.datagrams) to the default value.
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn datagrams_as_default(self) -> ContentBuilder<(T0, T1, T2, ::planus::DefaultValue)> {
					self.datagrams(::planus::DefaultValue)
				}
			}

			impl<T0, T1, T2, T3> ContentBuilder<(T0, T1, T2, T3)> {
				/// Finish writing the builder to get an [Offset](::planus::Offset) to a serialized [Content].
				#[inline]
				pub fn finish(self, builder: &mut ::planus::Builder) -> ::planus::Offset<Content>
				where
					Self: ::planus::WriteAsOffset<Content>,
				{
					::planus::WriteAsOffset::prepare(&self, builder)
				}
			}

			impl<
					T0: ::planus::WriteAsDefault<u64, u64>,
					T1: ::planus::WriteAsDefault<u64, u64>,
					T2: ::planus::WriteAsDefault<u64, u64>,
					T3: ::planus::WriteAsDefault<u64, u64>,
				> ::planus::WriteAs<::planus::Offset<Content>> for ContentBuilder<(T0, T1, T2, T3)>
			{
				type Prepared = ::planus::Offset<Content>;

				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Content> {
					::planus::WriteAsOffset::prepare(self, builder)
				}
			}

			impl<
					T0: ::planus::WriteAsDefault<u64, u64>,
					T1: ::planus::WriteAsDefault<u64, u64>,
					T2: ::planus::WriteAsDefault<u64, u64>,
					T3: ::planus::WriteAsDefault<u64, u64>,
				> ::planus::WriteAsOptional<::planus::Offset<Content>> for ContentBuilder<(T0, T1, T2, T3)>
			{
				type Prepared = ::planus::Offset<Content>;

				#[inline]
				fn prepare(
					&self,
					builder: &mut ::planus::Builder,
				) -> ::core::option::Option<::planus::Offset<Content>> {
					::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
				}
			}

			impl<
					T0: ::planus::WriteAsDefault<u64, u64>,
					T1: ::planus::WriteAsDefault<u64, u64>,
					T2: ::planus::WriteAsDefault<u64, u64>,
					T3: ::planus::WriteAsDefault<u64, u64>,
				> ::planus::WriteAsOffset<Content> for ContentBuilder<(T0, T1, T2, T3)>
			{
				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Content> {
					let (v0, v1, v2, v3) = &self.0;
					Content::create(builder, v0, v1, v2, v3)
				}
			}

			/// Reference to a deserialized [Content].
			#[derive(Copy, Clone)]
			pub struct ContentRef<'a>(#[allow(dead_code)] ::planus::table_reader::Table<'a>);

			impl<'a> ContentRef<'a> {
				/// Getter for the [`bytes` field](Content#structfield.bytes).
				#[inline]
				pub fn bytes(&self) -> ::planus::Result<u64> {
					::core::result::Result::Ok(self.0.access(0, "Content", "bytes")?.unwrap_or(0))
				}

				/// Getter for the [`frames` field](Content#structfield.frames).
				#[inline]
				pub fn frames(&self) -> ::planus::Result<u64> {
					::core::result::Result::Ok(self.0.access(1, "Content", "frames")?.unwrap_or(0))
				}

				/// Getter for the [`groups` field](Content#structfield.groups).
				#[inline]
				pub fn groups(&self) -> ::planus::Result<u64> {
					::core::result::Result::Ok(self.0.access(2, "Content", "groups")?.unwrap_or(0))
				}

				/// Getter for the [`datagrams` field](Content#structfield.datagrams).
				#[inline]
				pub fn datagrams(&self) -> ::planus::Result<u64> {
					::core::result::Result::Ok(self.0.access(3, "Content", "datagrams")?.unwrap_or(0))
				}
			}

			impl<'a> ::core::fmt::Debug for ContentRef<'a> {
				fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
					let mut f = f.debug_struct("ContentRef");
					f.field("bytes", &self.bytes());
					f.field("frames", &self.frames());
					f.field("groups", &self.groups());
					f.field("datagrams", &self.datagrams());
					f.finish()
				}
			}

			impl<'a> ::core::convert::TryFrom<ContentRef<'a>> for Content {
				type Error = ::planus::Error;

				#[allow(unreachable_code)]
				fn try_from(value: ContentRef<'a>) -> ::planus::Result<Self> {
					::core::result::Result::Ok(Self {
						bytes: ::core::convert::TryInto::try_into(value.bytes()?)?,
						frames: ::core::convert::TryInto::try_into(value.frames()?)?,
						groups: ::core::convert::TryInto::try_into(value.groups()?)?,
						datagrams: ::core::convert::TryInto::try_into(value.datagrams()?)?,
					})
				}
			}

			impl<'a> ::planus::TableRead<'a> for ContentRef<'a> {
				#[inline]
				fn from_buffer(
					buffer: ::planus::SliceWithStartOffset<'a>,
					offset: usize,
				) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
					::core::result::Result::Ok(Self(::planus::table_reader::Table::from_buffer(buffer, offset)?))
				}
			}

			impl<'a> ::planus::VectorReadInner<'a> for ContentRef<'a> {
				type Error = ::planus::Error;
				const STRIDE: usize = 4;

				unsafe fn from_buffer(
					buffer: ::planus::SliceWithStartOffset<'a>,
					offset: usize,
				) -> ::planus::Result<Self> {
					::planus::TableRead::from_buffer(buffer, offset).map_err(|error_kind| {
						error_kind.with_error_location("[ContentRef]", "get", buffer.offset_from_start)
					})
				}
			}

			/// # Safety
			/// The planus compiler generates implementations that initialize
			/// the bytes in `write_values`.
			unsafe impl ::planus::VectorWrite<::planus::Offset<Content>> for Content {
				type Value = ::planus::Offset<Content>;
				const STRIDE: usize = 4;
				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> Self::Value {
					::planus::WriteAs::prepare(self, builder)
				}

				#[inline]
				unsafe fn write_values(
					values: &[::planus::Offset<Content>],
					bytes: *mut ::core::mem::MaybeUninit<u8>,
					buffer_position: u32,
				) {
					let bytes = bytes as *mut [::core::mem::MaybeUninit<u8>; 4];
					for (i, v) in ::core::iter::Iterator::enumerate(values.iter()) {
						::planus::WriteAsPrimitive::write(
							v,
							::planus::Cursor::new(unsafe { &mut *bytes.add(i) }),
							buffer_position - (Self::STRIDE * i) as u32,
						);
					}
				}
			}

			impl<'a> ::planus::ReadAsRoot<'a> for ContentRef<'a> {
				fn read_as_root(slice: &'a [u8]) -> ::planus::Result<Self> {
					::planus::TableRead::from_buffer(
						::planus::SliceWithStartOffset {
							buffer: slice,
							offset_from_start: 0,
						},
						0,
					)
					.map_err(|error_kind| error_kind.with_error_location("[ContentRef]", "read_as_root", 0))
				}
			}

			///  Cumulative traffic counters for one broadcast on one tier and role.
			///
			/// Generated from these locations:
			/// * Table `Traffic` in the file `stats.fbs:27`
			#[derive(Clone, Debug, PartialEq, PartialOrd, Eq, Ord, Hash, ::serde::Serialize, ::serde::Deserialize)]
			pub struct Traffic {
				/// The field `announces_started` in the table `Traffic`
				pub announces_started: u64,
				/// The field `announces_ended` in the table `Traffic`
				pub announces_ended: u64,
				/// The field `announced_bytes` in the table `Traffic`
				pub announced_bytes: u64,
				/// The field `broadcasts_started` in the table `Traffic`
				pub broadcasts_started: u64,
				/// The field `broadcasts_ended` in the table `Traffic`
				pub broadcasts_ended: u64,
				/// The field `subscriptions_started` in the table `Traffic`
				pub subscriptions_started: u64,
				/// The field `subscriptions_ended` in the table `Traffic`
				pub subscriptions_ended: u64,
				/// The field `fetches` in the table `Traffic`
				pub fetches: u64,
				/// The field `bytes` in the table `Traffic`
				pub bytes: u64,
				/// The field `frames` in the table `Traffic`
				pub frames: u64,
				/// The field `groups` in the table `Traffic`
				pub groups: u64,
				/// The field `datagrams` in the table `Traffic`
				pub datagrams: u64,
				///  Content skipped because it aged past a subscriber's latency budget.
				pub stale: ::core::option::Option<::planus::alloc::boxed::Box<self::Content>>,
			}

			#[allow(clippy::derivable_impls)]
			impl ::core::default::Default for Traffic {
				fn default() -> Self {
					Self {
						announces_started: 0,
						announces_ended: 0,
						announced_bytes: 0,
						broadcasts_started: 0,
						broadcasts_ended: 0,
						subscriptions_started: 0,
						subscriptions_ended: 0,
						fetches: 0,
						bytes: 0,
						frames: 0,
						groups: 0,
						datagrams: 0,
						stale: ::core::default::Default::default(),
					}
				}
			}

			impl Traffic {
				/// Creates a [TrafficBuilder] for serializing an instance of this table.
				#[inline]
				pub fn builder() -> TrafficBuilder<()> {
					TrafficBuilder(())
				}

				#[allow(clippy::too_many_arguments)]
				pub fn create(
					builder: &mut ::planus::Builder,
					field_announces_started: impl ::planus::WriteAsDefault<u64, u64>,
					field_announces_ended: impl ::planus::WriteAsDefault<u64, u64>,
					field_announced_bytes: impl ::planus::WriteAsDefault<u64, u64>,
					field_broadcasts_started: impl ::planus::WriteAsDefault<u64, u64>,
					field_broadcasts_ended: impl ::planus::WriteAsDefault<u64, u64>,
					field_subscriptions_started: impl ::planus::WriteAsDefault<u64, u64>,
					field_subscriptions_ended: impl ::planus::WriteAsDefault<u64, u64>,
					field_fetches: impl ::planus::WriteAsDefault<u64, u64>,
					field_bytes: impl ::planus::WriteAsDefault<u64, u64>,
					field_frames: impl ::planus::WriteAsDefault<u64, u64>,
					field_groups: impl ::planus::WriteAsDefault<u64, u64>,
					field_datagrams: impl ::planus::WriteAsDefault<u64, u64>,
					field_stale: impl ::planus::WriteAsOptional<::planus::Offset<self::Content>>,
				) -> ::planus::Offset<Self> {
					let prepared_announces_started = field_announces_started.prepare(builder, &0);
					let prepared_announces_ended = field_announces_ended.prepare(builder, &0);
					let prepared_announced_bytes = field_announced_bytes.prepare(builder, &0);
					let prepared_broadcasts_started = field_broadcasts_started.prepare(builder, &0);
					let prepared_broadcasts_ended = field_broadcasts_ended.prepare(builder, &0);
					let prepared_subscriptions_started = field_subscriptions_started.prepare(builder, &0);
					let prepared_subscriptions_ended = field_subscriptions_ended.prepare(builder, &0);
					let prepared_fetches = field_fetches.prepare(builder, &0);
					let prepared_bytes = field_bytes.prepare(builder, &0);
					let prepared_frames = field_frames.prepare(builder, &0);
					let prepared_groups = field_groups.prepare(builder, &0);
					let prepared_datagrams = field_datagrams.prepare(builder, &0);
					let prepared_stale = field_stale.prepare(builder);

					let mut table_writer: ::planus::table_writer::TableWriter<30> = ::core::default::Default::default();
					if prepared_announces_started.is_some() {
						table_writer.write_entry::<u64>(0);
					}
					if prepared_announces_ended.is_some() {
						table_writer.write_entry::<u64>(1);
					}
					if prepared_announced_bytes.is_some() {
						table_writer.write_entry::<u64>(2);
					}
					if prepared_broadcasts_started.is_some() {
						table_writer.write_entry::<u64>(3);
					}
					if prepared_broadcasts_ended.is_some() {
						table_writer.write_entry::<u64>(4);
					}
					if prepared_subscriptions_started.is_some() {
						table_writer.write_entry::<u64>(5);
					}
					if prepared_subscriptions_ended.is_some() {
						table_writer.write_entry::<u64>(6);
					}
					if prepared_fetches.is_some() {
						table_writer.write_entry::<u64>(7);
					}
					if prepared_bytes.is_some() {
						table_writer.write_entry::<u64>(8);
					}
					if prepared_frames.is_some() {
						table_writer.write_entry::<u64>(9);
					}
					if prepared_groups.is_some() {
						table_writer.write_entry::<u64>(10);
					}
					if prepared_datagrams.is_some() {
						table_writer.write_entry::<u64>(11);
					}
					if prepared_stale.is_some() {
						table_writer.write_entry::<::planus::Offset<self::Content>>(12);
					}

					unsafe {
						table_writer.finish(builder, |object_writer| {
							if let ::core::option::Option::Some(prepared_announces_started) = prepared_announces_started
							{
								object_writer.write::<_, _, 8>(&prepared_announces_started);
							}
							if let ::core::option::Option::Some(prepared_announces_ended) = prepared_announces_ended {
								object_writer.write::<_, _, 8>(&prepared_announces_ended);
							}
							if let ::core::option::Option::Some(prepared_announced_bytes) = prepared_announced_bytes {
								object_writer.write::<_, _, 8>(&prepared_announced_bytes);
							}
							if let ::core::option::Option::Some(prepared_broadcasts_started) =
								prepared_broadcasts_started
							{
								object_writer.write::<_, _, 8>(&prepared_broadcasts_started);
							}
							if let ::core::option::Option::Some(prepared_broadcasts_ended) = prepared_broadcasts_ended {
								object_writer.write::<_, _, 8>(&prepared_broadcasts_ended);
							}
							if let ::core::option::Option::Some(prepared_subscriptions_started) =
								prepared_subscriptions_started
							{
								object_writer.write::<_, _, 8>(&prepared_subscriptions_started);
							}
							if let ::core::option::Option::Some(prepared_subscriptions_ended) =
								prepared_subscriptions_ended
							{
								object_writer.write::<_, _, 8>(&prepared_subscriptions_ended);
							}
							if let ::core::option::Option::Some(prepared_fetches) = prepared_fetches {
								object_writer.write::<_, _, 8>(&prepared_fetches);
							}
							if let ::core::option::Option::Some(prepared_bytes) = prepared_bytes {
								object_writer.write::<_, _, 8>(&prepared_bytes);
							}
							if let ::core::option::Option::Some(prepared_frames) = prepared_frames {
								object_writer.write::<_, _, 8>(&prepared_frames);
							}
							if let ::core::option::Option::Some(prepared_groups) = prepared_groups {
								object_writer.write::<_, _, 8>(&prepared_groups);
							}
							if let ::core::option::Option::Some(prepared_datagrams) = prepared_datagrams {
								object_writer.write::<_, _, 8>(&prepared_datagrams);
							}
							if let ::core::option::Option::Some(prepared_stale) = prepared_stale {
								object_writer.write::<_, _, 4>(&prepared_stale);
							}
						});
					}
					builder.current_offset()
				}
			}

			impl ::planus::WriteAs<::planus::Offset<Traffic>> for Traffic {
				type Prepared = ::planus::Offset<Self>;

				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Traffic> {
					::planus::WriteAsOffset::prepare(self, builder)
				}
			}

			impl ::planus::WriteAsOptional<::planus::Offset<Traffic>> for Traffic {
				type Prepared = ::planus::Offset<Self>;

				#[inline]
				fn prepare(
					&self,
					builder: &mut ::planus::Builder,
				) -> ::core::option::Option<::planus::Offset<Traffic>> {
					::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
				}
			}

			impl ::planus::WriteAsOffset<Traffic> for Traffic {
				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Traffic> {
					Traffic::create(
						builder,
						self.announces_started,
						self.announces_ended,
						self.announced_bytes,
						self.broadcasts_started,
						self.broadcasts_ended,
						self.subscriptions_started,
						self.subscriptions_ended,
						self.fetches,
						self.bytes,
						self.frames,
						self.groups,
						self.datagrams,
						&self.stale,
					)
				}
			}

			/// Builder for serializing an instance of the [Traffic] type.
			///
			/// Can be created using the [Traffic::builder] method.
			#[derive(Debug)]
			#[must_use]
			pub struct TrafficBuilder<State>(State);

			impl TrafficBuilder<()> {
				/// Setter for the [`announces_started` field](Traffic#structfield.announces_started).
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn announces_started<T0>(self, value: T0) -> TrafficBuilder<(T0,)>
				where
					T0: ::planus::WriteAsDefault<u64, u64>,
				{
					TrafficBuilder((value,))
				}

				/// Sets the [`announces_started` field](Traffic#structfield.announces_started) to the default value.
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn announces_started_as_default(self) -> TrafficBuilder<(::planus::DefaultValue,)> {
					self.announces_started(::planus::DefaultValue)
				}
			}

			impl<T0> TrafficBuilder<(T0,)> {
				/// Setter for the [`announces_ended` field](Traffic#structfield.announces_ended).
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn announces_ended<T1>(self, value: T1) -> TrafficBuilder<(T0, T1)>
				where
					T1: ::planus::WriteAsDefault<u64, u64>,
				{
					let (v0,) = self.0;
					TrafficBuilder((v0, value))
				}

				/// Sets the [`announces_ended` field](Traffic#structfield.announces_ended) to the default value.
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn announces_ended_as_default(self) -> TrafficBuilder<(T0, ::planus::DefaultValue)> {
					self.announces_ended(::planus::DefaultValue)
				}
			}

			impl<T0, T1> TrafficBuilder<(T0, T1)> {
				/// Setter for the [`announced_bytes` field](Traffic#structfield.announced_bytes).
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn announced_bytes<T2>(self, value: T2) -> TrafficBuilder<(T0, T1, T2)>
				where
					T2: ::planus::WriteAsDefault<u64, u64>,
				{
					let (v0, v1) = self.0;
					TrafficBuilder((v0, v1, value))
				}

				/// Sets the [`announced_bytes` field](Traffic#structfield.announced_bytes) to the default value.
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn announced_bytes_as_default(self) -> TrafficBuilder<(T0, T1, ::planus::DefaultValue)> {
					self.announced_bytes(::planus::DefaultValue)
				}
			}

			impl<T0, T1, T2> TrafficBuilder<(T0, T1, T2)> {
				/// Setter for the [`broadcasts_started` field](Traffic#structfield.broadcasts_started).
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn broadcasts_started<T3>(self, value: T3) -> TrafficBuilder<(T0, T1, T2, T3)>
				where
					T3: ::planus::WriteAsDefault<u64, u64>,
				{
					let (v0, v1, v2) = self.0;
					TrafficBuilder((v0, v1, v2, value))
				}

				/// Sets the [`broadcasts_started` field](Traffic#structfield.broadcasts_started) to the default value.
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn broadcasts_started_as_default(self) -> TrafficBuilder<(T0, T1, T2, ::planus::DefaultValue)> {
					self.broadcasts_started(::planus::DefaultValue)
				}
			}

			impl<T0, T1, T2, T3> TrafficBuilder<(T0, T1, T2, T3)> {
				/// Setter for the [`broadcasts_ended` field](Traffic#structfield.broadcasts_ended).
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn broadcasts_ended<T4>(self, value: T4) -> TrafficBuilder<(T0, T1, T2, T3, T4)>
				where
					T4: ::planus::WriteAsDefault<u64, u64>,
				{
					let (v0, v1, v2, v3) = self.0;
					TrafficBuilder((v0, v1, v2, v3, value))
				}

				/// Sets the [`broadcasts_ended` field](Traffic#structfield.broadcasts_ended) to the default value.
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn broadcasts_ended_as_default(self) -> TrafficBuilder<(T0, T1, T2, T3, ::planus::DefaultValue)> {
					self.broadcasts_ended(::planus::DefaultValue)
				}
			}

			impl<T0, T1, T2, T3, T4> TrafficBuilder<(T0, T1, T2, T3, T4)> {
				/// Setter for the [`subscriptions_started` field](Traffic#structfield.subscriptions_started).
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn subscriptions_started<T5>(self, value: T5) -> TrafficBuilder<(T0, T1, T2, T3, T4, T5)>
				where
					T5: ::planus::WriteAsDefault<u64, u64>,
				{
					let (v0, v1, v2, v3, v4) = self.0;
					TrafficBuilder((v0, v1, v2, v3, v4, value))
				}

				/// Sets the [`subscriptions_started` field](Traffic#structfield.subscriptions_started) to the default value.
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn subscriptions_started_as_default(
					self,
				) -> TrafficBuilder<(T0, T1, T2, T3, T4, ::planus::DefaultValue)> {
					self.subscriptions_started(::planus::DefaultValue)
				}
			}

			impl<T0, T1, T2, T3, T4, T5> TrafficBuilder<(T0, T1, T2, T3, T4, T5)> {
				/// Setter for the [`subscriptions_ended` field](Traffic#structfield.subscriptions_ended).
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn subscriptions_ended<T6>(self, value: T6) -> TrafficBuilder<(T0, T1, T2, T3, T4, T5, T6)>
				where
					T6: ::planus::WriteAsDefault<u64, u64>,
				{
					let (v0, v1, v2, v3, v4, v5) = self.0;
					TrafficBuilder((v0, v1, v2, v3, v4, v5, value))
				}

				/// Sets the [`subscriptions_ended` field](Traffic#structfield.subscriptions_ended) to the default value.
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn subscriptions_ended_as_default(
					self,
				) -> TrafficBuilder<(T0, T1, T2, T3, T4, T5, ::planus::DefaultValue)> {
					self.subscriptions_ended(::planus::DefaultValue)
				}
			}

			impl<T0, T1, T2, T3, T4, T5, T6> TrafficBuilder<(T0, T1, T2, T3, T4, T5, T6)> {
				/// Setter for the [`fetches` field](Traffic#structfield.fetches).
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn fetches<T7>(self, value: T7) -> TrafficBuilder<(T0, T1, T2, T3, T4, T5, T6, T7)>
				where
					T7: ::planus::WriteAsDefault<u64, u64>,
				{
					let (v0, v1, v2, v3, v4, v5, v6) = self.0;
					TrafficBuilder((v0, v1, v2, v3, v4, v5, v6, value))
				}

				/// Sets the [`fetches` field](Traffic#structfield.fetches) to the default value.
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn fetches_as_default(
					self,
				) -> TrafficBuilder<(T0, T1, T2, T3, T4, T5, T6, ::planus::DefaultValue)> {
					self.fetches(::planus::DefaultValue)
				}
			}

			impl<T0, T1, T2, T3, T4, T5, T6, T7> TrafficBuilder<(T0, T1, T2, T3, T4, T5, T6, T7)> {
				/// Setter for the [`bytes` field](Traffic#structfield.bytes).
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn bytes<T8>(self, value: T8) -> TrafficBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8)>
				where
					T8: ::planus::WriteAsDefault<u64, u64>,
				{
					let (v0, v1, v2, v3, v4, v5, v6, v7) = self.0;
					TrafficBuilder((v0, v1, v2, v3, v4, v5, v6, v7, value))
				}

				/// Sets the [`bytes` field](Traffic#structfield.bytes) to the default value.
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn bytes_as_default(
					self,
				) -> TrafficBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, ::planus::DefaultValue)> {
					self.bytes(::planus::DefaultValue)
				}
			}

			impl<T0, T1, T2, T3, T4, T5, T6, T7, T8> TrafficBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8)> {
				/// Setter for the [`frames` field](Traffic#structfield.frames).
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn frames<T9>(self, value: T9) -> TrafficBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9)>
				where
					T9: ::planus::WriteAsDefault<u64, u64>,
				{
					let (v0, v1, v2, v3, v4, v5, v6, v7, v8) = self.0;
					TrafficBuilder((v0, v1, v2, v3, v4, v5, v6, v7, v8, value))
				}

				/// Sets the [`frames` field](Traffic#structfield.frames) to the default value.
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn frames_as_default(
					self,
				) -> TrafficBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, ::planus::DefaultValue)> {
					self.frames(::planus::DefaultValue)
				}
			}

			impl<T0, T1, T2, T3, T4, T5, T6, T7, T8, T9> TrafficBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9)> {
				/// Setter for the [`groups` field](Traffic#structfield.groups).
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn groups<T10>(self, value: T10) -> TrafficBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10)>
				where
					T10: ::planus::WriteAsDefault<u64, u64>,
				{
					let (v0, v1, v2, v3, v4, v5, v6, v7, v8, v9) = self.0;
					TrafficBuilder((v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, value))
				}

				/// Sets the [`groups` field](Traffic#structfield.groups) to the default value.
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn groups_as_default(
					self,
				) -> TrafficBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, ::planus::DefaultValue)> {
					self.groups(::planus::DefaultValue)
				}
			}

			impl<T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10> TrafficBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10)> {
				/// Setter for the [`datagrams` field](Traffic#structfield.datagrams).
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn datagrams<T11>(
					self,
					value: T11,
				) -> TrafficBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11)>
				where
					T11: ::planus::WriteAsDefault<u64, u64>,
				{
					let (v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, v10) = self.0;
					TrafficBuilder((v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, value))
				}

				/// Sets the [`datagrams` field](Traffic#structfield.datagrams) to the default value.
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn datagrams_as_default(
					self,
				) -> TrafficBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, ::planus::DefaultValue)> {
					self.datagrams(::planus::DefaultValue)
				}
			}

			impl<T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11>
				TrafficBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11)>
			{
				/// Setter for the [`stale` field](Traffic#structfield.stale).
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn stale<T12>(
					self,
					value: T12,
				) -> TrafficBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12)>
				where
					T12: ::planus::WriteAsOptional<::planus::Offset<self::Content>>,
				{
					let (v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, v11) = self.0;
					TrafficBuilder((v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, v11, value))
				}

				/// Sets the [`stale` field](Traffic#structfield.stale) to null.
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn stale_as_null(self) -> TrafficBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, ())> {
					self.stale(())
				}
			}

			impl<T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12>
				TrafficBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12)>
			{
				/// Finish writing the builder to get an [Offset](::planus::Offset) to a serialized [Traffic].
				#[inline]
				pub fn finish(self, builder: &mut ::planus::Builder) -> ::planus::Offset<Traffic>
				where
					Self: ::planus::WriteAsOffset<Traffic>,
				{
					::planus::WriteAsOffset::prepare(&self, builder)
				}
			}

			impl<
					T0: ::planus::WriteAsDefault<u64, u64>,
					T1: ::planus::WriteAsDefault<u64, u64>,
					T2: ::planus::WriteAsDefault<u64, u64>,
					T3: ::planus::WriteAsDefault<u64, u64>,
					T4: ::planus::WriteAsDefault<u64, u64>,
					T5: ::planus::WriteAsDefault<u64, u64>,
					T6: ::planus::WriteAsDefault<u64, u64>,
					T7: ::planus::WriteAsDefault<u64, u64>,
					T8: ::planus::WriteAsDefault<u64, u64>,
					T9: ::planus::WriteAsDefault<u64, u64>,
					T10: ::planus::WriteAsDefault<u64, u64>,
					T11: ::planus::WriteAsDefault<u64, u64>,
					T12: ::planus::WriteAsOptional<::planus::Offset<self::Content>>,
				> ::planus::WriteAs<::planus::Offset<Traffic>>
				for TrafficBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12)>
			{
				type Prepared = ::planus::Offset<Traffic>;

				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Traffic> {
					::planus::WriteAsOffset::prepare(self, builder)
				}
			}

			impl<
					T0: ::planus::WriteAsDefault<u64, u64>,
					T1: ::planus::WriteAsDefault<u64, u64>,
					T2: ::planus::WriteAsDefault<u64, u64>,
					T3: ::planus::WriteAsDefault<u64, u64>,
					T4: ::planus::WriteAsDefault<u64, u64>,
					T5: ::planus::WriteAsDefault<u64, u64>,
					T6: ::planus::WriteAsDefault<u64, u64>,
					T7: ::planus::WriteAsDefault<u64, u64>,
					T8: ::planus::WriteAsDefault<u64, u64>,
					T9: ::planus::WriteAsDefault<u64, u64>,
					T10: ::planus::WriteAsDefault<u64, u64>,
					T11: ::planus::WriteAsDefault<u64, u64>,
					T12: ::planus::WriteAsOptional<::planus::Offset<self::Content>>,
				> ::planus::WriteAsOptional<::planus::Offset<Traffic>>
				for TrafficBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12)>
			{
				type Prepared = ::planus::Offset<Traffic>;

				#[inline]
				fn prepare(
					&self,
					builder: &mut ::planus::Builder,
				) -> ::core::option::Option<::planus::Offset<Traffic>> {
					::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
				}
			}

			impl<
					T0: ::planus::WriteAsDefault<u64, u64>,
					T1: ::planus::WriteAsDefault<u64, u64>,
					T2: ::planus::WriteAsDefault<u64, u64>,
					T3: ::planus::WriteAsDefault<u64, u64>,
					T4: ::planus::WriteAsDefault<u64, u64>,
					T5: ::planus::WriteAsDefault<u64, u64>,
					T6: ::planus::WriteAsDefault<u64, u64>,
					T7: ::planus::WriteAsDefault<u64, u64>,
					T8: ::planus::WriteAsDefault<u64, u64>,
					T9: ::planus::WriteAsDefault<u64, u64>,
					T10: ::planus::WriteAsDefault<u64, u64>,
					T11: ::planus::WriteAsDefault<u64, u64>,
					T12: ::planus::WriteAsOptional<::planus::Offset<self::Content>>,
				> ::planus::WriteAsOffset<Traffic> for TrafficBuilder<(T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10, T11, T12)>
			{
				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Traffic> {
					let (v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, v11, v12) = &self.0;
					Traffic::create(builder, v0, v1, v2, v3, v4, v5, v6, v7, v8, v9, v10, v11, v12)
				}
			}

			/// Reference to a deserialized [Traffic].
			#[derive(Copy, Clone)]
			pub struct TrafficRef<'a>(#[allow(dead_code)] ::planus::table_reader::Table<'a>);

			impl<'a> TrafficRef<'a> {
				/// Getter for the [`announces_started` field](Traffic#structfield.announces_started).
				#[inline]
				pub fn announces_started(&self) -> ::planus::Result<u64> {
					::core::result::Result::Ok(self.0.access(0, "Traffic", "announces_started")?.unwrap_or(0))
				}

				/// Getter for the [`announces_ended` field](Traffic#structfield.announces_ended).
				#[inline]
				pub fn announces_ended(&self) -> ::planus::Result<u64> {
					::core::result::Result::Ok(self.0.access(1, "Traffic", "announces_ended")?.unwrap_or(0))
				}

				/// Getter for the [`announced_bytes` field](Traffic#structfield.announced_bytes).
				#[inline]
				pub fn announced_bytes(&self) -> ::planus::Result<u64> {
					::core::result::Result::Ok(self.0.access(2, "Traffic", "announced_bytes")?.unwrap_or(0))
				}

				/// Getter for the [`broadcasts_started` field](Traffic#structfield.broadcasts_started).
				#[inline]
				pub fn broadcasts_started(&self) -> ::planus::Result<u64> {
					::core::result::Result::Ok(self.0.access(3, "Traffic", "broadcasts_started")?.unwrap_or(0))
				}

				/// Getter for the [`broadcasts_ended` field](Traffic#structfield.broadcasts_ended).
				#[inline]
				pub fn broadcasts_ended(&self) -> ::planus::Result<u64> {
					::core::result::Result::Ok(self.0.access(4, "Traffic", "broadcasts_ended")?.unwrap_or(0))
				}

				/// Getter for the [`subscriptions_started` field](Traffic#structfield.subscriptions_started).
				#[inline]
				pub fn subscriptions_started(&self) -> ::planus::Result<u64> {
					::core::result::Result::Ok(self.0.access(5, "Traffic", "subscriptions_started")?.unwrap_or(0))
				}

				/// Getter for the [`subscriptions_ended` field](Traffic#structfield.subscriptions_ended).
				#[inline]
				pub fn subscriptions_ended(&self) -> ::planus::Result<u64> {
					::core::result::Result::Ok(self.0.access(6, "Traffic", "subscriptions_ended")?.unwrap_or(0))
				}

				/// Getter for the [`fetches` field](Traffic#structfield.fetches).
				#[inline]
				pub fn fetches(&self) -> ::planus::Result<u64> {
					::core::result::Result::Ok(self.0.access(7, "Traffic", "fetches")?.unwrap_or(0))
				}

				/// Getter for the [`bytes` field](Traffic#structfield.bytes).
				#[inline]
				pub fn bytes(&self) -> ::planus::Result<u64> {
					::core::result::Result::Ok(self.0.access(8, "Traffic", "bytes")?.unwrap_or(0))
				}

				/// Getter for the [`frames` field](Traffic#structfield.frames).
				#[inline]
				pub fn frames(&self) -> ::planus::Result<u64> {
					::core::result::Result::Ok(self.0.access(9, "Traffic", "frames")?.unwrap_or(0))
				}

				/// Getter for the [`groups` field](Traffic#structfield.groups).
				#[inline]
				pub fn groups(&self) -> ::planus::Result<u64> {
					::core::result::Result::Ok(self.0.access(10, "Traffic", "groups")?.unwrap_or(0))
				}

				/// Getter for the [`datagrams` field](Traffic#structfield.datagrams).
				#[inline]
				pub fn datagrams(&self) -> ::planus::Result<u64> {
					::core::result::Result::Ok(self.0.access(11, "Traffic", "datagrams")?.unwrap_or(0))
				}

				/// Getter for the [`stale` field](Traffic#structfield.stale).
				#[inline]
				pub fn stale(&self) -> ::planus::Result<::core::option::Option<self::ContentRef<'a>>> {
					self.0.access(12, "Traffic", "stale")
				}
			}

			impl<'a> ::core::fmt::Debug for TrafficRef<'a> {
				fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
					let mut f = f.debug_struct("TrafficRef");
					f.field("announces_started", &self.announces_started());
					f.field("announces_ended", &self.announces_ended());
					f.field("announced_bytes", &self.announced_bytes());
					f.field("broadcasts_started", &self.broadcasts_started());
					f.field("broadcasts_ended", &self.broadcasts_ended());
					f.field("subscriptions_started", &self.subscriptions_started());
					f.field("subscriptions_ended", &self.subscriptions_ended());
					f.field("fetches", &self.fetches());
					f.field("bytes", &self.bytes());
					f.field("frames", &self.frames());
					f.field("groups", &self.groups());
					f.field("datagrams", &self.datagrams());
					if let ::core::option::Option::Some(field_stale) = self.stale().transpose() {
						f.field("stale", &field_stale);
					}
					f.finish()
				}
			}

			impl<'a> ::core::convert::TryFrom<TrafficRef<'a>> for Traffic {
				type Error = ::planus::Error;

				#[allow(unreachable_code)]
				fn try_from(value: TrafficRef<'a>) -> ::planus::Result<Self> {
					::core::result::Result::Ok(Self {
						announces_started: ::core::convert::TryInto::try_into(value.announces_started()?)?,
						announces_ended: ::core::convert::TryInto::try_into(value.announces_ended()?)?,
						announced_bytes: ::core::convert::TryInto::try_into(value.announced_bytes()?)?,
						broadcasts_started: ::core::convert::TryInto::try_into(value.broadcasts_started()?)?,
						broadcasts_ended: ::core::convert::TryInto::try_into(value.broadcasts_ended()?)?,
						subscriptions_started: ::core::convert::TryInto::try_into(value.subscriptions_started()?)?,
						subscriptions_ended: ::core::convert::TryInto::try_into(value.subscriptions_ended()?)?,
						fetches: ::core::convert::TryInto::try_into(value.fetches()?)?,
						bytes: ::core::convert::TryInto::try_into(value.bytes()?)?,
						frames: ::core::convert::TryInto::try_into(value.frames()?)?,
						groups: ::core::convert::TryInto::try_into(value.groups()?)?,
						datagrams: ::core::convert::TryInto::try_into(value.datagrams()?)?,
						stale: if let ::core::option::Option::Some(stale) = value.stale()? {
							::core::option::Option::Some(::planus::alloc::boxed::Box::new(
								::core::convert::TryInto::try_into(stale)?,
							))
						} else {
							::core::option::Option::None
						},
					})
				}
			}

			impl<'a> ::planus::TableRead<'a> for TrafficRef<'a> {
				#[inline]
				fn from_buffer(
					buffer: ::planus::SliceWithStartOffset<'a>,
					offset: usize,
				) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
					::core::result::Result::Ok(Self(::planus::table_reader::Table::from_buffer(buffer, offset)?))
				}
			}

			impl<'a> ::planus::VectorReadInner<'a> for TrafficRef<'a> {
				type Error = ::planus::Error;
				const STRIDE: usize = 4;

				unsafe fn from_buffer(
					buffer: ::planus::SliceWithStartOffset<'a>,
					offset: usize,
				) -> ::planus::Result<Self> {
					::planus::TableRead::from_buffer(buffer, offset).map_err(|error_kind| {
						error_kind.with_error_location("[TrafficRef]", "get", buffer.offset_from_start)
					})
				}
			}

			/// # Safety
			/// The planus compiler generates implementations that initialize
			/// the bytes in `write_values`.
			unsafe impl ::planus::VectorWrite<::planus::Offset<Traffic>> for Traffic {
				type Value = ::planus::Offset<Traffic>;
				const STRIDE: usize = 4;
				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> Self::Value {
					::planus::WriteAs::prepare(self, builder)
				}

				#[inline]
				unsafe fn write_values(
					values: &[::planus::Offset<Traffic>],
					bytes: *mut ::core::mem::MaybeUninit<u8>,
					buffer_position: u32,
				) {
					let bytes = bytes as *mut [::core::mem::MaybeUninit<u8>; 4];
					for (i, v) in ::core::iter::Iterator::enumerate(values.iter()) {
						::planus::WriteAsPrimitive::write(
							v,
							::planus::Cursor::new(unsafe { &mut *bytes.add(i) }),
							buffer_position - (Self::STRIDE * i) as u32,
						);
					}
				}
			}

			impl<'a> ::planus::ReadAsRoot<'a> for TrafficRef<'a> {
				fn read_as_root(slice: &'a [u8]) -> ::planus::Result<Self> {
					::planus::TableRead::from_buffer(
						::planus::SliceWithStartOffset {
							buffer: slice,
							offset_from_start: 0,
						},
						0,
					)
					.map_err(|error_kind| error_kind.with_error_location("[TrafficRef]", "read_as_root", 0))
				}
			}

			///  Cumulative connects and disconnects for one auth root on one tier.
			///
			/// Generated from these locations:
			/// * Table `Presence` in the file `stats.fbs:45`
			#[derive(Clone, Debug, PartialEq, PartialOrd, Eq, Ord, Hash, ::serde::Serialize, ::serde::Deserialize)]
			pub struct Presence {
				/// The field `sessions_started` in the table `Presence`
				pub sessions_started: u64,
				/// The field `sessions_ended` in the table `Presence`
				pub sessions_ended: u64,
			}

			#[allow(clippy::derivable_impls)]
			impl ::core::default::Default for Presence {
				fn default() -> Self {
					Self {
						sessions_started: 0,
						sessions_ended: 0,
					}
				}
			}

			impl Presence {
				/// Creates a [PresenceBuilder] for serializing an instance of this table.
				#[inline]
				pub fn builder() -> PresenceBuilder<()> {
					PresenceBuilder(())
				}

				#[allow(clippy::too_many_arguments)]
				pub fn create(
					builder: &mut ::planus::Builder,
					field_sessions_started: impl ::planus::WriteAsDefault<u64, u64>,
					field_sessions_ended: impl ::planus::WriteAsDefault<u64, u64>,
				) -> ::planus::Offset<Self> {
					let prepared_sessions_started = field_sessions_started.prepare(builder, &0);
					let prepared_sessions_ended = field_sessions_ended.prepare(builder, &0);

					let mut table_writer: ::planus::table_writer::TableWriter<8> = ::core::default::Default::default();
					if prepared_sessions_started.is_some() {
						table_writer.write_entry::<u64>(0);
					}
					if prepared_sessions_ended.is_some() {
						table_writer.write_entry::<u64>(1);
					}

					unsafe {
						table_writer.finish(builder, |object_writer| {
							if let ::core::option::Option::Some(prepared_sessions_started) = prepared_sessions_started {
								object_writer.write::<_, _, 8>(&prepared_sessions_started);
							}
							if let ::core::option::Option::Some(prepared_sessions_ended) = prepared_sessions_ended {
								object_writer.write::<_, _, 8>(&prepared_sessions_ended);
							}
						});
					}
					builder.current_offset()
				}
			}

			impl ::planus::WriteAs<::planus::Offset<Presence>> for Presence {
				type Prepared = ::planus::Offset<Self>;

				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Presence> {
					::planus::WriteAsOffset::prepare(self, builder)
				}
			}

			impl ::planus::WriteAsOptional<::planus::Offset<Presence>> for Presence {
				type Prepared = ::planus::Offset<Self>;

				#[inline]
				fn prepare(
					&self,
					builder: &mut ::planus::Builder,
				) -> ::core::option::Option<::planus::Offset<Presence>> {
					::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
				}
			}

			impl ::planus::WriteAsOffset<Presence> for Presence {
				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Presence> {
					Presence::create(builder, self.sessions_started, self.sessions_ended)
				}
			}

			/// Builder for serializing an instance of the [Presence] type.
			///
			/// Can be created using the [Presence::builder] method.
			#[derive(Debug)]
			#[must_use]
			pub struct PresenceBuilder<State>(State);

			impl PresenceBuilder<()> {
				/// Setter for the [`sessions_started` field](Presence#structfield.sessions_started).
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn sessions_started<T0>(self, value: T0) -> PresenceBuilder<(T0,)>
				where
					T0: ::planus::WriteAsDefault<u64, u64>,
				{
					PresenceBuilder((value,))
				}

				/// Sets the [`sessions_started` field](Presence#structfield.sessions_started) to the default value.
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn sessions_started_as_default(self) -> PresenceBuilder<(::planus::DefaultValue,)> {
					self.sessions_started(::planus::DefaultValue)
				}
			}

			impl<T0> PresenceBuilder<(T0,)> {
				/// Setter for the [`sessions_ended` field](Presence#structfield.sessions_ended).
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn sessions_ended<T1>(self, value: T1) -> PresenceBuilder<(T0, T1)>
				where
					T1: ::planus::WriteAsDefault<u64, u64>,
				{
					let (v0,) = self.0;
					PresenceBuilder((v0, value))
				}

				/// Sets the [`sessions_ended` field](Presence#structfield.sessions_ended) to the default value.
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn sessions_ended_as_default(self) -> PresenceBuilder<(T0, ::planus::DefaultValue)> {
					self.sessions_ended(::planus::DefaultValue)
				}
			}

			impl<T0, T1> PresenceBuilder<(T0, T1)> {
				/// Finish writing the builder to get an [Offset](::planus::Offset) to a serialized [Presence].
				#[inline]
				pub fn finish(self, builder: &mut ::planus::Builder) -> ::planus::Offset<Presence>
				where
					Self: ::planus::WriteAsOffset<Presence>,
				{
					::planus::WriteAsOffset::prepare(&self, builder)
				}
			}

			impl<T0: ::planus::WriteAsDefault<u64, u64>, T1: ::planus::WriteAsDefault<u64, u64>>
				::planus::WriteAs<::planus::Offset<Presence>> for PresenceBuilder<(T0, T1)>
			{
				type Prepared = ::planus::Offset<Presence>;

				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Presence> {
					::planus::WriteAsOffset::prepare(self, builder)
				}
			}

			impl<T0: ::planus::WriteAsDefault<u64, u64>, T1: ::planus::WriteAsDefault<u64, u64>>
				::planus::WriteAsOptional<::planus::Offset<Presence>> for PresenceBuilder<(T0, T1)>
			{
				type Prepared = ::planus::Offset<Presence>;

				#[inline]
				fn prepare(
					&self,
					builder: &mut ::planus::Builder,
				) -> ::core::option::Option<::planus::Offset<Presence>> {
					::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
				}
			}

			impl<T0: ::planus::WriteAsDefault<u64, u64>, T1: ::planus::WriteAsDefault<u64, u64>>
				::planus::WriteAsOffset<Presence> for PresenceBuilder<(T0, T1)>
			{
				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<Presence> {
					let (v0, v1) = &self.0;
					Presence::create(builder, v0, v1)
				}
			}

			/// Reference to a deserialized [Presence].
			#[derive(Copy, Clone)]
			pub struct PresenceRef<'a>(#[allow(dead_code)] ::planus::table_reader::Table<'a>);

			impl<'a> PresenceRef<'a> {
				/// Getter for the [`sessions_started` field](Presence#structfield.sessions_started).
				#[inline]
				pub fn sessions_started(&self) -> ::planus::Result<u64> {
					::core::result::Result::Ok(self.0.access(0, "Presence", "sessions_started")?.unwrap_or(0))
				}

				/// Getter for the [`sessions_ended` field](Presence#structfield.sessions_ended).
				#[inline]
				pub fn sessions_ended(&self) -> ::planus::Result<u64> {
					::core::result::Result::Ok(self.0.access(1, "Presence", "sessions_ended")?.unwrap_or(0))
				}
			}

			impl<'a> ::core::fmt::Debug for PresenceRef<'a> {
				fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
					let mut f = f.debug_struct("PresenceRef");
					f.field("sessions_started", &self.sessions_started());
					f.field("sessions_ended", &self.sessions_ended());
					f.finish()
				}
			}

			impl<'a> ::core::convert::TryFrom<PresenceRef<'a>> for Presence {
				type Error = ::planus::Error;

				#[allow(unreachable_code)]
				fn try_from(value: PresenceRef<'a>) -> ::planus::Result<Self> {
					::core::result::Result::Ok(Self {
						sessions_started: ::core::convert::TryInto::try_into(value.sessions_started()?)?,
						sessions_ended: ::core::convert::TryInto::try_into(value.sessions_ended()?)?,
					})
				}
			}

			impl<'a> ::planus::TableRead<'a> for PresenceRef<'a> {
				#[inline]
				fn from_buffer(
					buffer: ::planus::SliceWithStartOffset<'a>,
					offset: usize,
				) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
					::core::result::Result::Ok(Self(::planus::table_reader::Table::from_buffer(buffer, offset)?))
				}
			}

			impl<'a> ::planus::VectorReadInner<'a> for PresenceRef<'a> {
				type Error = ::planus::Error;
				const STRIDE: usize = 4;

				unsafe fn from_buffer(
					buffer: ::planus::SliceWithStartOffset<'a>,
					offset: usize,
				) -> ::planus::Result<Self> {
					::planus::TableRead::from_buffer(buffer, offset).map_err(|error_kind| {
						error_kind.with_error_location("[PresenceRef]", "get", buffer.offset_from_start)
					})
				}
			}

			/// # Safety
			/// The planus compiler generates implementations that initialize
			/// the bytes in `write_values`.
			unsafe impl ::planus::VectorWrite<::planus::Offset<Presence>> for Presence {
				type Value = ::planus::Offset<Presence>;
				const STRIDE: usize = 4;
				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> Self::Value {
					::planus::WriteAs::prepare(self, builder)
				}

				#[inline]
				unsafe fn write_values(
					values: &[::planus::Offset<Presence>],
					bytes: *mut ::core::mem::MaybeUninit<u8>,
					buffer_position: u32,
				) {
					let bytes = bytes as *mut [::core::mem::MaybeUninit<u8>; 4];
					for (i, v) in ::core::iter::Iterator::enumerate(values.iter()) {
						::planus::WriteAsPrimitive::write(
							v,
							::planus::Cursor::new(unsafe { &mut *bytes.add(i) }),
							buffer_position - (Self::STRIDE * i) as u32,
						);
					}
				}
			}

			impl<'a> ::planus::ReadAsRoot<'a> for PresenceRef<'a> {
				fn read_as_root(slice: &'a [u8]) -> ::planus::Result<Self> {
					::planus::TableRead::from_buffer(
						::planus::SliceWithStartOffset {
							buffer: slice,
							offset_from_start: 0,
						},
						0,
					)
					.map_err(|error_kind| error_kind.with_error_location("[PresenceRef]", "read_as_root", 0))
				}
			}

			///  One broadcast's counters, keyed by its path.
			///
			/// Generated from these locations:
			/// * Table `TrafficEntry` in the file `stats.fbs:51`
			#[derive(Clone, Debug, PartialEq, PartialOrd, Eq, Ord, Hash, ::serde::Serialize, ::serde::Deserialize)]
			pub struct TrafficEntry {
				/// The field `path` in the table `TrafficEntry`
				pub path: ::planus::alloc::string::String,
				/// The field `traffic` in the table `TrafficEntry`
				pub traffic: ::planus::alloc::boxed::Box<self::Traffic>,
			}

			#[allow(clippy::derivable_impls)]
			impl ::core::default::Default for TrafficEntry {
				fn default() -> Self {
					Self {
						path: ::core::default::Default::default(),
						traffic: ::core::default::Default::default(),
					}
				}
			}

			impl TrafficEntry {
				/// Creates a [TrafficEntryBuilder] for serializing an instance of this table.
				#[inline]
				pub fn builder() -> TrafficEntryBuilder<()> {
					TrafficEntryBuilder(())
				}

				#[allow(clippy::too_many_arguments)]
				pub fn create(
					builder: &mut ::planus::Builder,
					field_path: impl ::planus::WriteAs<::planus::Offset<str>>,
					field_traffic: impl ::planus::WriteAs<::planus::Offset<self::Traffic>>,
				) -> ::planus::Offset<Self> {
					let prepared_path = field_path.prepare(builder);
					let prepared_traffic = field_traffic.prepare(builder);

					let mut table_writer: ::planus::table_writer::TableWriter<8> = ::core::default::Default::default();
					table_writer.write_entry::<::planus::Offset<str>>(0);
					table_writer.write_entry::<::planus::Offset<self::Traffic>>(1);

					unsafe {
						table_writer.finish(builder, |object_writer| {
							object_writer.write::<_, _, 4>(&prepared_path);
							object_writer.write::<_, _, 4>(&prepared_traffic);
						});
					}
					builder.current_offset()
				}
			}

			impl ::planus::WriteAs<::planus::Offset<TrafficEntry>> for TrafficEntry {
				type Prepared = ::planus::Offset<Self>;

				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<TrafficEntry> {
					::planus::WriteAsOffset::prepare(self, builder)
				}
			}

			impl ::planus::WriteAsOptional<::planus::Offset<TrafficEntry>> for TrafficEntry {
				type Prepared = ::planus::Offset<Self>;

				#[inline]
				fn prepare(
					&self,
					builder: &mut ::planus::Builder,
				) -> ::core::option::Option<::planus::Offset<TrafficEntry>> {
					::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
				}
			}

			impl ::planus::WriteAsOffset<TrafficEntry> for TrafficEntry {
				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<TrafficEntry> {
					TrafficEntry::create(builder, &self.path, &self.traffic)
				}
			}

			/// Builder for serializing an instance of the [TrafficEntry] type.
			///
			/// Can be created using the [TrafficEntry::builder] method.
			#[derive(Debug)]
			#[must_use]
			pub struct TrafficEntryBuilder<State>(State);

			impl TrafficEntryBuilder<()> {
				/// Setter for the [`path` field](TrafficEntry#structfield.path).
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn path<T0>(self, value: T0) -> TrafficEntryBuilder<(T0,)>
				where
					T0: ::planus::WriteAs<::planus::Offset<str>>,
				{
					TrafficEntryBuilder((value,))
				}
			}

			impl<T0> TrafficEntryBuilder<(T0,)> {
				/// Setter for the [`traffic` field](TrafficEntry#structfield.traffic).
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn traffic<T1>(self, value: T1) -> TrafficEntryBuilder<(T0, T1)>
				where
					T1: ::planus::WriteAs<::planus::Offset<self::Traffic>>,
				{
					let (v0,) = self.0;
					TrafficEntryBuilder((v0, value))
				}
			}

			impl<T0, T1> TrafficEntryBuilder<(T0, T1)> {
				/// Finish writing the builder to get an [Offset](::planus::Offset) to a serialized [TrafficEntry].
				#[inline]
				pub fn finish(self, builder: &mut ::planus::Builder) -> ::planus::Offset<TrafficEntry>
				where
					Self: ::planus::WriteAsOffset<TrafficEntry>,
				{
					::planus::WriteAsOffset::prepare(&self, builder)
				}
			}

			impl<
					T0: ::planus::WriteAs<::planus::Offset<str>>,
					T1: ::planus::WriteAs<::planus::Offset<self::Traffic>>,
				> ::planus::WriteAs<::planus::Offset<TrafficEntry>> for TrafficEntryBuilder<(T0, T1)>
			{
				type Prepared = ::planus::Offset<TrafficEntry>;

				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<TrafficEntry> {
					::planus::WriteAsOffset::prepare(self, builder)
				}
			}

			impl<
					T0: ::planus::WriteAs<::planus::Offset<str>>,
					T1: ::planus::WriteAs<::planus::Offset<self::Traffic>>,
				> ::planus::WriteAsOptional<::planus::Offset<TrafficEntry>> for TrafficEntryBuilder<(T0, T1)>
			{
				type Prepared = ::planus::Offset<TrafficEntry>;

				#[inline]
				fn prepare(
					&self,
					builder: &mut ::planus::Builder,
				) -> ::core::option::Option<::planus::Offset<TrafficEntry>> {
					::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
				}
			}

			impl<
					T0: ::planus::WriteAs<::planus::Offset<str>>,
					T1: ::planus::WriteAs<::planus::Offset<self::Traffic>>,
				> ::planus::WriteAsOffset<TrafficEntry> for TrafficEntryBuilder<(T0, T1)>
			{
				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<TrafficEntry> {
					let (v0, v1) = &self.0;
					TrafficEntry::create(builder, v0, v1)
				}
			}

			/// Reference to a deserialized [TrafficEntry].
			#[derive(Copy, Clone)]
			pub struct TrafficEntryRef<'a>(#[allow(dead_code)] ::planus::table_reader::Table<'a>);

			impl<'a> TrafficEntryRef<'a> {
				/// Getter for the [`path` field](TrafficEntry#structfield.path).
				#[inline]
				pub fn path(&self) -> ::planus::Result<&'a ::core::primitive::str> {
					self.0.access_required(0, "TrafficEntry", "path")
				}

				/// Getter for the [`traffic` field](TrafficEntry#structfield.traffic).
				#[inline]
				pub fn traffic(&self) -> ::planus::Result<self::TrafficRef<'a>> {
					self.0.access_required(1, "TrafficEntry", "traffic")
				}
			}

			impl<'a> ::core::fmt::Debug for TrafficEntryRef<'a> {
				fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
					let mut f = f.debug_struct("TrafficEntryRef");
					f.field("path", &self.path());
					f.field("traffic", &self.traffic());
					f.finish()
				}
			}

			impl<'a> ::core::convert::TryFrom<TrafficEntryRef<'a>> for TrafficEntry {
				type Error = ::planus::Error;

				#[allow(unreachable_code)]
				fn try_from(value: TrafficEntryRef<'a>) -> ::planus::Result<Self> {
					::core::result::Result::Ok(Self {
						path: ::core::convert::Into::into(value.path()?),
						traffic: ::planus::alloc::boxed::Box::new(::core::convert::TryInto::try_into(
							value.traffic()?,
						)?),
					})
				}
			}

			impl<'a> ::planus::TableRead<'a> for TrafficEntryRef<'a> {
				#[inline]
				fn from_buffer(
					buffer: ::planus::SliceWithStartOffset<'a>,
					offset: usize,
				) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
					::core::result::Result::Ok(Self(::planus::table_reader::Table::from_buffer(buffer, offset)?))
				}
			}

			impl<'a> ::planus::VectorReadInner<'a> for TrafficEntryRef<'a> {
				type Error = ::planus::Error;
				const STRIDE: usize = 4;

				unsafe fn from_buffer(
					buffer: ::planus::SliceWithStartOffset<'a>,
					offset: usize,
				) -> ::planus::Result<Self> {
					::planus::TableRead::from_buffer(buffer, offset).map_err(|error_kind| {
						error_kind.with_error_location("[TrafficEntryRef]", "get", buffer.offset_from_start)
					})
				}
			}

			/// # Safety
			/// The planus compiler generates implementations that initialize
			/// the bytes in `write_values`.
			unsafe impl ::planus::VectorWrite<::planus::Offset<TrafficEntry>> for TrafficEntry {
				type Value = ::planus::Offset<TrafficEntry>;
				const STRIDE: usize = 4;
				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> Self::Value {
					::planus::WriteAs::prepare(self, builder)
				}

				#[inline]
				unsafe fn write_values(
					values: &[::planus::Offset<TrafficEntry>],
					bytes: *mut ::core::mem::MaybeUninit<u8>,
					buffer_position: u32,
				) {
					let bytes = bytes as *mut [::core::mem::MaybeUninit<u8>; 4];
					for (i, v) in ::core::iter::Iterator::enumerate(values.iter()) {
						::planus::WriteAsPrimitive::write(
							v,
							::planus::Cursor::new(unsafe { &mut *bytes.add(i) }),
							buffer_position - (Self::STRIDE * i) as u32,
						);
					}
				}
			}

			impl<'a> ::planus::ReadAsRoot<'a> for TrafficEntryRef<'a> {
				fn read_as_root(slice: &'a [u8]) -> ::planus::Result<Self> {
					::planus::TableRead::from_buffer(
						::planus::SliceWithStartOffset {
							buffer: slice,
							offset_from_start: 0,
						},
						0,
					)
					.map_err(|error_kind| error_kind.with_error_location("[TrafficEntryRef]", "read_as_root", 0))
				}
			}

			///  One auth root's session gauge, keyed by the root.
			///
			/// Generated from these locations:
			/// * Table `SessionsEntry` in the file `stats.fbs:57`
			#[derive(Clone, Debug, PartialEq, PartialOrd, Eq, Ord, Hash, ::serde::Serialize, ::serde::Deserialize)]
			pub struct SessionsEntry {
				/// The field `root` in the table `SessionsEntry`
				pub root: ::planus::alloc::string::String,
				/// The field `presence` in the table `SessionsEntry`
				pub presence: ::planus::alloc::boxed::Box<self::Presence>,
			}

			#[allow(clippy::derivable_impls)]
			impl ::core::default::Default for SessionsEntry {
				fn default() -> Self {
					Self {
						root: ::core::default::Default::default(),
						presence: ::core::default::Default::default(),
					}
				}
			}

			impl SessionsEntry {
				/// Creates a [SessionsEntryBuilder] for serializing an instance of this table.
				#[inline]
				pub fn builder() -> SessionsEntryBuilder<()> {
					SessionsEntryBuilder(())
				}

				#[allow(clippy::too_many_arguments)]
				pub fn create(
					builder: &mut ::planus::Builder,
					field_root: impl ::planus::WriteAs<::planus::Offset<str>>,
					field_presence: impl ::planus::WriteAs<::planus::Offset<self::Presence>>,
				) -> ::planus::Offset<Self> {
					let prepared_root = field_root.prepare(builder);
					let prepared_presence = field_presence.prepare(builder);

					let mut table_writer: ::planus::table_writer::TableWriter<8> = ::core::default::Default::default();
					table_writer.write_entry::<::planus::Offset<str>>(0);
					table_writer.write_entry::<::planus::Offset<self::Presence>>(1);

					unsafe {
						table_writer.finish(builder, |object_writer| {
							object_writer.write::<_, _, 4>(&prepared_root);
							object_writer.write::<_, _, 4>(&prepared_presence);
						});
					}
					builder.current_offset()
				}
			}

			impl ::planus::WriteAs<::planus::Offset<SessionsEntry>> for SessionsEntry {
				type Prepared = ::planus::Offset<Self>;

				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<SessionsEntry> {
					::planus::WriteAsOffset::prepare(self, builder)
				}
			}

			impl ::planus::WriteAsOptional<::planus::Offset<SessionsEntry>> for SessionsEntry {
				type Prepared = ::planus::Offset<Self>;

				#[inline]
				fn prepare(
					&self,
					builder: &mut ::planus::Builder,
				) -> ::core::option::Option<::planus::Offset<SessionsEntry>> {
					::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
				}
			}

			impl ::planus::WriteAsOffset<SessionsEntry> for SessionsEntry {
				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<SessionsEntry> {
					SessionsEntry::create(builder, &self.root, &self.presence)
				}
			}

			/// Builder for serializing an instance of the [SessionsEntry] type.
			///
			/// Can be created using the [SessionsEntry::builder] method.
			#[derive(Debug)]
			#[must_use]
			pub struct SessionsEntryBuilder<State>(State);

			impl SessionsEntryBuilder<()> {
				/// Setter for the [`root` field](SessionsEntry#structfield.root).
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn root<T0>(self, value: T0) -> SessionsEntryBuilder<(T0,)>
				where
					T0: ::planus::WriteAs<::planus::Offset<str>>,
				{
					SessionsEntryBuilder((value,))
				}
			}

			impl<T0> SessionsEntryBuilder<(T0,)> {
				/// Setter for the [`presence` field](SessionsEntry#structfield.presence).
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn presence<T1>(self, value: T1) -> SessionsEntryBuilder<(T0, T1)>
				where
					T1: ::planus::WriteAs<::planus::Offset<self::Presence>>,
				{
					let (v0,) = self.0;
					SessionsEntryBuilder((v0, value))
				}
			}

			impl<T0, T1> SessionsEntryBuilder<(T0, T1)> {
				/// Finish writing the builder to get an [Offset](::planus::Offset) to a serialized [SessionsEntry].
				#[inline]
				pub fn finish(self, builder: &mut ::planus::Builder) -> ::planus::Offset<SessionsEntry>
				where
					Self: ::planus::WriteAsOffset<SessionsEntry>,
				{
					::planus::WriteAsOffset::prepare(&self, builder)
				}
			}

			impl<
					T0: ::planus::WriteAs<::planus::Offset<str>>,
					T1: ::planus::WriteAs<::planus::Offset<self::Presence>>,
				> ::planus::WriteAs<::planus::Offset<SessionsEntry>> for SessionsEntryBuilder<(T0, T1)>
			{
				type Prepared = ::planus::Offset<SessionsEntry>;

				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<SessionsEntry> {
					::planus::WriteAsOffset::prepare(self, builder)
				}
			}

			impl<
					T0: ::planus::WriteAs<::planus::Offset<str>>,
					T1: ::planus::WriteAs<::planus::Offset<self::Presence>>,
				> ::planus::WriteAsOptional<::planus::Offset<SessionsEntry>> for SessionsEntryBuilder<(T0, T1)>
			{
				type Prepared = ::planus::Offset<SessionsEntry>;

				#[inline]
				fn prepare(
					&self,
					builder: &mut ::planus::Builder,
				) -> ::core::option::Option<::planus::Offset<SessionsEntry>> {
					::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
				}
			}

			impl<
					T0: ::planus::WriteAs<::planus::Offset<str>>,
					T1: ::planus::WriteAs<::planus::Offset<self::Presence>>,
				> ::planus::WriteAsOffset<SessionsEntry> for SessionsEntryBuilder<(T0, T1)>
			{
				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<SessionsEntry> {
					let (v0, v1) = &self.0;
					SessionsEntry::create(builder, v0, v1)
				}
			}

			/// Reference to a deserialized [SessionsEntry].
			#[derive(Copy, Clone)]
			pub struct SessionsEntryRef<'a>(#[allow(dead_code)] ::planus::table_reader::Table<'a>);

			impl<'a> SessionsEntryRef<'a> {
				/// Getter for the [`root` field](SessionsEntry#structfield.root).
				#[inline]
				pub fn root(&self) -> ::planus::Result<&'a ::core::primitive::str> {
					self.0.access_required(0, "SessionsEntry", "root")
				}

				/// Getter for the [`presence` field](SessionsEntry#structfield.presence).
				#[inline]
				pub fn presence(&self) -> ::planus::Result<self::PresenceRef<'a>> {
					self.0.access_required(1, "SessionsEntry", "presence")
				}
			}

			impl<'a> ::core::fmt::Debug for SessionsEntryRef<'a> {
				fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
					let mut f = f.debug_struct("SessionsEntryRef");
					f.field("root", &self.root());
					f.field("presence", &self.presence());
					f.finish()
				}
			}

			impl<'a> ::core::convert::TryFrom<SessionsEntryRef<'a>> for SessionsEntry {
				type Error = ::planus::Error;

				#[allow(unreachable_code)]
				fn try_from(value: SessionsEntryRef<'a>) -> ::planus::Result<Self> {
					::core::result::Result::Ok(Self {
						root: ::core::convert::Into::into(value.root()?),
						presence: ::planus::alloc::boxed::Box::new(::core::convert::TryInto::try_into(
							value.presence()?,
						)?),
					})
				}
			}

			impl<'a> ::planus::TableRead<'a> for SessionsEntryRef<'a> {
				#[inline]
				fn from_buffer(
					buffer: ::planus::SliceWithStartOffset<'a>,
					offset: usize,
				) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
					::core::result::Result::Ok(Self(::planus::table_reader::Table::from_buffer(buffer, offset)?))
				}
			}

			impl<'a> ::planus::VectorReadInner<'a> for SessionsEntryRef<'a> {
				type Error = ::planus::Error;
				const STRIDE: usize = 4;

				unsafe fn from_buffer(
					buffer: ::planus::SliceWithStartOffset<'a>,
					offset: usize,
				) -> ::planus::Result<Self> {
					::planus::TableRead::from_buffer(buffer, offset).map_err(|error_kind| {
						error_kind.with_error_location("[SessionsEntryRef]", "get", buffer.offset_from_start)
					})
				}
			}

			/// # Safety
			/// The planus compiler generates implementations that initialize
			/// the bytes in `write_values`.
			unsafe impl ::planus::VectorWrite<::planus::Offset<SessionsEntry>> for SessionsEntry {
				type Value = ::planus::Offset<SessionsEntry>;
				const STRIDE: usize = 4;
				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> Self::Value {
					::planus::WriteAs::prepare(self, builder)
				}

				#[inline]
				unsafe fn write_values(
					values: &[::planus::Offset<SessionsEntry>],
					bytes: *mut ::core::mem::MaybeUninit<u8>,
					buffer_position: u32,
				) {
					let bytes = bytes as *mut [::core::mem::MaybeUninit<u8>; 4];
					for (i, v) in ::core::iter::Iterator::enumerate(values.iter()) {
						::planus::WriteAsPrimitive::write(
							v,
							::planus::Cursor::new(unsafe { &mut *bytes.add(i) }),
							buffer_position - (Self::STRIDE * i) as u32,
						);
					}
				}
			}

			impl<'a> ::planus::ReadAsRoot<'a> for SessionsEntryRef<'a> {
				fn read_as_root(slice: &'a [u8]) -> ::planus::Result<Self> {
					::planus::TableRead::from_buffer(
						::planus::SliceWithStartOffset {
							buffer: slice,
							offset_from_start: 0,
						},
						0,
					)
					.map_err(|error_kind| error_kind.with_error_location("[SessionsEntryRef]", "read_as_root", 0))
				}
			}

			///  A frame of a publisher or subscriber track, sorted by path.
			///
			/// Generated from these locations:
			/// * Table `TrafficFrame` in the file `stats.fbs:63`
			#[derive(Clone, Debug, PartialEq, PartialOrd, Eq, Ord, Hash, ::serde::Serialize, ::serde::Deserialize)]
			pub struct TrafficFrame {
				/// The field `entries` in the table `TrafficFrame`
				pub entries: ::planus::alloc::vec::Vec<self::TrafficEntry>,
			}

			#[allow(clippy::derivable_impls)]
			impl ::core::default::Default for TrafficFrame {
				fn default() -> Self {
					Self {
						entries: ::core::default::Default::default(),
					}
				}
			}

			impl TrafficFrame {
				/// Creates a [TrafficFrameBuilder] for serializing an instance of this table.
				#[inline]
				pub fn builder() -> TrafficFrameBuilder<()> {
					TrafficFrameBuilder(())
				}

				#[allow(clippy::too_many_arguments)]
				pub fn create(
					builder: &mut ::planus::Builder,
					field_entries: impl ::planus::WriteAs<::planus::Offset<[::planus::Offset<self::TrafficEntry>]>>,
				) -> ::planus::Offset<Self> {
					let prepared_entries = field_entries.prepare(builder);

					let mut table_writer: ::planus::table_writer::TableWriter<6> = ::core::default::Default::default();
					table_writer.write_entry::<::planus::Offset<[::planus::Offset<self::TrafficEntry>]>>(0);

					unsafe {
						table_writer.finish(builder, |object_writer| {
							object_writer.write::<_, _, 4>(&prepared_entries);
						});
					}
					builder.current_offset()
				}
			}

			impl ::planus::WriteAs<::planus::Offset<TrafficFrame>> for TrafficFrame {
				type Prepared = ::planus::Offset<Self>;

				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<TrafficFrame> {
					::planus::WriteAsOffset::prepare(self, builder)
				}
			}

			impl ::planus::WriteAsOptional<::planus::Offset<TrafficFrame>> for TrafficFrame {
				type Prepared = ::planus::Offset<Self>;

				#[inline]
				fn prepare(
					&self,
					builder: &mut ::planus::Builder,
				) -> ::core::option::Option<::planus::Offset<TrafficFrame>> {
					::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
				}
			}

			impl ::planus::WriteAsOffset<TrafficFrame> for TrafficFrame {
				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<TrafficFrame> {
					TrafficFrame::create(builder, &self.entries)
				}
			}

			/// Builder for serializing an instance of the [TrafficFrame] type.
			///
			/// Can be created using the [TrafficFrame::builder] method.
			#[derive(Debug)]
			#[must_use]
			pub struct TrafficFrameBuilder<State>(State);

			impl TrafficFrameBuilder<()> {
				/// Setter for the [`entries` field](TrafficFrame#structfield.entries).
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn entries<T0>(self, value: T0) -> TrafficFrameBuilder<(T0,)>
				where
					T0: ::planus::WriteAs<::planus::Offset<[::planus::Offset<self::TrafficEntry>]>>,
				{
					TrafficFrameBuilder((value,))
				}
			}

			impl<T0> TrafficFrameBuilder<(T0,)> {
				/// Finish writing the builder to get an [Offset](::planus::Offset) to a serialized [TrafficFrame].
				#[inline]
				pub fn finish(self, builder: &mut ::planus::Builder) -> ::planus::Offset<TrafficFrame>
				where
					Self: ::planus::WriteAsOffset<TrafficFrame>,
				{
					::planus::WriteAsOffset::prepare(&self, builder)
				}
			}

			impl<T0: ::planus::WriteAs<::planus::Offset<[::planus::Offset<self::TrafficEntry>]>>>
				::planus::WriteAs<::planus::Offset<TrafficFrame>> for TrafficFrameBuilder<(T0,)>
			{
				type Prepared = ::planus::Offset<TrafficFrame>;

				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<TrafficFrame> {
					::planus::WriteAsOffset::prepare(self, builder)
				}
			}

			impl<T0: ::planus::WriteAs<::planus::Offset<[::planus::Offset<self::TrafficEntry>]>>>
				::planus::WriteAsOptional<::planus::Offset<TrafficFrame>> for TrafficFrameBuilder<(T0,)>
			{
				type Prepared = ::planus::Offset<TrafficFrame>;

				#[inline]
				fn prepare(
					&self,
					builder: &mut ::planus::Builder,
				) -> ::core::option::Option<::planus::Offset<TrafficFrame>> {
					::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
				}
			}

			impl<T0: ::planus::WriteAs<::planus::Offset<[::planus::Offset<self::TrafficEntry>]>>>
				::planus::WriteAsOffset<TrafficFrame> for TrafficFrameBuilder<(T0,)>
			{
				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<TrafficFrame> {
					let (v0,) = &self.0;
					TrafficFrame::create(builder, v0)
				}
			}

			/// Reference to a deserialized [TrafficFrame].
			#[derive(Copy, Clone)]
			pub struct TrafficFrameRef<'a>(#[allow(dead_code)] ::planus::table_reader::Table<'a>);

			impl<'a> TrafficFrameRef<'a> {
				/// Getter for the [`entries` field](TrafficFrame#structfield.entries).
				#[inline]
				pub fn entries(
					&self,
				) -> ::planus::Result<::planus::Vector<'a, ::planus::Result<self::TrafficEntryRef<'a>>>> {
					self.0.access_required(0, "TrafficFrame", "entries")
				}
			}

			impl<'a> ::core::fmt::Debug for TrafficFrameRef<'a> {
				fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
					let mut f = f.debug_struct("TrafficFrameRef");
					f.field("entries", &self.entries());
					f.finish()
				}
			}

			impl<'a> ::core::convert::TryFrom<TrafficFrameRef<'a>> for TrafficFrame {
				type Error = ::planus::Error;

				#[allow(unreachable_code)]
				fn try_from(value: TrafficFrameRef<'a>) -> ::planus::Result<Self> {
					::core::result::Result::Ok(Self {
						entries: value.entries()?.to_vec_result()?,
					})
				}
			}

			impl<'a> ::planus::TableRead<'a> for TrafficFrameRef<'a> {
				#[inline]
				fn from_buffer(
					buffer: ::planus::SliceWithStartOffset<'a>,
					offset: usize,
				) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
					::core::result::Result::Ok(Self(::planus::table_reader::Table::from_buffer(buffer, offset)?))
				}
			}

			impl<'a> ::planus::VectorReadInner<'a> for TrafficFrameRef<'a> {
				type Error = ::planus::Error;
				const STRIDE: usize = 4;

				unsafe fn from_buffer(
					buffer: ::planus::SliceWithStartOffset<'a>,
					offset: usize,
				) -> ::planus::Result<Self> {
					::planus::TableRead::from_buffer(buffer, offset).map_err(|error_kind| {
						error_kind.with_error_location("[TrafficFrameRef]", "get", buffer.offset_from_start)
					})
				}
			}

			/// # Safety
			/// The planus compiler generates implementations that initialize
			/// the bytes in `write_values`.
			unsafe impl ::planus::VectorWrite<::planus::Offset<TrafficFrame>> for TrafficFrame {
				type Value = ::planus::Offset<TrafficFrame>;
				const STRIDE: usize = 4;
				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> Self::Value {
					::planus::WriteAs::prepare(self, builder)
				}

				#[inline]
				unsafe fn write_values(
					values: &[::planus::Offset<TrafficFrame>],
					bytes: *mut ::core::mem::MaybeUninit<u8>,
					buffer_position: u32,
				) {
					let bytes = bytes as *mut [::core::mem::MaybeUninit<u8>; 4];
					for (i, v) in ::core::iter::Iterator::enumerate(values.iter()) {
						::planus::WriteAsPrimitive::write(
							v,
							::planus::Cursor::new(unsafe { &mut *bytes.add(i) }),
							buffer_position - (Self::STRIDE * i) as u32,
						);
					}
				}
			}

			impl<'a> ::planus::ReadAsRoot<'a> for TrafficFrameRef<'a> {
				fn read_as_root(slice: &'a [u8]) -> ::planus::Result<Self> {
					::planus::TableRead::from_buffer(
						::planus::SliceWithStartOffset {
							buffer: slice,
							offset_from_start: 0,
						},
						0,
					)
					.map_err(|error_kind| error_kind.with_error_location("[TrafficFrameRef]", "read_as_root", 0))
				}
			}

			///  A frame of a sessions track, sorted by root.
			///
			/// Generated from these locations:
			/// * Table `SessionsFrame` in the file `stats.fbs:68`
			#[derive(Clone, Debug, PartialEq, PartialOrd, Eq, Ord, Hash, ::serde::Serialize, ::serde::Deserialize)]
			pub struct SessionsFrame {
				/// The field `entries` in the table `SessionsFrame`
				pub entries: ::planus::alloc::vec::Vec<self::SessionsEntry>,
			}

			#[allow(clippy::derivable_impls)]
			impl ::core::default::Default for SessionsFrame {
				fn default() -> Self {
					Self {
						entries: ::core::default::Default::default(),
					}
				}
			}

			impl SessionsFrame {
				/// Creates a [SessionsFrameBuilder] for serializing an instance of this table.
				#[inline]
				pub fn builder() -> SessionsFrameBuilder<()> {
					SessionsFrameBuilder(())
				}

				#[allow(clippy::too_many_arguments)]
				pub fn create(
					builder: &mut ::planus::Builder,
					field_entries: impl ::planus::WriteAs<::planus::Offset<[::planus::Offset<self::SessionsEntry>]>>,
				) -> ::planus::Offset<Self> {
					let prepared_entries = field_entries.prepare(builder);

					let mut table_writer: ::planus::table_writer::TableWriter<6> = ::core::default::Default::default();
					table_writer.write_entry::<::planus::Offset<[::planus::Offset<self::SessionsEntry>]>>(0);

					unsafe {
						table_writer.finish(builder, |object_writer| {
							object_writer.write::<_, _, 4>(&prepared_entries);
						});
					}
					builder.current_offset()
				}
			}

			impl ::planus::WriteAs<::planus::Offset<SessionsFrame>> for SessionsFrame {
				type Prepared = ::planus::Offset<Self>;

				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<SessionsFrame> {
					::planus::WriteAsOffset::prepare(self, builder)
				}
			}

			impl ::planus::WriteAsOptional<::planus::Offset<SessionsFrame>> for SessionsFrame {
				type Prepared = ::planus::Offset<Self>;

				#[inline]
				fn prepare(
					&self,
					builder: &mut ::planus::Builder,
				) -> ::core::option::Option<::planus::Offset<SessionsFrame>> {
					::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
				}
			}

			impl ::planus::WriteAsOffset<SessionsFrame> for SessionsFrame {
				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<SessionsFrame> {
					SessionsFrame::create(builder, &self.entries)
				}
			}

			/// Builder for serializing an instance of the [SessionsFrame] type.
			///
			/// Can be created using the [SessionsFrame::builder] method.
			#[derive(Debug)]
			#[must_use]
			pub struct SessionsFrameBuilder<State>(State);

			impl SessionsFrameBuilder<()> {
				/// Setter for the [`entries` field](SessionsFrame#structfield.entries).
				#[inline]
				#[allow(clippy::type_complexity)]
				pub fn entries<T0>(self, value: T0) -> SessionsFrameBuilder<(T0,)>
				where
					T0: ::planus::WriteAs<::planus::Offset<[::planus::Offset<self::SessionsEntry>]>>,
				{
					SessionsFrameBuilder((value,))
				}
			}

			impl<T0> SessionsFrameBuilder<(T0,)> {
				/// Finish writing the builder to get an [Offset](::planus::Offset) to a serialized [SessionsFrame].
				#[inline]
				pub fn finish(self, builder: &mut ::planus::Builder) -> ::planus::Offset<SessionsFrame>
				where
					Self: ::planus::WriteAsOffset<SessionsFrame>,
				{
					::planus::WriteAsOffset::prepare(&self, builder)
				}
			}

			impl<T0: ::planus::WriteAs<::planus::Offset<[::planus::Offset<self::SessionsEntry>]>>>
				::planus::WriteAs<::planus::Offset<SessionsFrame>> for SessionsFrameBuilder<(T0,)>
			{
				type Prepared = ::planus::Offset<SessionsFrame>;

				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<SessionsFrame> {
					::planus::WriteAsOffset::prepare(self, builder)
				}
			}

			impl<T0: ::planus::WriteAs<::planus::Offset<[::planus::Offset<self::SessionsEntry>]>>>
				::planus::WriteAsOptional<::planus::Offset<SessionsFrame>> for SessionsFrameBuilder<(T0,)>
			{
				type Prepared = ::planus::Offset<SessionsFrame>;

				#[inline]
				fn prepare(
					&self,
					builder: &mut ::planus::Builder,
				) -> ::core::option::Option<::planus::Offset<SessionsFrame>> {
					::core::option::Option::Some(::planus::WriteAsOffset::prepare(self, builder))
				}
			}

			impl<T0: ::planus::WriteAs<::planus::Offset<[::planus::Offset<self::SessionsEntry>]>>>
				::planus::WriteAsOffset<SessionsFrame> for SessionsFrameBuilder<(T0,)>
			{
				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> ::planus::Offset<SessionsFrame> {
					let (v0,) = &self.0;
					SessionsFrame::create(builder, v0)
				}
			}

			/// Reference to a deserialized [SessionsFrame].
			#[derive(Copy, Clone)]
			pub struct SessionsFrameRef<'a>(#[allow(dead_code)] ::planus::table_reader::Table<'a>);

			impl<'a> SessionsFrameRef<'a> {
				/// Getter for the [`entries` field](SessionsFrame#structfield.entries).
				#[inline]
				pub fn entries(
					&self,
				) -> ::planus::Result<::planus::Vector<'a, ::planus::Result<self::SessionsEntryRef<'a>>>> {
					self.0.access_required(0, "SessionsFrame", "entries")
				}
			}

			impl<'a> ::core::fmt::Debug for SessionsFrameRef<'a> {
				fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
					let mut f = f.debug_struct("SessionsFrameRef");
					f.field("entries", &self.entries());
					f.finish()
				}
			}

			impl<'a> ::core::convert::TryFrom<SessionsFrameRef<'a>> for SessionsFrame {
				type Error = ::planus::Error;

				#[allow(unreachable_code)]
				fn try_from(value: SessionsFrameRef<'a>) -> ::planus::Result<Self> {
					::core::result::Result::Ok(Self {
						entries: value.entries()?.to_vec_result()?,
					})
				}
			}

			impl<'a> ::planus::TableRead<'a> for SessionsFrameRef<'a> {
				#[inline]
				fn from_buffer(
					buffer: ::planus::SliceWithStartOffset<'a>,
					offset: usize,
				) -> ::core::result::Result<Self, ::planus::errors::ErrorKind> {
					::core::result::Result::Ok(Self(::planus::table_reader::Table::from_buffer(buffer, offset)?))
				}
			}

			impl<'a> ::planus::VectorReadInner<'a> for SessionsFrameRef<'a> {
				type Error = ::planus::Error;
				const STRIDE: usize = 4;

				unsafe fn from_buffer(
					buffer: ::planus::SliceWithStartOffset<'a>,
					offset: usize,
				) -> ::planus::Result<Self> {
					::planus::TableRead::from_buffer(buffer, offset).map_err(|error_kind| {
						error_kind.with_error_location("[SessionsFrameRef]", "get", buffer.offset_from_start)
					})
				}
			}

			/// # Safety
			/// The planus compiler generates implementations that initialize
			/// the bytes in `write_values`.
			unsafe impl ::planus::VectorWrite<::planus::Offset<SessionsFrame>> for SessionsFrame {
				type Value = ::planus::Offset<SessionsFrame>;
				const STRIDE: usize = 4;
				#[inline]
				fn prepare(&self, builder: &mut ::planus::Builder) -> Self::Value {
					::planus::WriteAs::prepare(self, builder)
				}

				#[inline]
				unsafe fn write_values(
					values: &[::planus::Offset<SessionsFrame>],
					bytes: *mut ::core::mem::MaybeUninit<u8>,
					buffer_position: u32,
				) {
					let bytes = bytes as *mut [::core::mem::MaybeUninit<u8>; 4];
					for (i, v) in ::core::iter::Iterator::enumerate(values.iter()) {
						::planus::WriteAsPrimitive::write(
							v,
							::planus::Cursor::new(unsafe { &mut *bytes.add(i) }),
							buffer_position - (Self::STRIDE * i) as u32,
						);
					}
				}
			}

			impl<'a> ::planus::ReadAsRoot<'a> for SessionsFrameRef<'a> {
				fn read_as_root(slice: &'a [u8]) -> ::planus::Result<Self> {
					::planus::TableRead::from_buffer(
						::planus::SliceWithStartOffset {
							buffer: slice,
							offset_from_start: 0,
						},
						0,
					)
					.map_err(|error_kind| error_kind.with_error_location("[SessionsFrameRef]", "read_as_root", 0))
				}
			}
		}
	}
}
