/// Why a TS import or export stopped.
///
/// A damaged packet, PES, or access unit is not one: the importer counts it in its
/// [`stats`](super::stats) and carries on.
#[derive(Clone, Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// A PAT listed more than one program, and the import was not told which one to take.
	///
	/// Importing them all onto one broadcast would put unrelated clocks on one timeline, so the
	/// import stops instead. Pick one with [`Import::with_program`](super::Import::with_program), or
	/// import each as its own broadcast with [`Programs`](super::Programs).
	#[error("transport stream carries {} programs ({})", .programs.len(), list(.programs))]
	MultiplePrograms {
		/// The program numbers the PAT lists, in PAT order.
		programs: Vec<u16>,
	},

	/// The PAT does not list the program the import was told to take.
	#[error("transport stream has no program {program}; its PAT lists {}", list(.programs))]
	MissingProgram {
		/// The selected program number.
		program: u16,
		/// The program numbers the PAT lists, in PAT order.
		programs: Vec<u16>,
	},

	/// The source's clock stepped onto another timeline, or its PAT named another program, so
	/// what follows is another broadcast. [`Republish`](super::Republish) carries on into a new
	/// instance instead.
	#[error("the source {0}; what follows is a new broadcast")]
	Restarted(super::Step),

	/// The input ended before it declared anything to publish.
	#[error("input ended before {0}")]
	Empty(&'static str),

	/// The input breaks the MPEG-TS or a codec's rules past what the import or export recovers
	/// from: a TS byte stream for an import, a broadcast's catalog and frames for an export.
	#[error("{0}")]
	Malformed(String),

	/// The input is well-formed but needs what this import or export does not carry, such as a
	/// codec, a layout change, or more buffer or rate than its configuration allows.
	#[error("{0}")]
	Unsupported(String),

	/// Parsing the media or publishing it failed.
	#[error(transparent)]
	Mux(Box<crate::Error>),
}

/// A result whose failure is a TS [`Error`].
pub(super) type Result<T, E = Error> = std::result::Result<T, E>;

fn list(programs: &[u16]) -> String {
	programs.iter().map(u16::to_string).collect::<Vec<_>>().join(", ")
}

impl From<crate::Error> for Error {
	/// A TS error that crossed into [`crate::Error`] comes back out as itself.
	fn from(err: crate::Error) -> Self {
		match err {
			crate::Error::Ts(err) => err,
			err => Self::Mux(Box::new(err)),
		}
	}
}

impl From<super::schedule::Error> for Error {
	fn from(err: super::schedule::Error) -> Self {
		match err {
			super::schedule::Error::Unset => Self::Malformed(err.to_string()),
			err => Self::Unsupported(err.to_string()),
		}
	}
}

// Flattened to its message so `mpeg2ts` stays out of this crate's public API. The export meets it
// only on a value out of a field's range.
impl From<mpeg2ts::Error> for Error {
	fn from(err: mpeg2ts::Error) -> Self {
		Self::Malformed(crate::error::message(err))
	}
}

/// Every error a TS function's `?` meets on its way to [`Error::Mux`].
macro_rules! mux_errors {
	($($ty:ty),* $(,)?) => {
		$(
			impl From<$ty> for Error {
				fn from(err: $ty) -> Self {
					crate::Error::from(err).into()
				}
			}
		)*
	};
}

mux_errors!(
	moq_net::Error,
	moq_net::TimeOverflow,
	hang::Error,
	crate::codec::aac::Error,
	crate::codec::aac::adts::Error,
	crate::codec::annexb::Error,
	crate::codec::h264::Error,
	crate::codec::h265::Error,
	crate::codec::legacy::Error,
	crate::codec::opus::Error,
	crate::container::MissingKeyframe,
	crate::container::TimestampRewind,
	crate::jitter::Error,
);

/// [`Error::Malformed`], formatted.
macro_rules! malformed {
	($($arg:tt)*) => {
		$crate::container::ts::Error::Malformed(format!($($arg)*))
	};
}

/// [`Error::Unsupported`], formatted.
macro_rules! unsupported {
	($($arg:tt)*) => {
		$crate::container::ts::Error::Unsupported(format!($($arg)*))
	};
}

/// Return [`Error::Malformed`], formatted.
macro_rules! bail {
	($($arg:tt)*) => {
		return Err($crate::container::ts::error::malformed!($($arg)*).into())
	};
}

/// Return [`Error::Malformed`], formatted, unless `cond` holds.
macro_rules! ensure {
	($cond:expr, $($arg:tt)*) => {
		if !$cond {
			$crate::container::ts::error::bail!($($arg)*);
		}
	};
}

/// Return [`Error::Unsupported`], formatted.
macro_rules! bail_unsupported {
	($($arg:tt)*) => {
		return Err($crate::container::ts::error::unsupported!($($arg)*).into())
	};
}

/// Return [`Error::Unsupported`], formatted, unless `cond` holds.
macro_rules! ensure_supported {
	($cond:expr, $($arg:tt)*) => {
		if !$cond {
			$crate::container::ts::error::bail_unsupported!($($arg)*);
		}
	};
}

pub(super) use {bail, bail_unsupported, ensure, ensure_supported, malformed, unsupported};

/// A missing value as [`Error::Malformed`].
pub(super) trait Context<T> {
	/// `msg` when the value is missing.
	fn context(self, msg: &str) -> Result<T>;
}

impl<T> Context<T> for Option<T> {
	fn context(self, msg: &str) -> Result<T> {
		self.ok_or_else(|| Error::Malformed(msg.to_string()))
	}
}
