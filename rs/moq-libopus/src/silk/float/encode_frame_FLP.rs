pub mod SigProc_FLP_h {
    #[inline]
    pub unsafe fn silk_short2float_array(out: *mut f32, in_0: *const i16, length: i32) {
        let mut k: i32 = 0;
        k = length - 1;
        while k >= 0 {
            *out.offset(k as isize) = *in_0.offset(k as isize) as f32;
            k -= 1;
        }
    }
}
pub use self::SigProc_FLP_h::silk_short2float_array;
use crate::celt::entcode::{ec_ctx_saved, ec_tell};
use crate::celt::entenc::ec_enc;
use crate::externs::{memcpy, memmove};
use crate::silk::define::{
    CODE_CONDITIONALLY, LA_SHAPE_MS, MAX_CONSECUTIVE_DTX, NB_SPEECH_FRAMES_BEFORE_DTX,
    N_LEVELS_QGAIN, TYPE_NO_VOICE_ACTIVITY, TYPE_UNVOICED, VAD_NO_ACTIVITY,
};
use crate::silk::encode_indices::silk_encode_indices;
use crate::silk::encode_pulses::silk_encode_pulses;
use crate::silk::float::find_pitch_lags_FLP::silk_find_pitch_lags_FLP;
use crate::silk::float::find_pred_coefs_FLP::silk_find_pred_coefs_FLP;
use crate::silk::float::noise_shape_analysis_FLP::silk_noise_shape_analysis_FLP;
use crate::silk::float::process_gains_FLP::silk_process_gains_FLP;
use crate::silk::float::structs_FLP::{silk_encoder_control_FLP, silk_encoder_state_FLP};
use crate::silk::float::wrappers_FLP::silk_NSQ_wrapper_FLP;
use crate::silk::gain_quant::{silk_gains_ID, silk_gains_dequant, silk_gains_quant};
use crate::silk::log2lin::silk_log2lin;
use crate::silk::structs::{silk_nsq_state, SideInfoIndices};
use crate::silk::LP_variable_cutoff::silk_LP_variable_cutoff;
use crate::silk::SigProc_FIX::silk_min_int;
use crate::silk::VAD::silk_VAD_GetSA_Q8_c;

pub unsafe fn silk_encode_do_VAD_FLP(psEnc: *mut silk_encoder_state_FLP, activity: i32) {
    let activity_threshold: i32 = ((0.05f32 * ((1) << 8) as f32) as f64 + 0.5f64) as i32;
    silk_VAD_GetSA_Q8_c(
        &mut (*psEnc).sCmn,
        ((*psEnc).sCmn.inputBuf).as_mut_ptr().offset(1 as isize) as *const i16,
    );
    if activity == VAD_NO_ACTIVITY && (*psEnc).sCmn.speech_activity_Q8 >= activity_threshold {
        (*psEnc).sCmn.speech_activity_Q8 = activity_threshold - 1;
    }
    if (*psEnc).sCmn.speech_activity_Q8 < activity_threshold {
        (*psEnc).sCmn.indices.signalType = TYPE_NO_VOICE_ACTIVITY as i8;
        (*psEnc).sCmn.noSpeechCounter += 1;
        if (*psEnc).sCmn.noSpeechCounter <= NB_SPEECH_FRAMES_BEFORE_DTX {
            (*psEnc).sCmn.inDTX = 0;
        } else if (*psEnc).sCmn.noSpeechCounter > MAX_CONSECUTIVE_DTX + NB_SPEECH_FRAMES_BEFORE_DTX
        {
            (*psEnc).sCmn.noSpeechCounter = NB_SPEECH_FRAMES_BEFORE_DTX;
            (*psEnc).sCmn.inDTX = 0;
        }
        (*psEnc).sCmn.VAD_flags[(*psEnc).sCmn.nFramesEncoded as usize] = 0;
    } else {
        (*psEnc).sCmn.noSpeechCounter = 0;
        (*psEnc).sCmn.inDTX = 0;
        (*psEnc).sCmn.indices.signalType = TYPE_UNVOICED as i8;
        (*psEnc).sCmn.VAD_flags[(*psEnc).sCmn.nFramesEncoded as usize] = 1;
    };
}
pub unsafe fn silk_encode_frame_FLP(
    psEnc: *mut silk_encoder_state_FLP,
    pnBytesOut: *mut i32,
    mut psRangeEnc: Option<&mut ec_enc>,
    condCoding: i32,
    maxBits: i32,
    useCBR: i32,
) -> i32 {
    let mut sEncCtrl: silk_encoder_control_FLP = silk_encoder_control_FLP {
        Gains: [0.; 4],
        PredCoef: [[0.; 16]; 2],
        LTPCoef: [0.; 20],
        LTP_scale: 0.,
        pitchL: [0; 4],
        AR: [0.; 96],
        LF_MA_shp: [0.; 4],
        LF_AR_shp: [0.; 4],
        Tilt: [0.; 4],
        HarmShapeGain: [0.; 4],
        Lambda: 0.,
        input_quality: 0.,
        coding_quality: 0.,
        predGain: 0.,
        LTPredCodGain: 0.,
        ResNrg: [0.; 4],
        GainsUnq_Q16: [0; 4],
        lastGainIndexPrev: 0,
    };
    let mut i: i32 = 0;
    let mut iter: i32 = 0;
    let mut maxIter: i32 = 0;
    let mut found_upper: i32 = 0;
    let mut found_lower: i32 = 0;
    let ret: i32 = 0;
    let mut x_frame: *mut f32 = 0 as *mut f32;
    let mut res_pitch_frame: *mut f32 = 0 as *mut f32;
    let mut res_pitch: [f32; 672] = [0.; 672];
    let mut sRangeEnc_copy = ec_ctx_saved::default();
    let mut sRangeEnc_copy2 = ec_ctx_saved::default();
    let mut sNSQ_copy: silk_nsq_state = silk_nsq_state {
        xq: [0; 640],
        sLTP_shp_Q14: [0; 640],
        sLPC_Q14: [0; 96],
        sAR2_Q14: [0; 24],
        sLF_AR_shp_Q14: 0,
        sDiff_shp_Q14: 0,
        lagPrev: 0,
        sLTP_buf_idx: 0,
        sLTP_shp_buf_idx: 0,
        rand_seed: 0,
        prev_gain_Q16: 0,
        rewhite_flag: 0,
    };
    let mut sNSQ_copy2: silk_nsq_state = silk_nsq_state {
        xq: [0; 640],
        sLTP_shp_Q14: [0; 640],
        sLPC_Q14: [0; 96],
        sAR2_Q14: [0; 24],
        sLF_AR_shp_Q14: 0,
        sDiff_shp_Q14: 0,
        lagPrev: 0,
        sLTP_buf_idx: 0,
        sLTP_shp_buf_idx: 0,
        rand_seed: 0,
        prev_gain_Q16: 0,
        rewhite_flag: 0,
    };
    let mut seed_copy: i32 = 0;
    let mut nBits: i32 = 0;
    let mut nBits_lower: i32 = 0;
    let mut nBits_upper: i32 = 0;
    let mut gainMult_lower: i32 = 0;
    let mut gainMult_upper: i32 = 0;
    let mut gainsID: i32 = 0;
    let mut gainsID_lower: i32 = 0;
    let mut gainsID_upper: i32 = 0;
    let mut gainMult_Q8: i16 = 0;
    let mut ec_prevLagIndex_copy: i16 = 0;
    let mut ec_prevSignalType_copy: i32 = 0;
    let mut LastGainIndex_copy2: i8 = 0;
    let mut pGains_Q16: [i32; 4] = [0; 4];
    let mut ec_buf_copy: [u8; 1275] = [0; 1275];
    let mut gain_lock: [i32; 4] = [0, 0, 0, 0];
    let mut best_gain_mult: [i16; 4] = [0; 4];
    let mut best_sum: [i32; 4] = [0; 4];
    gainMult_upper = 0;
    gainMult_lower = gainMult_upper;
    nBits_upper = gainMult_lower;
    nBits_lower = nBits_upper;
    LastGainIndex_copy2 = nBits_lower as i8;
    let fresh0 = (*psEnc).sCmn.frameCounter;
    (*psEnc).sCmn.frameCounter = (*psEnc).sCmn.frameCounter + 1;
    (*psEnc).sCmn.indices.Seed = (fresh0 & 3) as i8;
    x_frame = ((*psEnc).x_buf)
        .as_mut_ptr()
        .offset((*psEnc).sCmn.ltp_mem_length as isize);
    res_pitch_frame = res_pitch
        .as_mut_ptr()
        .offset((*psEnc).sCmn.ltp_mem_length as isize);
    silk_LP_variable_cutoff(
        &mut (*psEnc).sCmn.sLP,
        ((*psEnc).sCmn.inputBuf).as_mut_ptr().offset(1 as isize),
        (*psEnc).sCmn.frame_length,
    );
    silk_short2float_array(
        x_frame.offset((LA_SHAPE_MS * (*psEnc).sCmn.fs_kHz) as isize),
        ((*psEnc).sCmn.inputBuf).as_mut_ptr().offset(1 as isize),
        (*psEnc).sCmn.frame_length,
    );
    i = 0;
    while i < 8 {
        *x_frame.offset(
            (LA_SHAPE_MS * (*psEnc).sCmn.fs_kHz + i * ((*psEnc).sCmn.frame_length >> 3)) as isize,
        ) += (1 - (i & 2)) as f32 * 1e-6f32;
        i += 1;
    }
    if (*psEnc).sCmn.prefillFlag == 0 {
        let psRangeEnc = &mut **psRangeEnc.as_mut().unwrap();

        silk_find_pitch_lags_FLP(
            psEnc,
            &mut sEncCtrl,
            res_pitch.as_mut_ptr(),
            x_frame as *const f32,
            (*psEnc).sCmn.arch,
        );
        silk_noise_shape_analysis_FLP(psEnc, &mut sEncCtrl, res_pitch_frame, x_frame);
        silk_find_pred_coefs_FLP(
            psEnc,
            &mut sEncCtrl,
            res_pitch_frame as *const f32,
            x_frame as *const f32,
            condCoding,
        );
        silk_process_gains_FLP(psEnc, &mut sEncCtrl, condCoding);
        silk_LBRR_encode_FLP(psEnc, &mut sEncCtrl, x_frame as *const f32, condCoding);
        maxIter = 6;
        gainMult_Q8 = ((1 * ((1) << 8)) as f64 + 0.5f64) as i32 as i16;
        found_lower = 0;
        found_upper = 0;
        gainsID = silk_gains_ID(
            ((*psEnc).sCmn.indices.GainsIndices).as_mut_ptr() as *const i8,
            (*psEnc).sCmn.nb_subfr,
        );
        gainsID_lower = -1;
        gainsID_upper = -1;
        sRangeEnc_copy = psRangeEnc.save();
        memcpy(
            &mut sNSQ_copy as *mut silk_nsq_state as *mut core::ffi::c_void,
            &mut (*psEnc).sCmn.sNSQ as *mut silk_nsq_state as *const core::ffi::c_void,
            ::core::mem::size_of::<silk_nsq_state>() as u64,
        );
        seed_copy = (*psEnc).sCmn.indices.Seed as i32;
        ec_prevLagIndex_copy = (*psEnc).sCmn.ec_prevLagIndex;
        ec_prevSignalType_copy = (*psEnc).sCmn.ec_prevSignalType;
        iter = 0;
        loop {
            if gainsID == gainsID_lower {
                nBits = nBits_lower;
            } else if gainsID == gainsID_upper {
                nBits = nBits_upper;
            } else {
                if iter > 0 {
                    psRangeEnc.restore(sRangeEnc_copy);
                    memcpy(
                        &mut (*psEnc).sCmn.sNSQ as *mut silk_nsq_state as *mut core::ffi::c_void,
                        &mut sNSQ_copy as *mut silk_nsq_state as *const core::ffi::c_void,
                        ::core::mem::size_of::<silk_nsq_state>() as u64,
                    );
                    (*psEnc).sCmn.indices.Seed = seed_copy as i8;
                    (*psEnc).sCmn.ec_prevLagIndex = ec_prevLagIndex_copy;
                    (*psEnc).sCmn.ec_prevSignalType = ec_prevSignalType_copy;
                }
                silk_NSQ_wrapper_FLP(
                    psEnc,
                    &mut sEncCtrl,
                    &mut (*psEnc).sCmn.indices,
                    &mut (*psEnc).sCmn.sNSQ,
                    ((*psEnc).sCmn.pulses).as_mut_ptr(),
                    x_frame as *const f32,
                );
                if iter == maxIter && found_lower == 0 {
                    sRangeEnc_copy2 = psRangeEnc.save();
                }
                silk_encode_indices(
                    &mut (*psEnc).sCmn,
                    psRangeEnc,
                    (*psEnc).sCmn.nFramesEncoded,
                    0,
                    condCoding,
                );
                silk_encode_pulses(
                    psRangeEnc,
                    (*psEnc).sCmn.indices.signalType as i32,
                    (*psEnc).sCmn.indices.quantOffsetType as i32,
                    &mut (*psEnc).sCmn.pulses,
                    (*psEnc).sCmn.frame_length,
                );
                nBits = ec_tell(psRangeEnc);
                if iter == maxIter && found_lower == 0 && nBits > maxBits {
                    psRangeEnc.restore(sRangeEnc_copy2);
                    (*psEnc).sShape.LastGainIndex = sEncCtrl.lastGainIndexPrev;
                    i = 0;
                    while i < (*psEnc).sCmn.nb_subfr {
                        (*psEnc).sCmn.indices.GainsIndices[i as usize] = 4;
                        i += 1;
                    }
                    if condCoding != CODE_CONDITIONALLY {
                        (*psEnc).sCmn.indices.GainsIndices[0 as usize] = sEncCtrl.lastGainIndexPrev;
                    }
                    (*psEnc).sCmn.ec_prevLagIndex = ec_prevLagIndex_copy;
                    (*psEnc).sCmn.ec_prevSignalType = ec_prevSignalType_copy;
                    i = 0;
                    while i < (*psEnc).sCmn.frame_length {
                        (*psEnc).sCmn.pulses[i as usize] = 0;
                        i += 1;
                    }
                    silk_encode_indices(
                        &mut (*psEnc).sCmn,
                        psRangeEnc,
                        (*psEnc).sCmn.nFramesEncoded,
                        0,
                        condCoding,
                    );
                    silk_encode_pulses(
                        psRangeEnc,
                        (*psEnc).sCmn.indices.signalType as i32,
                        (*psEnc).sCmn.indices.quantOffsetType as i32,
                        &mut (*psEnc).sCmn.pulses,
                        (*psEnc).sCmn.frame_length,
                    );
                    nBits = ec_tell(psRangeEnc);
                }
                if useCBR == 0 && iter == 0 && nBits <= maxBits {
                    break;
                }
            }
            if iter == maxIter {
                if found_lower != 0 && (gainsID == gainsID_lower || nBits > maxBits) {
                    psRangeEnc.restore(sRangeEnc_copy2);
                    assert!(sRangeEnc_copy2.offs <= 1275);
                    memcpy(
                        (*psRangeEnc).buf.as_mut_ptr() as *mut core::ffi::c_void,
                        ec_buf_copy.as_mut_ptr() as *const core::ffi::c_void,
                        sRangeEnc_copy2.offs as u64,
                    );
                    memcpy(
                        &mut (*psEnc).sCmn.sNSQ as *mut silk_nsq_state as *mut core::ffi::c_void,
                        &mut sNSQ_copy2 as *mut silk_nsq_state as *const core::ffi::c_void,
                        ::core::mem::size_of::<silk_nsq_state>() as u64,
                    );
                    (*psEnc).sShape.LastGainIndex = LastGainIndex_copy2;
                }
                break;
            } else {
                if nBits > maxBits {
                    if found_lower == 0 && iter >= 2 {
                        sEncCtrl.Lambda = if sEncCtrl.Lambda * 1.5f32 > 1.5f32 {
                            sEncCtrl.Lambda * 1.5f32
                        } else {
                            1.5f32
                        };
                        (*psEnc).sCmn.indices.quantOffsetType = 0;
                        found_upper = 0;
                        gainsID_upper = -1;
                    } else {
                        found_upper = 1;
                        nBits_upper = nBits;
                        gainMult_upper = gainMult_Q8 as i32;
                        gainsID_upper = gainsID;
                    }
                } else {
                    if !(nBits < maxBits - 5) {
                        break;
                    }
                    found_lower = 1;
                    nBits_lower = nBits;
                    gainMult_lower = gainMult_Q8 as i32;
                    if gainsID != gainsID_lower {
                        gainsID_lower = gainsID;
                        sRangeEnc_copy2 = psRangeEnc.save();
                        assert!((*psRangeEnc).offs <= 1275);
                        memcpy(
                            ec_buf_copy.as_mut_ptr() as *mut core::ffi::c_void,
                            (*psRangeEnc).buf.as_mut_ptr() as *const core::ffi::c_void,
                            (*psRangeEnc).offs as u64,
                        );
                        memcpy(
                            &mut sNSQ_copy2 as *mut silk_nsq_state as *mut core::ffi::c_void,
                            &mut (*psEnc).sCmn.sNSQ as *mut silk_nsq_state
                                as *const core::ffi::c_void,
                            ::core::mem::size_of::<silk_nsq_state>() as u64,
                        );
                        LastGainIndex_copy2 = (*psEnc).sShape.LastGainIndex;
                    }
                }
                if found_lower == 0 && nBits > maxBits {
                    let mut j: i32 = 0;
                    i = 0;
                    while i < (*psEnc).sCmn.nb_subfr {
                        let mut sum: i32 = 0;
                        j = i * (*psEnc).sCmn.subfr_length;
                        while j < (i + 1) * (*psEnc).sCmn.subfr_length {
                            sum += ((*psEnc).sCmn.pulses[j as usize] as i32).abs();
                            j += 1;
                        }
                        if iter == 0 || sum < best_sum[i as usize] && gain_lock[i as usize] == 0 {
                            best_sum[i as usize] = sum;
                            best_gain_mult[i as usize] = gainMult_Q8;
                        } else {
                            gain_lock[i as usize] = 1;
                        }
                        i += 1;
                    }
                }
                if found_lower & found_upper == 0 {
                    if nBits > maxBits {
                        if (gainMult_Q8 as i32) < 16384 {
                            gainMult_Q8 = (gainMult_Q8 as i32 * 2) as i16;
                        } else {
                            gainMult_Q8 = 32767;
                        }
                    } else {
                        let mut gain_factor_Q16: i32 = 0;
                        gain_factor_Q16 = silk_log2lin(
                            (((nBits - maxBits) as u32) << 7) as i32 / (*psEnc).sCmn.frame_length
                                + ((16 * ((1) << 7)) as f64 + 0.5f64) as i32,
                        );
                        gainMult_Q8 =
                            (gain_factor_Q16 as i64 * gainMult_Q8 as i64 >> 16) as i32 as i16;
                    }
                } else {
                    gainMult_Q8 = (gainMult_lower
                        + (gainMult_upper - gainMult_lower) * (maxBits - nBits_lower)
                            / (nBits_upper - nBits_lower)) as i16;
                    if gainMult_Q8 as i32 > gainMult_lower + (gainMult_upper - gainMult_lower >> 2)
                    {
                        gainMult_Q8 =
                            (gainMult_lower + (gainMult_upper - gainMult_lower >> 2)) as i16;
                    } else if (gainMult_Q8 as i32)
                        < gainMult_upper - (gainMult_upper - gainMult_lower >> 2)
                    {
                        gainMult_Q8 =
                            (gainMult_upper - (gainMult_upper - gainMult_lower >> 2)) as i16;
                    }
                }
                i = 0;
                while i < (*psEnc).sCmn.nb_subfr {
                    let mut tmp: i16 = 0;
                    if gain_lock[i as usize] != 0 {
                        tmp = best_gain_mult[i as usize];
                    } else {
                        tmp = gainMult_Q8;
                    }
                    pGains_Q16[i as usize] = (((if 0x80000000 as u32 as i32 >> 8 > 0x7fffffff >> 8 {
                        if (sEncCtrl.GainsUnq_Q16[i as usize] as i64 * tmp as i64 >> 16) as i32
                            > 0x80000000 as u32 as i32 >> 8
                        {
                            0x80000000 as u32 as i32 >> 8
                        } else {
                            if ((sEncCtrl.GainsUnq_Q16[i as usize] as i64 * tmp as i64 >> 16)
                                as i32)
                                < 0x7fffffff >> 8
                            {
                                0x7fffffff >> 8
                            } else {
                                (sEncCtrl.GainsUnq_Q16[i as usize] as i64 * tmp as i64 >> 16) as i32
                            }
                        }
                    } else {
                        if (sEncCtrl.GainsUnq_Q16[i as usize] as i64 * tmp as i64 >> 16) as i32
                            > 0x7fffffff >> 8
                        {
                            0x7fffffff >> 8
                        } else {
                            if ((sEncCtrl.GainsUnq_Q16[i as usize] as i64 * tmp as i64 >> 16)
                                as i32)
                                < 0x80000000 as u32 as i32 >> 8
                            {
                                0x80000000 as u32 as i32 >> 8
                            } else {
                                (sEncCtrl.GainsUnq_Q16[i as usize] as i64 * tmp as i64 >> 16) as i32
                            }
                        }
                    }) as u32)
                        << 8) as i32;
                    i += 1;
                }
                (*psEnc).sShape.LastGainIndex = sEncCtrl.lastGainIndexPrev;
                silk_gains_quant(
                    ((*psEnc).sCmn.indices.GainsIndices).as_mut_ptr(),
                    pGains_Q16.as_mut_ptr(),
                    &mut (*psEnc).sShape.LastGainIndex,
                    (condCoding == CODE_CONDITIONALLY) as i32,
                    (*psEnc).sCmn.nb_subfr,
                );
                gainsID = silk_gains_ID(
                    ((*psEnc).sCmn.indices.GainsIndices).as_mut_ptr() as *const i8,
                    (*psEnc).sCmn.nb_subfr,
                );
                i = 0;
                while i < (*psEnc).sCmn.nb_subfr {
                    sEncCtrl.Gains[i as usize] = pGains_Q16[i as usize] as f32 / 65536.0f32;
                    i += 1;
                }
                iter += 1;
            }
        }
    }
    memmove(
        ((*psEnc).x_buf).as_mut_ptr() as *mut core::ffi::c_void,
        &mut *((*psEnc).x_buf)
            .as_mut_ptr()
            .offset((*psEnc).sCmn.frame_length as isize) as *mut f32
            as *const core::ffi::c_void,
        (((*psEnc).sCmn.ltp_mem_length + 5 * (*psEnc).sCmn.fs_kHz) as u64)
            .wrapping_mul(::core::mem::size_of::<f32>() as u64),
    );
    if (*psEnc).sCmn.prefillFlag != 0 {
        *pnBytesOut = 0;
        return ret;
    }
    (*psEnc).sCmn.prevLag = sEncCtrl.pitchL[((*psEnc).sCmn.nb_subfr - 1) as usize];
    (*psEnc).sCmn.prevSignalType = (*psEnc).sCmn.indices.signalType;
    (*psEnc).sCmn.first_frame_after_reset = 0;
    *pnBytesOut = (ec_tell(psRangeEnc.unwrap()) + 7) >> 3;

    ret
}
#[inline]
unsafe fn silk_LBRR_encode_FLP(
    psEnc: *mut silk_encoder_state_FLP,
    psEncCtrl: *mut silk_encoder_control_FLP,
    xfw: *const f32,
    condCoding: i32,
) {
    let mut k: i32 = 0;
    let mut Gains_Q16: [i32; 4] = [0; 4];
    let mut TempGains: [f32; 4] = [0.; 4];
    let psIndices_LBRR: *mut SideInfoIndices = &mut *((*psEnc).sCmn.indices_LBRR)
        .as_mut_ptr()
        .offset((*psEnc).sCmn.nFramesEncoded as isize)
        as *mut SideInfoIndices;
    let mut sNSQ_LBRR: silk_nsq_state = silk_nsq_state {
        xq: [0; 640],
        sLTP_shp_Q14: [0; 640],
        sLPC_Q14: [0; 96],
        sAR2_Q14: [0; 24],
        sLF_AR_shp_Q14: 0,
        sDiff_shp_Q14: 0,
        lagPrev: 0,
        sLTP_buf_idx: 0,
        sLTP_shp_buf_idx: 0,
        rand_seed: 0,
        prev_gain_Q16: 0,
        rewhite_flag: 0,
    };
    if (*psEnc).sCmn.LBRR_enabled != 0
        && (*psEnc).sCmn.speech_activity_Q8 > ((0.3f32 * ((1) << 8) as f32) as f64 + 0.5f64) as i32
    {
        (*psEnc).sCmn.LBRR_flags[(*psEnc).sCmn.nFramesEncoded as usize] = 1;
        memcpy(
            &mut sNSQ_LBRR as *mut silk_nsq_state as *mut core::ffi::c_void,
            &mut (*psEnc).sCmn.sNSQ as *mut silk_nsq_state as *const core::ffi::c_void,
            ::core::mem::size_of::<silk_nsq_state>() as u64,
        );
        memcpy(
            psIndices_LBRR as *mut core::ffi::c_void,
            &mut (*psEnc).sCmn.indices as *mut SideInfoIndices as *const core::ffi::c_void,
            ::core::mem::size_of::<SideInfoIndices>() as u64,
        );
        memcpy(
            TempGains.as_mut_ptr() as *mut core::ffi::c_void,
            ((*psEncCtrl).Gains).as_mut_ptr() as *const core::ffi::c_void,
            ((*psEnc).sCmn.nb_subfr as u64).wrapping_mul(::core::mem::size_of::<f32>() as u64),
        );
        if (*psEnc).sCmn.nFramesEncoded == 0
            || (*psEnc).sCmn.LBRR_flags[((*psEnc).sCmn.nFramesEncoded - 1) as usize] == 0
        {
            (*psEnc).sCmn.LBRRprevLastGainIndex = (*psEnc).sShape.LastGainIndex;
            (*psIndices_LBRR).GainsIndices[0 as usize] =
                ((*psIndices_LBRR).GainsIndices[0 as usize] as i32
                    + (*psEnc).sCmn.LBRR_GainIncreases) as i8;
            (*psIndices_LBRR).GainsIndices[0 as usize] = silk_min_int(
                (*psIndices_LBRR).GainsIndices[0 as usize] as i32,
                N_LEVELS_QGAIN - 1,
            ) as i8;
        }
        silk_gains_dequant(
            Gains_Q16.as_mut_ptr(),
            ((*psIndices_LBRR).GainsIndices).as_mut_ptr() as *const i8,
            &mut (*psEnc).sCmn.LBRRprevLastGainIndex,
            (condCoding == CODE_CONDITIONALLY) as i32,
            (*psEnc).sCmn.nb_subfr,
        );
        k = 0;
        while k < (*psEnc).sCmn.nb_subfr {
            (*psEncCtrl).Gains[k as usize] = Gains_Q16[k as usize] as f32 * (1.0f32 / 65536.0f32);
            k += 1;
        }
        silk_NSQ_wrapper_FLP(
            psEnc,
            psEncCtrl,
            psIndices_LBRR,
            &mut sNSQ_LBRR,
            ((*psEnc).sCmn.pulses_LBRR[(*psEnc).sCmn.nFramesEncoded as usize]).as_mut_ptr(),
            xfw,
        );
        memcpy(
            ((*psEncCtrl).Gains).as_mut_ptr() as *mut core::ffi::c_void,
            TempGains.as_mut_ptr() as *const core::ffi::c_void,
            ((*psEnc).sCmn.nb_subfr as u64).wrapping_mul(::core::mem::size_of::<f32>() as u64),
        );
    }
}
