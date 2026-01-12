use crate::{
	coding::*,
	ietf::{Message, Parameters, Version as IetfVersion},
};
use num_enum::IntoPrimitive;

#[derive(Debug, Copy, Clone, IntoPrimitive, Eq, Hash, PartialEq)]
#[repr(u64)]
pub enum SetupParameter {
	Path = 1,
	MaxRequestId = 2,
	AuthorizationToken = 3,
	MaxAuthTokenCacheSize = 4,
	Authority = 5,
	Implementation = 7,
}

/// Sent by the client to setup the session.
#[derive(Debug, Clone)]
pub struct ClientSetup {
	/// The list of supported versions in preferred order.
	pub versions: Versions,

	/// Extensions.
	pub parameters: Parameters,
}

impl Message for ClientSetup {
	const ID: u64 = 0x20;

	/// Decode a client setup message.
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: IetfVersion) -> Result<Self, DecodeError> {
		let versions = Versions::decode(r, version)?;
		let parameters = Parameters::decode(r, version)?;

		Ok(Self { versions, parameters })
	}

	/// Encode a client setup message.
	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: IetfVersion) {
		self.versions.encode(w, version);
		self.parameters.encode(w, version);
	}
}

/// Sent by the server in response to a client setup.
#[derive(Debug, Clone)]
pub struct ServerSetup {
	/// The list of supported versions in preferred order.
	pub version: Version,

	/// Supported extensions.
	pub parameters: Parameters,
}

impl Message for ServerSetup {
	const ID: u64 = 0x21;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: IetfVersion) {
		self.version.encode(w, version);
		self.parameters.encode(w, version);
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: IetfVersion) -> Result<Self, DecodeError> {
		let version = Version::decode(r, version)?;
		let parameters = Parameters::decode(r, version)?;

		Ok(Self { version, parameters })
	}
}
