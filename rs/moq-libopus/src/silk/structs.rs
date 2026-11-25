use crate::silk::resampler::ResamplerState;

#[derive(Copy, Clone)]
#[repr(C)]
pub struct silk_NLSF_CB_struct {
    pub nVectors: i16,
    pub order: i16,
    pub quantStepSize_Q16: i16,
    pub invQuantStepSize_Q6: i16,
    pub CB1_NLSF_Q8: &'static [u8],
    pub CB1_Wght_Q9: &'static [i16],
    pub CB1_iCDF: &'static [u8],
    pub pred_Q8: &'static [u8],
    pub ec_sel: &'static [u8],
    pub ec_iCDF: &'static [u8],
    pub ec_Rates_Q5: &'static [u8],
    pub deltaMin_Q15: &'static [i16],
}
#[derive(Copy, Clone)]
#[repr(C)]
pub struct SideInfoIndices {
    pub GainsIndices: [i8; 4],
    pub LTPIndex: [i8; 4],
    pub NLSFIndices: [i8; 17],
    pub lagIndex: i16,
    pub contourIndex: i8,
    pub signalType: i8,
    pub quantOffsetType: i8,
    pub NLSFInterpCoef_Q2: i8,
    pub PERIndex: i8,
    pub LTP_scaleIndex: i8,
    pub Seed: i8,
}
#[derive(Copy, Clone)]
#[repr(C)]
pub struct silk_PLC_struct {
    pub pitchL_Q8: i32,
    pub LTPCoef_Q14: [i16; 5],
    pub prevLPC_Q12: [i16; 16],
    pub last_frame_lost: i32,
    pub rand_seed: i32,
    pub randScale_Q14: i16,
    pub conc_energy: i32,
    pub conc_energy_shift: i32,
    pub prevLTP_scale_Q14: i16,
    pub prevGain_Q16: [i32; 2],
    pub fs_kHz: i32,
    pub nb_subfr: i32,
    pub subfr_length: i32,
}
#[derive(Copy, Clone)]
#[repr(C)]
pub struct silk_CNG_struct {
    pub CNG_exc_buf_Q14: [i32; 320],
    pub CNG_smth_NLSF_Q15: [i16; 16],
    pub CNG_synth_state: [i32; 16],
    pub CNG_smth_Gain_Q16: i32,
    pub rand_seed: i32,
    pub fs_kHz: i32,
}
#[derive(Copy, Clone)]
#[repr(C)]
pub struct silk_decoder_state {
    pub prev_gain_Q16: i32,
    pub exc_Q14: [i32; 320],
    pub sLPC_Q14_buf: [i32; 16],
    pub outBuf: [i16; 480],
    pub lagPrev: i32,
    pub LastGainIndex: i8,
    pub fs_kHz: i32,
    pub fs_API_hz: i32,
    pub nb_subfr: i32,
    pub frame_length: i32,
    pub subfr_length: i32,
    pub ltp_mem_length: i32,
    pub LPC_order: i32,
    pub prevNLSF_Q15: [i16; 16],
    pub first_frame_after_reset: i32,
    pub pitch_lag_low_bits_iCDF: &'static [u8],
    pub pitch_contour_iCDF: &'static [u8],
    pub nFramesDecoded: i32,
    pub nFramesPerPacket: i32,
    pub ec_prevSignalType: i32,
    pub ec_prevLagIndex: i16,
    pub VAD_flags: [i32; 3],
    pub LBRR_flag: i32,
    pub LBRR_flags: [i32; 3],
    pub resampler_state: ResamplerState,
    pub psNLSF_CB: &'static silk_NLSF_CB_struct,
    pub indices: SideInfoIndices,
    pub sCNG: silk_CNG_struct,
    pub lossCnt: i32,
    pub prevSignalType: i32,
    pub arch: i32,
    pub sPLC: silk_PLC_struct,
}
#[derive(Copy, Clone)]
#[repr(C)]
pub struct silk_decoder_control {
    pub pitchL: [i32; 4],
    pub Gains_Q16: [i32; 4],
    pub PredCoef_Q12: [[i16; 16]; 2],
    pub LTPCoef_Q14: [i16; 20],
    pub LTP_scale_Q14: i32,
}

#[derive(Copy, Clone)]
#[repr(C)]
pub struct silk_nsq_state {
    pub xq: [i16; 640],
    pub sLTP_shp_Q14: [i32; 640],
    pub sLPC_Q14: [i32; 96],
    pub sAR2_Q14: [i32; 24],
    pub sLF_AR_shp_Q14: i32,
    pub sDiff_shp_Q14: i32,
    pub lagPrev: i32,
    pub sLTP_buf_idx: i32,
    pub sLTP_shp_buf_idx: i32,
    pub rand_seed: i32,
    pub prev_gain_Q16: i32,
    pub rewhite_flag: i32,
}
#[derive(Copy, Clone)]
#[repr(C)]
pub struct silk_VAD_state {
    pub AnaState: [i32; 2],
    pub AnaState1: [i32; 2],
    pub AnaState2: [i32; 2],
    pub XnrgSubfr: [i32; 4],
    pub NrgRatioSmth_Q8: [i32; 4],
    pub HPstate: i16,
    pub NL: [i32; 4],
    pub inv_NL: [i32; 4],
    pub NoiseLevelBias: [i32; 4],
    pub counter: i32,
}
#[derive(Copy, Clone)]
#[repr(C)]
pub struct silk_LP_state {
    pub In_LP_State: [i32; 2],
    pub transition_frame_no: i32,
    pub mode: i32,
    pub saved_fs_kHz: i32,
}
#[derive(Copy, Clone)]
#[repr(C)]
pub struct silk_encoder_state {
    pub In_HP_State: [i32; 2],
    pub variable_HP_smth1_Q15: i32,
    pub variable_HP_smth2_Q15: i32,
    pub sLP: silk_LP_state,
    pub sVAD: silk_VAD_state,
    pub sNSQ: silk_nsq_state,
    pub prev_NLSFq_Q15: [i16; 16],
    pub speech_activity_Q8: i32,
    pub allow_bandwidth_switch: i32,
    pub LBRRprevLastGainIndex: i8,
    pub prevSignalType: i8,
    pub prevLag: i32,
    pub pitch_LPC_win_length: i32,
    pub max_pitch_lag: i32,
    pub API_fs_Hz: i32,
    pub prev_API_fs_Hz: i32,
    pub maxInternal_fs_Hz: i32,
    pub minInternal_fs_Hz: i32,
    pub desiredInternal_fs_Hz: i32,
    pub fs_kHz: i32,
    pub nb_subfr: i32,
    pub frame_length: i32,
    pub subfr_length: i32,
    pub ltp_mem_length: i32,
    pub la_pitch: i32,
    pub la_shape: i32,
    pub shapeWinLength: i32,
    pub TargetRate_bps: i32,
    pub PacketSize_ms: i32,
    pub PacketLoss_perc: i32,
    pub frameCounter: i32,
    pub Complexity: i32,
    pub nStatesDelayedDecision: i32,
    pub useInterpolatedNLSFs: i32,
    pub shapingLPCOrder: i32,
    pub predictLPCOrder: i32,
    pub pitchEstimationComplexity: i32,
    pub pitchEstimationLPCOrder: i32,
    pub pitchEstimationThreshold_Q16: i32,
    pub sum_log_gain_Q7: i32,
    pub NLSF_MSVQ_Survivors: i32,
    pub first_frame_after_reset: i32,
    pub controlled_since_last_payload: i32,
    pub warping_Q16: i32,
    pub useCBR: i32,
    pub prefillFlag: i32,
    pub pitch_lag_low_bits_iCDF: &'static [u8],
    pub pitch_contour_iCDF: &'static [u8],
    pub psNLSF_CB: &'static silk_NLSF_CB_struct,
    pub input_quality_bands_Q15: [i32; 4],
    pub input_tilt_Q15: i32,
    pub SNR_dB_Q7: i32,
    pub VAD_flags: [i8; 3],
    pub LBRR_flag: i8,
    pub LBRR_flags: [i32; 3],
    pub indices: SideInfoIndices,
    pub pulses: [i8; 320],
    pub arch: i32,
    pub inputBuf: [i16; 322],
    pub inputBufIx: i32,
    pub nFramesPerPacket: i32,
    pub nFramesEncoded: i32,
    pub nChannelsAPI: i32,
    pub nChannelsInternal: i32,
    pub channelNb: i32,
    pub frames_since_onset: i32,
    pub ec_prevSignalType: i32,
    pub ec_prevLagIndex: i16,
    pub resampler_state: ResamplerState,
    pub useDTX: i32,
    pub inDTX: i32,
    pub noSpeechCounter: i32,
    pub useInBandFEC: i32,
    pub LBRR_enabled: i32,
    pub LBRR_GainIncreases: i32,
    pub indices_LBRR: [SideInfoIndices; 3],
    pub pulses_LBRR: [[i8; 320]; 3],
}

#[derive(Copy, Clone)]
#[repr(C)]
pub struct stereo_enc_state {
    pub pred_prev_Q13: [i16; 2],
    pub sMid: [i16; 2],
    pub sSide: [i16; 2],
    pub mid_side_amp_Q0: [i32; 4],
    pub smth_width_Q14: i16,
    pub width_prev_Q14: i16,
    pub silent_side_len: i16,
    pub predIx: [[[i8; 3]; 2]; 3],
    pub mid_only_flags: [i8; 3],
}

#[derive(Copy, Clone)]
#[repr(C)]
pub struct stereo_dec_state {
    pub pred_prev_Q13: [i16; 2],
    pub sMid: [i16; 2],
    pub sSide: [i16; 2],
}
