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

#[cfg(test)]
mod tests {
	use super::*;
	use bytes::BytesMut;

	fn encode_message<M: Message>(msg: &M, version: IetfVersion) -> Vec<u8> {
		let mut buf = BytesMut::new();
		msg.encode_msg(&mut buf, version);
		buf.to_vec()
	}

	fn decode_message<M: Message>(bytes: &[u8], version: IetfVersion) -> Result<M, DecodeError> {
		let mut buf = bytes::Bytes::from(bytes.to_vec());
		M::decode_msg(&mut buf, version)
	}

	#[test]
	fn test_client_setup_v14_round_trip() {
		let msg = ClientSetup {
			versions: vec![Version(IetfVersion::Draft14 as u64)].into(),
			parameters: Parameters::default(),
		};

		let encoded = encode_message(&msg, IetfVersion::Draft14);
		let decoded: ClientSetup = decode_message(&encoded, IetfVersion::Draft14).unwrap();

		assert_eq!(decoded.versions.len(), 1);
		assert_eq!(decoded.versions[0], Version(IetfVersion::Draft14 as u64));
	}

	#[test]
	fn test_client_setup_v15_round_trip() {
		let msg = ClientSetup {
			versions: vec![Version(IetfVersion::Draft15 as u64)].into(),
			parameters: Parameters::default(),
		};

		let encoded = encode_message(&msg, IetfVersion::Draft15);
		let decoded: ClientSetup = decode_message(&encoded, IetfVersion::Draft15).unwrap();

		// v15 doesn't encode versions, so decoded should have [Draft15]
		assert_eq!(decoded.versions.len(), 1);
		assert_eq!(decoded.versions[0], Version(IetfVersion::Draft15 as u64));
	}

	#[test]
	fn test_server_setup_v14_round_trip() {
		let msg = ServerSetup {
			version: Version(IetfVersion::Draft14 as u64),
			parameters: Parameters::default(),
		};

		let encoded = encode_message(&msg, IetfVersion::Draft14);
		let decoded: ServerSetup = decode_message(&encoded, IetfVersion::Draft14).unwrap();

		assert_eq!(decoded.version, Version(IetfVersion::Draft14 as u64));
	}

	#[test]
	fn test_server_setup_v15_round_trip() {
		let msg = ServerSetup {
			version: Version(IetfVersion::Draft15 as u64),
			parameters: Parameters::default(),
		};

		let encoded = encode_message(&msg, IetfVersion::Draft15);
		let decoded: ServerSetup = decode_message(&encoded, IetfVersion::Draft15).unwrap();

		// v15 doesn't encode version, so decoded should be Draft15
		assert_eq!(decoded.version, Version(IetfVersion::Draft15 as u64));
	}
}
