use crate::externs::{free, malloc};

pub mod arch_h {
    pub type opus_val16 = f32;
    pub type opus_val32 = f32;
    pub const CELT_SIG_SCALE: f32 = 32768.0f32;
    pub const Q15ONE: f32 = 1.0f32;
    pub const EPSILON: f32 = 1e-15f32;
    pub const VERY_SMALL: f32 = 1e-30f32;
}
pub mod stddef_h {
    pub type size_t = u64;
    pub const NULL: i32 = 0;
}
pub mod xmmintrin_h {
    #[cfg(target_arch = "x86")]
    pub use core::arch::x86::{__m128, _mm_cvt_ss2si, _mm_cvtss_si32, _mm_set_ss};
    #[cfg(target_arch = "x86_64")]
    pub use core::arch::x86_64::{__m128, _mm_cvt_ss2si, _mm_cvtss_si32, _mm_set_ss};
}
pub mod cpu_support_h {
    #[inline]
    pub unsafe fn opus_select_arch() -> i32 {
        return 0;
    }
}
use self::arch_h::{opus_val16, opus_val32, CELT_SIG_SCALE, EPSILON, Q15ONE, VERY_SMALL};
pub use self::cpu_support_h::opus_select_arch;
pub use self::stddef_h::{size_t, NULL};
use crate::celt::celt::{
    CELT_GET_MODE_REQUEST, CELT_SET_ANALYSIS_REQUEST, CELT_SET_CHANNELS_REQUEST,
    CELT_SET_END_BAND_REQUEST, CELT_SET_PREDICTION_REQUEST, CELT_SET_SIGNALLING_REQUEST,
    CELT_SET_SILK_INFO_REQUEST, CELT_SET_START_BAND_REQUEST, OPUS_SET_ENERGY_MASK_REQUEST,
    OPUS_SET_LFE_REQUEST,
};
use crate::celt::celt_encoder::{
    celt_encode_with_ec, celt_encoder_get_size, celt_encoder_init, OpusCustomEncoder, SILKInfo,
};
use crate::celt::entcode::ec_tell;
use crate::celt::entenc::ec_enc;
use crate::celt::entenc::{ec_enc_bit_logp, ec_enc_done, ec_enc_init, ec_enc_shrink, ec_enc_uint};
use crate::celt::float_cast::FLOAT2INT16;
use crate::celt::mathops::{celt_exp2, celt_maxabs16, celt_sqrt};
use crate::celt::modes::OpusCustomMode;
use crate::celt::pitch::celt_inner_prod_c;
use crate::externs::{memcpy, memmove, memset};
use crate::silk::define::{
    DTX_ACTIVITY_THRESHOLD, MAX_CONSECUTIVE_DTX, NB_SPEECH_FRAMES_BEFORE_DTX, VAD_NO_DECISION,
};
use crate::silk::enc_API::silk_EncControlStruct;
use crate::silk::enc_API::{silk_Encode, silk_Get_Encoder_Size, silk_InitEncoder};
use crate::silk::float::structs_FLP::silk_encoder;
use crate::silk::lin2log::silk_lin2log;
use crate::silk::log2lin::silk_log2lin;
use crate::src::analysis::{
    downmix_func, run_analysis, tonality_analysis_init, tonality_analysis_reset, AnalysisInfo,
    TonalityAnalysisState,
};
use crate::src::opus_defines::{
    OPUS_ALLOC_FAIL, OPUS_APPLICATION_AUDIO, OPUS_APPLICATION_RESTRICTED_LOWDELAY,
    OPUS_APPLICATION_VOIP, OPUS_AUTO, OPUS_BAD_ARG, OPUS_BANDWIDTH_FULLBAND,
    OPUS_BANDWIDTH_MEDIUMBAND, OPUS_BANDWIDTH_NARROWBAND, OPUS_BANDWIDTH_SUPERWIDEBAND,
    OPUS_BANDWIDTH_WIDEBAND, OPUS_BITRATE_MAX, OPUS_BUFFER_TOO_SMALL, OPUS_FRAMESIZE_100_MS,
    OPUS_FRAMESIZE_10_MS, OPUS_FRAMESIZE_120_MS, OPUS_FRAMESIZE_20_MS, OPUS_FRAMESIZE_2_5_MS,
    OPUS_FRAMESIZE_40_MS, OPUS_FRAMESIZE_5_MS, OPUS_FRAMESIZE_60_MS, OPUS_FRAMESIZE_80_MS,
    OPUS_FRAMESIZE_ARG, OPUS_GET_APPLICATION_REQUEST, OPUS_GET_BANDWIDTH_REQUEST,
    OPUS_GET_BITRATE_REQUEST, OPUS_GET_COMPLEXITY_REQUEST, OPUS_GET_DTX_REQUEST,
    OPUS_GET_EXPERT_FRAME_DURATION_REQUEST, OPUS_GET_FINAL_RANGE_REQUEST,
    OPUS_GET_FORCE_CHANNELS_REQUEST, OPUS_GET_INBAND_FEC_REQUEST, OPUS_GET_IN_DTX_REQUEST,
    OPUS_GET_LOOKAHEAD_REQUEST, OPUS_GET_LSB_DEPTH_REQUEST, OPUS_GET_MAX_BANDWIDTH_REQUEST,
    OPUS_GET_PACKET_LOSS_PERC_REQUEST, OPUS_GET_PHASE_INVERSION_DISABLED_REQUEST,
    OPUS_GET_PREDICTION_DISABLED_REQUEST, OPUS_GET_SAMPLE_RATE_REQUEST, OPUS_GET_SIGNAL_REQUEST,
    OPUS_GET_VBR_CONSTRAINT_REQUEST, OPUS_GET_VBR_REQUEST, OPUS_INTERNAL_ERROR, OPUS_OK,
    OPUS_RESET_STATE, OPUS_SET_APPLICATION_REQUEST, OPUS_SET_BANDWIDTH_REQUEST,
    OPUS_SET_BITRATE_REQUEST, OPUS_SET_COMPLEXITY_REQUEST, OPUS_SET_DTX_REQUEST,
    OPUS_SET_EXPERT_FRAME_DURATION_REQUEST, OPUS_SET_FORCE_CHANNELS_REQUEST,
    OPUS_SET_INBAND_FEC_REQUEST, OPUS_SET_LSB_DEPTH_REQUEST, OPUS_SET_MAX_BANDWIDTH_REQUEST,
    OPUS_SET_PACKET_LOSS_PERC_REQUEST, OPUS_SET_PHASE_INVERSION_DISABLED_REQUEST,
    OPUS_SET_PREDICTION_DISABLED_REQUEST, OPUS_SET_SIGNAL_REQUEST, OPUS_SET_VBR_CONSTRAINT_REQUEST,
    OPUS_SET_VBR_REQUEST, OPUS_SIGNAL_MUSIC, OPUS_SIGNAL_VOICE, OPUS_UNIMPLEMENTED,
};
use crate::src::opus_private::{
    align, MODE_CELT_ONLY, MODE_HYBRID, MODE_SILK_ONLY, OPUS_GET_VOICE_RATIO_REQUEST,
    OPUS_SET_FORCE_MODE_REQUEST, OPUS_SET_VOICE_RATIO_REQUEST,
};
use crate::varargs::VarArgs;
use crate::{
    opus_custom_encoder_ctl, opus_packet_pad, opus_repacketizer_cat, opus_repacketizer_init,
    opus_repacketizer_out_range_impl, OpusRepacketizer,
};

#[derive(Copy, Clone)]
#[repr(C)]
pub struct OpusEncoder {
    pub(crate) celt_enc_offset: i32,
    pub(crate) silk_enc_offset: i32,
    pub(crate) silk_mode: silk_EncControlStruct,
    pub(crate) application: i32,
    pub(crate) channels: i32,
    pub(crate) delay_compensation: i32,
    pub(crate) force_channels: i32,
    pub(crate) signal_type: i32,
    pub(crate) user_bandwidth: i32,
    pub(crate) max_bandwidth: i32,
    pub(crate) user_forced_mode: i32,
    pub(crate) voice_ratio: i32,
    pub(crate) Fs: i32,
    pub(crate) use_vbr: i32,
    pub(crate) vbr_constraint: i32,
    pub(crate) variable_duration: i32,
    pub(crate) bitrate_bps: i32,
    pub(crate) user_bitrate_bps: i32,
    pub(crate) lsb_depth: i32,
    pub(crate) encoder_buffer: i32,
    pub(crate) lfe: i32,
    pub(crate) arch: i32,
    pub(crate) use_dtx: i32,
    pub(crate) analysis: TonalityAnalysisState,
    pub(crate) stream_channels: i32,
    pub(crate) hybrid_stereo_width_Q14: i16,
    pub(crate) variable_HP_smth2_Q15: i32,
    pub(crate) prev_HB_gain: opus_val16,
    pub(crate) hp_mem: [opus_val32; 4],
    pub(crate) mode: i32,
    pub(crate) prev_mode: i32,
    pub(crate) prev_channels: i32,
    pub(crate) prev_framesize: i32,
    pub(crate) bandwidth: i32,
    pub(crate) auto_bandwidth: i32,
    pub(crate) silk_bw_switch: i32,
    pub(crate) first: i32,
    pub(crate) energy_masking: *mut opus_val16,
    pub(crate) width_mem: StereoWidthState,
    pub(crate) delay_buffer: [opus_val16; 960],
    pub(crate) detected_bandwidth: i32,
    pub(crate) nb_no_activity_frames: i32,
    pub(crate) peak_signal_energy: opus_val32,
    pub(crate) nonfinal_frame: i32,
    pub(crate) rangeFinal: u32,
}
#[derive(Copy, Clone)]
#[repr(C)]
pub struct StereoWidthState {
    pub XX: opus_val32,
    pub XY: opus_val32,
    pub YY: opus_val32,
    pub smoothed_width: opus_val16,
    pub max_follower: opus_val16,
}
pub const PSEUDO_SNR_THRESHOLD: f32 = 316.23f32;
static mut mono_voice_bandwidth_thresholds: [i32; 8] =
    [9000, 700, 9000, 700, 13500, 1000, 14000, 2000];
static mut mono_music_bandwidth_thresholds: [i32; 8] =
    [9000, 700, 9000, 700, 11000, 1000, 12000, 2000];
static mut stereo_voice_bandwidth_thresholds: [i32; 8] =
    [9000, 700, 9000, 700, 13500, 1000, 14000, 2000];
static mut stereo_music_bandwidth_thresholds: [i32; 8] =
    [9000, 700, 9000, 700, 11000, 1000, 12000, 2000];
static mut stereo_voice_threshold: i32 = 19000;
static mut stereo_music_threshold: i32 = 17000;
static mut mode_thresholds: [[i32; 2]; 2] = [[64000, 10000], [44000, 10000]];
static mut fec_thresholds: [i32; 10] = [
    12000, 1000, 14000, 1000, 16000, 1000, 20000, 1000, 22000, 1000,
];
pub unsafe fn opus_encoder_get_size(channels: i32) -> i32 {
    let mut silkEncSizeBytes: i32 = 0;
    let mut celtEncSizeBytes: i32 = 0;
    let mut ret: i32 = 0;
    if channels < 1 || channels > 2 {
        return 0;
    }
    ret = silk_Get_Encoder_Size(&mut silkEncSizeBytes);
    if ret != 0 {
        return 0;
    }
    silkEncSizeBytes = align(silkEncSizeBytes);
    celtEncSizeBytes = celt_encoder_get_size(channels);
    return align(::core::mem::size_of::<OpusEncoder>() as u64 as i32)
        + silkEncSizeBytes
        + celtEncSizeBytes;
}
pub unsafe fn opus_encoder_init(
    st: *mut OpusEncoder,
    Fs: i32,
    channels: i32,
    application: i32,
) -> i32 {
    let mut silk_enc: *mut core::ffi::c_void = 0 as *mut core::ffi::c_void;
    let mut celt_enc: *mut OpusCustomEncoder = 0 as *mut OpusCustomEncoder;
    let mut err: i32 = 0;
    let mut ret: i32 = 0;
    let mut silkEncSizeBytes: i32 = 0;
    if Fs != 48000 && Fs != 24000 && Fs != 16000 && Fs != 12000 && Fs != 8000
        || channels != 1 && channels != 2
        || application != OPUS_APPLICATION_VOIP
            && application != OPUS_APPLICATION_AUDIO
            && application != OPUS_APPLICATION_RESTRICTED_LOWDELAY
    {
        return OPUS_BAD_ARG;
    }
    memset(
        st as *mut i8 as *mut core::ffi::c_void,
        0,
        (opus_encoder_get_size(channels) as u64).wrapping_mul(::core::mem::size_of::<i8>() as u64),
    );
    ret = silk_Get_Encoder_Size(&mut silkEncSizeBytes);
    if ret != 0 {
        return OPUS_BAD_ARG;
    }
    silkEncSizeBytes = align(silkEncSizeBytes);
    (*st).silk_enc_offset = align(::core::mem::size_of::<OpusEncoder>() as u64 as i32);
    (*st).celt_enc_offset = (*st).silk_enc_offset + silkEncSizeBytes;
    silk_enc = (st as *mut i8).offset((*st).silk_enc_offset as isize) as *mut core::ffi::c_void;
    celt_enc = (st as *mut i8).offset((*st).celt_enc_offset as isize) as *mut OpusCustomEncoder;
    (*st).channels = channels;
    (*st).stream_channels = (*st).channels;
    (*st).Fs = Fs;
    (*st).arch = opus_select_arch();
    ret = silk_InitEncoder(silk_enc, (*st).arch, &mut (*st).silk_mode);
    if ret != 0 {
        return OPUS_INTERNAL_ERROR;
    }
    (*st).silk_mode.nChannelsAPI = channels;
    (*st).silk_mode.nChannelsInternal = channels;
    (*st).silk_mode.API_sampleRate = (*st).Fs;
    (*st).silk_mode.maxInternalSampleRate = 16000;
    (*st).silk_mode.minInternalSampleRate = 8000;
    (*st).silk_mode.desiredInternalSampleRate = 16000;
    (*st).silk_mode.payloadSize_ms = 20;
    (*st).silk_mode.bitRate = 25000;
    (*st).silk_mode.packetLossPercentage = 0;
    (*st).silk_mode.complexity = 9;
    (*st).silk_mode.useInBandFEC = 0;
    (*st).silk_mode.useDTX = 0;
    (*st).silk_mode.useCBR = 0;
    (*st).silk_mode.reducedDependency = 0;
    err = celt_encoder_init(celt_enc, Fs, channels, (*st).arch);
    if err != OPUS_OK {
        return OPUS_INTERNAL_ERROR;
    }
    opus_custom_encoder_ctl!(celt_enc, CELT_SET_SIGNALLING_REQUEST, 0);
    opus_custom_encoder_ctl!(
        celt_enc,
        OPUS_SET_COMPLEXITY_REQUEST,
        (*st).silk_mode.complexity,
    );
    (*st).use_vbr = 1;
    (*st).vbr_constraint = 1;
    (*st).user_bitrate_bps = OPUS_AUTO;
    (*st).bitrate_bps = 3000 + Fs * channels;
    (*st).application = application;
    (*st).signal_type = OPUS_AUTO;
    (*st).user_bandwidth = OPUS_AUTO;
    (*st).max_bandwidth = OPUS_BANDWIDTH_FULLBAND;
    (*st).force_channels = OPUS_AUTO;
    (*st).user_forced_mode = OPUS_AUTO;
    (*st).voice_ratio = -1;
    (*st).encoder_buffer = (*st).Fs / 100;
    (*st).lsb_depth = 24;
    (*st).variable_duration = OPUS_FRAMESIZE_ARG;
    (*st).delay_compensation = (*st).Fs / 250;
    (*st).hybrid_stereo_width_Q14 = ((1) << 14) as i16;
    (*st).prev_HB_gain = Q15ONE;
    (*st).variable_HP_smth2_Q15 = ((silk_lin2log(60) as u32) << 8) as i32;
    (*st).first = 1;
    (*st).mode = MODE_HYBRID;
    (*st).bandwidth = OPUS_BANDWIDTH_FULLBAND;
    tonality_analysis_init(&mut (*st).analysis, (*st).Fs);
    (*st).analysis.application = (*st).application;
    return OPUS_OK;
}
unsafe fn gen_toc(mode: i32, mut framerate: i32, bandwidth: i32, channels: i32) -> u8 {
    let mut period: i32 = 0;
    let mut toc: u8 = 0;
    period = 0;
    while framerate < 400 {
        framerate <<= 1;
        period += 1;
    }
    if mode == MODE_SILK_ONLY {
        toc = (bandwidth - OPUS_BANDWIDTH_NARROWBAND << 5) as u8;
        toc = (toc as i32 | (period - 2) << 3) as u8;
    } else if mode == MODE_CELT_ONLY {
        let mut tmp: i32 = bandwidth - OPUS_BANDWIDTH_MEDIUMBAND;
        if tmp < 0 {
            tmp = 0;
        }
        toc = 0x80;
        toc = (toc as i32 | tmp << 5) as u8;
        toc = (toc as i32 | period << 3) as u8;
    } else {
        toc = 0x60;
        toc = (toc as i32 | bandwidth - OPUS_BANDWIDTH_SUPERWIDEBAND << 4) as u8;
        toc = (toc as i32 | (period - 2) << 3) as u8;
    }
    toc = (toc as i32 | ((channels == 2) as i32) << 2) as u8;
    return toc;
}
unsafe fn silk_biquad_float(
    in_0: *const opus_val16,
    B_Q28: *const i32,
    A_Q28: *const i32,
    S: *mut opus_val32,
    out: *mut opus_val16,
    len: i32,
    stride: i32,
) {
    let mut k: i32 = 0;
    let mut vout: opus_val32 = 0.;
    let mut inval: opus_val32 = 0.;
    let mut A: [opus_val32; 2] = [0.; 2];
    let mut B: [opus_val32; 3] = [0.; 3];
    A[0 as usize] = *A_Q28.offset(0 as isize) as f32 * (1.0f32 / ((1) << 28) as f32);
    A[1 as usize] = *A_Q28.offset(1 as isize) as f32 * (1.0f32 / ((1) << 28) as f32);
    B[0 as usize] = *B_Q28.offset(0 as isize) as f32 * (1.0f32 / ((1) << 28) as f32);
    B[1 as usize] = *B_Q28.offset(1 as isize) as f32 * (1.0f32 / ((1) << 28) as f32);
    B[2 as usize] = *B_Q28.offset(2 as isize) as f32 * (1.0f32 / ((1) << 28) as f32);
    k = 0;
    while k < len {
        inval = *in_0.offset((k * stride) as isize);
        vout = *S.offset(0 as isize) + B[0 as usize] * inval;
        *S.offset(0 as isize) =
            *S.offset(1 as isize) - vout * A[0 as usize] + B[1 as usize] * inval;
        *S.offset(1 as isize) = -vout * A[1 as usize] + B[2 as usize] * inval + VERY_SMALL;
        *out.offset((k * stride) as isize) = vout;
        k += 1;
    }
}
unsafe fn hp_cutoff(
    in_0: *const opus_val16,
    cutoff_Hz: i32,
    out: *mut opus_val16,
    hp_mem: *mut opus_val32,
    len: i32,
    channels: i32,
    Fs: i32,
    _arch: i32,
) {
    let mut B_Q28: [i32; 3] = [0; 3];
    let mut A_Q28: [i32; 2] = [0; 2];
    let mut Fc_Q19: i32 = 0;
    let mut r_Q28: i32 = 0;
    let mut r_Q22: i32 = 0;
    Fc_Q19 = (1.5f64 * 3.14159f64 / 1000 as f64 * ((1) << 19) as f64 + 0.5f64) as i32 as i16 as i32
        * cutoff_Hz as i16 as i32
        / (Fs / 1000);
    r_Q28 = (1.0f64 * ((1) << 28) as f64 + 0.5f64) as i32
        - (0.92f64 * ((1) << 9) as f64 + 0.5f64) as i32 * Fc_Q19;
    B_Q28[0 as usize] = r_Q28;
    B_Q28[1 as usize] = ((-r_Q28 as u32) << 1) as i32;
    B_Q28[2 as usize] = r_Q28;
    r_Q22 = r_Q28 >> 6;
    A_Q28[0 as usize] = (r_Q22 as i64
        * ((Fc_Q19 as i64 * Fc_Q19 as i64 >> 16) as i32
            - (2.0f64 * ((1) << 22) as f64 + 0.5f64) as i32) as i64
        >> 16) as i32;
    A_Q28[1 as usize] = (r_Q22 as i64 * r_Q22 as i64 >> 16) as i32;
    silk_biquad_float(
        in_0,
        B_Q28.as_mut_ptr(),
        A_Q28.as_mut_ptr(),
        hp_mem,
        out,
        len,
        channels,
    );
    if channels == 2 {
        silk_biquad_float(
            in_0.offset(1 as isize),
            B_Q28.as_mut_ptr(),
            A_Q28.as_mut_ptr(),
            hp_mem.offset(2 as isize),
            out.offset(1 as isize),
            len,
            channels,
        );
    }
}
unsafe fn dc_reject(
    in_0: *const opus_val16,
    cutoff_Hz: i32,
    out: *mut opus_val16,
    hp_mem: *mut opus_val32,
    len: i32,
    channels: i32,
    Fs: i32,
) {
    let mut i: i32 = 0;
    let mut coef: f32 = 0.;
    let mut coef2: f32 = 0.;
    coef = 6.3f32 * cutoff_Hz as f32 / Fs as f32;
    coef2 = 1 as f32 - coef;
    if channels == 2 {
        let mut m0: f32 = 0.;
        let mut m2: f32 = 0.;
        m0 = *hp_mem.offset(0 as isize);
        m2 = *hp_mem.offset(2 as isize);
        i = 0;
        while i < len {
            let mut x0: opus_val32 = 0.;
            let mut x1: opus_val32 = 0.;
            let mut out0: opus_val32 = 0.;
            let mut out1: opus_val32 = 0.;
            x0 = *in_0.offset((2 * i + 0) as isize);
            x1 = *in_0.offset((2 * i + 1) as isize);
            out0 = x0 - m0;
            out1 = x1 - m2;
            m0 = coef * x0 + VERY_SMALL + coef2 * m0;
            m2 = coef * x1 + VERY_SMALL + coef2 * m2;
            *out.offset((2 * i + 0) as isize) = out0;
            *out.offset((2 * i + 1) as isize) = out1;
            i += 1;
        }
        *hp_mem.offset(0 as isize) = m0;
        *hp_mem.offset(2 as isize) = m2;
    } else {
        let mut m0_0: f32 = 0.;
        m0_0 = *hp_mem.offset(0 as isize);
        i = 0;
        while i < len {
            let mut x: opus_val32 = 0.;
            let mut y: opus_val32 = 0.;
            x = *in_0.offset(i as isize);
            y = x - m0_0;
            m0_0 = coef * x + VERY_SMALL + coef2 * m0_0;
            *out.offset(i as isize) = y;
            i += 1;
        }
        *hp_mem.offset(0 as isize) = m0_0;
    };
}
unsafe fn stereo_fade(
    in_0: *const opus_val16,
    out: *mut opus_val16,
    mut g1: opus_val16,
    mut g2: opus_val16,
    overlap48: i32,
    frame_size: i32,
    channels: i32,
    window: *const opus_val16,
    Fs: i32,
) {
    let mut i: i32 = 0;
    let mut overlap: i32 = 0;
    let mut inc: i32 = 0;
    inc = 48000 / Fs;
    overlap = overlap48 / inc;
    g1 = Q15ONE - g1;
    g2 = Q15ONE - g2;
    i = 0;
    while i < overlap {
        let mut diff: opus_val32 = 0.;
        let mut g: opus_val16 = 0.;
        let mut w: opus_val16 = 0.;
        w = *window.offset((i * inc) as isize) * *window.offset((i * inc) as isize);
        g = w * g2 + (1.0f32 - w) * g1;
        diff = 0.5f32
            * (*in_0.offset((i * channels) as isize) - *in_0.offset((i * channels + 1) as isize));
        diff = g * diff;
        *out.offset((i * channels) as isize) = *out.offset((i * channels) as isize) - diff;
        *out.offset((i * channels + 1) as isize) = *out.offset((i * channels + 1) as isize) + diff;
        i += 1;
    }
    while i < frame_size {
        let mut diff_0: opus_val32 = 0.;
        diff_0 = 0.5f32
            * (*in_0.offset((i * channels) as isize) - *in_0.offset((i * channels + 1) as isize));
        diff_0 = g2 * diff_0;
        *out.offset((i * channels) as isize) = *out.offset((i * channels) as isize) - diff_0;
        *out.offset((i * channels + 1) as isize) =
            *out.offset((i * channels + 1) as isize) + diff_0;
        i += 1;
    }
}
unsafe fn gain_fade(
    in_0: *const opus_val16,
    out: *mut opus_val16,
    g1: opus_val16,
    g2: opus_val16,
    overlap48: i32,
    frame_size: i32,
    channels: i32,
    window: *const opus_val16,
    Fs: i32,
) {
    let mut i: i32 = 0;
    let mut inc: i32 = 0;
    let mut overlap: i32 = 0;
    let mut c: i32 = 0;
    inc = 48000 / Fs;
    overlap = overlap48 / inc;
    if channels == 1 {
        i = 0;
        while i < overlap {
            let mut g: opus_val16 = 0.;
            let mut w: opus_val16 = 0.;
            w = *window.offset((i * inc) as isize) * *window.offset((i * inc) as isize);
            g = w * g2 + (1.0f32 - w) * g1;
            *out.offset(i as isize) = g * *in_0.offset(i as isize);
            i += 1;
        }
    } else {
        i = 0;
        while i < overlap {
            let mut g_0: opus_val16 = 0.;
            let mut w_0: opus_val16 = 0.;
            w_0 = *window.offset((i * inc) as isize) * *window.offset((i * inc) as isize);
            g_0 = w_0 * g2 + (1.0f32 - w_0) * g1;
            *out.offset((i * 2) as isize) = g_0 * *in_0.offset((i * 2) as isize);
            *out.offset((i * 2 + 1) as isize) = g_0 * *in_0.offset((i * 2 + 1) as isize);
            i += 1;
        }
    }
    c = 0;
    loop {
        i = overlap;
        while i < frame_size {
            *out.offset((i * channels + c) as isize) =
                g2 * *in_0.offset((i * channels + c) as isize);
            i += 1;
        }
        c += 1;
        if !(c < channels) {
            break;
        }
    }
}
pub unsafe fn opus_encoder_create(
    Fs: i32,
    channels: i32,
    application: i32,
    error: *mut i32,
) -> *mut OpusEncoder {
    let mut ret: i32 = 0;
    let mut st: *mut OpusEncoder = 0 as *mut OpusEncoder;
    if Fs != 48000 && Fs != 24000 && Fs != 16000 && Fs != 12000 && Fs != 8000
        || channels != 1 && channels != 2
        || application != OPUS_APPLICATION_VOIP
            && application != OPUS_APPLICATION_AUDIO
            && application != OPUS_APPLICATION_RESTRICTED_LOWDELAY
    {
        if !error.is_null() {
            *error = OPUS_BAD_ARG;
        }
        return NULL as *mut OpusEncoder;
    }
    st = malloc(opus_encoder_get_size(channels) as size_t) as *mut OpusEncoder;
    if st.is_null() {
        if !error.is_null() {
            *error = OPUS_ALLOC_FAIL;
        }
        return NULL as *mut OpusEncoder;
    }
    ret = opus_encoder_init(st, Fs, channels, application);
    if !error.is_null() {
        *error = ret;
    }
    if ret != OPUS_OK {
        free(st as *mut core::ffi::c_void);
        st = NULL as *mut OpusEncoder;
    }
    return st;
}
unsafe fn user_bitrate_to_bitrate(
    st: *mut OpusEncoder,
    mut frame_size: i32,
    max_data_bytes: i32,
) -> i32 {
    if frame_size == 0 {
        frame_size = (*st).Fs / 400;
    }
    if (*st).user_bitrate_bps == OPUS_AUTO {
        return 60 * (*st).Fs / frame_size + (*st).Fs * (*st).channels;
    } else if (*st).user_bitrate_bps == OPUS_BITRATE_MAX {
        return max_data_bytes * 8 * (*st).Fs / frame_size;
    } else {
        return (*st).user_bitrate_bps;
    };
}
pub unsafe fn downmix_float(
    mut _x: *const core::ffi::c_void,
    y: *mut opus_val32,
    subframe: i32,
    offset: i32,
    c1: i32,
    c2: i32,
    C: i32,
) {
    let mut x: *const f32 = 0 as *const f32;
    let mut j: i32 = 0;
    x = _x as *const f32;
    j = 0;
    while j < subframe {
        *y.offset(j as isize) = *x.offset(((j + offset) * C + c1) as isize) * CELT_SIG_SCALE;
        j += 1;
    }
    if c2 > -1 {
        j = 0;
        while j < subframe {
            let ref mut fresh0 = *y.offset(j as isize);
            *fresh0 += *x.offset(((j + offset) * C + c2) as isize) * CELT_SIG_SCALE;
            j += 1;
        }
    } else if c2 == -(2) {
        let mut c: i32 = 0;
        c = 1;
        while c < C {
            j = 0;
            while j < subframe {
                let ref mut fresh1 = *y.offset(j as isize);
                *fresh1 += *x.offset(((j + offset) * C + c) as isize) * CELT_SIG_SCALE;
                j += 1;
            }
            c += 1;
        }
    }
}
pub unsafe fn downmix_int(
    mut _x: *const core::ffi::c_void,
    y: *mut opus_val32,
    subframe: i32,
    offset: i32,
    c1: i32,
    c2: i32,
    C: i32,
) {
    let mut x: *const i16 = 0 as *const i16;
    let mut j: i32 = 0;
    x = _x as *const i16;
    j = 0;
    while j < subframe {
        *y.offset(j as isize) = *x.offset(((j + offset) * C + c1) as isize) as opus_val32;
        j += 1;
    }
    if c2 > -1 {
        j = 0;
        while j < subframe {
            let ref mut fresh2 = *y.offset(j as isize);
            *fresh2 += *x.offset(((j + offset) * C + c2) as isize) as i32 as f32;
            j += 1;
        }
    } else if c2 == -(2) {
        let mut c: i32 = 0;
        c = 1;
        while c < C {
            j = 0;
            while j < subframe {
                let ref mut fresh3 = *y.offset(j as isize);
                *fresh3 += *x.offset(((j + offset) * C + c) as isize) as i32 as f32;
                j += 1;
            }
            c += 1;
        }
    }
}
pub unsafe fn frame_size_select(frame_size: i32, variable_duration: i32, Fs: i32) -> i32 {
    let mut new_size: i32 = 0;
    if frame_size < Fs / 400 {
        return -1;
    }
    if variable_duration == OPUS_FRAMESIZE_ARG {
        new_size = frame_size;
    } else if variable_duration >= OPUS_FRAMESIZE_2_5_MS
        && variable_duration <= OPUS_FRAMESIZE_120_MS
    {
        if variable_duration <= OPUS_FRAMESIZE_40_MS {
            new_size = (Fs / 400) << variable_duration - OPUS_FRAMESIZE_2_5_MS;
        } else {
            new_size = (variable_duration - OPUS_FRAMESIZE_2_5_MS - 2) * Fs / 50;
        }
    } else {
        return -1;
    }
    if new_size > frame_size {
        return -1;
    }
    if 400 * new_size != Fs
        && 200 * new_size != Fs
        && 100 * new_size != Fs
        && 50 * new_size != Fs
        && 25 * new_size != Fs
        && 50 * new_size != 3 * Fs
        && 50 * new_size != 4 * Fs
        && 50 * new_size != 5 * Fs
        && 50 * new_size != 6 * Fs
    {
        return -1;
    }
    return new_size;
}
pub unsafe fn compute_stereo_width(
    pcm: *const opus_val16,
    frame_size: i32,
    Fs: i32,
    mem: *mut StereoWidthState,
) -> opus_val16 {
    let mut xx: opus_val32 = 0.;
    let mut xy: opus_val32 = 0.;
    let mut yy: opus_val32 = 0.;
    let mut sqrt_xx: opus_val16 = 0.;
    let mut sqrt_yy: opus_val16 = 0.;
    let mut qrrt_xx: opus_val16 = 0.;
    let mut qrrt_yy: opus_val16 = 0.;
    let mut frame_rate: i32 = 0;
    let mut i: i32 = 0;
    let mut short_alpha: opus_val16 = 0.;
    frame_rate = Fs / frame_size;
    short_alpha =
        Q15ONE - 25 as opus_val32 * 1.0f32 / (if 50 > frame_rate { 50 } else { frame_rate }) as f32;
    yy = 0 as opus_val32;
    xy = yy;
    xx = xy;
    i = 0;
    while i < frame_size - 3 {
        let mut pxx: opus_val32 = 0 as opus_val32;
        let mut pxy: opus_val32 = 0 as opus_val32;
        let mut pyy: opus_val32 = 0 as opus_val32;
        let mut x: opus_val16 = 0.;
        let mut y: opus_val16 = 0.;
        x = *pcm.offset((2 * i) as isize);
        y = *pcm.offset((2 * i + 1) as isize);
        pxx = x * x;
        pxy = x * y;
        pyy = y * y;
        x = *pcm.offset((2 * i + 2) as isize);
        y = *pcm.offset((2 * i + 3) as isize);
        pxx += x * x;
        pxy += x * y;
        pyy += y * y;
        x = *pcm.offset((2 * i + 4) as isize);
        y = *pcm.offset((2 * i + 5) as isize);
        pxx += x * x;
        pxy += x * y;
        pyy += y * y;
        x = *pcm.offset((2 * i + 6) as isize);
        y = *pcm.offset((2 * i + 7) as isize);
        pxx += x * x;
        pxy += x * y;
        pyy += y * y;
        xx += pxx;
        xy += pxy;
        yy += pyy;
        i += 4;
    }
    if !(xx < 1e9f32) || xx != xx || !(yy < 1e9f32) || yy != yy {
        yy = 0 as opus_val32;
        xx = yy;
        xy = xx;
    }
    (*mem).XX += short_alpha * (xx - (*mem).XX);
    (*mem).XY += short_alpha * (xy - (*mem).XY);
    (*mem).YY += short_alpha * (yy - (*mem).YY);
    (*mem).XX = if 0 as f32 > (*mem).XX {
        0 as f32
    } else {
        (*mem).XX
    };
    (*mem).XY = if 0 as f32 > (*mem).XY {
        0 as f32
    } else {
        (*mem).XY
    };
    (*mem).YY = if 0 as f32 > (*mem).YY {
        0 as f32
    } else {
        (*mem).YY
    };
    if (if (*mem).XX > (*mem).YY {
        (*mem).XX
    } else {
        (*mem).YY
    }) > 8e-4f32
    {
        let mut corr: opus_val16 = 0.;
        let mut ldiff: opus_val16 = 0.;
        let mut width: opus_val16 = 0.;
        sqrt_xx = celt_sqrt((*mem).XX);
        sqrt_yy = celt_sqrt((*mem).YY);
        qrrt_xx = celt_sqrt(sqrt_xx);
        qrrt_yy = celt_sqrt(sqrt_yy);
        (*mem).XY = if (*mem).XY < sqrt_xx * sqrt_yy {
            (*mem).XY
        } else {
            sqrt_xx * sqrt_yy
        };
        corr = (*mem).XY / (1e-15f32 + sqrt_xx * sqrt_yy);
        ldiff = 1.0f32 * (qrrt_xx - qrrt_yy).abs() / (EPSILON + qrrt_xx + qrrt_yy);
        width = celt_sqrt(1.0f32 - corr * corr) * ldiff;
        (*mem).smoothed_width += (width - (*mem).smoothed_width) / frame_rate as f32;
        (*mem).max_follower =
            if (*mem).max_follower - 0.02f32 / frame_rate as f32 > (*mem).smoothed_width {
                (*mem).max_follower - 0.02f32 / frame_rate as f32
            } else {
                (*mem).smoothed_width
            };
    }
    return if 1.0f32 < 20 as opus_val32 * (*mem).max_follower {
        1.0f32
    } else {
        20 as opus_val32 * (*mem).max_follower
    };
}
unsafe fn decide_fec(
    useInBandFEC: i32,
    PacketLoss_perc: i32,
    last_fec: i32,
    mode: i32,
    bandwidth: *mut i32,
    rate: i32,
) -> i32 {
    let mut orig_bandwidth: i32 = 0;
    if useInBandFEC == 0 || PacketLoss_perc == 0 || mode == MODE_CELT_ONLY {
        return 0;
    }
    orig_bandwidth = *bandwidth;
    loop {
        let mut hysteresis: i32 = 0;
        let mut LBRR_rate_thres_bps: i32 = 0;
        LBRR_rate_thres_bps =
            fec_thresholds[(2 * (*bandwidth - OPUS_BANDWIDTH_NARROWBAND)) as usize];
        hysteresis = fec_thresholds[(2 * (*bandwidth - OPUS_BANDWIDTH_NARROWBAND) + 1) as usize];
        if last_fec == 1 {
            LBRR_rate_thres_bps -= hysteresis;
        }
        if last_fec == 0 {
            LBRR_rate_thres_bps += hysteresis;
        }
        LBRR_rate_thres_bps = ((LBRR_rate_thres_bps
            * (125
                - (if PacketLoss_perc < 25 {
                    PacketLoss_perc
                } else {
                    25
                }))) as i64
            * (0.01f64 * ((1) << 16) as f64 + 0.5f64) as i32 as i16 as i64
            >> 16) as i32;
        if rate > LBRR_rate_thres_bps {
            return 1;
        } else if PacketLoss_perc <= 5 {
            return 0;
        } else {
            if !(*bandwidth > OPUS_BANDWIDTH_NARROWBAND) {
                break;
            }
            *bandwidth -= 1;
        }
    }
    *bandwidth = orig_bandwidth;
    return 0;
}
unsafe fn compute_silk_rate_for_hybrid(
    mut rate: i32,
    bandwidth: i32,
    frame20ms: i32,
    vbr: i32,
    fec: i32,
    channels: i32,
) -> i32 {
    let mut entry: i32 = 0;
    let mut i: i32 = 0;
    let mut N: i32 = 0;
    let mut silk_rate: i32 = 0;
    static mut rate_table: [[i32; 5]; 7] = [
        [0, 0, 0, 0, 0],
        [12000, 10000, 10000, 11000, 11000],
        [16000, 13500, 13500, 15000, 15000],
        [20000, 16000, 16000, 18000, 18000],
        [24000, 18000, 18000, 21000, 21000],
        [32000, 22000, 22000, 28000, 28000],
        [64000, 38000, 38000, 50000, 50000],
    ];
    rate /= channels;
    entry = 1 + frame20ms + 2 * fec;
    N = (::core::mem::size_of::<[[i32; 5]; 7]>() as u64)
        .wrapping_div(::core::mem::size_of::<[i32; 5]>() as u64) as i32;
    i = 1;
    while i < N {
        if rate_table[i as usize][0 as usize] > rate {
            break;
        }
        i += 1;
    }
    if i == N {
        silk_rate = rate_table[(i - 1) as usize][entry as usize];
        silk_rate += (rate - rate_table[(i - 1) as usize][0 as usize]) / 2;
    } else {
        let mut lo: i32 = 0;
        let mut hi: i32 = 0;
        let mut x0: i32 = 0;
        let mut x1: i32 = 0;
        lo = rate_table[(i - 1) as usize][entry as usize];
        hi = rate_table[i as usize][entry as usize];
        x0 = rate_table[(i - 1) as usize][0 as usize];
        x1 = rate_table[i as usize][0 as usize];
        silk_rate = (lo * (x1 - rate) + hi * (rate - x0)) / (x1 - x0);
    }
    if vbr == 0 {
        silk_rate += 100;
    }
    if bandwidth == OPUS_BANDWIDTH_SUPERWIDEBAND {
        silk_rate += 300;
    }
    silk_rate *= channels;
    if channels == 2 && rate >= 12000 {
        silk_rate -= 1000;
    }
    return silk_rate;
}
unsafe fn compute_equiv_rate(
    bitrate: i32,
    channels: i32,
    frame_rate: i32,
    vbr: i32,
    mode: i32,
    complexity: i32,
    loss: i32,
) -> i32 {
    let mut equiv: i32 = 0;
    equiv = bitrate;
    if frame_rate > 50 {
        equiv -= (40 * channels + 20) * (frame_rate - 50);
    }
    if vbr == 0 {
        equiv -= equiv / 12;
    }
    equiv = equiv * (90 + complexity) / 100;
    if mode == MODE_SILK_ONLY || mode == MODE_HYBRID {
        if complexity < 2 {
            equiv = equiv * 4 / 5;
        }
        equiv -= equiv * loss / (6 * loss + 10);
    } else if mode == MODE_CELT_ONLY {
        if complexity < 5 {
            equiv = equiv * 9 / 10;
        }
    } else {
        equiv -= equiv * loss / (12 * loss + 20);
    }
    return equiv;
}
pub unsafe fn is_digital_silence(
    pcm: *const opus_val16,
    frame_size: i32,
    channels: i32,
    lsb_depth: i32,
) -> i32 {
    let mut silence: i32 = 0;
    let mut sample_max: opus_val32 = 0 as opus_val32;
    sample_max = celt_maxabs16(pcm, frame_size * channels);
    silence = (sample_max <= 1 as opus_val16 / ((1) << lsb_depth) as f32) as i32;
    return silence;
}
unsafe fn compute_frame_energy(
    pcm: *const opus_val16,
    frame_size: i32,
    channels: i32,
    _arch: i32,
) -> opus_val32 {
    let len: i32 = frame_size * channels;
    return celt_inner_prod_c(pcm, pcm, len) / len as f32;
}
unsafe fn decide_dtx_mode(
    activity_probability: f32,
    nb_no_activity_frames: *mut i32,
    peak_signal_energy: opus_val32,
    pcm: *const opus_val16,
    frame_size: i32,
    channels: i32,
    mut is_silence: i32,
    arch: i32,
) -> i32 {
    let mut noise_energy: opus_val32 = 0.;
    if is_silence == 0 {
        if activity_probability < DTX_ACTIVITY_THRESHOLD {
            noise_energy = compute_frame_energy(pcm, frame_size, channels, arch);
            is_silence = (peak_signal_energy >= PSEUDO_SNR_THRESHOLD * noise_energy) as i32;
        }
    }
    if is_silence != 0 {
        *nb_no_activity_frames += 1;
        if *nb_no_activity_frames > NB_SPEECH_FRAMES_BEFORE_DTX {
            if *nb_no_activity_frames <= NB_SPEECH_FRAMES_BEFORE_DTX + MAX_CONSECUTIVE_DTX {
                return 1;
            } else {
                *nb_no_activity_frames = NB_SPEECH_FRAMES_BEFORE_DTX;
            }
        }
    } else {
        *nb_no_activity_frames = 0;
    }
    return 0;
}
unsafe fn encode_multiframe_packet(
    st: *mut OpusEncoder,
    pcm: *const opus_val16,
    nb_frames: i32,
    frame_size: i32,
    data: *mut u8,
    out_data_bytes: i32,
    to_celt: i32,
    lsb_depth: i32,
    float_api: i32,
) -> i32 {
    let mut i: i32 = 0;
    let mut ret: i32 = 0;
    let mut bak_mode: i32 = 0;
    let mut bak_bandwidth: i32 = 0;
    let mut bak_channels: i32 = 0;
    let mut bak_to_mono: i32 = 0;
    let mut max_header_bytes: i32 = 0;
    let mut bytes_per_frame: i32 = 0;
    let mut cbr_bytes: i32 = 0;
    let mut repacketize_len: i32 = 0;
    let mut tmp_len: i32 = 0;
    max_header_bytes = if nb_frames == 2 {
        3
    } else {
        2 + (nb_frames - 1) * 2
    };
    if (*st).use_vbr != 0 || (*st).user_bitrate_bps == OPUS_BITRATE_MAX {
        repacketize_len = out_data_bytes;
    } else {
        cbr_bytes = 3 * (*st).bitrate_bps / (3 * 8 * (*st).Fs / (frame_size * nb_frames));
        repacketize_len = if cbr_bytes < out_data_bytes {
            cbr_bytes
        } else {
            out_data_bytes
        };
    }
    bytes_per_frame = if (1276) < 1 + (repacketize_len - max_header_bytes) / nb_frames {
        1276
    } else {
        1 + (repacketize_len - max_header_bytes) / nb_frames
    };
    let vla = (nb_frames * bytes_per_frame) as usize;
    let mut tmp_data: Vec<u8> = ::std::vec::from_elem(0, vla);
    let mut rp: [OpusRepacketizer; 1] = [OpusRepacketizer {
        toc: 0,
        nb_frames: 0,
        frames: [0 as *const u8; 48],
        len: [0; 48],
        framesize: 0,
    }; 1];
    opus_repacketizer_init(rp.as_mut_ptr());
    bak_mode = (*st).user_forced_mode;
    bak_bandwidth = (*st).user_bandwidth;
    bak_channels = (*st).force_channels;
    (*st).user_forced_mode = (*st).mode;
    (*st).user_bandwidth = (*st).bandwidth;
    (*st).force_channels = (*st).stream_channels;
    bak_to_mono = (*st).silk_mode.toMono;
    if bak_to_mono != 0 {
        (*st).force_channels = 1;
    } else {
        (*st).prev_channels = (*st).stream_channels;
    }
    i = 0;
    while i < nb_frames {
        (*st).silk_mode.toMono = 0;
        (*st).nonfinal_frame = (i < nb_frames - 1) as i32;
        if to_celt != 0 && i == nb_frames - 1 {
            (*st).user_forced_mode = MODE_CELT_ONLY;
        }
        tmp_len = opus_encode_native(
            st,
            pcm.offset((i * ((*st).channels * frame_size)) as isize),
            frame_size,
            tmp_data.as_mut_ptr().offset((i * bytes_per_frame) as isize),
            bytes_per_frame,
            lsb_depth,
            NULL as *const core::ffi::c_void,
            0,
            0,
            0,
            0,
            None,
            float_api,
        );
        if tmp_len < 0 {
            return OPUS_INTERNAL_ERROR;
        }
        ret = opus_repacketizer_cat(
            rp.as_mut_ptr(),
            tmp_data.as_mut_ptr().offset((i * bytes_per_frame) as isize),
            tmp_len,
        );
        if ret < 0 {
            return OPUS_INTERNAL_ERROR;
        }
        i += 1;
    }
    ret = opus_repacketizer_out_range_impl(
        rp.as_mut_ptr(),
        0,
        nb_frames,
        data,
        repacketize_len,
        0,
        ((*st).use_vbr == 0) as i32,
    );
    if ret < 0 {
        return OPUS_INTERNAL_ERROR;
    }
    (*st).user_forced_mode = bak_mode;
    (*st).user_bandwidth = bak_bandwidth;
    (*st).force_channels = bak_channels;
    (*st).silk_mode.toMono = bak_to_mono;
    return ret;
}
unsafe fn compute_redundancy_bytes(
    max_data_bytes: i32,
    bitrate_bps: i32,
    frame_rate: i32,
    channels: i32,
) -> i32 {
    let mut redundancy_bytes_cap: i32 = 0;
    let mut redundancy_bytes: i32 = 0;
    let mut redundancy_rate: i32 = 0;
    let mut base_bits: i32 = 0;
    let mut available_bits: i32 = 0;
    base_bits = 40 * channels + 20;
    redundancy_rate = bitrate_bps + base_bits * (200 - frame_rate);
    redundancy_rate = 3 * redundancy_rate / 2;
    redundancy_bytes = redundancy_rate / 1600;
    available_bits = max_data_bytes * 8 - 2 * base_bits;
    redundancy_bytes_cap = (available_bits * 240 / (240 + 48000 / frame_rate) + base_bits) / 8;
    redundancy_bytes = if redundancy_bytes < redundancy_bytes_cap {
        redundancy_bytes
    } else {
        redundancy_bytes_cap
    };
    if redundancy_bytes > 4 + 8 * channels {
        redundancy_bytes = if (257) < redundancy_bytes {
            257
        } else {
            redundancy_bytes
        };
    } else {
        redundancy_bytes = 0;
    }
    return redundancy_bytes;
}
pub unsafe fn opus_encode_native(
    st: *mut OpusEncoder,
    pcm: *const opus_val16,
    frame_size: i32,
    mut data: *mut u8,
    out_data_bytes: i32,
    mut lsb_depth: i32,
    analysis_pcm: *const core::ffi::c_void,
    analysis_size: i32,
    c1: i32,
    c2: i32,
    analysis_channels: i32,
    downmix: downmix_func,
    float_api: i32,
) -> i32 {
    let mut silk_enc: *mut core::ffi::c_void = 0 as *mut core::ffi::c_void;
    let mut celt_enc: *mut OpusCustomEncoder = 0 as *mut OpusCustomEncoder;
    let mut i: i32 = 0;
    let mut ret: i32 = 0;
    let mut nBytes: i32 = 0;
    let mut enc: ec_enc = ec_enc {
        buf: &mut [],
        storage: 0,
        end_offs: 0,
        end_window: 0,
        nend_bits: 0,
        nbits_total: 0,
        offs: 0,
        rng: 0,
        val: 0,
        ext: 0,
        rem: 0,
        error: 0,
    };
    let mut bytes_target: i32 = 0;
    let mut prefill: i32 = 0;
    let mut start_band: i32 = 0;
    let mut redundancy: i32 = 0;
    let mut redundancy_bytes: i32 = 0;
    let mut celt_to_silk: i32 = 0;
    let mut nb_compr_bytes: i32 = 0;
    let mut to_celt: i32 = 0;
    let mut redundant_rng: u32 = 0;
    let mut cutoff_Hz: i32 = 0;
    let mut hp_freq_smth1: i32 = 0;
    let mut voice_est: i32 = 0;
    let mut equiv_rate: i32 = 0;
    let mut delay_compensation: i32 = 0;
    let mut frame_rate: i32 = 0;
    let mut max_rate: i32 = 0;
    let mut curr_bandwidth: i32 = 0;
    let mut HB_gain: opus_val16 = 0.;
    let mut max_data_bytes: i32 = 0;
    let mut total_buffer: i32 = 0;
    let mut stereo_width: opus_val16 = 0.;
    let mut celt_mode: *const OpusCustomMode = 0 as *const OpusCustomMode;
    let mut analysis_info: AnalysisInfo = AnalysisInfo {
        valid: 0,
        tonality: 0.,
        tonality_slope: 0.,
        noisiness: 0.,
        activity: 0.,
        music_prob: 0.,
        music_prob_min: 0.,
        music_prob_max: 0.,
        bandwidth: 0,
        activity_probability: 0.,
        max_pitch_ratio: 0.,
        leak_boost: [0; 19],
    };
    let mut analysis_read_pos_bak: i32 = -1;
    let mut analysis_read_subframe_bak: i32 = -1;
    let mut is_silence: i32 = 0;
    max_data_bytes = if (1276) < out_data_bytes {
        1276
    } else {
        out_data_bytes
    };
    (*st).rangeFinal = 0;
    if frame_size <= 0 || max_data_bytes <= 0 {
        return OPUS_BAD_ARG;
    }
    if max_data_bytes == 1 && (*st).Fs == frame_size * 10 {
        return OPUS_BUFFER_TOO_SMALL;
    }
    silk_enc = (st as *mut i8).offset((*st).silk_enc_offset as isize) as *mut core::ffi::c_void;
    celt_enc = (st as *mut i8).offset((*st).celt_enc_offset as isize) as *mut OpusCustomEncoder;
    if (*st).application == OPUS_APPLICATION_RESTRICTED_LOWDELAY {
        delay_compensation = 0;
    } else {
        delay_compensation = (*st).delay_compensation;
    }
    lsb_depth = if lsb_depth < (*st).lsb_depth {
        lsb_depth
    } else {
        (*st).lsb_depth
    };
    opus_custom_encoder_ctl!(celt_enc, CELT_GET_MODE_REQUEST, &mut celt_mode);
    analysis_info.valid = 0;
    if (*st).silk_mode.complexity >= 7 && (*st).Fs >= 16000 {
        is_silence = is_digital_silence(pcm, frame_size, (*st).channels, lsb_depth);
        analysis_read_pos_bak = (*st).analysis.read_pos;
        analysis_read_subframe_bak = (*st).analysis.read_subframe;
        run_analysis(
            &mut (*st).analysis,
            celt_mode,
            analysis_pcm,
            analysis_size,
            frame_size,
            c1,
            c2,
            analysis_channels,
            (*st).Fs,
            lsb_depth,
            downmix,
            &mut analysis_info,
        );
        if is_silence == 0 && analysis_info.activity_probability > DTX_ACTIVITY_THRESHOLD {
            (*st).peak_signal_energy = if 0.999f32 * (*st).peak_signal_energy
                > compute_frame_energy(pcm, frame_size, (*st).channels, (*st).arch)
            {
                0.999f32 * (*st).peak_signal_energy
            } else {
                compute_frame_energy(pcm, frame_size, (*st).channels, (*st).arch)
            };
        }
    } else if (*st).analysis.initialized != 0 {
        tonality_analysis_reset(&mut (*st).analysis);
    }
    if is_silence == 0 {
        (*st).voice_ratio = -1;
    }
    (*st).detected_bandwidth = 0;
    if analysis_info.valid != 0 {
        let mut analysis_bandwidth: i32 = 0;
        if (*st).signal_type == OPUS_AUTO {
            let mut prob: f32 = 0.;
            if (*st).prev_mode == 0 {
                prob = analysis_info.music_prob;
            } else if (*st).prev_mode == MODE_CELT_ONLY {
                prob = analysis_info.music_prob_max;
            } else {
                prob = analysis_info.music_prob_min;
            }
            (*st).voice_ratio = (0.5 + (100.0 * (1.0 - prob))).floor() as i32;
        }
        analysis_bandwidth = analysis_info.bandwidth;
        if analysis_bandwidth <= 12 {
            (*st).detected_bandwidth = OPUS_BANDWIDTH_NARROWBAND;
        } else if analysis_bandwidth <= 14 {
            (*st).detected_bandwidth = OPUS_BANDWIDTH_MEDIUMBAND;
        } else if analysis_bandwidth <= 16 {
            (*st).detected_bandwidth = OPUS_BANDWIDTH_WIDEBAND;
        } else if analysis_bandwidth <= 18 {
            (*st).detected_bandwidth = OPUS_BANDWIDTH_SUPERWIDEBAND;
        } else {
            (*st).detected_bandwidth = OPUS_BANDWIDTH_FULLBAND;
        }
    }
    if (*st).channels == 2 && (*st).force_channels != 1 {
        stereo_width = compute_stereo_width(pcm, frame_size, (*st).Fs, &mut (*st).width_mem);
    } else {
        stereo_width = 0 as opus_val16;
    }
    total_buffer = delay_compensation;
    (*st).bitrate_bps = user_bitrate_to_bitrate(st, frame_size, max_data_bytes);
    frame_rate = (*st).Fs / frame_size;
    if (*st).use_vbr == 0 {
        let mut cbrBytes: i32 = 0;
        let frame_rate12: i32 = 12 * (*st).Fs / frame_size;
        cbrBytes =
            if (12 * (*st).bitrate_bps / 8 + frame_rate12 / 2) / frame_rate12 < max_data_bytes {
                (12 * (*st).bitrate_bps / 8 + frame_rate12 / 2) / frame_rate12
            } else {
                max_data_bytes
            };
        (*st).bitrate_bps = cbrBytes * frame_rate12 * 8 / 12;
        max_data_bytes = if 1 > cbrBytes { 1 } else { cbrBytes };
    }
    if max_data_bytes < 3
        || (*st).bitrate_bps < 3 * frame_rate * 8
        || frame_rate < 50 && (max_data_bytes * frame_rate < 300 || (*st).bitrate_bps < 2400)
    {
        let mut tocmode: i32 = (*st).mode;
        let mut bw: i32 = if (*st).bandwidth == 0 {
            OPUS_BANDWIDTH_NARROWBAND
        } else {
            (*st).bandwidth
        };
        let mut packet_code: i32 = 0;
        let mut num_multiframes: i32 = 0;
        if tocmode == 0 {
            tocmode = MODE_SILK_ONLY;
        }
        if frame_rate > 100 {
            tocmode = MODE_CELT_ONLY;
        }
        if frame_rate == 25 && tocmode != MODE_SILK_ONLY {
            frame_rate = 50;
            packet_code = 1;
        }
        if frame_rate <= 16 {
            if out_data_bytes == 1 || tocmode == MODE_SILK_ONLY && frame_rate != 10 {
                tocmode = MODE_SILK_ONLY;
                packet_code = (frame_rate <= 12) as i32;
                frame_rate = if frame_rate == 12 { 25 } else { 16 };
            } else {
                num_multiframes = 50 / frame_rate;
                frame_rate = 50;
                packet_code = 3;
            }
        }
        if tocmode == MODE_SILK_ONLY && bw > OPUS_BANDWIDTH_WIDEBAND {
            bw = OPUS_BANDWIDTH_WIDEBAND;
        } else if tocmode == MODE_CELT_ONLY && bw == OPUS_BANDWIDTH_MEDIUMBAND {
            bw = OPUS_BANDWIDTH_NARROWBAND;
        } else if tocmode == MODE_HYBRID && bw <= OPUS_BANDWIDTH_SUPERWIDEBAND {
            bw = OPUS_BANDWIDTH_SUPERWIDEBAND;
        }
        *data.offset(0 as isize) = gen_toc(tocmode, frame_rate, bw, (*st).stream_channels);
        let ref mut fresh4 = *data.offset(0 as isize);
        *fresh4 = (*fresh4 as i32 | packet_code) as u8;
        ret = if packet_code <= 1 { 1 } else { 2 };
        max_data_bytes = if max_data_bytes > ret {
            max_data_bytes
        } else {
            ret
        };
        if packet_code == 3 {
            *data.offset(1 as isize) = num_multiframes as u8;
        }
        if (*st).use_vbr == 0 {
            ret = opus_packet_pad(data, ret, max_data_bytes);
            if ret == OPUS_OK {
                ret = max_data_bytes;
            } else {
                ret = OPUS_INTERNAL_ERROR;
            }
        }
        return ret;
    }
    max_rate = frame_rate * max_data_bytes * 8;
    equiv_rate = compute_equiv_rate(
        (*st).bitrate_bps,
        (*st).channels,
        (*st).Fs / frame_size,
        (*st).use_vbr,
        0,
        (*st).silk_mode.complexity,
        (*st).silk_mode.packetLossPercentage,
    );
    if (*st).signal_type == OPUS_SIGNAL_VOICE {
        voice_est = 127;
    } else if (*st).signal_type == OPUS_SIGNAL_MUSIC {
        voice_est = 0;
    } else if (*st).voice_ratio >= 0 {
        voice_est = (*st).voice_ratio * 327 >> 8;
        if (*st).application == OPUS_APPLICATION_AUDIO {
            voice_est = if voice_est < 115 { voice_est } else { 115 };
        }
    } else if (*st).application == OPUS_APPLICATION_VOIP {
        voice_est = 115;
    } else {
        voice_est = 48;
    }
    if (*st).force_channels != OPUS_AUTO && (*st).channels == 2 {
        (*st).stream_channels = (*st).force_channels;
    } else if (*st).channels == 2 {
        let mut stereo_threshold: i32 = 0;
        stereo_threshold = stereo_music_threshold
            + (voice_est * voice_est * (stereo_voice_threshold - stereo_music_threshold) >> 14);
        if (*st).stream_channels == 2 {
            stereo_threshold -= 1000;
        } else {
            stereo_threshold += 1000;
        }
        (*st).stream_channels = if equiv_rate > stereo_threshold { 2 } else { 1 };
    } else {
        (*st).stream_channels = (*st).channels;
    }
    equiv_rate = compute_equiv_rate(
        (*st).bitrate_bps,
        (*st).stream_channels,
        (*st).Fs / frame_size,
        (*st).use_vbr,
        0,
        (*st).silk_mode.complexity,
        (*st).silk_mode.packetLossPercentage,
    );
    (*st).silk_mode.useDTX =
        ((*st).use_dtx != 0 && !(analysis_info.valid != 0 || is_silence != 0)) as i32;
    if (*st).application == OPUS_APPLICATION_RESTRICTED_LOWDELAY {
        (*st).mode = MODE_CELT_ONLY;
    } else if (*st).user_forced_mode == OPUS_AUTO {
        let mut mode_voice: i32 = 0;
        let mut mode_music: i32 = 0;
        let mut threshold: i32 = 0;
        mode_voice = ((1.0f32 - stereo_width) * mode_thresholds[0 as usize][0 as usize] as f32
            + stereo_width * mode_thresholds[1 as usize][0 as usize] as f32)
            as i32;
        mode_music = ((1.0f32 - stereo_width) * mode_thresholds[1 as usize][1 as usize] as f32
            + stereo_width * mode_thresholds[1 as usize][1 as usize] as f32)
            as i32;
        threshold = mode_music + (voice_est * voice_est * (mode_voice - mode_music) >> 14);
        if (*st).application == OPUS_APPLICATION_VOIP {
            threshold += 8000;
        }
        if (*st).prev_mode == MODE_CELT_ONLY {
            threshold -= 4000;
        } else if (*st).prev_mode > 0 {
            threshold += 4000;
        }
        (*st).mode = if equiv_rate >= threshold {
            MODE_CELT_ONLY
        } else {
            MODE_SILK_ONLY
        };
        if (*st).silk_mode.useInBandFEC != 0
            && (*st).silk_mode.packetLossPercentage > 128 - voice_est >> 4
        {
            (*st).mode = MODE_SILK_ONLY;
        }
        if (*st).silk_mode.useDTX != 0 && voice_est > 100 {
            (*st).mode = MODE_SILK_ONLY;
        }
        if max_data_bytes
            < (if frame_rate > 50 { 9000 } else { 6000 }) * frame_size / ((*st).Fs * 8)
        {
            (*st).mode = MODE_CELT_ONLY;
        }
    } else {
        (*st).mode = (*st).user_forced_mode;
    }
    if (*st).mode != MODE_CELT_ONLY && frame_size < (*st).Fs / 100 {
        (*st).mode = MODE_CELT_ONLY;
    }
    if (*st).lfe != 0 {
        (*st).mode = MODE_CELT_ONLY;
    }
    if (*st).prev_mode > 0
        && ((*st).mode != MODE_CELT_ONLY && (*st).prev_mode == MODE_CELT_ONLY
            || (*st).mode == MODE_CELT_ONLY && (*st).prev_mode != MODE_CELT_ONLY)
    {
        redundancy = 1;
        celt_to_silk = ((*st).mode != MODE_CELT_ONLY) as i32;
        if celt_to_silk == 0 {
            if frame_size >= (*st).Fs / 100 {
                (*st).mode = (*st).prev_mode;
                to_celt = 1;
            } else {
                redundancy = 0;
            }
        }
    }
    if (*st).stream_channels == 1
        && (*st).prev_channels == 2
        && (*st).silk_mode.toMono == 0
        && (*st).mode != MODE_CELT_ONLY
        && (*st).prev_mode != MODE_CELT_ONLY
    {
        (*st).silk_mode.toMono = 1;
        (*st).stream_channels = 2;
    } else {
        (*st).silk_mode.toMono = 0;
    }
    equiv_rate = compute_equiv_rate(
        (*st).bitrate_bps,
        (*st).stream_channels,
        (*st).Fs / frame_size,
        (*st).use_vbr,
        (*st).mode,
        (*st).silk_mode.complexity,
        (*st).silk_mode.packetLossPercentage,
    );
    if (*st).mode != MODE_CELT_ONLY && (*st).prev_mode == MODE_CELT_ONLY {
        let mut dummy: silk_EncControlStruct = silk_EncControlStruct {
            nChannelsAPI: 0,
            nChannelsInternal: 0,
            API_sampleRate: 0,
            maxInternalSampleRate: 0,
            minInternalSampleRate: 0,
            desiredInternalSampleRate: 0,
            payloadSize_ms: 0,
            bitRate: 0,
            packetLossPercentage: 0,
            complexity: 0,
            useInBandFEC: 0,
            LBRR_coded: 0,
            useDTX: 0,
            useCBR: 0,
            maxBits: 0,
            toMono: 0,
            opusCanSwitch: 0,
            reducedDependency: 0,
            internalSampleRate: 0,
            allowBandwidthSwitch: 0,
            inWBmodeWithoutVariableLP: 0,
            stereoWidth_Q14: 0,
            switchReady: 0,
            signalType: 0,
            offset: 0,
        };
        silk_InitEncoder(silk_enc, (*st).arch, &mut dummy);
        prefill = 1;
    }
    if (*st).mode == MODE_CELT_ONLY || (*st).first != 0 || (*st).silk_mode.allowBandwidthSwitch != 0
    {
        let mut voice_bandwidth_thresholds: *const i32 = 0 as *const i32;
        let mut music_bandwidth_thresholds: *const i32 = 0 as *const i32;
        let mut bandwidth_thresholds: [i32; 8] = [0; 8];
        let mut bandwidth: i32 = OPUS_BANDWIDTH_FULLBAND;
        if (*st).channels == 2 && (*st).force_channels != 1 {
            voice_bandwidth_thresholds = stereo_voice_bandwidth_thresholds.as_ptr();
            music_bandwidth_thresholds = stereo_music_bandwidth_thresholds.as_ptr();
        } else {
            voice_bandwidth_thresholds = mono_voice_bandwidth_thresholds.as_ptr();
            music_bandwidth_thresholds = mono_music_bandwidth_thresholds.as_ptr();
        }
        i = 0;
        while i < 8 {
            bandwidth_thresholds[i as usize] = *music_bandwidth_thresholds.offset(i as isize)
                + (voice_est
                    * voice_est
                    * (*voice_bandwidth_thresholds.offset(i as isize)
                        - *music_bandwidth_thresholds.offset(i as isize))
                    >> 14);
            i += 1;
        }
        loop {
            let mut threshold_0: i32 = 0;
            let mut hysteresis: i32 = 0;
            threshold_0 =
                bandwidth_thresholds[(2 * (bandwidth - OPUS_BANDWIDTH_MEDIUMBAND)) as usize];
            hysteresis =
                bandwidth_thresholds[(2 * (bandwidth - OPUS_BANDWIDTH_MEDIUMBAND) + 1) as usize];
            if (*st).first == 0 {
                if (*st).auto_bandwidth >= bandwidth {
                    threshold_0 -= hysteresis;
                } else {
                    threshold_0 += hysteresis;
                }
            }
            if equiv_rate >= threshold_0 {
                break;
            }
            bandwidth -= 1;
            if !(bandwidth > OPUS_BANDWIDTH_NARROWBAND) {
                break;
            }
        }
        if bandwidth == OPUS_BANDWIDTH_MEDIUMBAND {
            bandwidth = OPUS_BANDWIDTH_WIDEBAND;
        }
        (*st).auto_bandwidth = bandwidth;
        (*st).bandwidth = (*st).auto_bandwidth;
        if (*st).first == 0
            && (*st).mode != MODE_CELT_ONLY
            && (*st).silk_mode.inWBmodeWithoutVariableLP == 0
            && (*st).bandwidth > OPUS_BANDWIDTH_WIDEBAND
        {
            (*st).bandwidth = OPUS_BANDWIDTH_WIDEBAND;
        }
    }
    if (*st).bandwidth > (*st).max_bandwidth {
        (*st).bandwidth = (*st).max_bandwidth;
    }
    if (*st).user_bandwidth != OPUS_AUTO {
        (*st).bandwidth = (*st).user_bandwidth;
    }
    if (*st).mode != MODE_CELT_ONLY && max_rate < 15000 {
        (*st).bandwidth = if (*st).bandwidth < 1103 {
            (*st).bandwidth
        } else {
            1103
        };
    }
    if (*st).Fs <= 24000 && (*st).bandwidth > OPUS_BANDWIDTH_SUPERWIDEBAND {
        (*st).bandwidth = OPUS_BANDWIDTH_SUPERWIDEBAND;
    }
    if (*st).Fs <= 16000 && (*st).bandwidth > OPUS_BANDWIDTH_WIDEBAND {
        (*st).bandwidth = OPUS_BANDWIDTH_WIDEBAND;
    }
    if (*st).Fs <= 12000 && (*st).bandwidth > OPUS_BANDWIDTH_MEDIUMBAND {
        (*st).bandwidth = OPUS_BANDWIDTH_MEDIUMBAND;
    }
    if (*st).Fs <= 8000 && (*st).bandwidth > OPUS_BANDWIDTH_NARROWBAND {
        (*st).bandwidth = OPUS_BANDWIDTH_NARROWBAND;
    }
    if (*st).detected_bandwidth != 0 && (*st).user_bandwidth == OPUS_AUTO {
        let mut min_detected_bandwidth: i32 = 0;
        if equiv_rate <= 18000 * (*st).stream_channels && (*st).mode == MODE_CELT_ONLY {
            min_detected_bandwidth = OPUS_BANDWIDTH_NARROWBAND;
        } else if equiv_rate <= 24000 * (*st).stream_channels && (*st).mode == MODE_CELT_ONLY {
            min_detected_bandwidth = OPUS_BANDWIDTH_MEDIUMBAND;
        } else if equiv_rate <= 30000 * (*st).stream_channels {
            min_detected_bandwidth = OPUS_BANDWIDTH_WIDEBAND;
        } else if equiv_rate <= 44000 * (*st).stream_channels {
            min_detected_bandwidth = OPUS_BANDWIDTH_SUPERWIDEBAND;
        } else {
            min_detected_bandwidth = OPUS_BANDWIDTH_FULLBAND;
        }
        (*st).detected_bandwidth = if (*st).detected_bandwidth > min_detected_bandwidth {
            (*st).detected_bandwidth
        } else {
            min_detected_bandwidth
        };
        (*st).bandwidth = if (*st).bandwidth < (*st).detected_bandwidth {
            (*st).bandwidth
        } else {
            (*st).detected_bandwidth
        };
    }
    (*st).silk_mode.LBRR_coded = decide_fec(
        (*st).silk_mode.useInBandFEC,
        (*st).silk_mode.packetLossPercentage,
        (*st).silk_mode.LBRR_coded,
        (*st).mode,
        &mut (*st).bandwidth,
        equiv_rate,
    );
    opus_custom_encoder_ctl!(celt_enc, OPUS_SET_LSB_DEPTH_REQUEST, lsb_depth);
    if (*st).mode == MODE_CELT_ONLY && (*st).bandwidth == OPUS_BANDWIDTH_MEDIUMBAND {
        (*st).bandwidth = OPUS_BANDWIDTH_WIDEBAND;
    }
    if (*st).lfe != 0 {
        (*st).bandwidth = OPUS_BANDWIDTH_NARROWBAND;
    }
    curr_bandwidth = (*st).bandwidth;
    if (*st).mode == MODE_SILK_ONLY && curr_bandwidth > OPUS_BANDWIDTH_WIDEBAND {
        (*st).mode = MODE_HYBRID;
    }
    if (*st).mode == MODE_HYBRID && curr_bandwidth <= OPUS_BANDWIDTH_WIDEBAND {
        (*st).mode = MODE_SILK_ONLY;
    }
    if frame_size > (*st).Fs / 50 && (*st).mode != MODE_SILK_ONLY || frame_size > 3 * (*st).Fs / 50
    {
        let mut enc_frame_size: i32 = 0;
        let mut nb_frames: i32 = 0;
        if (*st).mode == MODE_SILK_ONLY {
            if frame_size == 2 * (*st).Fs / 25 {
                enc_frame_size = (*st).Fs / 25;
            } else if frame_size == 3 * (*st).Fs / 25 {
                enc_frame_size = 3 * (*st).Fs / 50;
            } else {
                enc_frame_size = (*st).Fs / 50;
            }
        } else {
            enc_frame_size = (*st).Fs / 50;
        }
        nb_frames = frame_size / enc_frame_size;
        if analysis_read_pos_bak != -1 {
            (*st).analysis.read_pos = analysis_read_pos_bak;
            (*st).analysis.read_subframe = analysis_read_subframe_bak;
        }
        ret = encode_multiframe_packet(
            st,
            pcm,
            nb_frames,
            enc_frame_size,
            data,
            out_data_bytes,
            to_celt,
            lsb_depth,
            float_api,
        );
        return ret;
    }
    if (*st).silk_bw_switch != 0 {
        redundancy = 1;
        celt_to_silk = 1;
        (*st).silk_bw_switch = 0;
        prefill = 2;
    }
    if (*st).mode == MODE_CELT_ONLY {
        redundancy = 0;
    }
    if redundancy != 0 {
        redundancy_bytes = compute_redundancy_bytes(
            max_data_bytes,
            (*st).bitrate_bps,
            frame_rate,
            (*st).stream_channels,
        );
        if redundancy_bytes == 0 {
            redundancy = 0;
        }
    }
    bytes_target =
        (if max_data_bytes - redundancy_bytes < (*st).bitrate_bps * frame_size / ((*st).Fs * 8) {
            max_data_bytes - redundancy_bytes
        } else {
            (*st).bitrate_bps * frame_size / ((*st).Fs * 8)
        }) - 1;
    data = data.offset(1 as isize);
    enc = ec_enc_init(std::slice::from_raw_parts_mut(
        data,
        (max_data_bytes - 1) as usize,
    ));
    let vla = ((total_buffer + frame_size) * (*st).channels) as usize;
    let mut pcm_buf: Vec<opus_val16> = ::std::vec::from_elem(0., vla);
    memcpy(
        pcm_buf.as_mut_ptr() as *mut core::ffi::c_void,
        &mut *((*st).delay_buffer)
            .as_mut_ptr()
            .offset((((*st).encoder_buffer - total_buffer) * (*st).channels) as isize)
            as *mut opus_val16 as *const core::ffi::c_void,
        ((total_buffer * (*st).channels) as u64)
            .wrapping_mul(::core::mem::size_of::<opus_val16>() as u64)
            .wrapping_add(
                (0 * pcm_buf.as_mut_ptr().offset_from(
                    &mut *((*st).delay_buffer)
                        .as_mut_ptr()
                        .offset((((*st).encoder_buffer - total_buffer) * (*st).channels) as isize),
                ) as i64) as u64,
            ),
    );
    if (*st).mode == MODE_CELT_ONLY {
        hp_freq_smth1 = ((silk_lin2log(60) as u32) << 8) as i32;
    } else {
        hp_freq_smth1 = (*(silk_enc as *mut silk_encoder)).state_Fxx[0 as usize]
            .sCmn
            .variable_HP_smth1_Q15;
    }
    (*st).variable_HP_smth2_Q15 = ((*st).variable_HP_smth2_Q15 as i64
        + ((hp_freq_smth1 - (*st).variable_HP_smth2_Q15) as i64
            * ((0.015f32 * ((1) << 16) as f32) as f64 + 0.5f64) as i32 as i16 as i64
            >> 16)) as i32;
    cutoff_Hz = silk_log2lin((*st).variable_HP_smth2_Q15 >> 8);
    if (*st).application == OPUS_APPLICATION_VOIP {
        hp_cutoff(
            pcm,
            cutoff_Hz,
            &mut *pcm_buf
                .as_mut_ptr()
                .offset((total_buffer * (*st).channels) as isize),
            ((*st).hp_mem).as_mut_ptr(),
            frame_size,
            (*st).channels,
            (*st).Fs,
            (*st).arch,
        );
    } else {
        dc_reject(
            pcm,
            3,
            &mut *pcm_buf
                .as_mut_ptr()
                .offset((total_buffer * (*st).channels) as isize),
            ((*st).hp_mem).as_mut_ptr(),
            frame_size,
            (*st).channels,
            (*st).Fs,
        );
    }
    if float_api != 0 {
        let mut sum: opus_val32 = 0.;
        sum = celt_inner_prod_c(
            &mut *pcm_buf
                .as_mut_ptr()
                .offset((total_buffer * (*st).channels) as isize),
            &mut *pcm_buf
                .as_mut_ptr()
                .offset((total_buffer * (*st).channels) as isize),
            frame_size * (*st).channels,
        );
        if !(sum < 1e9f32) || sum != sum {
            memset(
                &mut *pcm_buf
                    .as_mut_ptr()
                    .offset((total_buffer * (*st).channels) as isize)
                    as *mut opus_val16 as *mut core::ffi::c_void,
                0,
                ((frame_size * (*st).channels) as u64)
                    .wrapping_mul(::core::mem::size_of::<opus_val16>() as u64),
            );
            (*st).hp_mem[3 as usize] = 0 as opus_val32;
            (*st).hp_mem[2 as usize] = (*st).hp_mem[3 as usize];
            (*st).hp_mem[1 as usize] = (*st).hp_mem[2 as usize];
            (*st).hp_mem[0 as usize] = (*st).hp_mem[1 as usize];
        }
    }
    HB_gain = Q15ONE;
    if (*st).mode != MODE_CELT_ONLY {
        let mut total_bitRate: i32 = 0;
        let mut celt_rate: i32 = 0;
        let mut activity: i32 = 0;
        let vla_0 = ((*st).channels * frame_size) as usize;
        let mut pcm_silk: Vec<i16> = ::std::vec::from_elem(0, vla_0);
        activity = VAD_NO_DECISION;
        if analysis_info.valid != 0 {
            activity = (analysis_info.activity_probability >= DTX_ACTIVITY_THRESHOLD) as i32;
        }
        total_bitRate = 8 * bytes_target * frame_rate;
        if (*st).mode == MODE_HYBRID {
            (*st).silk_mode.bitRate = compute_silk_rate_for_hybrid(
                total_bitRate,
                curr_bandwidth,
                ((*st).Fs == 50 * frame_size) as i32,
                (*st).use_vbr,
                (*st).silk_mode.LBRR_coded,
                (*st).stream_channels,
            );
            if ((*st).energy_masking).is_null() {
                celt_rate = total_bitRate - (*st).silk_mode.bitRate;
                HB_gain = Q15ONE - celt_exp2(-celt_rate as f32 * (1.0f32 / 1024f32));
            }
        } else {
            (*st).silk_mode.bitRate = total_bitRate;
        }
        if !((*st).energy_masking).is_null() && (*st).use_vbr != 0 && (*st).lfe == 0 {
            let mut mask_sum: opus_val32 = 0 as opus_val32;
            let mut masking_depth: opus_val16 = 0.;
            let mut rate_offset: i32 = 0;
            let mut c: i32 = 0;
            let mut end: i32 = 17;
            let mut srate: i16 = 16000;
            if (*st).bandwidth == OPUS_BANDWIDTH_NARROWBAND {
                end = 13;
                srate = 8000;
            } else if (*st).bandwidth == OPUS_BANDWIDTH_MEDIUMBAND {
                end = 15;
                srate = 12000;
            }
            c = 0;
            while c < (*st).channels {
                i = 0;
                while i < end {
                    let mut mask: opus_val16 = 0.;
                    mask = if (if *((*st).energy_masking).offset((21 * c + i) as isize) < 0.5f32 {
                        *((*st).energy_masking).offset((21 * c + i) as isize)
                    } else {
                        0.5f32
                    }) > -2.0f32
                    {
                        if *((*st).energy_masking).offset((21 * c + i) as isize) < 0.5f32 {
                            *((*st).energy_masking).offset((21 * c + i) as isize)
                        } else {
                            0.5f32
                        }
                    } else {
                        -2.0f32
                    };
                    if mask > 0 as f32 {
                        mask = 0.5f32 * mask;
                    }
                    mask_sum += mask;
                    i += 1;
                }
                c += 1;
            }
            masking_depth = mask_sum / end as f32 * (*st).channels as f32;
            masking_depth += 0.2f32;
            rate_offset = (srate as opus_val32 * masking_depth) as i32;
            rate_offset = if rate_offset > -(2) * (*st).silk_mode.bitRate / 3 {
                rate_offset
            } else {
                -(2) * (*st).silk_mode.bitRate / 3
            };
            if (*st).bandwidth == OPUS_BANDWIDTH_SUPERWIDEBAND
                || (*st).bandwidth == OPUS_BANDWIDTH_FULLBAND
            {
                (*st).silk_mode.bitRate += 3 * rate_offset / 5;
            } else {
                (*st).silk_mode.bitRate += rate_offset;
            }
        }
        (*st).silk_mode.payloadSize_ms = 1000 * frame_size / (*st).Fs;
        (*st).silk_mode.nChannelsAPI = (*st).channels;
        (*st).silk_mode.nChannelsInternal = (*st).stream_channels;
        if curr_bandwidth == OPUS_BANDWIDTH_NARROWBAND {
            (*st).silk_mode.desiredInternalSampleRate = 8000;
        } else if curr_bandwidth == OPUS_BANDWIDTH_MEDIUMBAND {
            (*st).silk_mode.desiredInternalSampleRate = 12000;
        } else {
            assert!((*st).mode == 1001 || curr_bandwidth == 1103);
            (*st).silk_mode.desiredInternalSampleRate = 16000;
        }
        if (*st).mode == MODE_HYBRID {
            (*st).silk_mode.minInternalSampleRate = 16000;
        } else {
            (*st).silk_mode.minInternalSampleRate = 8000;
        }
        (*st).silk_mode.maxInternalSampleRate = 16000;
        if (*st).mode == MODE_SILK_ONLY {
            let mut effective_max_rate: i32 = max_rate;
            if frame_rate > 50 {
                effective_max_rate = effective_max_rate * 2 / 3;
            }
            if effective_max_rate < 8000 {
                (*st).silk_mode.maxInternalSampleRate = 12000;
                (*st).silk_mode.desiredInternalSampleRate =
                    if (12000) < (*st).silk_mode.desiredInternalSampleRate {
                        12000
                    } else {
                        (*st).silk_mode.desiredInternalSampleRate
                    };
            }
            if effective_max_rate < 7000 {
                (*st).silk_mode.maxInternalSampleRate = 8000;
                (*st).silk_mode.desiredInternalSampleRate =
                    if (8000) < (*st).silk_mode.desiredInternalSampleRate {
                        8000
                    } else {
                        (*st).silk_mode.desiredInternalSampleRate
                    };
            }
        }
        (*st).silk_mode.useCBR = ((*st).use_vbr == 0) as i32;
        (*st).silk_mode.maxBits = (max_data_bytes - 1) * 8;
        if redundancy != 0 && redundancy_bytes >= 2 {
            (*st).silk_mode.maxBits -= redundancy_bytes * 8 + 1;
            if (*st).mode == MODE_HYBRID {
                (*st).silk_mode.maxBits -= 20;
            }
        }
        if (*st).silk_mode.useCBR != 0 {
            if (*st).mode == MODE_HYBRID {
                (*st).silk_mode.maxBits =
                    if (*st).silk_mode.maxBits < (*st).silk_mode.bitRate * frame_size / (*st).Fs {
                        (*st).silk_mode.maxBits
                    } else {
                        (*st).silk_mode.bitRate * frame_size / (*st).Fs
                    };
            }
        } else if (*st).mode == MODE_HYBRID {
            let maxBitRate: i32 = compute_silk_rate_for_hybrid(
                (*st).silk_mode.maxBits * (*st).Fs / frame_size,
                curr_bandwidth,
                ((*st).Fs == 50 * frame_size) as i32,
                (*st).use_vbr,
                (*st).silk_mode.LBRR_coded,
                (*st).stream_channels,
            );
            (*st).silk_mode.maxBits = maxBitRate * frame_size / (*st).Fs;
        }
        if prefill != 0 {
            let mut zero: i32 = 0;
            let mut prefill_offset: i32 = 0;
            prefill_offset =
                (*st).channels * ((*st).encoder_buffer - (*st).delay_compensation - (*st).Fs / 400);
            gain_fade(
                ((*st).delay_buffer)
                    .as_mut_ptr()
                    .offset(prefill_offset as isize),
                ((*st).delay_buffer)
                    .as_mut_ptr()
                    .offset(prefill_offset as isize),
                0 as opus_val16,
                Q15ONE,
                (*celt_mode).overlap,
                (*st).Fs / 400,
                (*st).channels,
                (*celt_mode).window,
                (*st).Fs,
            );
            memset(
                ((*st).delay_buffer).as_mut_ptr() as *mut core::ffi::c_void,
                0,
                (prefill_offset as u64).wrapping_mul(::core::mem::size_of::<opus_val16>() as u64),
            );
            i = 0;
            while i < (*st).encoder_buffer * (*st).channels {
                *pcm_silk.as_mut_ptr().offset(i as isize) =
                    FLOAT2INT16((*st).delay_buffer[i as usize]);
                i += 1;
            }
            silk_Encode(
                silk_enc,
                &mut (*st).silk_mode,
                pcm_silk.as_mut_ptr(),
                (*st).encoder_buffer,
                None,
                &mut zero,
                prefill,
                activity,
            );
            (*st).silk_mode.opusCanSwitch = 0;
        }
        i = 0;
        while i < frame_size * (*st).channels {
            *pcm_silk.as_mut_ptr().offset(i as isize) = FLOAT2INT16(
                *pcm_buf
                    .as_mut_ptr()
                    .offset((total_buffer * (*st).channels + i) as isize),
            );
            i += 1;
        }
        ret = silk_Encode(
            silk_enc,
            &mut (*st).silk_mode,
            pcm_silk.as_mut_ptr(),
            frame_size,
            Some(&mut enc),
            &mut nBytes,
            0,
            activity,
        );
        if ret != 0 {
            return OPUS_INTERNAL_ERROR;
        }
        if (*st).mode == MODE_SILK_ONLY {
            if (*st).silk_mode.internalSampleRate == 8000 {
                curr_bandwidth = OPUS_BANDWIDTH_NARROWBAND;
            } else if (*st).silk_mode.internalSampleRate == 12000 {
                curr_bandwidth = OPUS_BANDWIDTH_MEDIUMBAND;
            } else if (*st).silk_mode.internalSampleRate == 16000 {
                curr_bandwidth = OPUS_BANDWIDTH_WIDEBAND;
            }
        } else {
            assert!((*st).silk_mode.internalSampleRate == 16000)
        };
        (*st).silk_mode.opusCanSwitch =
            ((*st).silk_mode.switchReady != 0 && (*st).nonfinal_frame == 0) as i32;
        if nBytes == 0 {
            (*st).rangeFinal = 0;
            *data.offset(-1 as isize) = gen_toc(
                (*st).mode,
                (*st).Fs / frame_size,
                curr_bandwidth,
                (*st).stream_channels,
            );
            return 1;
        }
        if (*st).silk_mode.opusCanSwitch != 0 {
            redundancy_bytes = compute_redundancy_bytes(
                max_data_bytes,
                (*st).bitrate_bps,
                frame_rate,
                (*st).stream_channels,
            );
            redundancy = (redundancy_bytes != 0) as i32;
            celt_to_silk = 0;
            (*st).silk_bw_switch = 1;
        }
    }
    let mut endband: i32 = 21;
    match curr_bandwidth {
        OPUS_BANDWIDTH_NARROWBAND => {
            endband = 13;
        }
        OPUS_BANDWIDTH_MEDIUMBAND | OPUS_BANDWIDTH_WIDEBAND => {
            endband = 17;
        }
        OPUS_BANDWIDTH_SUPERWIDEBAND => {
            endband = 19;
        }
        OPUS_BANDWIDTH_FULLBAND => {
            endband = 21;
        }
        _ => {}
    }
    opus_custom_encoder_ctl!(celt_enc, CELT_SET_END_BAND_REQUEST, endband);
    opus_custom_encoder_ctl!(celt_enc, CELT_SET_CHANNELS_REQUEST, (*st).stream_channels);
    opus_custom_encoder_ctl!(celt_enc, OPUS_SET_BITRATE_REQUEST, -1);
    if (*st).mode != MODE_SILK_ONLY {
        let mut celt_pred: opus_val32 = 2 as opus_val32;
        opus_custom_encoder_ctl!(celt_enc, OPUS_SET_VBR_REQUEST, 0);
        if (*st).silk_mode.reducedDependency != 0 {
            celt_pred = 0 as opus_val32;
        }
        opus_custom_encoder_ctl!(celt_enc, CELT_SET_PREDICTION_REQUEST, celt_pred as i32);
        if (*st).mode == MODE_HYBRID {
            if (*st).use_vbr != 0 {
                opus_custom_encoder_ctl!(
                    celt_enc,
                    OPUS_SET_BITRATE_REQUEST,
                    (*st).bitrate_bps - (*st).silk_mode.bitRate,
                );
                opus_custom_encoder_ctl!(celt_enc, OPUS_SET_VBR_CONSTRAINT_REQUEST, 0,);
            }
        } else if (*st).use_vbr != 0 {
            opus_custom_encoder_ctl!(celt_enc, OPUS_SET_VBR_REQUEST, 1);
            opus_custom_encoder_ctl!(
                celt_enc,
                OPUS_SET_VBR_CONSTRAINT_REQUEST,
                (*st).vbr_constraint,
            );
            opus_custom_encoder_ctl!(celt_enc, OPUS_SET_BITRATE_REQUEST, (*st).bitrate_bps);
        }
    }
    let vla_1 = ((*st).channels * (*st).Fs / 400) as usize;
    let mut tmp_prefill: Vec<opus_val16> = ::std::vec::from_elem(0., vla_1);
    if (*st).mode != MODE_SILK_ONLY && (*st).mode != (*st).prev_mode && (*st).prev_mode > 0 {
        memcpy(
            tmp_prefill.as_mut_ptr() as *mut core::ffi::c_void,
            &mut *((*st).delay_buffer).as_mut_ptr().offset(
                (((*st).encoder_buffer - total_buffer - (*st).Fs / 400) * (*st).channels) as isize,
            ) as *mut opus_val16 as *const core::ffi::c_void,
            (((*st).channels * (*st).Fs / 400) as u64)
                .wrapping_mul(::core::mem::size_of::<opus_val16>() as u64)
                .wrapping_add(
                    (0 * tmp_prefill.as_mut_ptr().offset_from(
                        &mut *((*st).delay_buffer).as_mut_ptr().offset(
                            (((*st).encoder_buffer - total_buffer - (*st).Fs / 400)
                                * (*st).channels) as isize,
                        ),
                    ) as i64) as u64,
                ),
        );
    }
    if (*st).channels * ((*st).encoder_buffer - (frame_size + total_buffer)) > 0 {
        memmove(
            ((*st).delay_buffer).as_mut_ptr() as *mut core::ffi::c_void,
            &mut *((*st).delay_buffer)
                .as_mut_ptr()
                .offset(((*st).channels * frame_size) as isize) as *mut opus_val16
                as *const core::ffi::c_void,
            (((*st).channels * ((*st).encoder_buffer - frame_size - total_buffer)) as u64)
                .wrapping_mul(::core::mem::size_of::<opus_val16>() as u64)
                .wrapping_add(
                    (0 * ((*st).delay_buffer).as_mut_ptr().offset_from(
                        &mut *((*st).delay_buffer)
                            .as_mut_ptr()
                            .offset(((*st).channels * frame_size) as isize),
                    ) as i64) as u64,
                ),
        );
        memcpy(
            &mut *((*st).delay_buffer).as_mut_ptr().offset(
                ((*st).channels * ((*st).encoder_buffer - frame_size - total_buffer)) as isize,
            ) as *mut opus_val16 as *mut core::ffi::c_void,
            &mut *pcm_buf.as_mut_ptr().offset(0 as isize) as *mut opus_val16
                as *const core::ffi::c_void,
            (((frame_size + total_buffer) * (*st).channels) as u64)
                .wrapping_mul(::core::mem::size_of::<opus_val16>() as u64)
                .wrapping_add(
                    (0 * (&mut *((*st).delay_buffer).as_mut_ptr().offset(
                        ((*st).channels * ((*st).encoder_buffer - frame_size - total_buffer))
                            as isize,
                    ) as *mut opus_val16)
                        .offset_from(&mut *pcm_buf.as_mut_ptr().offset(0 as isize))
                        as i64) as u64,
                ),
        );
    } else {
        memcpy(
            ((*st).delay_buffer).as_mut_ptr() as *mut core::ffi::c_void,
            &mut *pcm_buf.as_mut_ptr().offset(
                ((frame_size + total_buffer - (*st).encoder_buffer) * (*st).channels) as isize,
            ) as *mut opus_val16 as *const core::ffi::c_void,
            (((*st).encoder_buffer * (*st).channels) as u64)
                .wrapping_mul(::core::mem::size_of::<opus_val16>() as u64)
                .wrapping_add(
                    (0 * ((*st).delay_buffer).as_mut_ptr().offset_from(
                        &mut *pcm_buf.as_mut_ptr().offset(
                            ((frame_size + total_buffer - (*st).encoder_buffer) * (*st).channels)
                                as isize,
                        ),
                    ) as i64) as u64,
                ),
        );
    }
    if (*st).prev_HB_gain < Q15ONE || HB_gain < Q15ONE {
        gain_fade(
            pcm_buf.as_mut_ptr(),
            pcm_buf.as_mut_ptr(),
            (*st).prev_HB_gain,
            HB_gain,
            (*celt_mode).overlap,
            frame_size,
            (*st).channels,
            (*celt_mode).window,
            (*st).Fs,
        );
    }
    (*st).prev_HB_gain = HB_gain;
    if (*st).mode != MODE_HYBRID || (*st).stream_channels == 1 {
        if equiv_rate > 32000 {
            (*st).silk_mode.stereoWidth_Q14 = 16384;
        } else if equiv_rate < 16000 {
            (*st).silk_mode.stereoWidth_Q14 = 0;
        } else {
            (*st).silk_mode.stereoWidth_Q14 =
                16384 - 2048 * (32000 - equiv_rate) / (equiv_rate - 14000);
        }
    }
    if ((*st).energy_masking).is_null() && (*st).channels == 2 {
        if ((*st).hybrid_stereo_width_Q14 as i32) < (1) << 14
            || (*st).silk_mode.stereoWidth_Q14 < (1) << 14
        {
            let mut g1: opus_val16 = 0.;
            let mut g2: opus_val16 = 0.;
            g1 = (*st).hybrid_stereo_width_Q14 as opus_val16;
            g2 = (*st).silk_mode.stereoWidth_Q14 as opus_val16;
            g1 *= 1.0f32 / 16384 as f32;
            g2 *= 1.0f32 / 16384 as f32;
            stereo_fade(
                pcm_buf.as_mut_ptr(),
                pcm_buf.as_mut_ptr(),
                g1,
                g2,
                (*celt_mode).overlap,
                frame_size,
                (*st).channels,
                (*celt_mode).window,
                (*st).Fs,
            );
            (*st).hybrid_stereo_width_Q14 = (*st).silk_mode.stereoWidth_Q14 as i16;
        }
    }
    if (*st).mode != MODE_CELT_ONLY
        && ec_tell(&mut enc) + 17 + 20 * ((*st).mode == MODE_HYBRID) as i32
            <= 8 * (max_data_bytes - 1)
    {
        if (*st).mode == MODE_HYBRID {
            ec_enc_bit_logp(&mut enc, redundancy, 12);
        }
        if redundancy != 0 {
            let mut max_redundancy: i32 = 0;
            ec_enc_bit_logp(&mut enc, celt_to_silk, 1);
            if (*st).mode == MODE_HYBRID {
                max_redundancy = max_data_bytes - 1 - (ec_tell(&mut enc) + 8 + 3 + 7 >> 3);
            } else {
                max_redundancy = max_data_bytes - 1 - (ec_tell(&mut enc) + 7 >> 3);
            }
            redundancy_bytes = if max_redundancy < redundancy_bytes {
                max_redundancy
            } else {
                redundancy_bytes
            };
            redundancy_bytes = if (257)
                < (if 2 > redundancy_bytes {
                    2
                } else {
                    redundancy_bytes
                }) {
                257
            } else if 2 > redundancy_bytes {
                2
            } else {
                redundancy_bytes
            };
            if (*st).mode == MODE_HYBRID {
                ec_enc_uint(&mut enc, (redundancy_bytes - 2) as u32, 256);
            }
        }
    } else {
        redundancy = 0;
    }
    if redundancy == 0 {
        (*st).silk_bw_switch = 0;
        redundancy_bytes = 0;
    }
    if (*st).mode != MODE_CELT_ONLY {
        start_band = 17;
    }
    if (*st).mode == MODE_SILK_ONLY {
        ret = ec_tell(&mut enc) + 7 >> 3;
        ec_enc_done(&mut enc);
        nb_compr_bytes = ret;
    } else {
        nb_compr_bytes = max_data_bytes - 1 - redundancy_bytes;
        ec_enc_shrink(&mut enc, nb_compr_bytes as u32);
    }
    if redundancy != 0 || (*st).mode != MODE_SILK_ONLY {
        opus_custom_encoder_ctl!(celt_enc, CELT_SET_ANALYSIS_REQUEST, &mut analysis_info);
    }
    if (*st).mode == MODE_HYBRID {
        let mut info: SILKInfo = SILKInfo {
            signalType: 0,
            offset: 0,
        };
        info.signalType = (*st).silk_mode.signalType;
        info.offset = (*st).silk_mode.offset;
        opus_custom_encoder_ctl!(celt_enc, CELT_SET_SILK_INFO_REQUEST, &mut info);
    }
    if redundancy != 0 && celt_to_silk != 0 {
        let mut err: i32 = 0;
        opus_custom_encoder_ctl!(celt_enc, CELT_SET_START_BAND_REQUEST, 0);
        opus_custom_encoder_ctl!(celt_enc, OPUS_SET_VBR_REQUEST, 0);
        opus_custom_encoder_ctl!(celt_enc, OPUS_SET_BITRATE_REQUEST, -1);
        err = celt_encode_with_ec(
            celt_enc,
            pcm_buf.as_mut_ptr(),
            (*st).Fs / 200,
            data.offset(nb_compr_bytes as isize),
            redundancy_bytes,
            None,
        );
        if err < 0 {
            return OPUS_INTERNAL_ERROR;
        }
        opus_custom_encoder_ctl!(celt_enc, OPUS_GET_FINAL_RANGE_REQUEST, &mut redundant_rng);
        opus_custom_encoder_ctl!(celt_enc, OPUS_RESET_STATE);
    }
    opus_custom_encoder_ctl!(celt_enc, CELT_SET_START_BAND_REQUEST, start_band);
    if (*st).mode != MODE_SILK_ONLY {
        if (*st).mode != (*st).prev_mode && (*st).prev_mode > 0 {
            let mut dummy_0: [u8; 2] = [0; 2];
            opus_custom_encoder_ctl!(celt_enc, OPUS_RESET_STATE);
            celt_encode_with_ec(
                celt_enc,
                tmp_prefill.as_mut_ptr(),
                (*st).Fs / 400,
                dummy_0.as_mut_ptr(),
                2,
                None,
            );
            opus_custom_encoder_ctl!(celt_enc, CELT_SET_PREDICTION_REQUEST, 0);
        }
        if ec_tell(&mut enc) <= 8 * nb_compr_bytes {
            if redundancy != 0
                && celt_to_silk != 0
                && (*st).mode == MODE_HYBRID
                && (*st).use_vbr != 0
            {
                opus_custom_encoder_ctl!(
                    celt_enc,
                    OPUS_SET_BITRATE_REQUEST,
                    (*st).bitrate_bps - (*st).silk_mode.bitRate,
                );
            }
            opus_custom_encoder_ctl!(celt_enc, OPUS_SET_VBR_REQUEST, (*st).use_vbr);
            ret = celt_encode_with_ec(
                celt_enc,
                pcm_buf.as_mut_ptr(),
                frame_size,
                NULL as *mut u8,
                nb_compr_bytes,
                Some(&mut enc),
            );
            if ret < 0 {
                return OPUS_INTERNAL_ERROR;
            }
            if redundancy != 0
                && celt_to_silk != 0
                && (*st).mode == MODE_HYBRID
                && (*st).use_vbr != 0
            {
                memmove(
                    data.offset(ret as isize) as *mut core::ffi::c_void,
                    data.offset(nb_compr_bytes as isize) as *const core::ffi::c_void,
                    (redundancy_bytes as u64)
                        .wrapping_mul(::core::mem::size_of::<u8>() as u64)
                        .wrapping_add(
                            (0 * data
                                .offset(ret as isize)
                                .offset_from(data.offset(nb_compr_bytes as isize))
                                as i64) as u64,
                        ),
                );
                nb_compr_bytes = nb_compr_bytes + redundancy_bytes;
            }
        }
    }
    if redundancy != 0 && celt_to_silk == 0 {
        let mut err_0: i32 = 0;
        let mut dummy_1: [u8; 2] = [0; 2];
        let mut N2: i32 = 0;
        let mut N4: i32 = 0;
        N2 = (*st).Fs / 200;
        N4 = (*st).Fs / 400;
        opus_custom_encoder_ctl!(celt_enc, OPUS_RESET_STATE);
        opus_custom_encoder_ctl!(celt_enc, CELT_SET_START_BAND_REQUEST, 0);
        opus_custom_encoder_ctl!(celt_enc, CELT_SET_PREDICTION_REQUEST, 0);
        opus_custom_encoder_ctl!(celt_enc, OPUS_SET_VBR_REQUEST, 0);
        opus_custom_encoder_ctl!(celt_enc, OPUS_SET_BITRATE_REQUEST, -1);
        if (*st).mode == MODE_HYBRID {
            nb_compr_bytes = ret;
            ec_enc_shrink(&mut enc, nb_compr_bytes as u32);
        }
        celt_encode_with_ec(
            celt_enc,
            pcm_buf
                .as_mut_ptr()
                .offset(((*st).channels * (frame_size - N2 - N4)) as isize),
            N4,
            dummy_1.as_mut_ptr(),
            2,
            None,
        );
        err_0 = celt_encode_with_ec(
            celt_enc,
            pcm_buf
                .as_mut_ptr()
                .offset(((*st).channels * (frame_size - N2)) as isize),
            N2,
            data.offset(nb_compr_bytes as isize),
            redundancy_bytes,
            None,
        );
        if err_0 < 0 {
            return OPUS_INTERNAL_ERROR;
        }
        opus_custom_encoder_ctl!(celt_enc, OPUS_GET_FINAL_RANGE_REQUEST, &mut redundant_rng);
    }
    data = data.offset(-1);
    *data.offset(0 as isize) = gen_toc(
        (*st).mode,
        (*st).Fs / frame_size,
        curr_bandwidth,
        (*st).stream_channels,
    );
    (*st).rangeFinal = enc.rng ^ redundant_rng;
    if to_celt != 0 {
        (*st).prev_mode = MODE_CELT_ONLY;
    } else {
        (*st).prev_mode = (*st).mode;
    }
    (*st).prev_channels = (*st).stream_channels;
    (*st).prev_framesize = frame_size;
    (*st).first = 0;
    if (*st).use_dtx != 0 && (analysis_info.valid != 0 || is_silence != 0) {
        if decide_dtx_mode(
            analysis_info.activity_probability,
            &mut (*st).nb_no_activity_frames,
            (*st).peak_signal_energy,
            pcm,
            frame_size,
            (*st).channels,
            is_silence,
            (*st).arch,
        ) != 0
        {
            (*st).rangeFinal = 0;
            *data.offset(0 as isize) = gen_toc(
                (*st).mode,
                (*st).Fs / frame_size,
                curr_bandwidth,
                (*st).stream_channels,
            );
            return 1;
        }
    } else {
        (*st).nb_no_activity_frames = 0;
    }
    if ec_tell(&mut enc) > (max_data_bytes - 1) * 8 {
        if max_data_bytes < 2 {
            return OPUS_BUFFER_TOO_SMALL;
        }
        *data.offset(1 as isize) = 0;
        ret = 1;
        (*st).rangeFinal = 0;
    } else if (*st).mode == MODE_SILK_ONLY && redundancy == 0 {
        while ret > 2 && *data.offset(ret as isize) as i32 == 0 {
            ret -= 1;
        }
    }
    ret += 1 + redundancy_bytes;
    if (*st).use_vbr == 0 {
        if opus_packet_pad(data, ret, max_data_bytes) != OPUS_OK {
            return OPUS_INTERNAL_ERROR;
        }
        ret = max_data_bytes;
    }
    return ret;
}
pub unsafe fn opus_encode(
    st: *mut OpusEncoder,
    pcm: *const i16,
    analysis_frame_size: i32,
    data: *mut u8,
    max_data_bytes: i32,
) -> i32 {
    let mut i: i32 = 0;
    let mut ret: i32 = 0;
    let mut frame_size: i32 = 0;
    frame_size = frame_size_select(analysis_frame_size, (*st).variable_duration, (*st).Fs);
    if frame_size <= 0 {
        return OPUS_BAD_ARG;
    }
    let vla = (frame_size * (*st).channels) as usize;
    let mut in_0: Vec<f32> = ::std::vec::from_elem(0., vla);
    i = 0;
    while i < frame_size * (*st).channels {
        *in_0.as_mut_ptr().offset(i as isize) =
            1.0f32 / 32768 as f32 * *pcm.offset(i as isize) as i32 as f32;
        i += 1;
    }
    ret = opus_encode_native(
        st,
        in_0.as_mut_ptr(),
        frame_size,
        data,
        max_data_bytes,
        16,
        pcm as *const core::ffi::c_void,
        analysis_frame_size,
        0,
        -(2),
        (*st).channels,
        Some(
            downmix_int
                as unsafe fn(
                    *const core::ffi::c_void,
                    *mut opus_val32,
                    i32,
                    i32,
                    i32,
                    i32,
                    i32,
                ) -> (),
        ),
        0,
    );
    return ret;
}
pub unsafe fn opus_encode_float(
    st: *mut OpusEncoder,
    pcm: *const f32,
    analysis_frame_size: i32,
    data: *mut u8,
    out_data_bytes: i32,
) -> i32 {
    let mut frame_size: i32 = 0;
    frame_size = frame_size_select(analysis_frame_size, (*st).variable_duration, (*st).Fs);
    return opus_encode_native(
        st,
        pcm,
        frame_size,
        data,
        out_data_bytes,
        24,
        pcm as *const core::ffi::c_void,
        analysis_frame_size,
        0,
        -(2),
        (*st).channels,
        Some(
            downmix_float
                as unsafe fn(
                    *const core::ffi::c_void,
                    *mut opus_val32,
                    i32,
                    i32,
                    i32,
                    i32,
                    i32,
                ) -> (),
        ),
        1,
    );
}
pub unsafe fn opus_encoder_ctl_impl(st: *mut OpusEncoder, request: i32, args: VarArgs) -> i32 {
    let mut current_block: u64;
    let mut ret: i32 = 0;
    let mut celt_enc: *mut OpusCustomEncoder = 0 as *mut OpusCustomEncoder;
    ret = OPUS_OK;

    let mut ap = args;

    celt_enc = (st as *mut i8).offset((*st).celt_enc_offset as isize) as *mut OpusCustomEncoder;
    match request {
        OPUS_SET_APPLICATION_REQUEST => {
            let value: i32 = ap.arg::<i32>();
            if value != OPUS_APPLICATION_VOIP
                && value != OPUS_APPLICATION_AUDIO
                && value != OPUS_APPLICATION_RESTRICTED_LOWDELAY
                || (*st).first == 0 && (*st).application != value
            {
                ret = OPUS_BAD_ARG;
            } else {
                (*st).application = value;
                (*st).analysis.application = value;
            }
            current_block = 16167632229894708628;
        }
        OPUS_GET_APPLICATION_REQUEST => {
            let value_0 = ap.arg::<&mut i32>();
            *value_0 = (*st).application;
            current_block = 16167632229894708628;
        }
        OPUS_SET_BITRATE_REQUEST => {
            let mut value_1: i32 = ap.arg::<i32>();
            if value_1 != OPUS_AUTO && value_1 != OPUS_BITRATE_MAX {
                if value_1 <= 0 {
                    current_block = 12343738388509029619;
                } else {
                    if value_1 <= 500 {
                        value_1 = 500;
                    } else if value_1 > 300000 * (*st).channels {
                        value_1 = 300000 * (*st).channels;
                    }
                    current_block = 6057473163062296781;
                }
            } else {
                current_block = 6057473163062296781;
            }
            match current_block {
                12343738388509029619 => {}
                _ => {
                    (*st).user_bitrate_bps = value_1;
                    current_block = 16167632229894708628;
                }
            }
        }
        OPUS_GET_BITRATE_REQUEST => {
            let value_2 = ap.arg::<&mut i32>();
            *value_2 = user_bitrate_to_bitrate(st, (*st).prev_framesize, 1276);
            current_block = 16167632229894708628;
        }
        OPUS_SET_FORCE_CHANNELS_REQUEST => {
            let value_3: i32 = ap.arg::<i32>();
            if (value_3 < 1 || value_3 > (*st).channels) && value_3 != OPUS_AUTO {
                current_block = 12343738388509029619;
            } else {
                (*st).force_channels = value_3;
                current_block = 16167632229894708628;
            }
        }
        OPUS_GET_FORCE_CHANNELS_REQUEST => {
            let value_4 = ap.arg::<&mut i32>();
            *value_4 = (*st).force_channels;
            current_block = 16167632229894708628;
        }
        OPUS_SET_MAX_BANDWIDTH_REQUEST => {
            let value_5: i32 = ap.arg::<i32>();
            if value_5 < OPUS_BANDWIDTH_NARROWBAND || value_5 > OPUS_BANDWIDTH_FULLBAND {
                current_block = 12343738388509029619;
            } else {
                (*st).max_bandwidth = value_5;
                if (*st).max_bandwidth == OPUS_BANDWIDTH_NARROWBAND {
                    (*st).silk_mode.maxInternalSampleRate = 8000;
                } else if (*st).max_bandwidth == OPUS_BANDWIDTH_MEDIUMBAND {
                    (*st).silk_mode.maxInternalSampleRate = 12000;
                } else {
                    (*st).silk_mode.maxInternalSampleRate = 16000;
                }
                current_block = 16167632229894708628;
            }
        }
        OPUS_GET_MAX_BANDWIDTH_REQUEST => {
            let value_6 = ap.arg::<&mut i32>();
            *value_6 = (*st).max_bandwidth;
            current_block = 16167632229894708628;
        }
        OPUS_SET_BANDWIDTH_REQUEST => {
            let value_7: i32 = ap.arg::<i32>();
            if (value_7 < OPUS_BANDWIDTH_NARROWBAND || value_7 > OPUS_BANDWIDTH_FULLBAND)
                && value_7 != OPUS_AUTO
            {
                current_block = 12343738388509029619;
            } else {
                (*st).user_bandwidth = value_7;
                if (*st).user_bandwidth == OPUS_BANDWIDTH_NARROWBAND {
                    (*st).silk_mode.maxInternalSampleRate = 8000;
                } else if (*st).user_bandwidth == OPUS_BANDWIDTH_MEDIUMBAND {
                    (*st).silk_mode.maxInternalSampleRate = 12000;
                } else {
                    (*st).silk_mode.maxInternalSampleRate = 16000;
                }
                current_block = 16167632229894708628;
            }
        }
        OPUS_GET_BANDWIDTH_REQUEST => {
            let value_8 = ap.arg::<&mut i32>();
            *value_8 = (*st).bandwidth;
            current_block = 16167632229894708628;
        }
        OPUS_SET_DTX_REQUEST => {
            let value_9: i32 = ap.arg::<i32>();
            if value_9 < 0 || value_9 > 1 {
                current_block = 12343738388509029619;
            } else {
                (*st).use_dtx = value_9;
                current_block = 16167632229894708628;
            }
        }
        OPUS_GET_DTX_REQUEST => {
            let value_10 = ap.arg::<&mut i32>();
            *value_10 = (*st).use_dtx;
            current_block = 16167632229894708628;
        }
        OPUS_SET_COMPLEXITY_REQUEST => {
            let value_11: i32 = ap.arg::<i32>();
            if value_11 < 0 || value_11 > 10 {
                current_block = 12343738388509029619;
            } else {
                (*st).silk_mode.complexity = value_11;
                opus_custom_encoder_ctl!(celt_enc, OPUS_SET_COMPLEXITY_REQUEST, value_11);
                current_block = 16167632229894708628;
            }
        }
        OPUS_GET_COMPLEXITY_REQUEST => {
            let value_12 = ap.arg::<&mut i32>();
            *value_12 = (*st).silk_mode.complexity;
            current_block = 16167632229894708628;
        }
        OPUS_SET_INBAND_FEC_REQUEST => {
            let value_13: i32 = ap.arg::<i32>();
            if value_13 < 0 || value_13 > 1 {
                current_block = 12343738388509029619;
            } else {
                (*st).silk_mode.useInBandFEC = value_13;
                current_block = 16167632229894708628;
            }
        }
        OPUS_GET_INBAND_FEC_REQUEST => {
            let value_14 = ap.arg::<&mut i32>();
            *value_14 = (*st).silk_mode.useInBandFEC;
            current_block = 16167632229894708628;
        }
        OPUS_SET_PACKET_LOSS_PERC_REQUEST => {
            let value_15: i32 = ap.arg::<i32>();
            if value_15 < 0 || value_15 > 100 {
                current_block = 12343738388509029619;
            } else {
                (*st).silk_mode.packetLossPercentage = value_15;
                opus_custom_encoder_ctl!(celt_enc, OPUS_SET_PACKET_LOSS_PERC_REQUEST, value_15);
                current_block = 16167632229894708628;
            }
        }
        OPUS_GET_PACKET_LOSS_PERC_REQUEST => {
            let value_16 = ap.arg::<&mut i32>();
            *value_16 = (*st).silk_mode.packetLossPercentage;
            current_block = 16167632229894708628;
        }
        OPUS_SET_VBR_REQUEST => {
            let value_17: i32 = ap.arg::<i32>();
            if value_17 < 0 || value_17 > 1 {
                current_block = 12343738388509029619;
            } else {
                (*st).use_vbr = value_17;
                (*st).silk_mode.useCBR = 1 - value_17;
                current_block = 16167632229894708628;
            }
        }
        OPUS_GET_VBR_REQUEST => {
            let value_18 = ap.arg::<&mut i32>();
            *value_18 = (*st).use_vbr;
            current_block = 16167632229894708628;
        }
        OPUS_SET_VOICE_RATIO_REQUEST => {
            let value_19: i32 = ap.arg::<i32>();
            if value_19 < -1 || value_19 > 100 {
                current_block = 12343738388509029619;
            } else {
                (*st).voice_ratio = value_19;
                current_block = 16167632229894708628;
            }
        }
        OPUS_GET_VOICE_RATIO_REQUEST => {
            let value_20 = ap.arg::<&mut i32>();
            *value_20 = (*st).voice_ratio;
            current_block = 16167632229894708628;
        }
        OPUS_SET_VBR_CONSTRAINT_REQUEST => {
            let value_21: i32 = ap.arg::<i32>();
            if value_21 < 0 || value_21 > 1 {
                current_block = 12343738388509029619;
            } else {
                (*st).vbr_constraint = value_21;
                current_block = 16167632229894708628;
            }
        }
        OPUS_GET_VBR_CONSTRAINT_REQUEST => {
            let value_22 = ap.arg::<&mut i32>();
            *value_22 = (*st).vbr_constraint;
            current_block = 16167632229894708628;
        }
        OPUS_SET_SIGNAL_REQUEST => {
            let value_23: i32 = ap.arg::<i32>();
            if value_23 != OPUS_AUTO
                && value_23 != OPUS_SIGNAL_VOICE
                && value_23 != OPUS_SIGNAL_MUSIC
            {
                current_block = 12343738388509029619;
            } else {
                (*st).signal_type = value_23;
                current_block = 16167632229894708628;
            }
        }
        OPUS_GET_SIGNAL_REQUEST => {
            let value_24 = ap.arg::<&mut i32>();
            *value_24 = (*st).signal_type;
            current_block = 16167632229894708628;
        }
        OPUS_GET_LOOKAHEAD_REQUEST => {
            let value_25 = ap.arg::<&mut i32>();
            *value_25 = (*st).Fs / 400;
            if (*st).application != OPUS_APPLICATION_RESTRICTED_LOWDELAY {
                *value_25 += (*st).delay_compensation;
            }
            current_block = 16167632229894708628;
        }
        OPUS_GET_SAMPLE_RATE_REQUEST => {
            let value_26 = ap.arg::<&mut i32>();
            *value_26 = (*st).Fs;
            current_block = 16167632229894708628;
        }
        OPUS_GET_FINAL_RANGE_REQUEST => {
            let value_27 = ap.arg::<&mut u32>();
            *value_27 = (*st).rangeFinal;
            current_block = 16167632229894708628;
        }
        OPUS_SET_LSB_DEPTH_REQUEST => {
            let value_28: i32 = ap.arg::<i32>();
            if value_28 < 8 || value_28 > 24 {
                current_block = 12343738388509029619;
            } else {
                (*st).lsb_depth = value_28;
                current_block = 16167632229894708628;
            }
        }
        OPUS_GET_LSB_DEPTH_REQUEST => {
            let value_29 = ap.arg::<&mut i32>();
            *value_29 = (*st).lsb_depth;
            current_block = 16167632229894708628;
        }
        OPUS_SET_EXPERT_FRAME_DURATION_REQUEST => {
            let value_30: i32 = ap.arg::<i32>();
            if value_30 != OPUS_FRAMESIZE_ARG
                && value_30 != OPUS_FRAMESIZE_2_5_MS
                && value_30 != OPUS_FRAMESIZE_5_MS
                && value_30 != OPUS_FRAMESIZE_10_MS
                && value_30 != OPUS_FRAMESIZE_20_MS
                && value_30 != OPUS_FRAMESIZE_40_MS
                && value_30 != OPUS_FRAMESIZE_60_MS
                && value_30 != OPUS_FRAMESIZE_80_MS
                && value_30 != OPUS_FRAMESIZE_100_MS
                && value_30 != OPUS_FRAMESIZE_120_MS
            {
                current_block = 12343738388509029619;
            } else {
                (*st).variable_duration = value_30;
                current_block = 16167632229894708628;
            }
        }
        OPUS_GET_EXPERT_FRAME_DURATION_REQUEST => {
            let value_31 = ap.arg::<&mut i32>();
            *value_31 = (*st).variable_duration;
            current_block = 16167632229894708628;
        }
        OPUS_SET_PREDICTION_DISABLED_REQUEST => {
            let value_32: i32 = ap.arg::<i32>();
            if value_32 > 1 || value_32 < 0 {
                current_block = 12343738388509029619;
            } else {
                (*st).silk_mode.reducedDependency = value_32;
                current_block = 16167632229894708628;
            }
        }
        OPUS_GET_PREDICTION_DISABLED_REQUEST => {
            let value_33 = ap.arg::<&mut i32>();
            *value_33 = (*st).silk_mode.reducedDependency;
            current_block = 16167632229894708628;
        }
        OPUS_SET_PHASE_INVERSION_DISABLED_REQUEST => {
            let value_34: i32 = ap.arg::<i32>();
            if value_34 < 0 || value_34 > 1 {
                current_block = 12343738388509029619;
            } else {
                opus_custom_encoder_ctl!(
                    celt_enc,
                    OPUS_SET_PHASE_INVERSION_DISABLED_REQUEST,
                    value_34,
                );
                current_block = 16167632229894708628;
            }
        }
        OPUS_GET_PHASE_INVERSION_DISABLED_REQUEST => {
            let value_35 = ap.arg::<&mut i32>();
            opus_custom_encoder_ctl!(
                celt_enc,
                OPUS_GET_PHASE_INVERSION_DISABLED_REQUEST,
                value_35
            );
            current_block = 16167632229894708628;
        }
        OPUS_RESET_STATE => {
            let mut silk_enc: *mut core::ffi::c_void = 0 as *mut core::ffi::c_void;
            let mut dummy: silk_EncControlStruct = silk_EncControlStruct {
                nChannelsAPI: 0,
                nChannelsInternal: 0,
                API_sampleRate: 0,
                maxInternalSampleRate: 0,
                minInternalSampleRate: 0,
                desiredInternalSampleRate: 0,
                payloadSize_ms: 0,
                bitRate: 0,
                packetLossPercentage: 0,
                complexity: 0,
                useInBandFEC: 0,
                LBRR_coded: 0,
                useDTX: 0,
                useCBR: 0,
                maxBits: 0,
                toMono: 0,
                opusCanSwitch: 0,
                reducedDependency: 0,
                internalSampleRate: 0,
                allowBandwidthSwitch: 0,
                inWBmodeWithoutVariableLP: 0,
                stereoWidth_Q14: 0,
                switchReady: 0,
                signalType: 0,
                offset: 0,
            };
            let mut start: *mut i8 = 0 as *mut i8;
            silk_enc =
                (st as *mut i8).offset((*st).silk_enc_offset as isize) as *mut core::ffi::c_void;
            tonality_analysis_reset(&mut (*st).analysis);
            start = &mut (*st).stream_channels as *mut i32 as *mut i8;
            memset(
                start as *mut core::ffi::c_void,
                0,
                (::core::mem::size_of::<OpusEncoder>() as u64)
                    .wrapping_sub(start.offset_from(st as *mut i8) as i64 as u64)
                    .wrapping_mul(::core::mem::size_of::<i8>() as u64),
            );
            opus_custom_encoder_ctl!(celt_enc, OPUS_RESET_STATE);
            silk_InitEncoder(silk_enc, (*st).arch, &mut dummy);
            (*st).stream_channels = (*st).channels;
            (*st).hybrid_stereo_width_Q14 = ((1) << 14) as i16;
            (*st).prev_HB_gain = Q15ONE;
            (*st).first = 1;
            (*st).mode = MODE_HYBRID;
            (*st).bandwidth = OPUS_BANDWIDTH_FULLBAND;
            (*st).variable_HP_smth2_Q15 = ((silk_lin2log(60) as u32) << 8) as i32;
            current_block = 16167632229894708628;
        }
        OPUS_SET_FORCE_MODE_REQUEST => {
            let value_36: i32 = ap.arg::<i32>();
            if (value_36 < MODE_SILK_ONLY || value_36 > MODE_CELT_ONLY) && value_36 != OPUS_AUTO {
                current_block = 12343738388509029619;
            } else {
                (*st).user_forced_mode = value_36;
                current_block = 16167632229894708628;
            }
        }
        OPUS_SET_LFE_REQUEST => {
            let value_37: i32 = ap.arg::<i32>();
            (*st).lfe = value_37;
            ret = opus_custom_encoder_ctl!(celt_enc, OPUS_SET_LFE_REQUEST, value_37);
            current_block = 16167632229894708628;
        }
        OPUS_SET_ENERGY_MASK_REQUEST => {
            let value_38: *mut opus_val16 = ap.arg::<*mut opus_val16>();
            (*st).energy_masking = value_38;
            ret = opus_custom_encoder_ctl!(
                celt_enc,
                OPUS_SET_ENERGY_MASK_REQUEST,
                value_38.offset(value_38.offset_from(value_38) as i64 as isize),
            );
            current_block = 16167632229894708628;
        }
        OPUS_GET_IN_DTX_REQUEST => {
            let value_39: &mut i32 = ap.arg::<&mut i32>();
            if (*st).silk_mode.useDTX != 0
                && ((*st).prev_mode == MODE_SILK_ONLY || (*st).prev_mode == MODE_HYBRID)
            {
                let mut n: i32 = 0;
                let silk_enc_0: *mut core::ffi::c_void = (st as *mut i8)
                    .offset((*st).silk_enc_offset as isize)
                    as *mut core::ffi::c_void;
                *value_39 = 1;
                n = 0;
                while n < (*st).silk_mode.nChannelsInternal {
                    *value_39 = (*value_39 != 0
                        && (*(silk_enc_0 as *mut silk_encoder)).state_Fxx[n as usize]
                            .sCmn
                            .noSpeechCounter
                            >= NB_SPEECH_FRAMES_BEFORE_DTX) as i32;
                    n += 1;
                }
            } else if (*st).use_dtx != 0 {
                *value_39 = ((*st).nb_no_activity_frames >= NB_SPEECH_FRAMES_BEFORE_DTX) as i32;
            } else {
                *value_39 = 0;
            }
            current_block = 16167632229894708628;
        }
        CELT_GET_MODE_REQUEST => {
            let value_40: &mut *const OpusCustomMode = ap.arg::<&mut *const OpusCustomMode>();
            ret = opus_custom_encoder_ctl!(celt_enc, CELT_GET_MODE_REQUEST, value_40);
            current_block = 16167632229894708628;
        }
        _ => {
            ret = OPUS_UNIMPLEMENTED;
            current_block = 16167632229894708628;
        }
    }
    match current_block {
        12343738388509029619 => return OPUS_BAD_ARG,
        _ => return ret,
    };
}
#[macro_export]
macro_rules! opus_encoder_ctl {
    ($st:expr, $request:expr, $($args:expr),*) => {
        $crate::opus_encoder_ctl_impl($st, $request, $crate::varargs!($($args),*))
    };
    ($st:expr, $request:expr, $($args:expr),*,) => {
        opus_encoder_ctl!($st, $request, $($args),*)
    };
    ($st:expr, $request:expr) => {
        opus_encoder_ctl!($st, $request,)
    };
}
pub unsafe fn opus_encoder_destroy(st: *mut OpusEncoder) {
    free(st as *mut core::ffi::c_void);
}
