use std::fmt;
use std::ops::Deref;
use std::str::from_utf8;

use serde::{Deserialize, Serialize};

use str0m_proto::Id;
use str0m_proto::NonCryptographicRng;

macro_rules! str_id {
    ($id:ident, $name:literal, $num:tt, $new_len:tt) => {
        impl $id {
            /// Creates a new random id.
            pub fn new() -> $id {
                let mut arr = Id::<$num>::random().into_array();
                for i in $new_len..$num {
                    arr[i] = b' ';
                }
                $id(arr)
            }

            /// Converts an array of bytes to an id.
            pub const fn from_array(a: [u8; $num]) -> $id {
                $id(a)
            }
        }

        impl fmt::Display for $id {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let s: &str = self;
                write!(f, "{}", s)
            }
        }

        impl fmt::Debug for $id {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let s: &str = self;
                write!(f, "{}({})", $name, s)
            }
        }

        impl Deref for $id {
            type Target = str;

            fn deref(&self) -> &Self::Target {
                from_utf8(&self.0).expect("ascii id").trim()
            }
        }

        impl<'a> From<&'a str> for $id {
            fn from(v: &'a str) -> Self {
                let v = v
                    .chars()
                    .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                    .collect::<String>();

                let bytes = v.as_bytes();
                let bytes = &bytes[0..$num.min(bytes.len())];

                // pad with space.
                let mut array = [b' '; $num];

                let max = bytes.len().min(array.len());
                (&mut array[0..max]).copy_from_slice(bytes);

                $id(array)
            }
        }

        impl Default for $id {
            fn default() -> Self {
                $id::new()
            }
        }
    };
}

macro_rules! num_id {
    ($id:ident, $t:tt) => {
        impl $id {
            /// Creates a new random id.
            pub fn new() -> Self {
                loop {
                    let v = NonCryptographicRng::$t();
                    // At least Ssrc assigns special meaning to 0,
                    // but it's fine to avoid for the other numeric
                    // ids as well.
                    if v != 0 {
                        return $id(v);
                    }
                }
            }
        }

        impl Deref for $id {
            type Target = $t;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl From<$t> for $id {
            fn from(v: $t) -> Self {
                $id(v)
            }
        }

        impl fmt::Display for $id {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

/// Media identifier.
///
/// In SDP this is found per m-line with the attribute `a=mid:<mid>`.
///
/// When using Direct API we still need `Mid`, since they group individual
/// encoded streams. For example a simulcast of 3 layers would have
/// 3 incoming StreamRx, but since they belong to the same media,
/// the have the same `Mid`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Mid([u8; 16]);
str_id!(Mid, "Mid", 16, 3);

/// Identifier of a simulcast layer for an encoded stream.
///
/// The abbreviation means "RTP Stream Id", which is a very confusing name, because
/// everything in RTP is a stream. People tend to just call it "rid".
///
/// In SDP this is an optional value that will be seen in [`MediaData`][crate::media::MediaData]
/// if the remote peer is configured for simulcast.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rid([u8; 8]);
str_id!(Rid, "Rid", 8, 3);

/// Synchronization source.
///
/// Uniquely identifies a sending source of data. Each video/audio stream would be associated
/// with at least one synchronization source. Multiple sources for the same stream happens
/// for RTX (resend) and simulcast.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Ssrc(u32);
num_id!(Ssrc, u32);

impl Ssrc {
    /// Returns true if this is the probe SSRC (0).
    ///
    /// libwebrtc uses SSRC 0 for bandwidth estimation probes sent before
    /// video media starts. These probes require special handling.
    pub fn is_probe(&self) -> bool {
        self.0 == 0
    }
}

/// Payload type.
///
/// The payload type identifies which codec and format parameters a stream is sent with.
/// The mappings of Pt-Codec + parameters is negotiated in SDP OFFER/ANSWER.
///
/// PTs in RTP headers are 7 bits. Values >=128 are not valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Pt(u8);
num_id!(Pt, u8);

/// Identifier of an SDP session.
///
/// This value is rarely interesting, but is part of the SDP OFFER and ANSWER.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SessionId(u64);
num_id!(SessionId, u64);

/// Sequence number of an RTP packet.
///
/// An RTP packet is identified by: SSRC + SeqNo. However in the RTP header the sequence number
/// is a `u16`, meaning the value quite quickly "rolls over". To uniquely identify a packet,
/// str0m keeps track of the roll overs and converts the `u16` to `u64` in this `SeqNo`.
///
/// To get the RTP u16 value from a `SeqNo`, use `as_u16()` or cast it to u16.
///
/// ```
/// # use str0m::rtp::SeqNo;
/// let seq_no: SeqNo = 65_537.into();
///
/// // Use `as_u16()`.
/// let a = seq_no.as_u16();
/// // Discard upper 48 bits to get RTP u16.
/// let b = *seq_no as u16;
///
/// assert_eq!(a, 1);
/// assert_eq!(b, 1);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SeqNo(u64);
num_id!(SeqNo, u64);

#[cfg(feature = "drv")]
crate::drv_identity_copy!(Mid, Rid, Ssrc, Pt, SessionId, SeqNo);

/// TWCC-specific sequence number.
///
/// Transport-Wide Congestion Control uses its own sequence number space,
/// separate from RTP sequence numbers. This type ensures TWCC sequences
/// cannot be confused with RTP SeqNo values.
///
/// TWCC sequences are also u64 internally (tracking rollovers), though the
/// wire format uses u16.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default,
)]
pub struct TwccSeq(u64);
num_id!(TwccSeq, u64);

impl SeqNo {
    pub(crate) const MAX: SeqNo = SeqNo(u64::MAX);

    /// Check if the `other` sequence number is directly following this.
    #[inline(always)]
    pub fn is_next(&self, other: SeqNo) -> bool {
        if **self >= *other {
            return false;
        }
        *other - **self == 1
    }

    /// Increase (mutate) this sequence number and return the previous value.
    #[inline(always)]
    pub fn inc(&mut self) -> SeqNo {
        let n = SeqNo(self.0);
        self.0 += 1;
        n
    }

    #[inline(always)]
    pub(crate) fn is_max(&self) -> bool {
        self.0 == Self::MAX.0
    }

    /// The RTP header value (discarding the ROC).
    ///
    /// This is the same as discarding the top 48 bits by casting to a u16.
    ///
    /// ```
    /// # use str0m::rtp::SeqNo;
    /// let seq_no: SeqNo = 65_537.into();
    ///
    /// // Use `as_u16()`.
    /// let a = seq_no.as_u16();
    ///
    /// assert_eq!(a, 1);
    /// ```
    #[inline(always)]
    pub fn as_u16(&self) -> u16 {
        self.0 as u16
    }

    /// Get the rollover counter (ROC) value.
    ///
    /// ```
    /// # use str0m::rtp::SeqNo;
    /// // More than 2^16, thus rolled over.
    /// let seq_no: SeqNo = 95_000.into();
    ///
    /// assert_eq!(seq_no.roc(), 1);
    ///
    /// // Is the same as shifting 16 bits.
    /// assert_eq!(seq_no.roc(), 95_000 >> 16);
    /// ```
    #[inline(always)]
    pub fn roc(&self) -> u64 {
        self.0 >> 16
    }
}

impl TwccSeq {
    /// Check if the `other` sequence number is directly following this.
    #[inline(always)]
    pub fn is_next(&self, other: TwccSeq) -> bool {
        if **self >= *other {
            return false;
        }
        *other - **self == 1
    }

    /// Increase (mutate) this sequence number and return the previous value.
    #[inline(always)]
    pub fn inc(&mut self) -> TwccSeq {
        let n = TwccSeq(self.0);
        self.0 += 1;
        n
    }

    /// The TWCC wire format value (discarding the ROC).
    ///
    /// This is the same as discarding the top 48 bits by casting to a u16.
    #[inline(always)]
    pub fn as_u16(&self) -> u16 {
        self.0 as u16
    }

    /// Get the rollover counter (ROC) value.
    #[inline(always)]
    pub fn roc(&self) -> u64 {
        self.0 >> 16
    }
}

impl Default for SeqNo {
    fn default() -> Self {
        // https://www.rfc-editor.org/rfc/rfc3550#page-13
        // The initial value of the sequence number SHOULD be random (unpredictable)
        // to make known-plaintext attacks on encryption more difficult
        // Upper half of range is avoided in order to prevent SRTP wraparound issues
        // during startup.
        // Sequence number 0 is avoided for historical reasons, presumably to avoid
        // debugability or test usage conflicts.
        // i.e the range is (1, 2^15-1)
        Self((NonCryptographicRng::u16() % 32767 + 1) as u64)
    }
}

/// Probe cluster identifier for bandwidth estimation.
///
/// Used to tag TWCC packets as belonging to a specific probe cluster,
/// enabling analysis of probe results when feedback arrives.
///
/// Uses u64 to avoid wrap-around in long-running connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct TwccClusterId(u64);
num_id!(TwccClusterId, u64);

impl TwccClusterId {
    /// Increase (mutate) this cluster ID and return the previous value.
    #[inline(always)]
    pub fn inc(&mut self) -> TwccClusterId {
        let n = TwccClusterId(self.0);
        self.0 = self.0.wrapping_add(1);
        n
    }
}

impl Pt {
    /// Create a PT with a specific value.
    ///
    /// PTs are 7 bit. Values with 8 bits are not valid in RTP headers.
    pub const fn new_with_value(v: u8) -> Pt {
        Pt(v)
    }
}

/// A combination of Mid/Rid
///
/// In many cases they go hand-in-hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct MidRid(pub Mid, pub Option<Rid>);

impl MidRid {
    #[inline(always)]
    pub fn mid(&self) -> Mid {
        self.0
    }

    #[inline(always)]
    pub fn rid(&self) -> Option<Rid> {
        self.1
    }

    pub fn special_equals(&self, other: &MidRid) -> bool {
        self.0 == other.0 && (self.1.is_none() || self.1 == other.1)
    }
}
