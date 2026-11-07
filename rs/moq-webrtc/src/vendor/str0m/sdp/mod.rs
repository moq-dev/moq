use std::fmt;
use std::ops::Deref;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

mod data;
#[cfg(test)]
pub(crate) use data::RtpMap;
pub(crate) use data::is_no_stream;
pub(crate) use data::{FormatParam, Sdp, Session, SessionAttribute, Setup};
pub(crate) use data::{MediaAttribute, MediaLine, MediaType, Msid, Proto};
pub(crate) use data::{RestrictionId, Simulcast, SimulcastGroups, SimulcastLayer};

mod parser;

mod error;
pub use error::SdpError;

#[derive(Debug, PartialEq, Eq)]
/// SDP offer. Offers can be serialized via serde.
pub struct SdpOffer(Sdp);

impl SdpOffer {
    /// Takes the SDP string without any JSON wrapping and makes an `Offer`.
    pub fn from_sdp_string(input: &str) -> Result<Self, SdpError> {
        let sdp = Sdp::parse(input)?;
        Ok(SdpOffer(sdp))
    }

    /// Turns this offer into an SDP string, without any JSON wrapping.
    pub fn to_sdp_string(&self) -> String {
        self.0.to_string()
    }

    #[cfg(test)]
    pub(crate) fn into_inner(self) -> Sdp {
        self.0
    }
}

#[derive(Debug, PartialEq, Eq)]
/// SDP answer. Answers can be serialized via serde.
pub struct SdpAnswer(Sdp);

impl SdpAnswer {
    /// Takes the SDP string without any JSON wrapping and makes an `Answer`.
    pub fn from_sdp_string(input: &str) -> Result<Self, SdpError> {
        let sdp = Sdp::parse(input)?;
        Ok(SdpAnswer(sdp))
    }

    /// Turns this answer into an SDP string, without any JSON wrapping.
    pub fn to_sdp_string(&self) -> String {
        self.0.to_string()
    }
}

impl Deref for SdpOffer {
    type Target = Sdp;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Deref for SdpAnswer {
    type Target = Sdp;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<Sdp> for SdpOffer {
    fn from(v: Sdp) -> Self {
        SdpOffer(v)
    }
}

impl From<Sdp> for SdpAnswer {
    fn from(v: Sdp) -> Self {
        SdpAnswer(v)
    }
}

impl fmt::Display for SdpOffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", **self)
    }
}

impl fmt::Display for SdpAnswer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", **self)
    }
}

macro_rules! sdp_ser {
    ($Struct:tt, $Name:literal, $LCName:literal) => {
        impl Serialize for $Struct {
            fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                #[derive(Serialize)]
                struct Data {
                    r#type: &'static str,
                    sdp: String,
                }

                Data {
                    r#type: $LCName,
                    sdp: self.0.to_string(),
                }
                .serialize(s)
            }
        }

        impl<'de> Deserialize<'de> for $Struct {
            fn deserialize<D>(d: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                #[derive(Deserialize)]
                struct Data {
                    r#type: String,
                    sdp: String,
                }

                let data = Data::deserialize(d)?;

                if data.r#type != $LCName {
                    return Err(de::Error::custom(format!(
                        "Expected SDP type '{}', got '{}'",
                        $LCName, data.r#type
                    )));
                }

                let sdp = Sdp::parse(&data.sdp)
                    .map_err(|err| de::Error::custom(format!("Failed to parse SDP: {:?}", err)))?;

                Ok(Self(sdp))
            }
        }
    };
}

sdp_ser!(SdpOffer, "Offer", "offer");
sdp_ser!(SdpAnswer, "Answer", "answer");

#[cfg(test)]
mod test {
    use crate::VERSION;
    use crate::rtp_::SessionId;

    use super::*;

    fn sdp() -> Sdp {
        Sdp {
            session: Session {
                id: SessionId::from(123_u64),
                attrs: vec![],
                bw: None,
            },
            media_lines: vec![],
        }
    }

    #[test]
    fn serialize_deserialize_offer() {
        let offer = SdpOffer(sdp());
        let json = serde_json::to_string(&offer).unwrap();

        let expected = format!(
            "{{\"type\":\"offer\",\"sdp\":\
             \"v=0\\r\\no=str0m-{VERSION} 123 2 \
             IN IP4 0.0.0.0\\r\\ns=-\\r\\nt=0 0\\r\\n\"}}"
        );
        assert_eq!(json, expected);

        let offer2: SdpOffer = serde_json::from_str(&json).unwrap();

        assert_eq!(offer, offer2);
    }

    #[test]
    fn serialize_deserialize_answer() {
        let answer = SdpAnswer(sdp());
        let json = serde_json::to_string(&answer).unwrap();

        let expected = format!(
            "{{\"type\":\"answer\",\"sdp\":\
             \"v=0\\r\\no=str0m-{VERSION} 123 2 \
             IN IP4 0.0.0.0\\r\\ns=-\\r\\nt=0 0\\r\\n\"}}"
        );
        assert_eq!(json, expected);

        let answer2: SdpAnswer = serde_json::from_str(&json).unwrap();

        assert_eq!(answer, answer2);
    }
}
