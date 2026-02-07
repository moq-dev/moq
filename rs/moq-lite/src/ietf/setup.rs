use crate::{
	coding::*,
	ietf::{Message, Parameters, Version as IetfVersion},
};

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
		match version {
			IetfVersion::Draft14 => {
				let versions = Versions::decode(r, version)?;
				let parameters = Parameters::decode(r, version)?;
				Ok(Self { versions, parameters })
			}
			IetfVersion::Draft15 => {
				// Draft15: no versions list, just parameters
				let parameters = Parameters::decode(r, version)?;
				Ok(Self {
					versions: vec![Version(IetfVersion::Draft15 as u64)].into(),
					parameters,
				})
			}
		}
	}

	/// Encode a client setup message.
	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: IetfVersion) {
		match version {
			IetfVersion::Draft14 => {
				self.versions.encode(w, version);
				self.parameters.encode(w, version);
			}
			IetfVersion::Draft15 => {
				// Draft15: no versions list, just parameters
				self.parameters.encode(w, version);
			}
		}
	}
}

/// Sent by the server in response to a client setup.
#[derive(Debug, Clone)]
pub struct ServerSetup {
	/// The selected version.
	pub version: Version,

	/// Supported extensions.
	pub parameters: Parameters,
}

impl Message for ServerSetup {
	const ID: u64 = 0x21;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: IetfVersion) {
		match version {
			IetfVersion::Draft14 => {
				self.version.encode(w, version);
				self.parameters.encode(w, version);
			}
			IetfVersion::Draft15 => {
				// Draft15: no version field, just parameters
				self.parameters.encode(w, version);
			}
		}
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: IetfVersion) -> Result<Self, DecodeError> {
		match version {
			IetfVersion::Draft14 => {
				let version = Version::decode(r, version)?;
				let parameters = Parameters::decode(r, version)?;
				Ok(Self { version, parameters })
			}
			IetfVersion::Draft15 => {
				// Draft15: no version field, just parameters
				let parameters = Parameters::decode(r, version)?;
				Ok(Self {
					version: Version(IetfVersion::Draft15 as u64),
					parameters,
				})
			}
		}
	}
}
