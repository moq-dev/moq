use std::fmt;
use std::fmt::Formatter;
use std::io::Write;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::time::Duration;

use crc::{CRC_32_ISO_HDLC, Crc};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

pub const DEFAULT_MAX_RETRANSMITS: usize = 9;

#[derive(Debug)] // Purposely not `Clone` / `Copy` to ensure we always use the latest one everywhere.
pub struct StunTiming {
    pub initial_rto: Duration,
    pub max_retransmits: usize,
    pub max_rto: Duration,
}

impl StunTiming {
    /// Calculate the ICE timeout of a successful pair.
    pub fn timeout(&self) -> Duration {
        let base_timeout = (2..=self.max_retransmits)
            .map(|n| self.stun_resend_delay(n))
            .sum::<Duration>();

        base_timeout + self.stun_resend_delay(self.max_retransmits)
    }

    /// Calculate the send delay given how many times we tried.
    ///
    // Technically RTO should be calculated as per https://datatracker.ietf.org/doc/html/rfc2988, and
    // modified by https://datatracker.ietf.org/doc/html/rfc5389#section-7.2.1,
    // but chrome does it like this.
    // https://webrtc.googlesource.com/src/+/refs/heads/main/p2p/base/stun_request.cc
    pub fn stun_resend_delay(&self, send_count: usize) -> Duration {
        if send_count == 0 {
            return Duration::ZERO;
        }

        let retrans = (send_count - 1).min(self.max_retransmits);

        let rto = self.initial_rto.as_millis() << retrans;
        let capped = rto.min(self.max_rto.as_millis());

        Duration::from_millis(capped as u64)
    }

    pub fn stun_last_resend_delay(&self) -> Duration {
        self.stun_resend_delay(self.max_retransmits)
    }

    pub fn max_retransmits(&self) -> usize {
        self.max_retransmits
    }

    pub fn max_rto(&self) -> Duration {
        self.max_rto
    }
}

// Consult libwebrtc for default values here.
impl Default for StunTiming {
    fn default() -> Self {
        Self {
            initial_rto: Duration::from_millis(250),
            max_retransmits: DEFAULT_MAX_RETRANSMITS,
            // libwebrtc uses 8000 here but we want faster detection of gone peers.
            max_rto: Duration::from_millis(3000),
        }
    }
}

pub use super::error::StunError;

/// STUN transaction ID.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TransId([u8; 12]);

impl fmt::Debug for TransId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        DebugHex(&self.0).fmt(f)
    }
}

#[allow(clippy::new_without_default)]
impl TransId {
    /// A new random transaction id.
    pub fn new() -> Self {
        let mut t = [0_u8; 12];
        for v in &mut t {
            *v = NonCryptographicRng::u8();
        }
        TransId(t)
    }

    fn from_slice(s: &[u8]) -> Self {
        let mut t = [0_u8; 12];
        t[..].copy_from_slice(s);
        TransId(t)
    }
}

/// Represents a STUN message as fit for our purposes.
///
/// STUN is a very flexible protocol.
/// This implementations only provides what we need for our ICE implementation.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct StunMessage<'a> {
    method: Method,
    class: Class,
    trans_id: TransId,
    attrs: Attributes<'a>,
    integrity: &'a [u8],
    integrity_len: u16,
}

impl<'a> StunMessage<'a> {
    /// Parse a STUN message from a slice of bytes.
    pub fn parse(buf: &'a [u8]) -> Result<StunMessage<'a>, StunError> {
        if buf.len() < 4 {
            return Err(StunError::Parse("Buffer too short".into()));
        }

        let typ = (buf[0] as u16 & 0b0011_1111) << 8 | buf[1] as u16;
        let len = (buf[2] as u16) << 8 | buf[3] as u16;
        if len & 0b0000_0011 > 0 {
            return Err(StunError::Parse("len is not a multiple of 4".into()));
        }
        // Use addition here to avoid panic! if the UDP packet is under 20 bytes long.
        if (len as usize + 20) != buf.len() {
            return Err(StunError::Parse(
                "STUN length vs UDP packet mismatch".into(),
            ));
        }
        if &buf[4..8] != MAGIC {
            return Err(StunError::Parse("magic cookie mismatch".into()));
        }
        // typ is method and class
        // |M11|M10|M9|M8|M7|C1|M6|M5|M4|C0|M3|M2|M1|M0|
        // |11 |10 |9 |8 |7 |1 |6 |5 |4 |0 |3 |2 |1 |0 |
        let class = Class::from_typ(typ);
        let method = Method::from_typ(typ);
        let trans_id = TransId::from_slice(&buf[8..20]);

        let mut message_integrity_offset = None;

        let attrs = Attributes::parse(&buf[20..], trans_id, &mut message_integrity_offset)?;

        // length including message integrity attribute
        let (integrity_len, integrity) = if let Some(off) = message_integrity_offset {
            let integrity_len = (off + 4 + 20) as u16;

            // password as key is called "short-term credentials"
            // buffer from beginning including header (+20) to where message-integrity starts.
            let integrity = &buf[0..(off + 20)];

            (integrity_len, integrity)
        } else {
            (0_u16, [].as_slice())
        };

        if method == Method::Binding && class == Class::Success {
            // // message-integrity only includes the length up until and including
            // // the message-integrity attribute.
            if message_integrity_offset.is_none() {
                return Err(StunError::Parse(
                    "No message integrity in incoming STUN binding reply".into(),
                ));
            }

            if attrs.xor_mapped_address.is_none() {
                return Err(StunError::Parse("STUN packet missing mapped addr".into()));
            }
        } else if method == Method::Binding && class == Class::Request {
            // // message-integrity only includes the length up until and including
            // // the message-integrity attribute.
            if message_integrity_offset.is_none() {
                return Err(StunError::Parse(
                    "No message integrity in incoming STUN binding request".into(),
                ));
            }

            if attrs.split_username().is_none() {
                return Err(StunError::Parse("STUN packet missing username".into()));
            }
            if attrs.priority.is_none() {
                return Err(StunError::Parse("STUN packet missing priority".into()));
            }
        }

        Ok(StunMessage {
            class,
            method,
            trans_id,
            attrs,
            integrity,
            integrity_len,
        })
    }

    pub fn method(&self) -> Method {
        self.method
    }

    pub fn class(&self) -> Class {
        self.class
    }

    /// Whether this STUN message is a BINDING request.
    pub fn is_binding_request(&self) -> bool {
        self.method == Method::Binding && self.class == Class::Request
    }

    /// Whether this STUN message is a _successful_ BINDING response.
    pub fn is_successful_binding_response(&self) -> bool {
        self.method == Method::Binding && self.class == Class::Success
    }

    /// Whether this STUN message is a _failed_ BINDING response.
    ///
    /// In ICE, this is used for 487 Role Conflict (RFC 8445 §7.3.1.1).
    pub fn is_failed_binding_response(&self) -> bool {
        self.method == Method::Binding && self.class == Class::Failure
    }

    /// Whether this STUN message is an ALLOCATE request (TURN).
    pub fn is_allocate_request(&self) -> bool {
        self.method == Method::Allocate && self.class == Class::Request
    }

    /// Whether this STUN message is a CREATE_PERMISSION request (TURN).
    pub fn is_create_permission_request(&self) -> bool {
        self.method == Method::CreatePermission && self.class == Class::Request
    }

    /// Whether this STUN message is a CHANNEL_BIND request (TURN).
    pub fn is_channel_bind_request(&self) -> bool {
        self.method == Method::ChannelBind && self.class == Class::Request
    }

    /// Whether this STUN message is a REFRESH request (TURN).
    pub fn is_refresh_request(&self) -> bool {
        self.method == Method::Refresh && self.class == Class::Request
    }

    /// Whether this STUN message is a SEND indication (TURN).
    pub fn is_send_indication(&self) -> bool {
        self.method == Method::Send && self.class == Class::Indication
    }

    /// The transaction ID of this STUN message.
    pub fn trans_id(&self) -> TransId {
        self.trans_id
    }

    /// Returns the value of the USERNAME attribute, if present.
    pub fn username(&self) -> Option<&'a str> {
        self.attrs.username
    }

    /// If present, splits the value of the USERNAME attribute into local and remote (separated by `:`).
    pub fn split_username(&self) -> Option<(&str, &str)> {
        self.attrs.split_username()
    }

    /// Returns the value of the XOR-MAPPED-ADDRESS attribute, if present.
    pub fn mapped_address(&self) -> Option<SocketAddr> {
        self.attrs.xor_mapped_address
    }

    /// Returns the value of the PRIORITY attribute (ICE), if present.
    pub fn prio(&self) -> Option<u32> {
        self.attrs.priority
    }

    /// Returns whether the USE-CANDIDATE attribute (ICE) is present.
    pub fn use_candidate(&self) -> bool {
        self.attrs.use_candidate
    }

    /// Returns the value of the ERROR_CODE attribute, if present.
    pub fn error_code(&self) -> Option<(u16, &'a str)> {
        self.attrs.error_code
    }

    /// Returns the value of the CHANNEL_NUMBER attribute (TURN), if present.
    pub fn channel_number(&self) -> Option<u16> {
        self.attrs.channel_number
    }

    /// Returns the value of the LIFETIME attribute (TURN), if present.
    pub fn lifetime(&self) -> Option<u32> {
        self.attrs.lifetime
    }

    /// Returns the value of the XOR_PEER_ADDRESS attribute (TURN), if present.
    pub fn xor_peer_address(&self) -> Option<SocketAddr> {
        self.attrs.xor_peer_address
    }

    /// Returns the value of the DATA attribute (TURN), if present.
    pub fn data(&self) -> Option<&'a [u8]> {
        self.attrs.data
    }

    /// Returns the value of the REALM attribute, if present.
    pub fn realm(&self) -> Option<&'a str> {
        self.attrs.realm
    }

    /// Returns the value of the NONCE attribute, if present.
    pub fn nonce(&self) -> Option<&'a str> {
        self.attrs.nonce
    }

    /// Returns the value of the XOR_RELAYED_ADDRESS attribute (TURN), if present.
    pub fn xor_relayed_address(&self) -> Option<SocketAddr> {
        self.attrs.xor_relayed_address
    }

    /// Returns the value of the SOFTWARE attribute, if present.
    pub fn software(&self) -> Option<&'a str> {
        self.attrs.software
    }

    /// Returns the value of the ICE_CONTROLLED attribute (ICE), if present.
    pub fn ice_controlled(&self) -> Option<u64> {
        self.attrs.ice_controlled
    }

    /// Returns the value of the ICE_CONTROLLING attribute (ICE), if present.
    pub fn ice_controlling(&self) -> Option<u64> {
        self.attrs.ice_controlling
    }

    /// Returns the value of the NETWORK_COST attribute (ICE), if present.
    pub fn network_cost(&self) -> Option<(u16, u16)> {
        self.attrs.network_cost
    }

    /// Constructs a new BINDING request using the provided data.
    pub fn binding_request(
        username: &'a str,
        trans_id: TransId,
        controlling: bool,
        control_tie_breaker: u64,
        prio: u32,
        use_candidate: bool,
    ) -> Self {
        let mut builder = StunMessageBuilder::new()
            .binding()
            .request()
            .username(username)
            .prio(prio);

        if use_candidate {
            builder = builder.use_candidate();
        }

        if controlling {
            builder = builder.ice_controlling(control_tie_breaker);
        } else {
            builder = builder.ice_controlled(control_tie_breaker);
        }

        builder.build(trans_id)
    }

    /// Constructs a new STUN BINDING success reply using the builder.
    pub fn binding_reply(trans_id: TransId, mapped_address: SocketAddr) -> StunMessage<'a> {
        StunMessageBuilder::new()
            .binding()
            .success()
            .xor_mapped_address(mapped_address)
            .build(trans_id)
    }

    /// Constructs a STUN BINDING error reply with a 487 Role Conflict
    /// error code (RFC 8445 §7.3.1.1).
    pub fn binding_role_conflict_reply(trans_id: TransId) -> StunMessage<'a> {
        StunMessageBuilder::new()
            .binding()
            .failure()
            .error_code(487, "Role Conflict")
            .build(trans_id)
    }

    /// Verify the integrity of this message against the provided password.
    #[must_use]
    pub fn verify(&self, password: &[u8], sha1_hmac: impl Fn(&[u8], &[&[u8]]) -> [u8; 20]) -> bool {
        let (Some(integ), Some(typ), Some(rest)) = (
            self.attrs.message_integrity,
            self.integrity.get(..2),
            self.integrity.get(4..),
        ) else {
            return false;
        };

        let comp = sha1_hmac(password, &[typ, &self.integrity_len.to_be_bytes(), rest]);

        comp[..].ct_eq(integ).into()
    }

    /// Serialize this message into the provided buffer, returning the final length of the message.
    ///
    /// The provided password is used to authenticate the message if provided, otherwise no
    /// `MESSAGE-INTEGRITY` attribute will be present.
    pub fn to_bytes(
        self,
        password: Option<&[u8]>,
        buf: &mut [u8],
        sha1_hmac: impl Fn(&[u8], &[&[u8]]) -> [u8; 20],
    ) -> Result<usize, StunError> {
        const MSG_HEADER_LEN: usize = 20;
        const MSG_INTEGRITY_LEN: usize = 20;
        const FPRINT_LEN: usize = 4;
        const ATTR_TLV_LENGTH: usize = 4;

        let include_message_integrity = password.is_some();
        let message_integrity_len = if include_message_integrity {
            MSG_INTEGRITY_LEN + ATTR_TLV_LENGTH
        } else {
            0
        };

        let attr_len =
            self.attrs.padded_len() + message_integrity_len + FPRINT_LEN + ATTR_TLV_LENGTH;

        let mut buf = io::Cursor::new(buf);

        // Message header
        {
            let typ = self.class.to_u16() | self.method.to_u16();
            buf.write_all(&typ.to_be_bytes())?;

            // -8 for fingerprint
            buf.write_all(&((attr_len - 8) as u16).to_be_bytes())?;
            buf.write_all(MAGIC)?;
            buf.write_all(&self.trans_id.0)?;
        }

        // Custom attributes
        self.attrs.to_bytes(&mut buf, &self.trans_id)?;

        if include_message_integrity {
            // Message integrity
            buf.write_all(&Attributes::MESSAGE_INTEGRITY.to_be_bytes())?;
            buf.write_all(&(MSG_INTEGRITY_LEN as u16).to_be_bytes())?;
            buf.write_all(&[0; MSG_INTEGRITY_LEN])?; // placeholder
        }
        let integrity_value_offset = MSG_HEADER_LEN + self.attrs.padded_len() + ATTR_TLV_LENGTH;

        // Fingerprint
        buf.write_all(&Attributes::FINGERPRINT.to_be_bytes())?;
        buf.write_all(&(FPRINT_LEN as u16).to_be_bytes())?;
        buf.write_all(&[0; FPRINT_LEN])?; // placeholder
        let fingerprint_value_offest = integrity_value_offset + message_integrity_len;

        let buf = buf.into_inner();

        if let Some(password) = password {
            // Compute and fill in message integrity
            let hmac = sha1_hmac(
                password,
                &[&buf[0..(integrity_value_offset - ATTR_TLV_LENGTH)]],
            );
            buf[integrity_value_offset..(integrity_value_offset + MSG_INTEGRITY_LEN)]
                .copy_from_slice(&hmac);
        }

        // Fill in total message length
        buf[2..4].copy_from_slice(&(attr_len as u16).to_be_bytes());

        // Compute and fill in fingerprint
        let crc = Crc::<u32>::new(&CRC_32_ISO_HDLC)
            .checksum(&buf[0..(fingerprint_value_offest - ATTR_TLV_LENGTH)])
            ^ 0x5354_554e;
        buf[fingerprint_value_offest..(fingerprint_value_offest + FPRINT_LEN)]
            .copy_from_slice(&crc.to_be_bytes());

        Ok(MSG_HEADER_LEN + attr_len)
    }
}

const MAGIC: &[u8; 4] = &[0x21, 0x12, 0xA4, 0x42];

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Class {
    Request,
    Indication,
    Success,
    Failure,
    Unknown,
}

impl Class {
    fn from_typ(typ: u16) -> Self {
        use Class::*;
        match typ & 0b0000_0001_0001_0000 {
            0b0000_0000_0000_0000 => Request,
            0b0000_0000_0001_0000 => Indication,
            0b0000_0001_0000_0000 => Success,
            0b0000_0001_0001_0000 => Failure,
            _ => Unknown,
        }
    }

    fn to_u16(self) -> u16 {
        use Class::*;
        match self {
            Request => 0b0000_0000_0000_0000,
            Indication => 0b0000_0000_0001_0000,
            Success => 0b0000_0001_0000_0000,
            Failure => 0b0000_0001_0001_0000,
            _ => panic!("Unknown class"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Method {
    Binding,
    // TURN specific
    Allocate,
    Refresh,
    Send,
    Data,
    CreatePermission,
    ChannelBind,
    Unknown,
}

impl Method {
    fn from_typ(typ: u16) -> Self {
        use Method::*;
        match typ & 0b0011_1110_1110_1111 {
            0b0000_0000_0000_0001 => Binding,
            0b0000_0000_0000_0011 => Allocate,
            0b0000_0000_0000_0100 => Refresh,
            0b0000_0000_0000_0110 => Send,
            0b0000_0000_0000_0111 => Data,
            0b0000_0000_0000_1000 => CreatePermission,
            0b0000_0000_0000_1001 => ChannelBind,
            _ => Unknown,
        }
    }

    #[rustfmt::skip]
    fn to_u16(self) -> u16 {
        use Method::*;
        match self {
            Binding          => 0b0000_0000_0000_0001,
            Allocate         => 0b0000_0000_0000_0011,
            Refresh          => 0b0000_0000_0000_0100,
            Send             => 0b0000_0000_0000_0110,
            Data             => 0b0000_0000_0000_0111,
            CreatePermission => 0b0000_0000_0000_1000,
            ChannelBind      => 0b0000_0000_0000_1001,
            _ => panic!("Unknown method"),
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[rustfmt::skip]
pub struct Attributes<'a> {
    username: Option<&'a str>,               // < 128 utf8 chars
    message_integrity: Option<&'a [u8]>,     // 20 bytes sha-1
    error_code: Option<(u16, &'a str)>,      // 300-699 and reason phrase < 128 utf8 chars
    channel_number: Option<u16>,             // 0x000C https://tools.ietf.org/html/rfc5766#section-14.1
    lifetime: Option<u32>,                   // 0x000D https://tools.ietf.org/html/rfc5766#section-14.2
    xor_peer_address: Option<SocketAddr>,    // 0x0012
    data: Option<&'a [u8]>,                  // 0x0013
    realm: Option<&'a str>,                  // < 128 utf8 chars, 0x0014
    nonce: Option<&'a str>,                  // < 128 utf8 chars, 0x0015
    xor_relayed_address: Option<SocketAddr>, // 0x0016
    xor_mapped_address: Option<SocketAddr>,  // 0x0020
    software: Option<&'a str>,               // 0x0022
    fingerprint: Option<u32>,                // crc32
    priority: Option<u32>,                   // 0x0024 https://tools.ietf.org/html/rfc8445
    use_candidate: bool,                     // 0x0025
    ice_controlled: Option<u64>,             // 0x8029
    ice_controlling: Option<u64>,            // 0x802a
    // 0xc057 https://tools.ietf.org/html/draft-thatcher-ice-network-cost-00
    network_cost: Option<(u16, u16)>,
}

impl<'a> fmt::Debug for Attributes<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let debug_struct = &mut f.debug_struct("Attributes");

        if let Some(value) = self.username {
            debug_struct.field("username", &value);
        }
        if let Some(value) = self.message_integrity {
            debug_struct.field("message_integrity", &DebugHex(value));
        }
        if let Some(value) = self.error_code {
            debug_struct.field("error_code", &value);
        }
        if let Some(value) = self.channel_number {
            debug_struct.field("channel_number", &value);
        }
        if let Some(value) = self.lifetime {
            debug_struct.field("lifetime", &value);
        }
        if let Some(value) = self.xor_peer_address {
            debug_struct.field("xor_peer_address", &value);
        }
        if let Some(value) = self.data {
            debug_struct.field("data", &value);
        }
        if let Some(value) = self.realm {
            debug_struct.field("realm", &value);
        }
        if let Some(value) = self.nonce {
            debug_struct.field("nonce", &value);
        }
        if let Some(value) = self.xor_relayed_address {
            debug_struct.field("xor_relayed_address", &value);
        }
        if let Some(value) = self.xor_mapped_address {
            debug_struct.field("xor_mapped_address", &value);
        }
        if let Some(value) = self.software {
            debug_struct.field("software", &value);
        }
        if let Some(value) = self.fingerprint {
            debug_struct.field("fingerprint", &value);
        }
        if let Some(value) = self.priority {
            debug_struct.field("priority", &value);
        }
        if self.use_candidate {
            debug_struct.field("use_candidate", &true);
        }
        if let Some(value) = self.ice_controlled {
            debug_struct.field("ice_controlled", &value);
        }
        if let Some(value) = self.ice_controlling {
            debug_struct.field("ice_controlling", &value);
        }
        if let Some(value) = self.network_cost {
            debug_struct.field("network_cost", &value);
        }

        debug_struct.finish()
    }
}

impl<'a> Attributes<'a> {
    fn split_username(&self) -> Option<(&'a str, &'a str)> {
        // usernames are on the form gfNK:062g where
        // gfNK is my local sdp ice username and
        // 062g is the remote.

        let v = self.username?;
        let idx = v.find(':')?;

        if idx + 1 >= v.len() {
            return None;
        }

        let local = &v[..idx];
        let remote = &v[(idx + 1)..];

        Some((local, remote))
    }
}

use std::{io, str};

use str0m_proto::NonCryptographicRng;

const PAD: [u8; 4] = [0, 0, 0, 0];
impl<'a> Attributes<'a> {
    const ALTERNATE_SERVER: u16 = 0x8023;
    const FINGERPRINT: u16 = 0x8028;
    const ICE_CONTROLLED: u16 = 0x8029;
    const ICE_CONTROLLING: u16 = 0x802a;

    const MAPPED_ADDRESS: u16 = 0x0001;
    const USERNAME: u16 = 0x0006;
    const MESSAGE_INTEGRITY: u16 = 0x0008;
    const ERROR_CODE: u16 = 0x0009;
    const UNKNOWN_ATTRIBUTES: u16 = 0x000a;
    const CHANNEL_NUMBER: u16 = 0x000c;
    const LIFETIME: u16 = 0x000d;
    const XOR_PEER_ADDRESS: u16 = 0x0012;
    const DATA: u16 = 0x0013;
    const REALM: u16 = 0x0014;
    const NONCE: u16 = 0x0015;
    const XOR_RELAYED_ADDRESS: u16 = 0x0016;
    const XOR_MAPPED_ADDRESS: u16 = 0x0020;
    const SOFTWARE: u16 = 0x0022;
    const PRIORITY: u16 = 0x0024;
    const USE_CANDIDATE: u16 = 0x0025;

    const NETWORK_COST: u16 = 0xc057;

    fn padded_len(&self) -> usize {
        const ATTR_TLV_LENGTH: usize = 4;

        let username = self
            .username
            .map(|v| ATTR_TLV_LENGTH + v.len() + calculate_pad(v.len()))
            .unwrap_or_default();
        let ice_controlled = self
            .ice_controlled
            .map(|_| ATTR_TLV_LENGTH + 8)
            .unwrap_or_default();
        let ice_controlling = self
            .ice_controlling
            .map(|_| ATTR_TLV_LENGTH + 8)
            .unwrap_or_default();
        let priority = self
            .priority
            .map(|p| ATTR_TLV_LENGTH + p.to_le_bytes().len())
            .unwrap_or_default();
        let address = self
            .xor_mapped_address
            .map(|a| ATTR_TLV_LENGTH + if a.is_ipv4() { 8 } else { 20 })
            .unwrap_or_default();
        let use_candidate = if self.use_candidate {
            ATTR_TLV_LENGTH
        } else {
            0
        };
        let xor_peer_address = self
            .xor_peer_address
            .map(|a| ATTR_TLV_LENGTH + if a.is_ipv4() { 8 } else { 20 })
            .unwrap_or_default();
        let xor_relayed_address = self
            .xor_relayed_address
            .map(|a| ATTR_TLV_LENGTH + if a.is_ipv4() { 8 } else { 20 })
            .unwrap_or_default();
        let data = self
            .data
            .map(|d| ATTR_TLV_LENGTH + d.len() + calculate_pad(d.len()))
            .unwrap_or_default();
        let channel_number = self
            .channel_number
            .map(|_| ATTR_TLV_LENGTH + 4)
            .unwrap_or_default();
        let lifetime = self
            .lifetime
            .map(|_| ATTR_TLV_LENGTH + 4)
            .unwrap_or_default();
        let realm = self
            .realm
            .map(|v| ATTR_TLV_LENGTH + v.len() + calculate_pad(v.len()))
            .unwrap_or_default();
        let nonce = self
            .nonce
            .map(|v| ATTR_TLV_LENGTH + v.len() + calculate_pad(v.len()))
            .unwrap_or_default();
        let error_code = self
            .error_code
            .map(|(_, reason)| ATTR_TLV_LENGTH + 4 + reason.len() + calculate_pad(reason.len()))
            .unwrap_or_default();

        username
            + ice_controlled
            + ice_controlling
            + priority
            + address
            + use_candidate
            + xor_peer_address
            + xor_relayed_address
            + data
            + channel_number
            + lifetime
            + realm
            + nonce
            + error_code
    }

    fn to_bytes(self, out: &mut dyn Write, trans_id: &TransId) -> io::Result<()> {
        if let Some(v) = self.username {
            out.write_all(&Self::USERNAME.to_be_bytes())?;
            encode_str(Self::USERNAME, v, out)?;
            let pad = calculate_pad(v.len());
            out.write_all(&PAD[0..pad])?;
        }
        if let Some(v) = self.ice_controlled {
            out.write_all(&Self::ICE_CONTROLLED.to_be_bytes())?;
            out.write_all(&8_u16.to_be_bytes())?;
            out.write_all(&v.to_be_bytes())?;
        }
        if let Some(v) = self.ice_controlling {
            out.write_all(&Self::ICE_CONTROLLING.to_be_bytes())?;
            out.write_all(&8_u16.to_be_bytes())?;
            out.write_all(&v.to_be_bytes())?;
        }
        if let Some(v) = self.priority {
            out.write_all(&Self::PRIORITY.to_be_bytes())?;
            out.write_all(&4_u16.to_be_bytes())?;
            out.write_all(&v.to_be_bytes())?;
        }
        if let Some(v) = self.xor_mapped_address {
            let mut buf = [0_u8; 20];
            let len = encode_xor(v, &mut buf, trans_id);
            out.write_all(&Self::XOR_MAPPED_ADDRESS.to_be_bytes())?;
            out.write_all(&((len as u16).to_be_bytes()))?;
            out.write_all(&buf[0..len])?;
        }
        if self.use_candidate {
            out.write_all(&Self::USE_CANDIDATE.to_be_bytes())?;
            out.write_all(&0_u16.to_be_bytes())?;
        }
        if let Some(d) = self.data {
            if d.len() > u16::MAX as usize {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Data attribute too long, max 65535 bytes",
                ));
            }

            out.write_all(&Self::DATA.to_be_bytes())?;
            out.write_all(&(d.len() as u16).to_be_bytes())?;
            out.write_all(d)?;

            let pad = calculate_pad(d.len());
            out.write_all(&PAD[0..pad])?;
        }
        if let Some(v) = self.xor_relayed_address {
            let mut buf = [0_u8; 20];
            let len = encode_xor(v, &mut buf, trans_id);
            out.write_all(&Self::XOR_RELAYED_ADDRESS.to_be_bytes())?;
            out.write_all(&((len as u16).to_be_bytes()))?;
            out.write_all(&buf[0..len])?;
        }
        if let Some(v) = self.xor_peer_address {
            let mut buf = [0_u8; 20];
            let len = encode_xor(v, &mut buf, trans_id);
            out.write_all(&Self::XOR_PEER_ADDRESS.to_be_bytes())?;
            out.write_all(&((len as u16).to_be_bytes()))?;
            out.write_all(&buf[0..len])?;
        }
        if let Some(v) = self.channel_number {
            out.write_all(&Self::CHANNEL_NUMBER.to_be_bytes())?;
            out.write_all(&4_u16.to_be_bytes())?;
            out.write_all(&v.to_be_bytes())?;
            // RFU bytes
            out.write_all(&[0, 0])?;
        }
        if let Some(v) = self.lifetime {
            out.write_all(&Self::LIFETIME.to_be_bytes())?;
            out.write_all(&4_u16.to_be_bytes())?;
            out.write_all(&v.to_be_bytes())?;
        }
        if let Some(v) = self.realm {
            out.write_all(&Self::REALM.to_be_bytes())?;
            encode_str(Self::REALM, v, out)?;
            let pad = calculate_pad(v.len());
            out.write_all(&PAD[0..pad])?;
        }
        if let Some(v) = self.nonce {
            out.write_all(&Self::NONCE.to_be_bytes())?;
            encode_str(Self::NONCE, v, out)?;
            let pad = calculate_pad(v.len());
            out.write_all(&PAD[0..pad])?;
        }
        if let Some((code, reason)) = self.error_code {
            out.write_all(&Self::ERROR_CODE.to_be_bytes())?;
            // Length
            out.write_all(&(4_u16 + reason.len() as u16).to_be_bytes())?;
            // Reserved 16 bits
            out.write_all(&((0_u16).to_be_bytes()))?;
            // Reserved 5 high bits, class 3 bits
            out.write_all(&((0x7_u8 & (code / 100) as u8).to_be_bytes()))?;
            // code 8 bits
            out.write_all(&(((code % 100) as u8).to_be_bytes()))?;
            // Total written 8 bytes 4 byte aligned
            encode_str_no_len(Self::ERROR_CODE, reason, out)?;

            // Need to ensure padding is correct only with respect to reason since the
            // prior length was 4 byte aligned.
            let pad = calculate_pad(reason.len());
            out.write_all(&PAD[0..pad])?;
        }

        Ok(())
    }

    fn parse(
        mut buf: &'a [u8],
        trans_id: TransId,
        msg_integrity_off: &mut Option<usize>,
    ) -> Result<Attributes<'a>, StunError> {
        let mut attributes = Attributes::default();

        let mut off = 0;
        // With the exception of the FINGERPRINT
        //    attribute, which appears after MESSAGE-INTEGRITY, agents MUST ignore
        //    all other attributes that follow MESSAGE-INTEGRITY
        let mut ignore_rest = false;
        loop {
            if buf.is_empty() {
                break;
            }
            let typ = u16::from_le_bytes([buf[1], buf[0]]);
            let len = u16::from_le_bytes([buf[3], buf[2]]) as usize;
            // trace!(
            //     "STUN attribute typ 0x{:04x?} len {}: {:02x?}",
            //     typ,
            //     len,
            //     buf
            // );
            if len > buf.len() - 4 {
                return Err(StunError::Parse(format!(
                    "Bad STUN attribute length: {} > {}",
                    len,
                    buf.len() - 4,
                )));
            }
            if !ignore_rest || typ == Self::FINGERPRINT {
                match typ {
                    Self::MAPPED_ADDRESS => {
                        warn!("STUN got MappedAddress");
                    }
                    Self::USERNAME => {
                        // It MUST contain a UTF-8 [RFC3629] encoded sequence of less than 513 bytes
                        attributes.username = Some(decode_str(typ, 513, &buf[4..], len)?);
                    }
                    Self::MESSAGE_INTEGRITY => {
                        if len != 20 {
                            return Err(StunError::Parse(
                                "Expected message integrity to have length 20".into(),
                            ));
                        }
                        // message integrity is up until, but not including the message
                        // integrity attribute.
                        *msg_integrity_off = Some(off);
                        ignore_rest = true;
                        attributes.message_integrity = Some(&buf[4..24]);
                    }
                    Self::ERROR_CODE => {
                        if len < 4 {
                            return Err(StunError::Parse(
                                "Error code length must be at least 4".into(),
                            ));
                        }

                        if buf[4] != 0 || buf[5] != 0 || buf[6] & 0b1111_1000 != 0 {
                            return Err(StunError::Parse("Expected 0 at top of error code".into()));
                        }
                        let class = buf[6] as u16 * 100;
                        if !(300..=699).contains(&class) {
                            return Err(StunError::Parse(format!(
                                "Error class is not in range: {class}"
                            )));
                        }
                        let code = class + (buf[7] % 100) as u16;
                        // The reason phrase MUST be a UTF-8 [RFC3629] encoded sequence of less
                        // than 128 characters (which can be as long as 763 bytes).
                        attributes.error_code =
                            Some((code, decode_str(typ, 763, &buf[8..], len - 4)?));
                    }
                    Self::CHANNEL_NUMBER => {
                        if len != 4 {
                            return Err(StunError::Parse(format!(
                                "Channel number that isn't 4 in length, was {}",
                                len
                            )));
                        }
                        let bytes = [buf[4], buf[5]];
                        let channel = u16::from_be_bytes(bytes);
                        // Channel numbers must be between 0x4000 and 0x7FFF inclusive
                        // https://tools.ietf.org/html/rfc5766#section-2.5
                        if !(0x4000..=0x7FFF).contains(&channel) {
                            return Err(StunError::Parse(format!(
                                "Channel number {channel} is not in valid range 0x4000-0x7FFF"
                            )));
                        }
                        attributes.channel_number = Some(channel);
                    }
                    Self::LIFETIME => {
                        if len != 4 {
                            return Err(StunError::Parse("Lifetime that isn't 4 in length".into()));
                        }
                        let bytes = [buf[4], buf[5], buf[6], buf[7]];
                        attributes.lifetime = Some(u32::from_be_bytes(bytes));
                    }
                    Self::UNKNOWN_ATTRIBUTES => {
                        warn!("STUN got UnknownAttributes");
                    }
                    Self::XOR_PEER_ADDRESS => {
                        attributes.xor_peer_address = Some(decode_xor(&buf[4..], trans_id)?)
                    }
                    Self::DATA => {
                        attributes.data = Some(&buf[4..len + 4]);
                    }
                    Self::REALM => {
                        // It MUST be a UTF-8 [RFC3629] encoded sequence of less than
                        // 128 characters (which can be as long as 763 bytes)
                        attributes.realm = Some(decode_str(typ, 763, &buf[4..], len)?);
                    }
                    Self::NONCE => {
                        // It MUST be less than 128 characters (which can be as long as 763 bytes).
                        attributes.nonce = Some(decode_str(typ, 763, &buf[4..], len)?);
                    }
                    Self::XOR_RELAYED_ADDRESS => {
                        attributes.xor_relayed_address = Some(decode_xor(&buf[4..], trans_id)?);
                    }
                    Self::XOR_MAPPED_ADDRESS => {
                        attributes.xor_mapped_address = Some(decode_xor(&buf[4..], trans_id)?);
                    }
                    Self::SOFTWARE => {
                        // It MUST be a UTF-8 [RFC3629] encoded sequence of less than
                        // 128 characters (which can be as long as 763 bytes)
                        attributes.software = Some(decode_str(typ, 763, &buf[4..], len)?);
                    }
                    Self::PRIORITY => {
                        if len != 4 {
                            return Err(StunError::Parse("Priority that isnt 4 in length".into()));
                        }
                        let bytes = [buf[4], buf[5], buf[6], buf[7]];
                        attributes.priority = Some(u32::from_be_bytes(bytes));
                    }
                    Self::USE_CANDIDATE => {
                        if len != 0 {
                            return Err(StunError::Parse(
                                "UseCandidate that isnt 0 in length".into(),
                            ));
                        }
                        attributes.use_candidate = true;
                    }
                    Self::ALTERNATE_SERVER => {
                        warn!("STUN got AlternateServer");
                    }
                    Self::FINGERPRINT => {
                        if len != 4 {
                            return Err(StunError::Parse("Fingerprint length must be 4".into()));
                        }
                        let bytes = [buf[4], buf[5], buf[6], buf[7]];
                        attributes.fingerprint = Some(u32::from_be_bytes(bytes));
                    }
                    Self::ICE_CONTROLLED => {
                        if len != 8 {
                            return Err(StunError::Parse(
                                "IceControlled that isnt 8 in length".into(),
                            ));
                        }
                        let mut bytes = [0_u8; 8];
                        bytes.copy_from_slice(&buf[4..(4 + 8)]);
                        attributes.ice_controlled = Some(u64::from_be_bytes(bytes));
                    }
                    Self::ICE_CONTROLLING => {
                        if len != 8 {
                            return Err(StunError::Parse(
                                "IceControlling that isnt 8 in length".into(),
                            ));
                        }
                        let mut bytes = [0_u8; 8];
                        bytes.copy_from_slice(&buf[4..(4 + 8)]);
                        attributes.ice_controlling = Some(u64::from_be_bytes(bytes));
                    }
                    Self::NETWORK_COST => {
                        if len != 4 {
                            warn!("NetworkCost that isnt 4 in length");
                        } else {
                            let net_id = (buf[4] as u16) << 8 | buf[5] as u16;
                            let cost = (buf[6] as u16) << 8 | buf[7] as u16;
                            attributes.network_cost = Some((net_id, cost));
                        }
                    }
                    _ => {}
                }
            }
            // attributes are on even 32 bit boundaries
            let pad = calculate_pad(len);
            let pad_len = len + pad;
            buf = &buf[(4 + pad_len)..];
            off += 4 + pad_len;
        }
        Ok(attributes)
    }
}

fn calculate_pad(len: usize) -> usize {
    (4 - (len % 4)) % 4
}

fn encode_str(typ: u16, s: &str, out: &mut dyn Write) -> io::Result<()> {
    out.write_all(&(s.len() as u16).to_be_bytes())?;

    encode_str_no_len(typ, s, out)
}

fn encode_str_no_len(typ: u16, s: &str, out: &mut dyn Write) -> io::Result<()> {
    if s.len() > 128 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("0x{typ:04x?} too long str len: {}", s.len()),
        ));
    }
    out.write_all(s.as_bytes())?;
    Ok(())
}

fn decode_str(typ: u16, max_bytes: usize, buf: &[u8], len: usize) -> Result<&str, StunError> {
    if len > max_bytes {
        return Err(StunError::Parse(format!(
            "0x{typ:04x?} too long str len: {len} (max {max_bytes})"
        )));
    }
    match str::from_utf8(&buf[0..len]).ok() {
        Some(v) => Ok(v),
        None => Err(StunError::Parse(format!("0x{typ:04x?} malformed utf-8"))),
    }
}

fn encode_xor(addr: SocketAddr, buf: &mut [u8; 20], trans_id: &TransId) -> usize {
    let port = addr.port() ^ 0x2112;
    buf[2..4].copy_from_slice(&port.to_be_bytes());
    buf[1] = if addr.is_ipv4() { 1 } else { 2 };
    let ip_buf = &mut buf[4..];
    match addr {
        SocketAddr::V4(v) => {
            let bytes = v.ip().octets();
            for i in 0..4 {
                ip_buf[i] = bytes[i] ^ MAGIC[i];
            }
            8
        }
        SocketAddr::V6(v) => {
            let bytes = v.ip().octets();
            for i in 0..4 {
                ip_buf[i] = bytes[i] ^ MAGIC[i];
            }
            for i in 4..16 {
                ip_buf[i] = bytes[i] ^ trans_id.0[i - 4];
            }
            20
        }
    }
}

fn decode_xor(buf: &[u8], trans_id: TransId) -> Result<SocketAddr, StunError> {
    if buf.len() < 4 {
        return Err(StunError::Parse(format!(
            "XOR buffer is too short: {} < 4",
            buf.len()
        )));
    }
    let port = u16::from_be_bytes([buf[2], buf[3]]) ^ 0x2112;
    let ip_buf = &buf[4..];
    let ip = match buf[1] {
        1 if ip_buf.len() >= 4 => {
            let mut bytes = [0_u8; 4];
            for i in 0..4 {
                bytes[i] = ip_buf[i] ^ MAGIC[i];
            }
            IpAddr::V4(bytes.into())
        }
        2 if ip_buf.len() >= 16 => {
            let mut bytes = [0_u8; 16];
            for i in 0..4 {
                bytes[i] = ip_buf[i] ^ MAGIC[i];
            }
            for i in 4..16 {
                bytes[i] = ip_buf[i] ^ trans_id.0[i - 4];
            }
            IpAddr::V6(bytes.into())
        }
        v => {
            return Err(StunError::Parse(format!(
                "Unknown IP version ({v}) or insufficient buffer length for it ({})",
                ip_buf.len()
            )));
        }
    };

    Ok(SocketAddr::new(ip, port))
}

impl<'a> fmt::Debug for StunMessage<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StunMessage")
            .field("method", &self.method)
            .field("class", &self.class)
            .field("id", &self.trans_id)
            .field("attrs", &self.attrs)
            .field("integrity_len", &self.integrity.len())
            .finish()
    }
}

pub use builder::Builder as StunMessageBuilder;

mod builder {
    use super::{Attributes, Class, Method, StunMessage, TransId};
    use std::net::SocketAddr;

    /// Type state representing a builder where the STUN method has not yet been set.
    #[doc(hidden)]
    #[derive(Default, Debug, Clone)]
    pub struct NoMethod;

    /// Type state representing a builder where the STUN method has been set, but the class has not.
    #[doc(hidden)]
    #[derive(Debug, Clone)]
    pub struct HasMethod {
        method: Method,
    }

    /// Type state representing a builder where both the STUN method and class have been set.
    /// Attributes can now be added.
    #[doc(hidden)]
    #[derive(Debug, Clone)]
    pub struct HasClass {
        method: Method,
        class: Class,
    }

    /// A type-state builder for creating [`StunMessage`] instances.
    ///
    /// This builder guides the user through the required steps:
    /// 1. Set the STUN method (e.g., `binding()`, `allocate()`).
    /// 2. Set the STUN class (e.g., `request()`, `success()`).
    /// 3. Optionally set attributes (e.g., `username()`, `priority()`).
    /// 4. Call `build()` with a [`TransId`].
    #[derive(Default, Debug, Clone)]
    pub struct Builder<'a, State> {
        attrs: Attributes<'a>,
        state: State,
    }

    impl<'a> Builder<'a, NoMethod> {
        /// Creates a new STUN message builder, starting in the initial state
        /// where the method needs to be set.
        pub fn new() -> Self {
            Builder {
                attrs: Attributes::default(),
                state: NoMethod,
            }
        }
    }

    // Method Setters (Transition from NoMethod to HasMethod) ---
    impl<'a> Builder<'a, NoMethod> {
        fn set_method(self, method: Method) -> Builder<'a, HasMethod> {
            Builder {
                attrs: self.attrs,
                state: HasMethod { method },
            }
        }

        /// Sets the STUN method to BINDING.
        pub fn binding(self) -> Builder<'a, HasMethod> {
            self.set_method(Method::Binding)
        }

        /// Sets the STUN method to ALLOCATE (TURN).
        pub fn allocate(self) -> Builder<'a, HasMethod> {
            self.set_method(Method::Allocate)
        }

        /// Sets the STUN method to REFRESH (TURN).
        pub fn refresh(self) -> Builder<'a, HasMethod> {
            self.set_method(Method::Refresh)
        }

        /// Sets the STUN method to SEND (TURN).
        pub fn send(self) -> Builder<'a, HasMethod> {
            self.set_method(Method::Send)
        }

        /// Sets the STUN method to DATA (TURN).
        pub fn data(self) -> Builder<'a, HasMethod> {
            self.set_method(Method::Data)
        }

        /// Sets the STUN method to CREATE_PERMISSION (TURN).
        pub fn create_permission(self) -> Builder<'a, HasMethod> {
            self.set_method(Method::CreatePermission)
        }

        /// Sets the STUN method to CHANNEL_BIND (TURN).
        pub fn channel_bind(self) -> Builder<'a, HasMethod> {
            self.set_method(Method::ChannelBind)
        }
    }

    // Class Setters (Transition from HasMethod to HasClass) ---
    impl<'a> Builder<'a, HasMethod> {
        fn set_class(self, class: Class) -> Builder<'a, HasClass> {
            Builder {
                attrs: self.attrs,
                state: HasClass {
                    method: self.state.method,
                    class,
                },
            }
        }

        /// Sets the STUN class to Request.
        pub fn request(self) -> Builder<'a, HasClass> {
            self.set_class(Class::Request)
        }

        /// Sets the STUN class to Indication.
        pub fn indication(self) -> Builder<'a, HasClass> {
            self.set_class(Class::Indication)
        }

        /// Sets the STUN class to Success Response.
        pub fn success(self) -> Builder<'a, HasClass> {
            self.set_class(Class::Success)
        }

        /// Sets the STUN class to Error Response.
        pub fn failure(self) -> Builder<'a, HasClass> {
            self.set_class(Class::Failure)
        }
    }

    // Attribute Setters (Only on HasClass state) ---
    impl<'a> Builder<'a, HasClass> {
        /// Sets the USERNAME attribute.
        pub fn username(mut self, username: &'a str) -> Self {
            self.attrs.username = Some(username);
            self
        }

        /// Sets the ERROR_CODE attribute.
        pub fn error_code(mut self, code: u16, reason: &'a str) -> Self {
            self.attrs.error_code = Some((code, reason));
            self
        }

        /// Sets the CHANNEL_NUMBER attribute (TURN).
        pub fn channel_number(mut self, number: u16) -> Self {
            self.attrs.channel_number = Some(number);
            self
        }

        /// Sets the LIFETIME attribute (TURN).
        pub fn lifetime(mut self, lifetime: u32) -> Self {
            self.attrs.lifetime = Some(lifetime);
            self
        }

        /// Sets the XOR_PEER_ADDRESS attribute (TURN).
        pub fn xor_peer_address(mut self, addr: SocketAddr) -> Self {
            self.attrs.xor_peer_address = Some(addr);
            self
        }

        /// Sets the DATA attribute (TURN).
        pub fn data(mut self, data: &'a [u8]) -> Self {
            self.attrs.data = Some(data);
            self
        }

        /// Sets the REALM attribute.
        pub fn realm(mut self, realm: &'a str) -> Self {
            self.attrs.realm = Some(realm);
            self
        }

        /// Sets the NONCE attribute.
        pub fn nonce(mut self, nonce: &'a str) -> Self {
            self.attrs.nonce = Some(nonce);
            self
        }

        /// Sets the XOR_RELAYED_ADDRESS attribute (TURN).
        pub fn xor_relayed_address(mut self, addr: SocketAddr) -> Self {
            self.attrs.xor_relayed_address = Some(addr);
            self
        }

        /// Sets the XOR_MAPPED_ADDRESS attribute.
        pub fn xor_mapped_address(mut self, addr: SocketAddr) -> Self {
            self.attrs.xor_mapped_address = Some(addr);
            self
        }

        /// Sets the SOFTWARE attribute.
        pub fn software(mut self, software: &'a str) -> Self {
            self.attrs.software = Some(software);
            self
        }

        /// Sets the PRIORITY attribute (ICE).
        pub fn prio(mut self, prio: u32) -> Self {
            self.attrs.priority = Some(prio);
            self
        }

        /// Adds the USE_CANDIDATE attribute (ICE).
        pub fn use_candidate(mut self) -> Self {
            self.attrs.use_candidate = true;
            self
        }

        /// Sets the ICE_CONTROLLED attribute (ICE).
        pub fn ice_controlled(mut self, tie_breaker: u64) -> Self {
            self.attrs.ice_controlled = Some(tie_breaker);
            self
        }

        /// Sets the ICE_CONTROLLING attribute (ICE).
        pub fn ice_controlling(mut self, tie_breaker: u64) -> Self {
            self.attrs.ice_controlling = Some(tie_breaker);
            self
        }

        /// Sets the NETWORK_COST attribute (ICE).
        pub fn network_cost(mut self, net_id: u16, cost: u16) -> Self {
            self.attrs.network_cost = Some((net_id, cost));
            self
        }

        /// Builds the final [`StunMessage`].
        ///
        /// This method consumes the builder and requires a transaction ID.
        /// Note that `MESSAGE_INTEGRITY` and `FINGERPRINT` attributes are not
        /// added here; they are calculated and added during serialization in
        /// [`StunMessage::to_bytes()`].
        pub fn build(self, trans_id: TransId) -> StunMessage<'a> {
            StunMessage {
                method: self.state.method,
                class: self.state.class,
                trans_id,
                attrs: self.attrs,
                integrity: &[],   // Calculated during serialization
                integrity_len: 0, // Calculated during serialization
            }
        }
    }
}

struct DebugHex<'a>(&'a [u8]);

impl fmt::Debug for DebugHex<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{:x}", b)?;
        }

        Ok(())
    }
}

/// An incoming STUN packet.
#[derive(Debug)]
pub struct StunPacket<'a> {
    /// The protocol the socket this received data originated from is using.
    pub proto: str0m_proto::Protocol,
    /// The socket this received data originated from.
    pub source: SocketAddr,
    /// The destination socket of the datagram.
    pub destination: SocketAddr,
    /// The STUN message.
    pub message: StunMessage<'a>,
}

#[cfg(test)]
mod test {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

    fn sha1_hmac(key: &[u8], payloads: &[&[u8]]) -> [u8; 20] {
        use hmac::{Hmac, KeyInit, Mac};
        use sha1_::Sha1;
        let mut mac = Hmac::<Sha1>::new_from_slice(key).unwrap();
        for p in payloads {
            mac.update(p);
        }
        mac.finalize().into_bytes().into()
    }

    #[test]
    fn parse_stun_message() {
        const PACKET: &[u8] = &[
            0x00, 0x01, 0x00, 0x50, 0x21, 0x12, 0xa4, 0x42, 0x6a, 0x75, 0x63, 0x31, 0x35, 0x75,
            0x78, 0x55, 0x6e, 0x67, 0x47, 0x63, 0x00, 0x06, 0x00, 0x09, 0x70, 0x39, 0x4b, 0x41,
            0x3a, 0x53, 0x51, 0x41, 0x74, 0x00, 0x00, 0x00, 0xc0, 0x57, 0x00, 0x04, 0x00, 0x01,
            0x00, 0x0a, 0x80, 0x2a, 0x00, 0x08, 0x6e, 0xee, 0xc6, 0xe9, 0x7d, 0x18, 0x39, 0x5c,
            0x00, 0x25, 0x00, 0x00, 0x00, 0x24, 0x00, 0x04, 0x6e, 0x7f, 0x1e, 0xff, 0x00, 0x08,
            0x00, 0x14, 0x5d, 0x04, 0x25, 0xa0, 0x20, 0x7a, 0xb1, 0xe0, 0x54, 0x10, 0x22, 0x99,
            0xaa, 0xf9, 0x83, 0x9c, 0xa0, 0x76, 0xc6, 0xd5, 0x80, 0x28, 0x00, 0x04, 0x36, 0x0e,
            0x21, 0x9f,
        ];

        let packet = PACKET.to_vec();
        let message = StunMessage::parse(&packet).unwrap();
        assert!(message.verify(b"xJcE9AQAR7kczUDVOXRUCl", sha1_hmac));
    }

    #[test]
    fn minimal_debug_print() {
        let attrs = Attributes {
            username: Some("foo"),
            ..Default::default()
        };

        let dbg_print = format!("{attrs:?}");

        assert_eq!(dbg_print, r#"Attributes { username: "foo" }"#);
    }

    // README: IF YOU NEED TO ADJUST THIS TEST BECAUSE YOU ADDED AN ATTRIBUTE,
    // MAKE SURE TO ADJUST THE `fmt::Debug` impl.
    #[test]
    fn all_attributes_are_printed() {
        let attrs = Attributes {
            username: Some("foo"),
            message_integrity: Some(b"0000"),
            error_code: Some((401, "Unauthorized")),
            channel_number: Some(0x4000),
            lifetime: Some(3600),
            xor_peer_address: Some(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))),
            data: Some(&[0xDE, 0xAD, 0xBE, 0xEF]),
            realm: Some("baz"),
            nonce: Some("abcd"),
            xor_relayed_address: Some(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))),
            xor_mapped_address: Some(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))),
            software: Some("str0m"),
            fingerprint: Some(9999),
            priority: Some(1),
            use_candidate: true,
            ice_controlled: Some(10),
            ice_controlling: Some(100),
            network_cost: Some((10, 10)),
        };

        let dbg_print = format!("{attrs:?}");

        assert_eq!(
            dbg_print,
            "\
Attributes { \
username: \"foo\", \
message_integrity: 30303030, \
error_code: (401, \"Unauthorized\"), \
channel_number: 16384, \
lifetime: 3600, \
xor_peer_address: 127.0.0.1:0, \
data: [222, 173, 190, 239], \
realm: \"baz\", \
nonce: \"abcd\", \
xor_relayed_address: 127.0.0.1:0, \
xor_mapped_address: 127.0.0.1:0, \
software: \"str0m\", \
fingerprint: 9999, \
priority: 1, \
use_candidate: true, \
ice_controlled: 10, \
ice_controlling: 100, \
network_cost: (10, 10) \
}"
        );
    }

    #[test]
    fn test_username_4_bytes_no_padding() {
        let attrs = Attributes {
            username: Some("abcd"),
            ..Default::default()
        };
        let mut buf = vec![];
        let trans_id = TransId::new();
        attrs
            .to_bytes(&mut buf, &trans_id)
            .expect("To serialize attributes");
        assert_eq!(
            buf.len(),
            8,
            "A 4 byte username attribute should be 8 bytes, 4 for TVL and 4 for the username"
        );
    }

    #[test]
    fn parse_zero_length_buffer() {
        let result = StunMessage::parse(&[]);

        assert!(result.is_err());
    }

    #[test]
    fn parse_allocate_request() {
        let trans_id = TransId::new();
        let message = StunMessage {
            method: Method::Allocate,
            class: Class::Request,
            trans_id,
            attrs: Attributes {
                username: Some("user"),
                realm: Some("example.org"),
                nonce: Some("dcd98b7102dd2f0e8b11d0f600bfb0c093"),
                lifetime: Some(3600),
                ..Default::default()
            },
            integrity: &[],
            integrity_len: 0,
        };

        let mut buf = [0u8; 1024];
        let len = message
            .to_bytes(Some(b"password"), &mut buf, sha1_hmac)
            .unwrap();
        let serialized = &buf[..len];

        let parsed = StunMessage::parse(serialized).unwrap();

        assert_eq!(parsed.method, Method::Allocate);
        assert_eq!(parsed.class, Class::Request);
        assert_eq!(parsed.trans_id, trans_id);
        assert_eq!(parsed.attrs.username, Some("user"));
        assert_eq!(parsed.attrs.realm, Some("example.org"));
        assert_eq!(
            parsed.attrs.nonce,
            Some("dcd98b7102dd2f0e8b11d0f600bfb0c093")
        );
        assert_eq!(parsed.attrs.lifetime, Some(3600));
        assert!(parsed.verify(b"password", sha1_hmac));
    }

    #[test]
    fn parse_allocate_response() {
        let trans_id = TransId::new();
        let relayed_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 1234));
        let message = StunMessage {
            method: Method::Allocate,
            class: Class::Success,
            trans_id,
            attrs: Attributes {
                xor_relayed_address: Some(relayed_addr),
                lifetime: Some(1800),
                ..Default::default()
            },
            integrity: &[],
            integrity_len: 0,
        };

        let mut buf = [0u8; 1024];
        let len = message
            .to_bytes(Some(b"password"), &mut buf, sha1_hmac)
            .unwrap();
        let serialized = &buf[..len];

        let parsed = StunMessage::parse(serialized).unwrap();

        assert_eq!(parsed.method, Method::Allocate);
        assert_eq!(parsed.class, Class::Success);
        assert_eq!(parsed.trans_id, trans_id);
        assert_eq!(parsed.attrs.xor_relayed_address, Some(relayed_addr));
        assert_eq!(parsed.attrs.lifetime, Some(1800));
        assert!(parsed.verify(b"password", sha1_hmac));
    }

    #[test]
    fn parse_allocate_failure_no_integrity() {
        let trans_id = TransId::new();
        let message = StunMessage {
            method: Method::Allocate,
            class: Class::Failure,
            trans_id,
            attrs: Attributes {
                error_code: Some((401, "Unauthorized")),
                realm: Some("example.org"),
                nonce: Some("dcd98b7102dd2f0e8b11d0f600bfb0c093"),
                ..Default::default()
            },
            integrity: &[],
            integrity_len: 0,
        };

        let mut buf = [0u8; 1024];
        let len = message.to_bytes(None, &mut buf, sha1_hmac).unwrap();
        let serialized = &buf[..len];

        let parsed = StunMessage::parse(serialized).unwrap();

        assert_eq!(parsed.method, Method::Allocate);
        assert_eq!(parsed.class, Class::Failure);
        assert_eq!(parsed.trans_id, trans_id);
        assert_eq!(parsed.attrs.error_code, Some((401, "Unauthorized")));
        assert_eq!(parsed.attrs.realm, Some("example.org"));
        assert_eq!(
            parsed.attrs.nonce,
            Some("dcd98b7102dd2f0e8b11d0f600bfb0c093")
        );
    }

    #[test]
    fn parse_send_request() {
        // Data length of 3 bytes (0xDEADBE) requires 1 byte of padding
        let trans_id = TransId::new();
        let message = StunMessage {
            method: Method::Send,
            class: Class::Indication,
            trans_id,
            attrs: Attributes {
                username: Some("user"),
                realm: Some("example.org"),
                nonce: Some("dcd98b7102dd2f0e8b11d0f600bfb0c093"),
                // Data length of 3 bytes (0xDEADBE) requires 1 byte of padding
                data: Some(&[0xDE, 0xAD, 0xBE]),
                ..Default::default()
            },
            integrity: &[],
            integrity_len: 0,
        };

        let mut buf = [0u8; 1024];
        let len = message
            .to_bytes(Some(b"password"), &mut buf, sha1_hmac)
            .unwrap();
        let serialized = &buf[..len];

        let parsed = StunMessage::parse(serialized).unwrap();

        assert_eq!(parsed.method, Method::Send);
        assert_eq!(parsed.class, Class::Indication);
        assert_eq!(parsed.trans_id, trans_id);
        assert_eq!(parsed.attrs.username, Some("user"));
        assert_eq!(parsed.attrs.realm, Some("example.org"));
        assert_eq!(
            parsed.attrs.nonce,
            Some("dcd98b7102dd2f0e8b11d0f600bfb0c093")
        );
        assert_eq!(parsed.attrs.data, Some(&[0xDE, 0xAD, 0xBE][..]));
        assert!(parsed.verify(b"password", sha1_hmac));
    }

    #[test]
    fn verify_integrity_as_first_attribute() {
        let message = StunMessage {
            method: Method::Binding,
            class: Class::Failure,
            trans_id: TransId::new(),
            attrs: Attributes::default(),
            integrity: &[],
            integrity_len: 0,
        };

        let mut buf = [0u8; 1024];
        let len = message
            .to_bytes(Some(b"password"), &mut buf, sha1_hmac)
            .unwrap();
        let serialized = &buf[..len];

        // Header followed directly by MESSAGE-INTEGRITY.
        assert_eq!(&serialized[20..22], &[0x00, 0x08]);

        let parsed = StunMessage::parse(serialized).unwrap();

        assert!(parsed.verify(b"password", sha1_hmac));
        assert!(!parsed.verify(b"wrong", sha1_hmac));
    }

    #[test]
    fn verify_without_covered_bytes_is_false() {
        let message = StunMessage {
            method: Method::Binding,
            class: Class::Failure,
            trans_id: TransId::new(),
            attrs: Attributes {
                message_integrity: Some(&[0; 20]),
                ..Default::default()
            },
            integrity: &[],
            integrity_len: 0,
        };

        assert!(!message.verify(b"password", sha1_hmac));
    }

    #[test]
    fn parse_data_indication() {
        // Data length of 5 bytes (0xDEADBEEF00) requires 3 bytes of padding
        let trans_id = TransId::new();
        let message = StunMessage {
            method: Method::Data,
            class: Class::Indication,
            trans_id,
            attrs: Attributes {
                data: Some(&[0xDE, 0xAD, 0xBE, 0xEF, 0xF7]),
                ..Default::default()
            },
            integrity: &[],
            integrity_len: 0,
        };

        let mut buf = [0u8; 1024];
        let len = message
            .to_bytes(Some(b"password"), &mut buf, sha1_hmac)
            .unwrap();
        let serialized = &buf[..len];

        let parsed = StunMessage::parse(serialized).unwrap();

        assert_eq!(parsed.method, Method::Data);
        assert_eq!(parsed.class, Class::Indication);
        assert_eq!(parsed.trans_id, trans_id);
        assert_eq!(parsed.attrs.data, Some(&[0xDE, 0xAD, 0xBE, 0xEF, 0xF7][..]));
        assert!(parsed.verify(b"password", sha1_hmac));
    }

    #[test]
    fn parse_refresh_request() {
        let trans_id = TransId::new();
        let message = StunMessage {
            method: Method::Refresh,
            class: Class::Request,
            trans_id,
            attrs: Attributes {
                username: Some("user"),
                realm: Some("example.org"),
                nonce: Some("dcd98b7102dd2f0e8b11d0f600bfb0c093"),
                lifetime: Some(600),
                ..Default::default()
            },
            integrity: &[],
            integrity_len: 0,
        };

        let mut buf = [0u8; 1024];
        let len = message
            .to_bytes(Some(b"password"), &mut buf, sha1_hmac)
            .unwrap();
        let serialized = &buf[..len];

        let parsed = StunMessage::parse(serialized).unwrap();

        assert_eq!(parsed.method, Method::Refresh);
        assert_eq!(parsed.class, Class::Request);
        assert_eq!(parsed.trans_id, trans_id);
        assert_eq!(parsed.attrs.username, Some("user"));
        assert_eq!(parsed.attrs.realm, Some("example.org"));
        assert_eq!(
            parsed.attrs.nonce,
            Some("dcd98b7102dd2f0e8b11d0f600bfb0c093")
        );
        assert_eq!(parsed.attrs.lifetime, Some(600));
        assert!(parsed.verify(b"password", sha1_hmac));
    }

    #[test]
    fn parse_create_permission_request() {
        let trans_id = TransId::new();
        let peer_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 1), 8080));
        let message = StunMessage {
            method: Method::CreatePermission,
            class: Class::Request,
            trans_id,
            attrs: Attributes {
                username: Some("user"),
                realm: Some("example.org"),
                nonce: Some("dcd98b7102dd2f0e8b11d0f600bfb0c093"),
                xor_peer_address: Some(peer_addr),
                ..Default::default()
            },
            integrity: &[],
            integrity_len: 0,
        };

        let mut buf = [0u8; 1024];
        let len = message
            .to_bytes(Some(b"password"), &mut buf, sha1_hmac)
            .unwrap();
        let serialized = &buf[..len];

        let parsed = StunMessage::parse(serialized).unwrap();

        assert_eq!(parsed.method, Method::CreatePermission);
        assert_eq!(parsed.class, Class::Request);
        assert_eq!(parsed.trans_id, trans_id);
        assert_eq!(parsed.attrs.username, Some("user"));
        assert_eq!(parsed.attrs.realm, Some("example.org"));
        assert_eq!(
            parsed.attrs.nonce,
            Some("dcd98b7102dd2f0e8b11d0f600bfb0c093")
        );
        assert_eq!(parsed.attrs.xor_peer_address, Some(peer_addr));
        assert!(parsed.verify(b"password", sha1_hmac));
    }

    #[test]
    fn parse_channel_bind_request() {
        let trans_id = TransId::new();
        let peer_addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 1), 8080));
        let message = StunMessage {
            method: Method::ChannelBind,
            class: Class::Request,
            trans_id,
            attrs: Attributes {
                username: Some("user"),
                realm: Some("example.org"),
                nonce: Some("dcd98b7102dd2f0e8b11d0f600bfb0c093"),
                channel_number: Some(0x4000),
                xor_peer_address: Some(peer_addr),
                ..Default::default()
            },
            integrity: &[],
            integrity_len: 0,
        };

        let mut buf = [0u8; 1024];
        let len = message
            .to_bytes(Some(b"password"), &mut buf, sha1_hmac)
            .unwrap();
        let serialized = &buf[..len];

        let parsed = StunMessage::parse(serialized).unwrap();

        assert_eq!(parsed.method, Method::ChannelBind);
        assert_eq!(parsed.class, Class::Request);
        assert_eq!(parsed.trans_id, trans_id);
        assert_eq!(parsed.attrs.username, Some("user"));
        assert_eq!(parsed.attrs.realm, Some("example.org"));
        assert_eq!(
            parsed.attrs.nonce,
            Some("dcd98b7102dd2f0e8b11d0f600bfb0c093")
        );
        assert_eq!(parsed.attrs.channel_number, Some(0x4000));
        assert_eq!(parsed.attrs.xor_peer_address, Some(peer_addr));
        assert!(parsed.verify(b"password", sha1_hmac));
    }

    #[test]
    fn build_stun_binding_request_with_attrs() {
        let trans_id = TransId::new();
        let username = "test:user";
        let tie_breaker = 1234567890;
        let prio = 9876;

        let message = StunMessageBuilder::new()
            .binding()
            .request()
            .username(username)
            .prio(prio)
            .ice_controlling(tie_breaker)
            .use_candidate()
            .build(trans_id);

        assert_eq!(message.method(), Method::Binding);
        assert_eq!(message.class(), Class::Request);
        assert_eq!(message.trans_id(), trans_id);
        assert_eq!(message.attrs.username, Some(username));
        assert_eq!(message.attrs.priority, Some(prio));
        assert_eq!(message.attrs.ice_controlling, Some(tie_breaker));
        assert!(message.attrs.use_candidate);
        assert!(message.attrs.ice_controlled.is_none()); // Ensure others aren't set
    }

    #[test]
    fn build_stun_binding_success_with_attrs() {
        let trans_id = TransId::new();
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(1, 2, 3, 4), 5678));

        let message = StunMessageBuilder::new()
            .binding()
            .success()
            .xor_mapped_address(addr)
            .build(trans_id);

        assert_eq!(message.method(), Method::Binding);
        assert_eq!(message.class(), Class::Success);
        assert_eq!(message.trans_id(), trans_id);
        assert_eq!(message.attrs.xor_mapped_address, Some(addr));
    }

    #[test]
    fn build_stun_data_indication_with_attrs() {
        let trans_id = TransId::new();
        let data_payload: &[u8] = &[0xca, 0xfe, 0xba, 0xbe];

        let message = StunMessageBuilder::new()
            .data()
            .indication()
            .data(data_payload)
            .build(trans_id);

        assert_eq!(message.method(), Method::Data);
        assert_eq!(message.class(), Class::Indication);
        assert_eq!(message.trans_id(), trans_id);
        assert_eq!(message.attrs.data, Some(data_payload));
    }

    #[test]
    fn decode_xor_buffer_shorter_than_4_is_error() {
        let trans_id = TransId::new();
        for buf in [&[][..], &[0x00][..], &[0x00, 0x01, 0x02][..]] {
            let err = decode_xor(buf, trans_id).expect_err("short buffer must error");
            assert!(
                matches!(err, StunError::Parse(_)),
                "expected Parse error, got {err:?}"
            );
        }
    }

    #[test]
    fn decode_xor_ipv4_family_with_short_ip_buf_is_error() {
        let trans_id = TransId::new();
        // Family = IPv4 (0x01), valid port bytes, but fewer than 4 IP octets.
        for buf in [
            &[0x00, 0x01, 0x11, 0x22][..],
            &[0x00, 0x01, 0x11, 0x22, 0xaa][..],
            &[0x00, 0x01, 0x11, 0x22, 0xaa, 0xbb, 0xcc][..],
        ] {
            let err = decode_xor(buf, trans_id).expect_err("short ipv4 payload must error");
            assert!(
                matches!(err, StunError::Parse(_)),
                "expected Parse error, got {err:?}"
            );
        }
    }

    #[test]
    fn decode_xor_ipv6_family_with_short_ip_buf_is_error() {
        let trans_id = TransId::new();
        // Family = IPv6 (0x02), valid port bytes, but fewer than 16 IP octets.
        let mut buf = vec![0x00, 0x02, 0x11, 0x22];
        buf.extend_from_slice(&[0xaa; 15]);
        let err = decode_xor(&buf, trans_id).expect_err("short ipv6 payload must error");
        assert!(
            matches!(err, StunError::Parse(_)),
            "expected Parse error, got {err:?}"
        );

        // Zero IP bytes for IPv6 family is also insufficient.
        let err = decode_xor(&[0x00, 0x02, 0x11, 0x22], trans_id)
            .expect_err("empty ipv6 payload must error");
        assert!(matches!(err, StunError::Parse(_)));
    }

    #[test]
    fn decode_xor_unknown_family_is_error() {
        let trans_id = TransId::new();
        let buf = [0x00, 0x03, 0x11, 0x22, 0xaa, 0xbb, 0xcc, 0xdd];
        let err = decode_xor(&buf, trans_id).expect_err("unknown family must error");
        assert!(matches!(err, StunError::Parse(_)));
    }

    #[test]
    fn encode_decode_xor_ipv4_roundtrip() {
        let trans_id = TransId::new();
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 0, 2, 15), 54321));
        let mut buf = [0u8; 20];
        let len = encode_xor(addr, &mut buf, &trans_id);
        assert_eq!(len, 8);
        let decoded = decode_xor(&buf[..len], trans_id).expect("ipv4 roundtrip");
        assert_eq!(decoded, addr);
    }

    #[test]
    fn encode_decode_xor_ipv6_roundtrip() {
        let trans_id = TransId::new();
        let addr = SocketAddr::V6(SocketAddrV6::new(
            Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 0x1234),
            54321,
            0,
            0,
        ));
        let mut buf = [0u8; 20];
        let len = encode_xor(addr, &mut buf, &trans_id);
        assert_eq!(len, 20);
        let decoded = decode_xor(&buf[..len], trans_id).expect("ipv6 roundtrip");
        assert_eq!(decoded, addr);
    }
}
