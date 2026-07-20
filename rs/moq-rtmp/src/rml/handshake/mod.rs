/*!
This module provides functionality for handling the RTMP handshake process.  There are two types
of handshakes that can potentially be seen.

The first is the original RTMP handshake that is defined in the official RTMP specification
released by Adobe.  However, if a flash client connects using this handshake then the client
will not play h.264 video (it is assumed this is only a restriction in flash based clients).

Flash player 9 introduced a new handshake that utilizes SHA digests and Diffie-Hellman
encryption key negotiation, and it is required if a connected flash player
is going to display h.264 video.  While there is no official specifications for this format
this module is implemented using a clean-room specification found at
<https://www.cs.cmu.edu/~dst/Adobe/Gallery/RTMPE.txt>.

This handshake module allows for handling both the original and fp9+ handshake methods, and
determines which method of verification to use based on the packet 1 it receives.  The only
time this might fail is if a peer only accepts the originally specified RTMP handshake format
exactly and verifies that bytes 4-7 are zeroes.  At this point in time any (with the prevalence
of h.264 video) all clients and servers should work against the fp9 method so this should not
be an issue.

**Note:** At this point of time we only accept (and send) command bytes of 3, meaning that
no encryption is used.

*/

mod errors;

pub use self::errors::HandshakeError;

use hmac::{Hmac, KeyInit, Mac};
use rand::RngExt;
use sha2::Sha256;

const RTMP_PACKET_SIZE: usize = 1536;
const SHA256_DIGEST_LENGTH: usize = 32;
const P2_SIG_START_INDEX: usize = RTMP_PACKET_SIZE - SHA256_DIGEST_LENGTH;
const RANDOM_CRUD: [u8; 32] = [
	0xf0, 0xee, 0xc2, 0x4a, 0x80_u8, 0x68_u8, 0xbe_u8, 0xe8_u8, 0x2e_u8, 0x00_u8, 0xd0_u8, 0xd1_u8, 0x02_u8, 0x9e_u8,
	0x7e_u8, 0x57_u8, 0x6e_u8, 0xec_u8, 0x5d_u8, 0x2d_u8, 0x29_u8, 0x80_u8, 0x6f_u8, 0xab_u8, 0x93_u8, 0xb8_u8,
	0xe6_u8, 0x36_u8, 0xcf_u8, 0xeb_u8, 0x31_u8, 0xae_u8,
];
const GENUINE_FMS_CONST: &'static str = "Genuine Adobe Flash Media Server 001";
const GENUINE_FP_CONST: &'static str = "Genuine Adobe Flash Player 001";

/// Contains the result after processing bytes for the handshaking process
#[derive(PartialEq, Eq, Debug)]
pub enum HandshakeProcessResult {
	/// The handshake process is still on-going
	InProgress {
		/// Any bytes that should be sent to the peer as a response
		response_bytes: Vec<u8>,
	},

	/// The handshake process has successfully concluded
	Completed {
		/// Any bytes that should be sent to the peer as a response
		response_bytes: Vec<u8>,

		/// Any bytes left over after completing the handshake
		remaining_bytes: Vec<u8>,
	},
}

/// The type of peer being represented by the handshake.
///
/// This only matters due to the FP9 handshaking process, where the client and server use different
/// calculations for packet generation.
#[derive(Debug, Eq, PartialEq)]
pub enum PeerType {
	/// Handshake being represented as a server
	Server,

	/// Handshake being represented as a client
	Client,
}

struct MessageParts {
	before_digest: Vec<u8>,
	after_digest: Vec<u8>,
	digest: [u8; SHA256_DIGEST_LENGTH],
}

#[derive(Eq, PartialEq, Debug, Clone)]
enum Stage {
	NeedToSendP0AndP1,
	WaitingForPacket0,
	WaitingForPacket1,
	WaitingForPacket2,
	Complete,
}

/// Struct that handles the handshaking process.
///
/// It should be noted that the current system does not perform validation on the peer's p2 packet.
/// This is due to the complicated hmac verification.  While this verification was successful when
/// tested agains OBS, Ffmpeg, Mplayer, and Evostream, but for some reason Flash clients would fail
/// the hmac verification.
///
/// Due to the documentation on the fp9 handshake being third party, the hmac verification was
/// removed.  It is now assumed that as long as the peer sent us a p2 packet, and they did not
/// dicsonnect us after receiving our p2 packet, that the handshake was successful.  This has
/// allowed us to succeed in handshaking with flash players, and there are still enough checks that
/// it should be unlikely for too many false positives.
///
/// ## Examples
///
/// ```ignore
/// use rml_rtmp::handshake::{Handshake, PeerType, HandshakeProcessResult};
///
/// let mut client = Handshake::new(PeerType::Client);
/// let mut server = Handshake::new(PeerType::Server);
///
/// let c0_and_c1 = client.generate_outbound_p0_and_p1().unwrap();
/// let s0_s1_and_s2 = match server.process_bytes(&c0_and_c1[..]) {
///     Ok(HandshakeProcessResult::InProgress {response_bytes: bytes}) => bytes,
///     x => panic!("Unexpected process_bytes response: {:?}", x),
/// };
///
/// let c2 = match client.process_bytes(&s0_s1_and_s2[..]) {
///     Ok(HandshakeProcessResult::Completed {
///         response_bytes: bytes,
///         remaining_bytes: _
///     }) => bytes,
///     x => panic!("Unexpected s0_s1_and_s2 process_bytes response: {:?}", x),
/// };
///
/// match server.process_bytes(&c2[..]) {
///     Ok(HandshakeProcessResult::Completed {
///             response_bytes: _,
///             remaining_bytes: _
///         }) => {},
///     x => panic!("Unexpected process_bytes response: {:?}", x),
/// }
/// ```
///
pub struct Handshake {
	current_stage: Stage,
	peer_type: PeerType,
	command_byte: u8,
	input_buffer: Vec<u8>,
	sent_p1: [u8; RTMP_PACKET_SIZE],
	sent_digest: [u8; SHA256_DIGEST_LENGTH],
}

impl Handshake {
	/// Creates a new handshake handling instance.
	///
	/// The Flash Player 9 handshake requires generating a different packet 1 depending if you
	/// are the client or the server, and thus this must be specified when creating a new
	/// `Handshake` instance.
	pub fn new(peer_type: PeerType) -> Handshake {
		Handshake {
			current_stage: Stage::NeedToSendP0AndP1,
			command_byte: 0_u8,
			input_buffer: Vec::with_capacity(RTMP_PACKET_SIZE),
			sent_p1: [0_u8; RTMP_PACKET_SIZE],
			peer_type,
			sent_digest: [0_u8; SHA256_DIGEST_LENGTH],
		}
	}

	/// Creates the packets 0 and 1 that should get sent to the peer.  This is only strictly
	/// required to be called by the client in the connection process to initiate the handshake
	/// process.  The server can wait until `process_bytes()` is called, and the outbound
	/// packets #0 and #1 will be included as the handshake's response.
	///
	/// For now this only sends a command byte of 3 (no encryption).
	pub fn generate_outbound_p0_and_p1(&mut self) -> Result<Vec<u8>, HandshakeError> {
		const ADOBE_VERSION: [u8; 4] = [128_u8, 0_u8, 7_u8, 2_u8]; // Copied from jw player handshake

		// Leave time field as zero, version field as ADOBE_VERSION, and the rest of the packet
		// should be random data.  Part of the random data will be used to determine placement
		// of the digest offset
		fill_with_random_data(&mut self.sent_p1[8..1532]);
		self.sent_p1[4] = ADOBE_VERSION[0];
		self.sent_p1[5] = ADOBE_VERSION[1];
		self.sent_p1[6] = ADOBE_VERSION[2];
		self.sent_p1[7] = ADOBE_VERSION[3];

		let (digest_offset, constant_key) = match self.peer_type {
			PeerType::Server => (get_server_digest_offset(&self.sent_p1), GENUINE_FMS_CONST),
			PeerType::Client => (get_client_digest_offset(&self.sent_p1), GENUINE_FP_CONST),
		};

		{
			let key_bytes = constant_key.as_bytes();
			let pre_digest = &self.sent_p1[0..(digest_offset as usize)];
			let post_digest = &self.sent_p1[(digest_offset as usize + SHA256_DIGEST_LENGTH)..];
			self.sent_digest = calc_hmac_from_parts(&pre_digest, &post_digest, &key_bytes);
		}

		// Form packet #1
		for index in 0..SHA256_DIGEST_LENGTH {
			self.sent_p1[(digest_offset as usize) + index] = self.sent_digest[index];
		}

		let mut output = vec![3_u8];
		output.extend_from_slice(&self.sent_p1);

		self.current_stage = Stage::WaitingForPacket0;

		Ok(output)
	}

	/// Processes the passed in bytes as part of the handshake process.  If not enough bytes
	/// were received to complete the next handshake step then the handshake handler will
	/// hold onto the currently received bytes until it has enough to either error out or
	/// move onto the next stage of the handshake.
	///
	/// If the handshake is still in progress it will potentially return bytes that should be
	/// sent to the peer.  If the handshake has completed it will return any overflow bytes it
	/// received that were not part of the handshaking process.  This overflow will most likely
	/// contain RTMP chunks that need to be deserialized.
	///
	/// If the `Handshake` has not generated the outbound packets 0 and 1 yet, then
	/// the first call to `process_bytes` will include packets 0 and 1 in the `response_bytes`
	/// field.
	pub fn process_bytes(&mut self, data: &[u8]) -> Result<HandshakeProcessResult, HandshakeError> {
		self.input_buffer.extend_from_slice(data);

		let mut bytes_for_response: Vec<u8> = Vec::new();
		let mut left_over_bytes: Vec<u8> = Vec::new();

		loop {
			let starting_stage = self.current_stage.clone();
			let result = match self.current_stage {
				Stage::NeedToSendP0AndP1 => match self.generate_outbound_p0_and_p1() {
					Err(x) => Err(x),
					Ok(bytes) => Ok(HandshakeProcessResult::InProgress { response_bytes: bytes }),
				},
				Stage::WaitingForPacket0 => self.parse_p0(),
				Stage::WaitingForPacket1 => self.parse_p1(),
				Stage::WaitingForPacket2 => self.parse_p2(),
				Stage::Complete => Err(HandshakeError::HandshakeAlreadyCompleted),
			};

			match result {
				Err(x) => return Err(x),
				Ok(x) => match x {
					HandshakeProcessResult::InProgress { response_bytes: bytes } => bytes_for_response.extend(bytes),
					HandshakeProcessResult::Completed {
						response_bytes: _,
						remaining_bytes: bytes,
					} => left_over_bytes.extend(bytes),
				},
			};

			if self.current_stage == Stage::Complete || starting_stage == self.current_stage {
				// If we are still on the same stage assume that we didn't have
				// enough bytes to process the current packet and wait to try again
				break;
			}
		}

		if self.current_stage == Stage::Complete {
			Ok(HandshakeProcessResult::Completed {
				response_bytes: bytes_for_response,
				remaining_bytes: left_over_bytes,
			})
		} else {
			Ok(HandshakeProcessResult::InProgress {
				response_bytes: bytes_for_response,
			})
		}
	}

	fn parse_p0(&mut self) -> Result<HandshakeProcessResult, HandshakeError> {
		if self.input_buffer.len() == 0 {
			return Ok(HandshakeProcessResult::InProgress {
				response_bytes: Vec::new(),
			});
		}

		self.command_byte = self.input_buffer.remove(0);
		if self.command_byte != 3_u8 {
			return Err(HandshakeError::BadVersionId);
		};

		self.current_stage = Stage::WaitingForPacket1;
		Ok(HandshakeProcessResult::InProgress {
			response_bytes: Vec::new(),
		})
	}

	fn parse_p1(&mut self) -> Result<HandshakeProcessResult, HandshakeError> {
		if self.input_buffer.len() < RTMP_PACKET_SIZE {
			return Ok(HandshakeProcessResult::InProgress {
				response_bytes: Vec::new(),
			});
		}

		let received_packet_1: [u8; RTMP_PACKET_SIZE];
		{
			let mut handshake = [0_u8; RTMP_PACKET_SIZE];
			let mut index = 0;

			for x in self.input_buffer.drain(..RTMP_PACKET_SIZE) {
				handshake[index] = x;
				index = index + 1;
			}

			received_packet_1 = handshake;
		}

		// Test against the expected constant string the peer sent over
		let p1_key = match self.peer_type {
			PeerType::Server => GENUINE_FP_CONST.as_bytes().to_vec(),
			PeerType::Client => GENUINE_FMS_CONST.as_bytes().to_vec(),
		};

		let received_digest = match get_digest_for_received_packet(&received_packet_1, &p1_key) {
			Ok(digest) => digest,
			Err(HandshakeError::UnknownPacket1Format) => {
				// Since no digest was found chances are that this handshake is
				// not a fp9 handshake but instead is the handshake from the
				// original RTMP specification. If that's the case then this
				// isn't an error, we just need to send back an exact copy of
				// their p1 and we are good.
				//
				// Note that the original RTMP specification indicates a version
				// of 0 should be specified in the p1 packet, but some RTMP
				// destinations such as YouTube provide a non-zero version while
				// still expecting an original handshake.
				self.current_stage = Stage::WaitingForPacket2;
				return Ok(HandshakeProcessResult::InProgress {
					response_bytes: received_packet_1.to_vec(),
				});
			}
			Err(x) => return Err(x),
		};

		// generate packet 2 for a response
		let mut output_packet = [0_u8; RTMP_PACKET_SIZE];
		fill_with_random_data(&mut output_packet);

		let mut p2_key = match self.peer_type {
			PeerType::Server => GENUINE_FMS_CONST.as_bytes().to_vec(),
			PeerType::Client => GENUINE_FP_CONST.as_bytes().to_vec(),
		};

		p2_key.extend_from_slice(&RANDOM_CRUD[..]);

		let hmac1 = calc_hmac(&received_digest, &p2_key[..]);
		let hmac2 = calc_hmac(&output_packet[..P2_SIG_START_INDEX], &hmac1);

		// the hmac2 signature is written to the end of the p2 packet
		for index in 0..SHA256_DIGEST_LENGTH {
			output_packet[P2_SIG_START_INDEX + index] = hmac2[index];
		}

		self.current_stage = Stage::WaitingForPacket2;
		Ok(HandshakeProcessResult::InProgress {
			response_bytes: output_packet.to_vec(),
		})
	}

	fn parse_p2(&mut self) -> Result<HandshakeProcessResult, HandshakeError> {
		if self.input_buffer.len() < RTMP_PACKET_SIZE {
			return Ok(HandshakeProcessResult::InProgress {
				response_bytes: Vec::new(),
			});
		}

		let received_packet_2: [u8; RTMP_PACKET_SIZE];
		{
			let mut handshake = [0_u8; RTMP_PACKET_SIZE];
			handshake.copy_from_slice(&self.input_buffer[..RTMP_PACKET_SIZE]);

			received_packet_2 = handshake;
			self.input_buffer.drain(..RTMP_PACKET_SIZE);
		}

		// If the peer sent back a p2 that is an exact copy of our p1, accept it as that mean's it
		// is the old style handshake
		if &self.sent_p1[..] == &received_packet_2[..] {
			self.current_stage = Stage::Complete;
			let remaining_bytes = self.input_buffer.drain(..).collect();
			return Ok(HandshakeProcessResult::Completed {
				response_bytes: Vec::new(),
				remaining_bytes,
			});
		}

		// Not an exact match, so test the signature
		let mut peer_key = match self.peer_type {
			PeerType::Server => GENUINE_FP_CONST.as_bytes().to_vec(),
			PeerType::Client => GENUINE_FMS_CONST.as_bytes().to_vec(),
		};

		peer_key.extend_from_slice(&RANDOM_CRUD[..]);

		// TODO: Re-enable P2 verification.
		// Verification of packet 2 had to be commented out for flash players to work.  For some
		// reason flash players are failing the p2 validation even though VLC, ffmpeg, and others
		// are handshaking just fine.  For now I am just going to assume that the p2 they sent
		// us is fine if they don't disconnect after we sent them our p2, and can look at this
		// later if there's a reason to really care.

		//let expected_hmac = &received_packet_2[P2_SIG_START_INDEX..RTMP_PACKET_SIZE];
		//let hmac1 = calc_hmac(&self.sent_digest, &peer_key[..]);
		//let hmac2 = calc_hmac(&received_packet_2[..P2_SIG_START_INDEX], &hmac1);
		//if &expected_hmac[..] != &hmac2[..] {
		//    return Err(HandshakeError{kind: HandshakeErrorKind::InvalidP2Packet});
		//}

		self.current_stage = Stage::Complete;
		let bytes_left = self.input_buffer.drain(..).collect();
		Ok(HandshakeProcessResult::Completed {
			response_bytes: Vec::new(),
			remaining_bytes: bytes_left,
		})
	}
}

fn get_digest_for_received_packet(
	packet: &[u8; RTMP_PACKET_SIZE],
	key: &[u8],
) -> Result<[u8; SHA256_DIGEST_LENGTH], HandshakeError> {
	// According to the unofficial specification, peers may send messages with the digest pointer
	// either at index 8 or 772 with no known reason for why one would be used over the other.  For
	// the best compatibility just try both.

	let v1_offset = get_client_digest_offset(&packet);
	let v1_parts = get_message_parts(&packet, v1_offset)?;
	let v1_hmac = calc_hmac_from_parts(&v1_parts.before_digest, &v1_parts.after_digest, &key);

	let v2_offset = get_server_digest_offset(&packet);
	let v2_parts = get_message_parts(&packet, v2_offset)?;
	let v2_hmac = calc_hmac_from_parts(&v2_parts.before_digest, &v2_parts.after_digest, &key);

	match true {
		_ if v1_hmac == v1_parts.digest => Ok(v1_parts.digest),
		_ if v2_hmac == v2_parts.digest => Ok(v2_parts.digest),
		_ => Err(HandshakeError::UnknownPacket1Format),
	}
}

fn get_server_digest_offset(data: &[u8; RTMP_PACKET_SIZE]) -> u32 {
	let first_four_byte_sum = (data[772] as u32) + (data[773] as u32) + (data[774] as u32) + (data[775] as u32);
	let offset = (first_four_byte_sum % 728) + 776;
	offset
}

fn get_client_digest_offset(data: &[u8; RTMP_PACKET_SIZE]) -> u32 {
	let first_four_byte_sum = (data[8] as u32) + (data[9] as u32) + (data[10] as u32) + (data[11] as u32);
	let offset = (first_four_byte_sum % 728) + 12;
	offset
}

fn get_message_parts(handshake: &[u8; RTMP_PACKET_SIZE], digest_offset: u32) -> Result<MessageParts, HandshakeError> {
	let (part1, rest) = handshake.split_at(digest_offset as usize);
	let (raw_digest, part2) = rest.split_at(SHA256_DIGEST_LENGTH);

	let mut digest = [0_u8; SHA256_DIGEST_LENGTH];
	for index in 0..SHA256_DIGEST_LENGTH {
		digest[index] = raw_digest[index];
	}

	Ok(MessageParts {
		before_digest: Vec::from(part1),
		after_digest: Vec::from(part2),
		digest,
	})
}

fn calc_hmac_from_parts(part1: &[u8], part2: &[u8], key: &[u8]) -> [u8; SHA256_DIGEST_LENGTH] {
	let mut inputs = Vec::with_capacity(part1.len() + part2.len());
	for index in 0..part1.len() {
		inputs.push(part1[index]);
	}

	for index in 0..part2.len() {
		inputs.push(part2[index]);
	}

	calc_hmac(&inputs, &key)
}

fn calc_hmac(input: &[u8], key: &[u8]) -> [u8; SHA256_DIGEST_LENGTH] {
	let mut mac = Hmac::<Sha256>::new_from_slice(key).unwrap();
	mac.update(input);

	let result = mac.finalize();
	let array = result.into_bytes();

	if array.len() != 32 {
		panic!(
			"Expected hmac signature to be 32 byte array, instead it was a {} byte array",
			array.len()
		);
	}

	let mut output = [0_u8; 32];
	for index in 0..32 {
		output[index] = array[index];
	}

	output
}

fn fill_with_random_data(buffer: &mut [u8]) {
	let mut rng = rand::rng();
	for byte in buffer {
		*byte = rng.random();
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	// Since the handshake requires SHA digest computations, the easiest way to test this
	// is to take network traffic via JWPlayer's flash player and validate directly against that
	const JWPLAYER_C0: [u8; 1] = [3_u8];
	const JWPLAYER_C1: [u8; 1536] = [
		0x00_u8, 0x12_u8, 0x6c_u8, 0xbb_u8, 0x80_u8, 0x00_u8, 0x07_u8, 0x02_u8, 0x62_u8, 0x3f_u8, 0x16_u8, 0x27_u8,
		0xc6_u8, 0x1d_u8, 0xac_u8, 0x34_u8, 0x38_u8, 0x46_u8, 0xf2_u8, 0xbc_u8, 0x67_u8, 0xce_u8, 0xed_u8, 0xac_u8,
		0xe3_u8, 0x00_u8, 0x0d_u8, 0x73_u8, 0x54_u8, 0x46_u8, 0x03_u8, 0x95_u8, 0xba_u8, 0xc3_u8, 0x3b_u8, 0xd7_u8,
		0xf5_u8, 0xa4_u8, 0x40_u8, 0x5f_u8, 0xa9_u8, 0xd4_u8, 0x7b_u8, 0x0d_u8, 0x91_u8, 0xd9_u8, 0x98_u8, 0xbd_u8,
		0xaf_u8, 0x21_u8, 0xd0_u8, 0x3d_u8, 0xd4_u8, 0xf0_u8, 0x2f_u8, 0x91_u8, 0x47_u8, 0xd3_u8, 0xd4_u8, 0x70_u8,
		0x5b_u8, 0xd4_u8, 0xd0_u8, 0x67_u8, 0x84_u8, 0x64_u8, 0x2b_u8, 0x0d_u8, 0x29_u8, 0x66_u8, 0xbd_u8, 0x02_u8,
		0x09_u8, 0x86_u8, 0x8b_u8, 0x64_u8, 0x0d_u8, 0x45_u8, 0x01_u8, 0xb0_u8, 0xf8_u8, 0xa7_u8, 0xca_u8, 0x0e_u8,
		0xa2_u8, 0x47_u8, 0x6d_u8, 0x2a_u8, 0xec_u8, 0x94_u8, 0xc0_u8, 0xc8_u8, 0x75_u8, 0x1f_u8, 0x44_u8, 0x64_u8,
		0xb5_u8, 0xa9_u8, 0x18_u8, 0x3c_u8, 0x81_u8, 0xcb_u8, 0x86_u8, 0xc0_u8, 0x6d_u8, 0xe6_u8, 0x93_u8, 0x9d_u8,
		0x86_u8, 0x5c_u8, 0x96_u8, 0x43_u8, 0xc7_u8, 0xac_u8, 0x53_u8, 0xf7_u8, 0xb8_u8, 0xf8_u8, 0xbb_u8, 0x5d_u8,
		0x73_u8, 0xa7_u8, 0x5a_u8, 0x3a_u8, 0x78_u8, 0xf2_u8, 0xaa_u8, 0x88_u8, 0x21_u8, 0x2d_u8, 0x78_u8, 0x0f_u8,
		0x86_u8, 0xc0_u8, 0xcb_u8, 0x61_u8, 0x0d_u8, 0x03_u8, 0xcf_u8, 0x54_u8, 0x81_u8, 0xc7_u8, 0xab_u8, 0xd3_u8,
		0x76_u8, 0xb6_u8, 0x13_u8, 0x38_u8, 0x03_u8, 0xcf_u8, 0x53_u8, 0x96_u8, 0x41_u8, 0xa3_u8, 0xc9_u8, 0xbc_u8,
		0x8b_u8, 0x48_u8, 0x2a_u8, 0x58_u8, 0xc9_u8, 0xd3_u8, 0xf3_u8, 0x65_u8, 0x96_u8, 0x96_u8, 0x0f_u8, 0x1c_u8,
		0x8a_u8, 0x88_u8, 0xb3_u8, 0x7c_u8, 0xbb_u8, 0x53_u8, 0x40_u8, 0x53_u8, 0x47_u8, 0xa2_u8, 0xf8_u8, 0xbe_u8,
		0x57_u8, 0xe1_u8, 0x8a_u8, 0x3b_u8, 0xc1_u8, 0xf6_u8, 0xdc_u8, 0x97_u8, 0x32_u8, 0xfb_u8, 0xeb_u8, 0x4b_u8,
		0x06_u8, 0x8e_u8, 0x70_u8, 0x68_u8, 0x71_u8, 0x84_u8, 0x71_u8, 0xdc_u8, 0x6e_u8, 0xae_u8, 0x54_u8, 0xa5_u8,
		0xa7_u8, 0xb7_u8, 0x18_u8, 0xf8_u8, 0xdf_u8, 0x89_u8, 0xab_u8, 0x1f_u8, 0x04_u8, 0x64_u8, 0xa3_u8, 0xc1_u8,
		0x40_u8, 0x82_u8, 0xab_u8, 0x8d_u8, 0x7f_u8, 0x41_u8, 0xac_u8, 0xdd_u8, 0xc5_u8, 0x2c_u8, 0xe1_u8, 0xe5_u8,
		0x45_u8, 0x6f_u8, 0x00_u8, 0x72_u8, 0xdf_u8, 0x49_u8, 0xe8_u8, 0x7a_u8, 0x09_u8, 0x34_u8, 0xa3_u8, 0xce_u8,
		0xb9_u8, 0x06_u8, 0xd4_u8, 0x09_u8, 0x45_u8, 0x48_u8, 0x07_u8, 0x9b_u8, 0x82_u8, 0x9a_u8, 0xab_u8, 0xff_u8,
		0xf8_u8, 0x86_u8, 0x97_u8, 0xc3_u8, 0x90_u8, 0xd1_u8, 0x1d_u8, 0x24_u8, 0xe9_u8, 0x81_u8, 0x3b_u8, 0x22_u8,
		0x5f_u8, 0xb1_u8, 0x01_u8, 0x47_u8, 0xb7_u8, 0xb0_u8, 0xa4_u8, 0xc7_u8, 0x79_u8, 0x4c_u8, 0xf7_u8, 0xae_u8,
		0x09_u8, 0xdc_u8, 0x34_u8, 0xe9_u8, 0x25_u8, 0x2c_u8, 0x7c_u8, 0x46_u8, 0x7b_u8, 0x1b_u8, 0x02_u8, 0x7c_u8,
		0x07_u8, 0x2a_u8, 0xa2_u8, 0x6c_u8, 0xce_u8, 0xcc_u8, 0x01_u8, 0xfe_u8, 0xa2_u8, 0x02_u8, 0xbb_u8, 0xc1_u8,
		0x5d_u8, 0x41_u8, 0x21_u8, 0xea_u8, 0xd7_u8, 0x95_u8, 0x9e_u8, 0x26_u8, 0xfe_u8, 0x8d_u8, 0xdb_u8, 0xe8_u8,
		0x33_u8, 0x9a_u8, 0xf9_u8, 0x0e_u8, 0x1b_u8, 0x00_u8, 0xa7_u8, 0x28_u8, 0x84_u8, 0x52_u8, 0xd8_u8, 0x30_u8,
		0xb5_u8, 0x05_u8, 0xba_u8, 0x87_u8, 0xa9_u8, 0x23_u8, 0xe3_u8, 0x46_u8, 0xa5_u8, 0x78_u8, 0x10_u8, 0x7a_u8,
		0xe5_u8, 0xa9_u8, 0xcc_u8, 0xf1_u8, 0xa5_u8, 0xae_u8, 0x95_u8, 0xe8_u8, 0xd0_u8, 0xe4_u8, 0xc3_u8, 0x43_u8,
		0xc4_u8, 0x45_u8, 0x9c_u8, 0x4e_u8, 0xcd_u8, 0xa3_u8, 0x8c_u8, 0x52_u8, 0xc8_u8, 0x94_u8, 0x6c_u8, 0x86_u8,
		0xab_u8, 0x77_u8, 0xa4_u8, 0xde_u8, 0x39_u8, 0x0f_u8, 0x7b_u8, 0x98_u8, 0x0b_u8, 0xd3_u8, 0x94_u8, 0xe4_u8,
		0x21_u8, 0x40_u8, 0xb5_u8, 0x0d_u8, 0xc1_u8, 0x01_u8, 0x94_u8, 0x83_u8, 0xa4_u8, 0xc8_u8, 0xf2_u8, 0x27_u8,
		0xda_u8, 0x7f_u8, 0x3f_u8, 0x8a_u8, 0xce_u8, 0xfa_u8, 0x1d_u8, 0x2c_u8, 0xa2_u8, 0x39_u8, 0xa0_u8, 0x8a_u8,
		0x73_u8, 0x87_u8, 0x87_u8, 0x9f_u8, 0x9f_u8, 0xc8_u8, 0xa2_u8, 0xa4_u8, 0x0a_u8, 0x07_u8, 0x88_u8, 0x0d_u8,
		0x98_u8, 0x8e_u8, 0xd5_u8, 0xcb_u8, 0x1b_u8, 0x2b_u8, 0x00_u8, 0x7a_u8, 0xbb_u8, 0xaf_u8, 0xce_u8, 0x8a_u8,
		0x54_u8, 0x52_u8, 0x35_u8, 0x37_u8, 0x64_u8, 0xc3_u8, 0x6c_u8, 0xbc_u8, 0x07_u8, 0xe5_u8, 0x70_u8, 0x13_u8,
		0x1b_u8, 0x24_u8, 0xa6_u8, 0x9c_u8, 0x48_u8, 0xc4_u8, 0xa4_u8, 0x3f_u8, 0x38_u8, 0xd6_u8, 0x22_u8, 0x98_u8,
		0x89_u8, 0x9c_u8, 0x38_u8, 0x03_u8, 0xdc_u8, 0x1e_u8, 0x44_u8, 0xcf_u8, 0xe9_u8, 0x6c_u8, 0x5e_u8, 0x48_u8,
		0x9a_u8, 0x33_u8, 0xc4_u8, 0x9f_u8, 0xb9_u8, 0xc0_u8, 0xbe_u8, 0x79_u8, 0x6d_u8, 0x4c_u8, 0x9e_u8, 0x82_u8,
		0xab_u8, 0x61_u8, 0x6a_u8, 0xd3_u8, 0x95_u8, 0x1d_u8, 0x56_u8, 0xd2_u8, 0x12_u8, 0xbc_u8, 0x3b_u8, 0x15_u8,
		0x9c_u8, 0x1e_u8, 0x95_u8, 0x0a_u8, 0x36_u8, 0x2c_u8, 0x1e_u8, 0xfd_u8, 0xcb_u8, 0x73_u8, 0x46_u8, 0x4e_u8,
		0x4c_u8, 0xe5_u8, 0x53_u8, 0x63_u8, 0xae_u8, 0xf1_u8, 0x96_u8, 0xe4_u8, 0x76_u8, 0x75_u8, 0x28_u8, 0x36_u8,
		0x94_u8, 0xc9_u8, 0xb6_u8, 0x35_u8, 0xb7_u8, 0x5a_u8, 0x32_u8, 0xfa_u8, 0xd1_u8, 0x7c_u8, 0xe5_u8, 0x80_u8,
		0x0b_u8, 0x33_u8, 0x0c_u8, 0xaa_u8, 0x35_u8, 0xbf_u8, 0x96_u8, 0xc0_u8, 0xe5_u8, 0x02_u8, 0x55_u8, 0x80_u8,
		0x97_u8, 0x68_u8, 0x6d_u8, 0xf5_u8, 0x52_u8, 0xb3_u8, 0x4b_u8, 0x77_u8, 0x0c_u8, 0x1b_u8, 0x8a_u8, 0x55_u8,
		0xcd_u8, 0xa0_u8, 0x88_u8, 0x84_u8, 0xce_u8, 0x02_u8, 0x6c_u8, 0x99_u8, 0x76_u8, 0x91_u8, 0x7a_u8, 0x61_u8,
		0x79_u8, 0x3a_u8, 0xc1_u8, 0x66_u8, 0xcd_u8, 0xe9_u8, 0x36_u8, 0x73_u8, 0x2d_u8, 0x41_u8, 0xd2_u8, 0x2b_u8,
		0x05_u8, 0xc4_u8, 0x88_u8, 0x11_u8, 0x74_u8, 0x24_u8, 0x83_u8, 0x50_u8, 0xed_u8, 0x37_u8, 0x5e_u8, 0xc5_u8,
		0xc3_u8, 0xfa_u8, 0x84_u8, 0x4d_u8, 0x81_u8, 0xf3_u8, 0x2d_u8, 0xf7_u8, 0xf0_u8, 0xfd_u8, 0x08_u8, 0xbc_u8,
		0x10_u8, 0x9e_u8, 0xe2_u8, 0xef_u8, 0xdb_u8, 0x4f_u8, 0xcb_u8, 0x6e_u8, 0x9e_u8, 0x14_u8, 0x28_u8, 0x39_u8,
		0x3a_u8, 0x9a_u8, 0xfa_u8, 0x49_u8, 0xf8_u8, 0x63_u8, 0x63_u8, 0x8e_u8, 0xa7_u8, 0xe1_u8, 0xb6_u8, 0xdf_u8,
		0x37_u8, 0xbd_u8, 0xd7_u8, 0xa6_u8, 0xfd_u8, 0xcf_u8, 0x40_u8, 0x40_u8, 0x3d_u8, 0x00_u8, 0xb8_u8, 0x5b_u8,
		0x44_u8, 0x40_u8, 0x82_u8, 0x3e_u8, 0x49_u8, 0x9d_u8, 0xcb_u8, 0xf5_u8, 0xaa_u8, 0x30_u8, 0x08_u8, 0x04_u8,
		0x95_u8, 0x39_u8, 0x87_u8, 0xb9_u8, 0x1f_u8, 0xb3_u8, 0xb7_u8, 0xfc_u8, 0xe4_u8, 0x72_u8, 0x1e_u8, 0xbc_u8,
		0x82_u8, 0x7b_u8, 0x16_u8, 0x7f_u8, 0x2c_u8, 0xea_u8, 0x06_u8, 0x9e_u8, 0x5c_u8, 0xb1_u8, 0xb7_u8, 0x34_u8,
		0x46_u8, 0x62_u8, 0x11_u8, 0xf6_u8, 0x1e_u8, 0x4a_u8, 0xcd_u8, 0xeb_u8, 0xa8_u8, 0xed_u8, 0x1a_u8, 0xb6_u8,
		0x51_u8, 0xc3_u8, 0x68_u8, 0xfb_u8, 0x31_u8, 0x2d_u8, 0x9d_u8, 0x84_u8, 0x21_u8, 0x9e_u8, 0x96_u8, 0xbf_u8,
		0xe5_u8, 0x1b_u8, 0x6b_u8, 0x7b_u8, 0x83_u8, 0x47_u8, 0xdd_u8, 0x45_u8, 0xff_u8, 0xc2_u8, 0x70_u8, 0x5d_u8,
		0xc3_u8, 0xa5_u8, 0x1d_u8, 0x6b_u8, 0x79_u8, 0x27_u8, 0xd1_u8, 0x6d_u8, 0x45_u8, 0x47_u8, 0x7b_u8, 0x25_u8,
		0xaf_u8, 0xed_u8, 0x58_u8, 0x1d_u8, 0x8f_u8, 0x2d_u8, 0xcd_u8, 0xeb_u8, 0x25_u8, 0x5e_u8, 0x62_u8, 0x68_u8,
		0x5f_u8, 0x33_u8, 0xf3_u8, 0x50_u8, 0x81_u8, 0x0f_u8, 0x5f_u8, 0x95_u8, 0x85_u8, 0xf9_u8, 0x99_u8, 0x05_u8,
		0x1d_u8, 0xff_u8, 0x6c_u8, 0x9a_u8, 0x9e_u8, 0x3d_u8, 0x3d_u8, 0xd1_u8, 0x1f_u8, 0x53_u8, 0x3a_u8, 0x2e_u8,
		0x26_u8, 0x2e_u8, 0x6b_u8, 0xda_u8, 0xb5_u8, 0x41_u8, 0x6d_u8, 0x36_u8, 0x45_u8, 0x57_u8, 0x1f_u8, 0x0f_u8,
		0xea_u8, 0x24_u8, 0x3e_u8, 0xce_u8, 0x54_u8, 0x79_u8, 0x25_u8, 0x8a_u8, 0x9c_u8, 0x27_u8, 0xe8_u8, 0x72_u8,
		0x27_u8, 0x74_u8, 0x4e_u8, 0x05_u8, 0x71_u8, 0x01_u8, 0x9f_u8, 0x68_u8, 0xdf_u8, 0x44_u8, 0xc7_u8, 0x25_u8,
		0xc8_u8, 0xbc_u8, 0x95_u8, 0x7f_u8, 0x33_u8, 0xea_u8, 0x08_u8, 0xa9_u8, 0xc4_u8, 0x40_u8, 0x15_u8, 0x93_u8,
		0xac_u8, 0x69_u8, 0x04_u8, 0x8e_u8, 0xd9_u8, 0xb1_u8, 0x98_u8, 0x18_u8, 0xff_u8, 0x16_u8, 0x33_u8, 0x61_u8,
		0x18_u8, 0xb3_u8, 0x08_u8, 0xd0_u8, 0x84_u8, 0x8c_u8, 0x49_u8, 0xdc_u8, 0x22_u8, 0x2b_u8, 0x9c_u8, 0x09_u8,
		0xc5_u8, 0x56_u8, 0x97_u8, 0xed_u8, 0x80_u8, 0xeb_u8, 0x03_u8, 0xba_u8, 0x66_u8, 0x33_u8, 0xdc_u8, 0xf9_u8,
		0x7a_u8, 0xea_u8, 0xff_u8, 0xc6_u8, 0x27_u8, 0xef_u8, 0xd6_u8, 0x02_u8, 0x4e_u8, 0x1b_u8, 0xa7_u8, 0x2d_u8,
		0xfb_u8, 0x58_u8, 0xd7_u8, 0xe8_u8, 0x55_u8, 0x48_u8, 0x4b_u8, 0x85_u8, 0xb2_u8, 0x0c_u8, 0xea_u8, 0xac_u8,
		0x66_u8, 0x59_u8, 0x12_u8, 0x0e_u8, 0xcc_u8, 0x08_u8, 0xb9_u8, 0x1e_u8, 0x08_u8, 0xdb_u8, 0x7b_u8, 0x01_u8,
		0x60_u8, 0x70_u8, 0xb7_u8, 0xd2_u8, 0x49_u8, 0x62_u8, 0x5b_u8, 0x4e_u8, 0x45_u8, 0x9e_u8, 0xf4_u8, 0xf4_u8,
		0x9c_u8, 0x73_u8, 0xbd_u8, 0x20_u8, 0xaf_u8, 0xaf_u8, 0xc2_u8, 0xb9_u8, 0xcb_u8, 0x37_u8, 0x10_u8, 0x92_u8,
		0xed_u8, 0x8a_u8, 0x62_u8, 0x11_u8, 0x64_u8, 0x66_u8, 0xf4_u8, 0xe2_u8, 0x59_u8, 0x7e_u8, 0xaa_u8, 0x24_u8,
		0x76_u8, 0x64_u8, 0x18_u8, 0xab_u8, 0x34_u8, 0x6d_u8, 0x18_u8, 0xc8_u8, 0xc9_u8, 0x1f_u8, 0xba_u8, 0x62_u8,
		0x03_u8, 0x01_u8, 0xa9_u8, 0xfb_u8, 0xe3_u8, 0xe5_u8, 0x15_u8, 0x06_u8, 0x9d_u8, 0xb0_u8, 0x8f_u8, 0x49_u8,
		0xa3_u8, 0x4f_u8, 0x91_u8, 0x44_u8, 0x3a_u8, 0xd5_u8, 0x25_u8, 0xd0_u8, 0x55_u8, 0x52_u8, 0x0f_u8, 0x6b_u8,
		0x19_u8, 0x30_u8, 0xfb_u8, 0x9b_u8, 0x2a_u8, 0x47_u8, 0xef_u8, 0xbb_u8, 0xdd_u8, 0x36_u8, 0x36_u8, 0xad_u8,
		0x66_u8, 0x91_u8, 0x6f_u8, 0x88_u8, 0xe9_u8, 0xd2_u8, 0xb4_u8, 0x2d_u8, 0xcd_u8, 0x99_u8, 0xd2_u8, 0xb7_u8,
		0x0a_u8, 0xec_u8, 0xa1_u8, 0x6c_u8, 0xba_u8, 0xdb_u8, 0xf8_u8, 0x6a_u8, 0xd7_u8, 0xed_u8, 0x82_u8, 0xd3_u8,
		0x72_u8, 0x94_u8, 0x4c_u8, 0x57_u8, 0x5f_u8, 0x9a_u8, 0xaa_u8, 0xb4_u8, 0x04_u8, 0x92_u8, 0x52_u8, 0x36_u8,
		0xca_u8, 0x11_u8, 0xef_u8, 0x81_u8, 0x7a_u8, 0x83_u8, 0xa8_u8, 0x87_u8, 0x24_u8, 0x6d_u8, 0xe2_u8, 0x10_u8,
		0x43_u8, 0xd4_u8, 0xe2_u8, 0x9e_u8, 0x25_u8, 0x37_u8, 0x83_u8, 0xdc_u8, 0x72_u8, 0x7f_u8, 0x63_u8, 0x19_u8,
		0xf8_u8, 0x2a_u8, 0x84_u8, 0x94_u8, 0x6c_u8, 0xf2_u8, 0xf6_u8, 0xaf_u8, 0x4a_u8, 0x53_u8, 0x28_u8, 0xd8_u8,
		0xb8_u8, 0x5e_u8, 0xd0_u8, 0x1e_u8, 0x45_u8, 0x65_u8, 0x43_u8, 0xbd_u8, 0x72_u8, 0x4b_u8, 0x55_u8, 0x0a_u8,
		0x00_u8, 0xac_u8, 0x39_u8, 0x42_u8, 0xdc_u8, 0xef_u8, 0x9b_u8, 0x25_u8, 0x4e_u8, 0x36_u8, 0x61_u8, 0x2f_u8,
		0x0d_u8, 0xdb_u8, 0x80_u8, 0x0f_u8, 0x8f_u8, 0xe6_u8, 0x1e_u8, 0x0e_u8, 0xd2_u8, 0x7e_u8, 0x12_u8, 0x28_u8,
		0x56_u8, 0xf5_u8, 0x33_u8, 0x8c_u8, 0xa8_u8, 0x6e_u8, 0xfe_u8, 0x63_u8, 0x7f_u8, 0xfb_u8, 0x2e_u8, 0xf7_u8,
		0xde_u8, 0x0e_u8, 0x7c_u8, 0xd9_u8, 0x4c_u8, 0xa4_u8, 0x8d_u8, 0xb7_u8, 0x69_u8, 0xef_u8, 0xac_u8, 0x6e_u8,
		0x74_u8, 0x0c_u8, 0x85_u8, 0x75_u8, 0xdc_u8, 0x57_u8, 0x80_u8, 0xa0_u8, 0x2e_u8, 0xca_u8, 0xf4_u8, 0x8a_u8,
		0x17_u8, 0x0e_u8, 0x21_u8, 0x0e_u8, 0x7c_u8, 0x33_u8, 0xa3_u8, 0x8d_u8, 0xfe_u8, 0xb3_u8, 0xdf_u8, 0x5f_u8,
		0x7d_u8, 0x8b_u8, 0xe5_u8, 0x84_u8, 0x26_u8, 0x1a_u8, 0x3d_u8, 0x1a_u8, 0x76_u8, 0x8a_u8, 0x06_u8, 0x0d_u8,
		0xb0_u8, 0xb1_u8, 0x95_u8, 0xe9_u8, 0x14_u8, 0x61_u8, 0x3a_u8, 0xfb_u8, 0xf6_u8, 0xce_u8, 0x8b_u8, 0x5d_u8,
		0x6f_u8, 0x5a_u8, 0x91_u8, 0xc3_u8, 0x32_u8, 0x65_u8, 0xb3_u8, 0x1c_u8, 0xfa_u8, 0xfb_u8, 0xbe_u8, 0xd7_u8,
		0x2f_u8, 0xe9_u8, 0xd0_u8, 0xa8_u8, 0x24_u8, 0x0a_u8, 0x66_u8, 0xc7_u8, 0x60_u8, 0xdf_u8, 0xdc_u8, 0x83_u8,
		0x21_u8, 0xb2_u8, 0x28_u8, 0x2b_u8, 0x94_u8, 0xee_u8, 0x94_u8, 0x6d_u8, 0xa6_u8, 0x21_u8, 0x4e_u8, 0x07_u8,
		0xd1_u8, 0xe8_u8, 0x6b_u8, 0x1d_u8, 0xe9_u8, 0xd3_u8, 0x00_u8, 0xca_u8, 0xca_u8, 0x4c_u8, 0xd2_u8, 0x98_u8,
		0x7b_u8, 0xd0_u8, 0x37_u8, 0xde_u8, 0x78_u8, 0xfd_u8, 0x84_u8, 0x0e_u8, 0xf1_u8, 0x54_u8, 0x6d_u8, 0x2c_u8,
		0x26_u8, 0x82_u8, 0x53_u8, 0x37_u8, 0x01_u8, 0x01_u8, 0x23_u8, 0x67_u8, 0x4a_u8, 0x78_u8, 0xa6_u8, 0x12_u8,
		0x49_u8, 0x15_u8, 0xb9_u8, 0x25_u8, 0x87_u8, 0x06_u8, 0x8e_u8, 0xe7_u8, 0xaf_u8, 0x24_u8, 0x41_u8, 0x5e_u8,
		0x9e_u8, 0x8d_u8, 0x27_u8, 0x93_u8, 0xa6_u8, 0x80_u8, 0xae_u8, 0x72_u8, 0xa0_u8, 0x7c_u8, 0x7b_u8, 0x46_u8,
		0xd2_u8, 0x1e_u8, 0xcc_u8, 0x4e_u8, 0xb7_u8, 0xb5_u8, 0x17_u8, 0x28_u8, 0x73_u8, 0x82_u8, 0x33_u8, 0x20_u8,
		0x8e_u8, 0xfe_u8, 0xe2_u8, 0x39_u8, 0x38_u8, 0xe4_u8, 0xe7_u8, 0xf2_u8, 0xa0_u8, 0xa3_u8, 0xb9_u8, 0x76_u8,
		0x07_u8, 0xc5_u8, 0x36_u8, 0x51_u8, 0xe0_u8, 0x57_u8, 0x1a_u8, 0x49_u8, 0xf4_u8, 0x61_u8, 0xc6_u8, 0x1f_u8,
		0x48_u8, 0xf4_u8, 0x70_u8, 0x29_u8, 0xa1_u8, 0x2e_u8, 0xfb_u8, 0xba_u8, 0xfd_u8, 0x3f_u8, 0xb0_u8, 0xd0_u8,
		0x76_u8, 0xdb_u8, 0x18_u8, 0x7c_u8, 0x63_u8, 0xed_u8, 0xa1_u8, 0xe4_u8, 0xb5_u8, 0x50_u8, 0xb5_u8, 0x43_u8,
		0xa8_u8, 0x5d_u8, 0x49_u8, 0xf2_u8, 0xa4_u8, 0x07_u8, 0x96_u8, 0xf6_u8, 0x40_u8, 0xfc_u8, 0xef_u8, 0x9c_u8,
		0xc8_u8, 0x2c_u8, 0xe1_u8, 0xd0_u8, 0x70_u8, 0xcd_u8, 0x87_u8, 0x94_u8, 0x24_u8, 0xea_u8, 0xfa_u8, 0xf5_u8,
		0x56_u8, 0x39_u8, 0xeb_u8, 0x22_u8, 0xfa_u8, 0x64_u8, 0x54_u8, 0x4b_u8, 0x9d_u8, 0x40_u8, 0xb0_u8, 0x83_u8,
		0x5b_u8, 0xfa_u8, 0xb5_u8, 0x44_u8, 0x8e_u8, 0x6b_u8, 0x48_u8, 0x7e_u8, 0xfa_u8, 0x49_u8, 0xee_u8, 0x9a_u8,
		0x82_u8, 0x73_u8, 0x7f_u8, 0x25_u8, 0xb1_u8, 0x0e_u8, 0x06_u8, 0x43_u8, 0xf4_u8, 0xaa_u8, 0xd4_u8, 0x92_u8,
		0x72_u8, 0x7f_u8, 0xf2_u8, 0xb5_u8, 0x8f_u8, 0x4b_u8, 0xac_u8, 0x9b_u8, 0x24_u8, 0xaf_u8, 0x28_u8, 0xee_u8,
		0x48_u8, 0xd4_u8, 0x39_u8, 0x68_u8, 0x8f_u8, 0x59_u8, 0x61_u8, 0x2c_u8, 0xaf_u8, 0x93_u8, 0x0b_u8, 0xb2_u8,
		0x86_u8, 0xa2_u8, 0x3e_u8, 0x21_u8, 0xdb_u8, 0x78_u8, 0x7e_u8, 0x9e_u8, 0xdb_u8, 0xcc_u8, 0x46_u8, 0xb9_u8,
		0x97_u8, 0x49_u8, 0x0c_u8, 0x2c_u8, 0x32_u8, 0xab_u8, 0x3d_u8, 0x39_u8, 0xab_u8, 0x44_u8, 0x7b_u8, 0x7c_u8,
		0xaf_u8, 0xf3_u8, 0x32_u8, 0x0d_u8, 0xcc_u8, 0x5b_u8, 0xad_u8, 0x42_u8, 0x57_u8, 0xf2_u8, 0x0d_u8, 0x2f_u8,
		0x1b_u8, 0xe3_u8, 0xbf_u8, 0xf2_u8, 0xe3_u8, 0xe7_u8, 0xb4_u8, 0x9a_u8, 0x29_u8, 0x37_u8, 0x78_u8, 0x4a_u8,
		0x11_u8, 0xcd_u8, 0x9f_u8, 0xcc_u8, 0x6e_u8, 0xbb_u8, 0xdc_u8, 0x45_u8, 0xb0_u8, 0xdd_u8, 0x0b_u8, 0x83_u8,
		0xc0_u8, 0xc0_u8, 0x0d_u8, 0x51_u8, 0xbc_u8, 0xbb_u8, 0x75_u8, 0x12_u8, 0x6a_u8, 0x85_u8, 0xba_u8, 0x71_u8,
		0x80_u8, 0x5d_u8, 0x9b_u8, 0x6c_u8, 0xa4_u8, 0x93_u8, 0xf4_u8, 0xae_u8, 0xba_u8, 0x28_u8, 0x82_u8, 0xd0_u8,
		0x56_u8, 0x79_u8, 0xdc_u8, 0x39_u8, 0x6d_u8, 0xbc_u8, 0x49_u8, 0x62_u8, 0x7d_u8, 0x51_u8, 0x60_u8, 0xaa_u8,
		0x8d_u8, 0x01_u8, 0xd9_u8, 0x15_u8, 0xd0_u8, 0x9c_u8, 0xf9_u8, 0x36_u8, 0x5f_u8, 0x82_u8, 0x1f_u8, 0x2a_u8,
		0xfc_u8, 0xd6_u8, 0xc1_u8, 0x18_u8, 0x87_u8, 0xd8_u8, 0x89_u8, 0x49_u8, 0x75_u8, 0x29_u8, 0xfe_u8, 0xbc_u8,
		0x35_u8, 0x37_u8, 0x54_u8, 0xec_u8, 0x0e_u8, 0x41_u8, 0x9a_u8, 0xec_u8, 0x45_u8, 0x37_u8, 0xf7_u8, 0x46_u8,
		0xbd_u8, 0x17_u8, 0x06_u8, 0xb3_u8, 0xf1_u8, 0xc7_u8, 0x70_u8, 0x9a_u8, 0x2b_u8, 0x5a_u8, 0x13_u8, 0x3a_u8,
		0x58_u8, 0xfc_u8, 0xb3_u8, 0x2b_u8, 0xd0_u8, 0x16_u8, 0x07_u8, 0x47_u8, 0xc1_u8, 0xd1_u8, 0x4b_u8, 0x7d_u8,
		0x77_u8, 0x17_u8, 0xd9_u8, 0x34_u8, 0x5a_u8, 0x09_u8, 0xd2_u8, 0x8c_u8, 0xfc_u8, 0x6e_u8, 0x39_u8, 0x59_u8,
	];

	#[test]
	fn can_start_client_handshake() {
		let handshake = Handshake::new(PeerType::Server);

		assert_eq!(handshake.current_stage, Stage::NeedToSendP0AndP1);
	}

	#[test]
	fn bad_version_if_first_byte_is_not_a_3() {
		let mut handshake = Handshake::new(PeerType::Server);
		let input = [4_u8];

		match handshake.process_bytes(&input) {
			Ok(_) => panic!("No error was returned"),
			Err(HandshakeError::BadVersionId) => {}
			Err(x) => panic!("Unexpected error received of {:?}", x),
		}
	}

	#[test]
	fn can_accept_jw_player_example_p0_and_p1() {
		let mut handshake = Handshake::new(PeerType::Server);
		let s0_and_s1 = match handshake.generate_outbound_p0_and_p1() {
			Err(x) => panic!("Unexpected error: {:?}", x),
			Ok(x) => x,
		};

		assert_eq!(s0_and_s1.len(), 1537, "Expected s0_and_s1 to be 1537 in length");
		assert_eq!(&s0_and_s1[0..1], [3_u8], "Expected s0 to be a 3");
		assert_eq!(handshake.current_stage, Stage::WaitingForPacket0);

		let p0_response = match handshake.process_bytes(&JWPLAYER_C0) {
			Ok(HandshakeProcessResult::InProgress { response_bytes: data }) => data,
			Ok(x) => panic!("Unexpected response of {:?}", x),
			Err(x) => panic!("Unexpected error of {:?}", x),
		};

		assert_eq!(p0_response.len(), 0, "Empty p0 response expected");
		assert_eq!(handshake.current_stage, Stage::WaitingForPacket1);

		let p1_response = match handshake.process_bytes(&JWPLAYER_C1) {
			Ok(HandshakeProcessResult::InProgress { response_bytes: data }) => data,
			Ok(x) => panic!("Unexpected response of {:?}", x),
			Err(x) => panic!("Unexpected error of {:?}", x),
		};

		assert_eq!(p1_response.len(), 1536, "Expected 1536 bytes as p1 response");
		assert_eq!(handshake.current_stage, Stage::WaitingForPacket2);

		let remaining_bytes = match handshake.process_bytes(&s0_and_s1[1..]) {
			Ok(HandshakeProcessResult::Completed {
				response_bytes: _,
				remaining_bytes: data,
			}) => data,
			Ok(x) => panic!("Unexpected response of {:?}", x),
			Err(x) => panic!("Unexpected error of {:?}", x),
		};

		assert_eq!(remaining_bytes.len(), 0, "Expected no remaining bytes");
		assert_eq!(handshake.current_stage, Stage::Complete);
	}

	#[test]
	fn can_handshake_with_client_sending_original_rtmp_specification_handshake() {
		// For this test to be accurate, bytes 4-7 must be zeros, just like the spec calls for
		let mut c0_and_c1 = [0_u8; RTMP_PACKET_SIZE + 1];
		c0_and_c1[0] = 3;
		fill_with_random_data(&mut c0_and_c1[9..RTMP_PACKET_SIZE + 1]);

		let mut handshake = Handshake::new(PeerType::Server);
		let s0_and_s1 = match handshake.generate_outbound_p0_and_p1() {
			Err(x) => panic!("Unexpected error: {:?}", x),
			Ok(x) => x,
		};

		let s2 = match handshake.process_bytes(&c0_and_c1) {
			Ok(HandshakeProcessResult::InProgress { response_bytes: data }) => data,
			Ok(x) => panic!("Unexpected response of {:?}", x),
			Err(x) => panic!("Unexpected error of {:?}", x),
		};

		assert_eq!(&s2[..], &c0_and_c1[1..], "Expected s2 value matching our c1");
		assert_eq!(handshake.current_stage, Stage::WaitingForPacket2);

		let remaining_bytes = match handshake.process_bytes(&s0_and_s1[1..]) {
			Ok(HandshakeProcessResult::Completed {
				response_bytes: _,
				remaining_bytes: data,
			}) => data,
			Ok(x) => panic!("Unexpected response of {:?}", x),
			Err(x) => panic!("Unexpected error of {:?}", x),
		};

		assert_eq!(remaining_bytes.len(), 0, "Expected no remaining bytes");
		assert_eq!(handshake.current_stage, Stage::Complete);
	}

	#[test]
	fn can_handshake_with_itself() {
		// This is the best way to verify we can handle the fp9 handshake method
		// without reimplementing the exact algorithims for the test.

		let mut client = Handshake::new(PeerType::Client);
		let mut server = Handshake::new(PeerType::Server);

		let c0_and_c1 = match client.generate_outbound_p0_and_p1() {
			Ok(bytes) => bytes,
			x => panic!("Unexpected process_bytes response: {:?}", x),
		};

		assert_eq!(client.current_stage, Stage::WaitingForPacket0);

		let s0_s1_and_s2 = match server.process_bytes(&c0_and_c1[..]) {
			Ok(HandshakeProcessResult::InProgress { response_bytes: bytes }) => bytes,
			x => panic!("Unexpected process_bytes response: {:?}", x),
		};

		assert_eq!(server.current_stage, Stage::WaitingForPacket2);

		let c2 = match client.process_bytes(&s0_s1_and_s2[..]) {
			Ok(HandshakeProcessResult::Completed {
				response_bytes: bytes,
				remaining_bytes: _,
			}) => bytes,
			x => panic!("Unexpected s0_s1_and_s2 process_bytes response: {:?}", x),
		};

		assert_eq!(client.current_stage, Stage::Complete);

		match server.process_bytes(&c2[..]) {
			Ok(HandshakeProcessResult::Completed {
				response_bytes: _,
				remaining_bytes: _,
			}) => {}
			x => panic!("Unexpected process_bytes response: {:?}", x),
		}

		assert_eq!(server.current_stage, Stage::Complete);
	}

	#[test]
	fn sends_outbound_p0_p1_if_p0_received_and_outbound_p0_and_p1_not_yet_sent() {
		let mut handshake = Handshake::new(PeerType::Server);
		let input = [3_u8];

		let response = match handshake.process_bytes(&input) {
			Ok(HandshakeProcessResult::InProgress { response_bytes: bytes }) => bytes,
			x => panic!("Unexpected process_bytes response: {:?}", x),
		};

		assert_eq!(response.len(), 1537, "Unexpected length of response");
		assert_eq!(handshake.current_stage, Stage::WaitingForPacket1);
	}

	#[test]
	fn hmac_test() {
		let data1 = "Hi ".as_bytes();
		let data2 = "There".as_bytes();
		let key = [0x0b; 20];
		let hmac = calc_hmac_from_parts(&data1, &data2, &key);

		let expected = [
			176_u8, 52_u8, 76_u8, 97_u8, 216_u8, 219_u8, 56_u8, 83_u8, 92_u8, 168_u8, 175_u8, 206_u8, 175_u8, 11_u8,
			241_u8, 43_u8, 136_u8, 29_u8, 194_u8, 0_u8, 201_u8, 131_u8, 61_u8, 167_u8, 38_u8, 233_u8, 55_u8, 108_u8,
			46_u8, 50_u8, 207_u8, 247_u8,
		];

		assert_eq!(&expected[..], &hmac[..]);
	}

	#[test]
	fn hmac_test2() {
		let data1 = "Hi There".as_bytes();
		let key = [0x0b; 20];
		let hmac = calc_hmac(&data1, &key);

		let expected = [
			176_u8, 52_u8, 76_u8, 97_u8, 216_u8, 219_u8, 56_u8, 83_u8, 92_u8, 168_u8, 175_u8, 206_u8, 175_u8, 11_u8,
			241_u8, 43_u8, 136_u8, 29_u8, 194_u8, 0_u8, 201_u8, 131_u8, 61_u8, 167_u8, 38_u8, 233_u8, 55_u8, 108_u8,
			46_u8, 50_u8, 207_u8, 247_u8,
		];

		assert_eq!(&expected[..], &hmac[..]);
	}

	#[test]
	fn hmac_test3() {
		let mut mac = Hmac::<Sha256>::new_from_slice(&[0x0b; 20]).unwrap();
		mac.update(b"Hi There");

		let expected = [
			176_u8, 52_u8, 76_u8, 97_u8, 216_u8, 219_u8, 56_u8, 83_u8, 92_u8, 168_u8, 175_u8, 206_u8, 175_u8, 11_u8,
			241_u8, 43_u8, 136_u8, 29_u8, 194_u8, 0_u8, 201_u8, 131_u8, 61_u8, 167_u8, 38_u8, 233_u8, 55_u8, 108_u8,
			46_u8, 50_u8, 207_u8, 247_u8,
		];

		let result = mac.finalize();
		let code_bytes = result.into_bytes();
		assert_eq!(&expected[..], &code_bytes[..]);
	}

	#[test]
	fn can_get_digest_from_c1() {
		match get_digest_for_received_packet(&JWPLAYER_C1, &(GENUINE_FP_CONST.as_bytes())) {
			Ok(_) => {}
			Err(x) => panic!("Unexpected error: {:?}", x),
		}
	}

	#[test]
	fn can_get_message_parts_correctly() {
		let mut message = [0_u8; RTMP_PACKET_SIZE];
		let offset: u32 = 500;

		for index in 0..message.len() {
			message[index] = match index {
				x if x < (offset as usize) => 1,
				x if x > (offset as usize) + SHA256_DIGEST_LENGTH - 1 => 3,
				_ => 2,
			};
		}

		let result = match get_message_parts(&message, offset) {
			Ok(x) => x,
			Err(x) => panic!("Error occurred: {:?}", x),
		};

		let expected_before = [1_u8; 500];
		let expected_digest = [2_u8; SHA256_DIGEST_LENGTH];
		let expected_after = [3_u8; RTMP_PACKET_SIZE - 500 - SHA256_DIGEST_LENGTH];

		assert_eq!(&result.before_digest[..], &expected_before[..], "Before did not match");
		assert_eq!(&result.after_digest[..], &expected_after[..], "After did not match");
		assert_eq!(&result.digest[..], &expected_digest[..], "Digest did not match");
	}

	#[test]
	fn can_get_correct_digest_offsets() {
		let packet1 = [0_u8; RTMP_PACKET_SIZE];

		let client_offset_1 = get_client_digest_offset(&packet1);
		let server_offset_1 = get_server_digest_offset(&packet1);

		let client_offset_2 = get_client_digest_offset(&JWPLAYER_C1);
		let server_offset_2 = get_server_digest_offset(&JWPLAYER_C1);

		assert_eq!(client_offset_1, 12, "Bad client offset #1");
		assert_eq!(server_offset_1, 776, "Bad server offset #1");

		assert_eq!(client_offset_2, 234, "Bad client offset #2");
		assert_eq!(server_offset_2, 1153, "Bad server offset #2");
	}
}
