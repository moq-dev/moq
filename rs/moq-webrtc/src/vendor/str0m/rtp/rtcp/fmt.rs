/// Number of _something_ in the RTCP packet.
///
/// PacketType determines how to interpret the count field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedbackMessageType {
    /// When packet type SenderReport or ReceiverReport.
    ///
    /// The contained u8 is number of receiver reports.
    ReceptionReport(u8),

    /// When packet type SourceDescription (SDES) or Goodbye
    ///
    /// The contained u8 is number of contained SDES or Goodbyes.
    SourceCount(u8),

    /// When packet type ApplicationDefined
    ///
    /// The contained u8 is a subtype which is upp to the application.
    Subtype(u8),

    /// When packet type is TransportLayerFeedback.
    TransportFeedback(TransportType),

    /// When packet type is PayloadSpecificFeedback.
    PayloadFeedback(PayloadType),

    /// When the packet type is ExtendedReport
    NotUsed,
}

impl FeedbackMessageType {
    pub fn count(&self) -> u8 {
        match self {
            FeedbackMessageType::ReceptionReport(v) => *v,
            FeedbackMessageType::SourceCount(v) => *v,
            _ => panic!("Not a count"),
        }
    }
}

impl From<FeedbackMessageType> for u8 {
    fn from(val: FeedbackMessageType) -> Self {
        use FeedbackMessageType::*;
        match val {
            ReceptionReport(v) | SourceCount(v) | Subtype(v) => {
                assert!(v <= 31, "rtcp fmt when count must be <= 31");
                v
            }
            TransportFeedback(v) => v as u8,
            PayloadFeedback(v) => v as u8,
            NotUsed => 0,
        }
    }
}

/// Subtypes of [`FeedbackMessageType::TransportFeedback`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportType {
    /// Nack RTCP packet.
    ///
    /// Definition: <https://www.rfc-editor.org/rfc/rfc4585#section-6.2.1>
    Nack = 1,

    /// Transportwide congestion control packet.
    ///
    /// Definition: <https://tools.ietf.org/html/draft-holmer-rmcat-transport-wide-cc-extensions-01>
    TransportWide = 15,
}

impl TryFrom<u8> for TransportType {
    type Error = &'static str;

    fn try_from(v: u8) -> Result<Self, Self::Error> {
        use TransportType::*;
        match v {
            1 => Ok(Nack),
            15 => Ok(TransportWide),
            _ => {
                trace!("Uknown TransportType: {}", v);
                Err("Uknown TransportType")
            }
        }
    }
}

/// Subtypes of [`FeedbackMessageType::PayloadFeedback`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadType {
    /// PLI packet type.
    ///
    /// Definition: <https://www.rfc-editor.org/rfc/rfc4585#section-6.3.1>
    PictureLossIndication = 1,

    /// SLI packet type.
    ///
    /// Definition: <https://www.rfc-editor.org/rfc/rfc4585#section-6.3.2>
    SliceLossIndication = 2,

    /// RPSI packet type.
    ///
    /// Definition: <https://www.rfc-editor.org/rfc/rfc4585#section-6.3.3>
    ReferencePictureSelectionIndication = 3,

    /// FIR packet type.
    ///
    /// Definition: <https://www.rfc-editor.org/rfc/rfc5104.html#section-4.3.1>
    FullIntraRequest = 4,

    /// Application specific type.
    ///
    /// Definition: <https://www.rfc-editor.org/rfc/rfc4585#section-6.4>
    ApplicationLayer = 15,
}

impl TryFrom<u8> for PayloadType {
    type Error = &'static str;

    fn try_from(v: u8) -> Result<Self, Self::Error> {
        use PayloadType::*;
        match v {
            1 => Ok(PictureLossIndication),
            2 => Ok(SliceLossIndication),
            3 => Ok(ReferencePictureSelectionIndication),
            4 => Ok(FullIntraRequest),
            15 => Ok(ApplicationLayer),
            _ => {
                trace!("Uknown PayloadType: {}", v);
                Err("Uknown PayloadType")
            }
        }
    }
}
