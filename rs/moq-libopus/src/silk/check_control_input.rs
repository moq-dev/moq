#[allow(unused)]
pub mod errors_h {
    pub const SILK_NO_ERROR: i32 = 0;
    pub const SILK_ENC_INVALID_COMPLEXITY_SETTING: i32 = -(106);
    pub const SILK_ENC_INVALID_NUMBER_OF_CHANNELS_ERROR: i32 = -(111);
    pub const SILK_ENC_INVALID_INBAND_FEC_SETTING: i32 = -(107);
    pub const SILK_ENC_INVALID_CBR_SETTING: i32 = -(109);
    pub const SILK_ENC_INVALID_DTX_SETTING: i32 = -(108);
    pub const SILK_ENC_INVALID_LOSS_RATE: i32 = -(105);
    pub const SILK_ENC_PACKET_SIZE_NOT_SUPPORTED: i32 = -(103);
    pub const SILK_ENC_FS_NOT_SUPPORTED: i32 = -(102);
}

pub use self::errors_h::{
    SILK_ENC_FS_NOT_SUPPORTED, SILK_ENC_INVALID_CBR_SETTING, SILK_ENC_INVALID_COMPLEXITY_SETTING,
    SILK_ENC_INVALID_DTX_SETTING, SILK_ENC_INVALID_INBAND_FEC_SETTING, SILK_ENC_INVALID_LOSS_RATE,
    SILK_ENC_INVALID_NUMBER_OF_CHANNELS_ERROR, SILK_ENC_PACKET_SIZE_NOT_SUPPORTED, SILK_NO_ERROR,
};
use crate::silk::define::ENCODER_NUM_CHANNELS;
use crate::silk::enc_API::silk_EncControlStruct;

pub unsafe fn check_control_input(encControl: *mut silk_EncControlStruct) -> i32 {
    // The assert(0) here are weird..
    // they are optional (enabled only if ENABLE_ASSERTIONS is defined)
    // if they are disabled - the function returns various error codes..
    // will figure something out later

    assert!(!encControl.is_null());
    if (*encControl).API_sampleRate != 8000
        && (*encControl).API_sampleRate != 12000
        && (*encControl).API_sampleRate != 16000
        && (*encControl).API_sampleRate != 24000
        && (*encControl).API_sampleRate != 32000
        && (*encControl).API_sampleRate != 44100
        && (*encControl).API_sampleRate != 48000
        || (*encControl).desiredInternalSampleRate != 8000
            && (*encControl).desiredInternalSampleRate != 12000
            && (*encControl).desiredInternalSampleRate != 16000
        || (*encControl).maxInternalSampleRate != 8000
            && (*encControl).maxInternalSampleRate != 12000
            && (*encControl).maxInternalSampleRate != 16000
        || (*encControl).minInternalSampleRate != 8000
            && (*encControl).minInternalSampleRate != 12000
            && (*encControl).minInternalSampleRate != 16000
        || (*encControl).minInternalSampleRate > (*encControl).desiredInternalSampleRate
        || (*encControl).maxInternalSampleRate < (*encControl).desiredInternalSampleRate
        || (*encControl).minInternalSampleRate > (*encControl).maxInternalSampleRate
    {
        panic!("libopus: assert(0) called");
        // return SILK_ENC_FS_NOT_SUPPORTED;
    }
    if (*encControl).payloadSize_ms != 10
        && (*encControl).payloadSize_ms != 20
        && (*encControl).payloadSize_ms != 40
        && (*encControl).payloadSize_ms != 60
    {
        panic!("libopus: assert(0) called");
        // return SILK_ENC_PACKET_SIZE_NOT_SUPPORTED;
    }
    if (*encControl).packetLossPercentage < 0 || (*encControl).packetLossPercentage > 100 {
        panic!("libopus: assert(0) called");
        // return SILK_ENC_INVALID_LOSS_RATE;
    }
    if (*encControl).useDTX < 0 || (*encControl).useDTX > 1 {
        panic!("libopus: assert(0) called");
        // return SILK_ENC_INVALID_DTX_SETTING;
    }
    if (*encControl).useCBR < 0 || (*encControl).useCBR > 1 {
        panic!("libopus: assert(0) called");
        // return SILK_ENC_INVALID_CBR_SETTING;
    }
    if (*encControl).useInBandFEC < 0 || (*encControl).useInBandFEC > 1 {
        panic!("libopus: assert(0) called");
        // return SILK_ENC_INVALID_INBAND_FEC_SETTING;
    }
    if (*encControl).nChannelsAPI < 1 || (*encControl).nChannelsAPI > ENCODER_NUM_CHANNELS {
        panic!("libopus: assert(0) called");
        // return SILK_ENC_INVALID_NUMBER_OF_CHANNELS_ERROR;
    }
    if (*encControl).nChannelsInternal < 1 || (*encControl).nChannelsInternal > ENCODER_NUM_CHANNELS
    {
        panic!("libopus: assert(0) called");
        // return SILK_ENC_INVALID_NUMBER_OF_CHANNELS_ERROR;
    }
    if (*encControl).nChannelsInternal > (*encControl).nChannelsAPI {
        panic!("libopus: assert(0) called");
        // return SILK_ENC_INVALID_NUMBER_OF_CHANNELS_ERROR;
    }
    if (*encControl).complexity < 0 || (*encControl).complexity > 10 {
        panic!("libopus: assert(0) called");
        // return SILK_ENC_INVALID_COMPLEXITY_SETTING;
    }
    return SILK_NO_ERROR;
}
