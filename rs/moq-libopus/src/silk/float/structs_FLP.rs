use crate::silk::structs::{silk_encoder_state, stereo_enc_state};

#[derive(Copy, Clone)]
#[repr(C)]
pub struct silk_encoder {
    pub state_Fxx: [silk_encoder_state_FLP; 2],
    pub sStereo: stereo_enc_state,
    pub nBitsUsedLBRR: i32,
    pub nBitsExceeded: i32,
    pub nChannelsAPI: i32,
    pub nChannelsInternal: i32,
    pub nPrevChannelsInternal: i32,
    pub timeSinceSwitchAllowed_ms: i32,
    pub allowBandwidthSwitch: i32,
    pub prev_decode_only_middle: i32,
}
#[derive(Copy, Clone)]
#[repr(C)]
pub struct silk_encoder_state_FLP {
    pub sCmn: silk_encoder_state,
    pub sShape: silk_shape_state_FLP,
    pub x_buf: [f32; 720],
    pub LTPCorr: f32,
}
#[derive(Copy, Clone)]
#[repr(C)]
pub struct silk_shape_state_FLP {
    pub LastGainIndex: i8,
    pub HarmShapeGain_smth: f32,
    pub Tilt_smth: f32,
}
#[derive(Copy, Clone)]
#[repr(C)]
pub struct silk_encoder_control_FLP {
    pub Gains: [f32; 4],
    pub PredCoef: [[f32; 16]; 2],
    pub LTPCoef: [f32; 20],
    pub LTP_scale: f32,
    pub pitchL: [i32; 4],
    pub AR: [f32; 96],
    pub LF_MA_shp: [f32; 4],
    pub LF_AR_shp: [f32; 4],
    pub Tilt: [f32; 4],
    pub HarmShapeGain: [f32; 4],
    pub Lambda: f32,
    pub input_quality: f32,
    pub coding_quality: f32,
    pub predGain: f32,
    pub LTPredCodGain: f32,
    pub ResNrg: [f32; 4],
    pub GainsUnq_Q16: [i32; 4],
    pub lastGainIndexPrev: i8,
}
