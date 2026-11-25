pub mod xmmintrin_h {
    #[cfg(target_arch = "x86")]
    pub use core::arch::x86::{__m128, _mm_cvt_ss2si, _mm_cvtss_si32, _mm_set_ss};
    #[cfg(target_arch = "x86_64")]
    pub use core::arch::x86_64::{__m128, _mm_cvt_ss2si, _mm_cvtss_si32, _mm_set_ss};
}
pub mod errors_h {
    pub const SILK_ENC_PACKET_SIZE_NOT_SUPPORTED: i32 = -(103);
    pub const SILK_NO_ERROR: i32 = 0;
}
pub mod SigProc_FLP_h {
    #[inline]
    pub unsafe fn silk_float2short_array(out: *mut i16, in_0: *const f32, length: i32) {
        let mut k: i32 = 0;
        k = length - 1;
        while k >= 0 {
            *out.offset(k as isize) = (if float2int(*in_0.offset(k as isize)) > silk_int16_MAX {
                silk_int16_MAX
            } else if float2int(*in_0.offset(k as isize)) < silk_int16_MIN {
                silk_int16_MIN
            } else {
                float2int(*in_0.offset(k as isize))
            }) as i16;
            k -= 1;
        }
    }
    #[inline]
    pub unsafe fn silk_short2float_array(out: *mut f32, in_0: *const i16, length: i32) {
        let mut k: i32 = 0;
        k = length - 1;
        while k >= 0 {
            *out.offset(k as isize) = *in_0.offset(k as isize) as f32;
            k -= 1;
        }
    }
    use super::typedef_h::{silk_int16_MAX, silk_int16_MIN};
    use crate::celt::float_cast::float2int;
}
pub mod typedef_h {
    pub const silk_int16_MIN: i32 = i16::MIN as i32;
    pub const silk_int16_MAX: i32 = i16::MAX as i32;
}
use self::errors_h::{SILK_ENC_PACKET_SIZE_NOT_SUPPORTED, SILK_NO_ERROR};
pub use self::typedef_h::{silk_int16_MAX, silk_int16_MIN};
pub use self::SigProc_FLP_h::{silk_float2short_array, silk_short2float_array};
use crate::externs::memset;
use crate::silk::control_audio_bandwidth::silk_control_audio_bandwidth;
use crate::silk::define::{
    LA_SHAPE_MS, MAX_DEL_DEC_STATES, MAX_LPC_ORDER, MAX_NB_SUBFR, MIN_LPC_ORDER,
    SUB_FRAME_LENGTH_MS, TYPE_NO_VOICE_ACTIVITY,
};
use crate::silk::enc_API::silk_EncControlStruct;
use crate::silk::float::structs_FLP::{silk_encoder_state_FLP, silk_shape_state_FLP};
use crate::silk::pitch_est_tables::{
    SILK_PE_MAX_COMPLEX, SILK_PE_MID_COMPLEX, SILK_PE_MIN_COMPLEX,
};
use crate::silk::resampler::{silk_resampler, silk_resampler_init};
use crate::silk::structs::{silk_encoder_state, silk_nsq_state};
use crate::silk::tables_NLSF_CB_NB_MB::silk_NLSF_CB_NB_MB;
use crate::silk::tables_NLSF_CB_WB::silk_NLSF_CB_WB;
use crate::silk::tables_other::{silk_uniform4_iCDF, silk_uniform6_iCDF, silk_uniform8_iCDF};
use crate::silk::tables_pitch_lag::{
    silk_pitch_contour_10_ms_NB_iCDF, silk_pitch_contour_10_ms_iCDF, silk_pitch_contour_NB_iCDF,
    silk_pitch_contour_iCDF,
};
use crate::silk::SigProc_FIX::{silk_max_int, silk_min_int};

pub unsafe fn silk_control_encoder(
    psEnc: *mut silk_encoder_state_FLP,
    encControl: *mut silk_EncControlStruct,
    allow_bw_switch: i32,
    channelNb: i32,
    force_fs_kHz: i32,
) -> i32 {
    let mut fs_kHz: i32 = 0;
    let mut ret: i32 = 0;
    (*psEnc).sCmn.useDTX = (*encControl).useDTX;
    (*psEnc).sCmn.useCBR = (*encControl).useCBR;
    (*psEnc).sCmn.API_fs_Hz = (*encControl).API_sampleRate;
    (*psEnc).sCmn.maxInternal_fs_Hz = (*encControl).maxInternalSampleRate;
    (*psEnc).sCmn.minInternal_fs_Hz = (*encControl).minInternalSampleRate;
    (*psEnc).sCmn.desiredInternal_fs_Hz = (*encControl).desiredInternalSampleRate;
    (*psEnc).sCmn.useInBandFEC = (*encControl).useInBandFEC;
    (*psEnc).sCmn.nChannelsAPI = (*encControl).nChannelsAPI;
    (*psEnc).sCmn.nChannelsInternal = (*encControl).nChannelsInternal;
    (*psEnc).sCmn.allow_bandwidth_switch = allow_bw_switch;
    (*psEnc).sCmn.channelNb = channelNb;
    if (*psEnc).sCmn.controlled_since_last_payload != 0 && (*psEnc).sCmn.prefillFlag == 0 {
        if (*psEnc).sCmn.API_fs_Hz != (*psEnc).sCmn.prev_API_fs_Hz && (*psEnc).sCmn.fs_kHz > 0 {
            ret += silk_setup_resamplers(psEnc, (*psEnc).sCmn.fs_kHz);
        }
        return ret;
    }
    fs_kHz = silk_control_audio_bandwidth(&mut (*psEnc).sCmn, encControl);
    if force_fs_kHz != 0 {
        fs_kHz = force_fs_kHz;
    }
    ret += silk_setup_resamplers(psEnc, fs_kHz);
    ret += silk_setup_fs(psEnc, fs_kHz, (*encControl).payloadSize_ms);
    ret += silk_setup_complexity(&mut (*psEnc).sCmn, (*encControl).complexity);
    (*psEnc).sCmn.PacketLoss_perc = (*encControl).packetLossPercentage;
    ret += silk_setup_LBRR(&mut (*psEnc).sCmn, encControl);
    (*psEnc).sCmn.controlled_since_last_payload = 1;
    return ret;
}
unsafe fn silk_setup_resamplers(psEnc: *mut silk_encoder_state_FLP, fs_kHz: i32) -> i32 {
    let mut ret: i32 = SILK_NO_ERROR;
    if (*psEnc).sCmn.fs_kHz != fs_kHz || (*psEnc).sCmn.prev_API_fs_Hz != (*psEnc).sCmn.API_fs_Hz {
        if (*psEnc).sCmn.fs_kHz == 0 {
            (*psEnc).sCmn.resampler_state =
                silk_resampler_init((*psEnc).sCmn.API_fs_Hz, fs_kHz * 1000, 1);
        } else {
            let mut new_buf_samples: i32 = 0;
            let mut api_buf_samples: i32 = 0;
            let mut old_buf_samples: i32 = 0;
            let mut buf_length_ms: i32 = 0;
            buf_length_ms = ((((*psEnc).sCmn.nb_subfr * 5) as u32) << 1) as i32 + LA_SHAPE_MS;
            old_buf_samples = buf_length_ms * (*psEnc).sCmn.fs_kHz;
            new_buf_samples = buf_length_ms * fs_kHz;
            let vla = (if old_buf_samples > new_buf_samples {
                old_buf_samples
            } else {
                new_buf_samples
            }) as usize;
            let mut x_bufFIX: Vec<i16> = ::std::vec::from_elem(0, vla);
            silk_float2short_array(
                x_bufFIX.as_mut_ptr(),
                ((*psEnc).x_buf).as_mut_ptr(),
                old_buf_samples,
            );

            /* Initialize resampler for temporary resampling of x_buf data to API_fs_Hz */
            let mut temp_resampler_state = silk_resampler_init(
                (*psEnc).sCmn.fs_kHz as i16 as i32 * 1000,
                (*psEnc).sCmn.API_fs_Hz,
                0,
            );

            /* Calculate number of samples to temporarily upsample */
            api_buf_samples = buf_length_ms * ((*psEnc).sCmn.API_fs_Hz / 1000);

            /* Temporary resampling of x_buf data to API_fs_Hz */
            let vla_0 = api_buf_samples as usize;
            let mut x_buf_API_fs_Hz: Vec<i16> = ::std::vec::from_elem(0, vla_0);
            ret += silk_resampler(
                &mut temp_resampler_state,
                &mut x_buf_API_fs_Hz,
                &x_bufFIX[..old_buf_samples as usize],
            );
            (*psEnc).sCmn.resampler_state =
                silk_resampler_init((*psEnc).sCmn.API_fs_Hz, fs_kHz as i16 as i32 * 1000, 1);
            ret += silk_resampler(
                &mut (*psEnc).sCmn.resampler_state,
                &mut x_bufFIX,
                &x_buf_API_fs_Hz[..api_buf_samples as usize],
            );
            silk_short2float_array(
                ((*psEnc).x_buf).as_mut_ptr(),
                x_bufFIX.as_mut_ptr(),
                new_buf_samples,
            );
        }
    }
    (*psEnc).sCmn.prev_API_fs_Hz = (*psEnc).sCmn.API_fs_Hz;
    return ret;
}
unsafe fn silk_setup_fs(
    psEnc: *mut silk_encoder_state_FLP,
    fs_kHz: i32,
    PacketSize_ms: i32,
) -> i32 {
    let mut ret: i32 = SILK_NO_ERROR;
    if PacketSize_ms != (*psEnc).sCmn.PacketSize_ms {
        if PacketSize_ms != 10 && PacketSize_ms != 20 && PacketSize_ms != 40 && PacketSize_ms != 60
        {
            ret = SILK_ENC_PACKET_SIZE_NOT_SUPPORTED;
        }
        if PacketSize_ms <= 10 {
            (*psEnc).sCmn.nFramesPerPacket = 1;
            (*psEnc).sCmn.nb_subfr = if PacketSize_ms == 10 { 2 } else { 1 };
            (*psEnc).sCmn.frame_length = PacketSize_ms as i16 as i32 * fs_kHz as i16 as i32;
            (*psEnc).sCmn.pitch_LPC_win_length =
                (10 + ((2) << 1)) as i16 as i32 * fs_kHz as i16 as i32;
            if (*psEnc).sCmn.fs_kHz == 8 {
                (*psEnc).sCmn.pitch_contour_iCDF = &silk_pitch_contour_10_ms_NB_iCDF;
            } else {
                (*psEnc).sCmn.pitch_contour_iCDF = &silk_pitch_contour_10_ms_iCDF;
            }
        } else {
            (*psEnc).sCmn.nFramesPerPacket = PacketSize_ms / (5 * 4);
            (*psEnc).sCmn.nb_subfr = MAX_NB_SUBFR;
            (*psEnc).sCmn.frame_length = 20 * fs_kHz as i16 as i32;
            (*psEnc).sCmn.pitch_LPC_win_length =
                (20 + ((2) << 1)) as i16 as i32 * fs_kHz as i16 as i32;
            if (*psEnc).sCmn.fs_kHz == 8 {
                (*psEnc).sCmn.pitch_contour_iCDF = &silk_pitch_contour_NB_iCDF;
            } else {
                (*psEnc).sCmn.pitch_contour_iCDF = &silk_pitch_contour_iCDF;
            }
        }
        (*psEnc).sCmn.PacketSize_ms = PacketSize_ms;
        (*psEnc).sCmn.TargetRate_bps = 0;
    }
    assert!(fs_kHz == 8 || fs_kHz == 12 || fs_kHz == 16);
    assert!((*psEnc).sCmn.nb_subfr == 2 || (*psEnc).sCmn.nb_subfr == 4);
    if (*psEnc).sCmn.fs_kHz != fs_kHz {
        memset(
            &mut (*psEnc).sShape as *mut silk_shape_state_FLP as *mut core::ffi::c_void,
            0,
            ::core::mem::size_of::<silk_shape_state_FLP>() as u64,
        );
        memset(
            &mut (*psEnc).sCmn.sNSQ as *mut silk_nsq_state as *mut core::ffi::c_void,
            0,
            ::core::mem::size_of::<silk_nsq_state>() as u64,
        );
        memset(
            ((*psEnc).sCmn.prev_NLSFq_Q15).as_mut_ptr() as *mut core::ffi::c_void,
            0,
            ::core::mem::size_of::<[i16; 16]>() as u64,
        );
        memset(
            &mut (*psEnc).sCmn.sLP.In_LP_State as *mut [i32; 2] as *mut core::ffi::c_void,
            0,
            ::core::mem::size_of::<[i32; 2]>() as u64,
        );
        (*psEnc).sCmn.inputBufIx = 0;
        (*psEnc).sCmn.nFramesEncoded = 0;
        (*psEnc).sCmn.TargetRate_bps = 0;
        (*psEnc).sCmn.prevLag = 100;
        (*psEnc).sCmn.first_frame_after_reset = 1;
        (*psEnc).sShape.LastGainIndex = 10;
        (*psEnc).sCmn.sNSQ.lagPrev = 100;
        (*psEnc).sCmn.sNSQ.prev_gain_Q16 = 65536;
        (*psEnc).sCmn.prevSignalType = TYPE_NO_VOICE_ACTIVITY as i8;
        (*psEnc).sCmn.fs_kHz = fs_kHz;
        if (*psEnc).sCmn.fs_kHz == 8 {
            if (*psEnc).sCmn.nb_subfr == MAX_NB_SUBFR {
                (*psEnc).sCmn.pitch_contour_iCDF = &silk_pitch_contour_NB_iCDF;
            } else {
                (*psEnc).sCmn.pitch_contour_iCDF = &silk_pitch_contour_10_ms_NB_iCDF;
            }
        } else if (*psEnc).sCmn.nb_subfr == MAX_NB_SUBFR {
            (*psEnc).sCmn.pitch_contour_iCDF = &silk_pitch_contour_iCDF;
        } else {
            (*psEnc).sCmn.pitch_contour_iCDF = &silk_pitch_contour_10_ms_iCDF;
        }
        if (*psEnc).sCmn.fs_kHz == 8 || (*psEnc).sCmn.fs_kHz == 12 {
            (*psEnc).sCmn.predictLPCOrder = MIN_LPC_ORDER;
            (*psEnc).sCmn.psNLSF_CB = &silk_NLSF_CB_NB_MB;
        } else {
            (*psEnc).sCmn.predictLPCOrder = MAX_LPC_ORDER;
            (*psEnc).sCmn.psNLSF_CB = &silk_NLSF_CB_WB;
        }
        (*psEnc).sCmn.subfr_length = SUB_FRAME_LENGTH_MS * fs_kHz;
        (*psEnc).sCmn.frame_length =
            (*psEnc).sCmn.subfr_length as i16 as i32 * (*psEnc).sCmn.nb_subfr as i16 as i32;
        (*psEnc).sCmn.ltp_mem_length = 20 * fs_kHz as i16 as i32;
        (*psEnc).sCmn.la_pitch = 2 * fs_kHz as i16 as i32;
        (*psEnc).sCmn.max_pitch_lag = 18 * fs_kHz as i16 as i32;
        if (*psEnc).sCmn.nb_subfr == MAX_NB_SUBFR {
            (*psEnc).sCmn.pitch_LPC_win_length =
                (20 + ((2) << 1)) as i16 as i32 * fs_kHz as i16 as i32;
        } else {
            (*psEnc).sCmn.pitch_LPC_win_length =
                (10 + ((2) << 1)) as i16 as i32 * fs_kHz as i16 as i32;
        }
        if (*psEnc).sCmn.fs_kHz == 16 {
            (*psEnc).sCmn.pitch_lag_low_bits_iCDF = &silk_uniform8_iCDF;
        } else if (*psEnc).sCmn.fs_kHz == 12 {
            (*psEnc).sCmn.pitch_lag_low_bits_iCDF = &silk_uniform6_iCDF;
        } else {
            (*psEnc).sCmn.pitch_lag_low_bits_iCDF = &silk_uniform4_iCDF;
        }
    }
    assert!((*psEnc).sCmn.subfr_length * (*psEnc).sCmn.nb_subfr == (*psEnc).sCmn.frame_length);
    return ret;
}
unsafe fn silk_setup_complexity(psEncC: *mut silk_encoder_state, Complexity: i32) -> i32 {
    let ret: i32 = 0;
    assert!(Complexity >= 0 && Complexity <= 10);
    if Complexity < 1 {
        (*psEncC).pitchEstimationComplexity = SILK_PE_MIN_COMPLEX;
        (*psEncC).pitchEstimationThreshold_Q16 = (0.8f64 * ((1) << 16) as f64 + 0.5f64) as i32;
        (*psEncC).pitchEstimationLPCOrder = 6;
        (*psEncC).shapingLPCOrder = 12;
        (*psEncC).la_shape = 3 * (*psEncC).fs_kHz;
        (*psEncC).nStatesDelayedDecision = 1;
        (*psEncC).useInterpolatedNLSFs = 0;
        (*psEncC).NLSF_MSVQ_Survivors = 2;
        (*psEncC).warping_Q16 = 0;
    } else if Complexity < 2 {
        (*psEncC).pitchEstimationComplexity = SILK_PE_MID_COMPLEX;
        (*psEncC).pitchEstimationThreshold_Q16 = (0.76f64 * ((1) << 16) as f64 + 0.5f64) as i32;
        (*psEncC).pitchEstimationLPCOrder = 8;
        (*psEncC).shapingLPCOrder = 14;
        (*psEncC).la_shape = 5 * (*psEncC).fs_kHz;
        (*psEncC).nStatesDelayedDecision = 1;
        (*psEncC).useInterpolatedNLSFs = 0;
        (*psEncC).NLSF_MSVQ_Survivors = 3;
        (*psEncC).warping_Q16 = 0;
    } else if Complexity < 3 {
        (*psEncC).pitchEstimationComplexity = SILK_PE_MIN_COMPLEX;
        (*psEncC).pitchEstimationThreshold_Q16 = (0.8f64 * ((1) << 16) as f64 + 0.5f64) as i32;
        (*psEncC).pitchEstimationLPCOrder = 6;
        (*psEncC).shapingLPCOrder = 12;
        (*psEncC).la_shape = 3 * (*psEncC).fs_kHz;
        (*psEncC).nStatesDelayedDecision = 2;
        (*psEncC).useInterpolatedNLSFs = 0;
        (*psEncC).NLSF_MSVQ_Survivors = 2;
        (*psEncC).warping_Q16 = 0;
    } else if Complexity < 4 {
        (*psEncC).pitchEstimationComplexity = SILK_PE_MID_COMPLEX;
        (*psEncC).pitchEstimationThreshold_Q16 = (0.76f64 * ((1) << 16) as f64 + 0.5f64) as i32;
        (*psEncC).pitchEstimationLPCOrder = 8;
        (*psEncC).shapingLPCOrder = 14;
        (*psEncC).la_shape = 5 * (*psEncC).fs_kHz;
        (*psEncC).nStatesDelayedDecision = 2;
        (*psEncC).useInterpolatedNLSFs = 0;
        (*psEncC).NLSF_MSVQ_Survivors = 4;
        (*psEncC).warping_Q16 = 0;
    } else if Complexity < 6 {
        (*psEncC).pitchEstimationComplexity = SILK_PE_MID_COMPLEX;
        (*psEncC).pitchEstimationThreshold_Q16 = (0.74f64 * ((1) << 16) as f64 + 0.5f64) as i32;
        (*psEncC).pitchEstimationLPCOrder = 10;
        (*psEncC).shapingLPCOrder = 16;
        (*psEncC).la_shape = 5 * (*psEncC).fs_kHz;
        (*psEncC).nStatesDelayedDecision = 2;
        (*psEncC).useInterpolatedNLSFs = 1;
        (*psEncC).NLSF_MSVQ_Survivors = 6;
        (*psEncC).warping_Q16 =
            (*psEncC).fs_kHz * ((0.015f32 * ((1) << 16) as f32) as f64 + 0.5f64) as i32;
    } else if Complexity < 8 {
        (*psEncC).pitchEstimationComplexity = SILK_PE_MID_COMPLEX;
        (*psEncC).pitchEstimationThreshold_Q16 = (0.72f64 * ((1) << 16) as f64 + 0.5f64) as i32;
        (*psEncC).pitchEstimationLPCOrder = 12;
        (*psEncC).shapingLPCOrder = 20;
        (*psEncC).la_shape = 5 * (*psEncC).fs_kHz;
        (*psEncC).nStatesDelayedDecision = 3;
        (*psEncC).useInterpolatedNLSFs = 1;
        (*psEncC).NLSF_MSVQ_Survivors = 8;
        (*psEncC).warping_Q16 =
            (*psEncC).fs_kHz * ((0.015f32 * ((1) << 16) as f32) as f64 + 0.5f64) as i32;
    } else {
        (*psEncC).pitchEstimationComplexity = SILK_PE_MAX_COMPLEX;
        (*psEncC).pitchEstimationThreshold_Q16 = (0.7f64 * ((1) << 16) as f64 + 0.5f64) as i32;
        (*psEncC).pitchEstimationLPCOrder = 16;
        (*psEncC).shapingLPCOrder = 24;
        (*psEncC).la_shape = 5 * (*psEncC).fs_kHz;
        (*psEncC).nStatesDelayedDecision = MAX_DEL_DEC_STATES;
        (*psEncC).useInterpolatedNLSFs = 1;
        (*psEncC).NLSF_MSVQ_Survivors = 16;
        (*psEncC).warping_Q16 =
            (*psEncC).fs_kHz * ((0.015f32 * ((1) << 16) as f32) as f64 + 0.5f64) as i32;
    }
    (*psEncC).pitchEstimationLPCOrder =
        silk_min_int((*psEncC).pitchEstimationLPCOrder, (*psEncC).predictLPCOrder);
    (*psEncC).shapeWinLength = SUB_FRAME_LENGTH_MS * (*psEncC).fs_kHz + 2 * (*psEncC).la_shape;
    (*psEncC).Complexity = Complexity;
    assert!((*psEncC).pitchEstimationLPCOrder <= 16);
    assert!((*psEncC).shapingLPCOrder <= 24);
    assert!((*psEncC).nStatesDelayedDecision <= 4);
    assert!((*psEncC).warping_Q16 <= 32767);
    assert!((*psEncC).la_shape <= 5 * 16);
    assert!((*psEncC).shapeWinLength <= 15 * 16);
    return ret;
}
#[inline]
unsafe fn silk_setup_LBRR(
    psEncC: *mut silk_encoder_state,
    encControl: *const silk_EncControlStruct,
) -> i32 {
    let mut LBRR_in_previous_packet: i32 = 0;
    let ret: i32 = SILK_NO_ERROR;
    LBRR_in_previous_packet = (*psEncC).LBRR_enabled;
    (*psEncC).LBRR_enabled = (*encControl).LBRR_coded;
    if (*psEncC).LBRR_enabled != 0 {
        if LBRR_in_previous_packet == 0 {
            (*psEncC).LBRR_GainIncreases = 7;
        } else {
            (*psEncC).LBRR_GainIncreases = silk_max_int(
                7 - ((*psEncC).PacketLoss_perc as i64
                    * (0.4f64 * ((1) << 16) as f64 + 0.5f64) as i32 as i16 as i64
                    >> 16) as i32,
                2,
            );
        }
    }
    return ret;
}
