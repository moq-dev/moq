pub mod typedef_h {
    pub const silk_int32_MAX: i32 = i32::MAX;
    pub const silk_int16_MIN: i32 = i16::MIN as i32;
    pub const silk_int16_MAX: i32 = i16::MAX as i32;
}
pub mod NSQ_h {
    #[inline]
    pub unsafe fn silk_noise_shape_quantizer_short_prediction_c(
        buf32: *const i32,
        coef16: *const i16,
        order: i32,
    ) -> i32 {
        let mut out: i32 = 0;
        out = order >> 1;
        out = (out as i64
            + (*buf32.offset(0 as isize) as i64 * *coef16.offset(0 as isize) as i64 >> 16))
            as i32;
        out = (out as i64
            + (*buf32.offset(-1 as isize) as i64 * *coef16.offset(1 as isize) as i64 >> 16))
            as i32;
        out = (out as i64
            + (*buf32.offset(-(2) as isize) as i64 * *coef16.offset(2 as isize) as i64 >> 16))
            as i32;
        out = (out as i64
            + (*buf32.offset(-(3) as isize) as i64 * *coef16.offset(3 as isize) as i64 >> 16))
            as i32;
        out = (out as i64
            + (*buf32.offset(-(4) as isize) as i64 * *coef16.offset(4 as isize) as i64 >> 16))
            as i32;
        out = (out as i64
            + (*buf32.offset(-(5) as isize) as i64 * *coef16.offset(5 as isize) as i64 >> 16))
            as i32;
        out = (out as i64
            + (*buf32.offset(-(6) as isize) as i64 * *coef16.offset(6 as isize) as i64 >> 16))
            as i32;
        out = (out as i64
            + (*buf32.offset(-(7) as isize) as i64 * *coef16.offset(7 as isize) as i64 >> 16))
            as i32;
        out = (out as i64
            + (*buf32.offset(-(8) as isize) as i64 * *coef16.offset(8 as isize) as i64 >> 16))
            as i32;
        out = (out as i64
            + (*buf32.offset(-(9) as isize) as i64 * *coef16.offset(9 as isize) as i64 >> 16))
            as i32;
        if order == 16 {
            out = (out as i64
                + (*buf32.offset(-(10) as isize) as i64 * *coef16.offset(10 as isize) as i64 >> 16))
                as i32;
            out = (out as i64
                + (*buf32.offset(-(11) as isize) as i64 * *coef16.offset(11 as isize) as i64 >> 16))
                as i32;
            out = (out as i64
                + (*buf32.offset(-(12) as isize) as i64 * *coef16.offset(12 as isize) as i64 >> 16))
                as i32;
            out = (out as i64
                + (*buf32.offset(-(13) as isize) as i64 * *coef16.offset(13 as isize) as i64 >> 16))
                as i32;
            out = (out as i64
                + (*buf32.offset(-(14) as isize) as i64 * *coef16.offset(14 as isize) as i64 >> 16))
                as i32;
            out = (out as i64
                + (*buf32.offset(-(15) as isize) as i64 * *coef16.offset(15 as isize) as i64 >> 16))
                as i32;
        }
        return out;
    }
}
pub use self::typedef_h::{silk_int16_MAX, silk_int16_MIN, silk_int32_MAX};
pub use self::NSQ_h::silk_noise_shape_quantizer_short_prediction_c;
use crate::externs::{memcpy, memmove, memset};
use crate::silk::define::{
    DECISION_DELAY, HARM_SHAPE_FIR_TAPS, LTP_ORDER, MAX_LPC_ORDER, MAX_SHAPE_LPC_ORDER,
    NSQ_LPC_BUF_LENGTH, TYPE_VOICED,
};
use crate::silk::structs::{silk_encoder_state, silk_nsq_state, SideInfoIndices};
use crate::silk::tables_other::silk_Quantization_Offsets_Q10;
use crate::silk::Inlines::{silk_DIV32_varQ, silk_INVERSE32_varQ};
use crate::silk::LPC_analysis_filter::silk_LPC_analysis_filter;
use crate::silk::SigProc_FIX::silk_min_int;

#[derive(Copy, Clone)]
#[repr(C)]
pub struct NSQ_del_dec_struct {
    pub sLPC_Q14: [i32; 96],
    pub RandState: [i32; 40],
    pub Q_Q10: [i32; 40],
    pub Xq_Q14: [i32; 40],
    pub Pred_Q15: [i32; 40],
    pub Shape_Q14: [i32; 40],
    pub sAR2_Q14: [i32; 24],
    pub LF_AR_Q14: i32,
    pub Diff_Q14: i32,
    pub Seed: i32,
    pub SeedInit: i32,
    pub RD_Q10: i32,
}
#[derive(Copy, Clone)]
#[repr(C)]
pub struct NSQ_sample_struct {
    pub Q_Q10: i32,
    pub RD_Q10: i32,
    pub xq_Q14: i32,
    pub LF_AR_Q14: i32,
    pub Diff_Q14: i32,
    pub sLTP_shp_Q14: i32,
    pub LPC_exc_Q14: i32,
}
pub type NSQ_sample_pair = [NSQ_sample_struct; 2];
pub unsafe fn silk_NSQ_del_dec_c(
    psEncC: *const silk_encoder_state,
    NSQ: *mut silk_nsq_state,
    psIndices: *mut SideInfoIndices,
    mut x16: *const i16,
    mut pulses: *mut i8,
    PredCoef_Q12: *const i16,
    LTPCoef_Q14: *const i16,
    AR_Q13: *const i16,
    HarmShapeGain_Q14: *const i32,
    Tilt_Q14: *const i32,
    LF_shp_Q14: *const i32,
    Gains_Q16: *const i32,
    pitchL: *const i32,
    Lambda_Q10: i32,
    LTP_scale_Q14: i32,
) {
    let mut i: i32 = 0;
    let mut k: i32 = 0;
    let mut lag: i32 = 0;
    let mut start_idx: i32 = 0;
    let mut LSF_interpolation_flag: i32 = 0;
    let mut Winner_ind: i32 = 0;
    let mut subfr: i32 = 0;
    let mut last_smple_idx: i32 = 0;
    let mut smpl_buf_idx: i32 = 0;
    let mut decisionDelay: i32 = 0;
    let mut A_Q12: *const i16 = 0 as *const i16;
    let mut B_Q14: *const i16 = 0 as *const i16;
    let mut AR_shp_Q13: *const i16 = 0 as *const i16;
    let mut pxq: *mut i16 = 0 as *mut i16;
    let mut HarmShapeFIRPacked_Q14: i32 = 0;
    let mut offset_Q10: i32 = 0;
    let mut RDmin_Q10: i32 = 0;
    let mut Gain_Q10: i32 = 0;
    let mut psDD: *mut NSQ_del_dec_struct = 0 as *mut NSQ_del_dec_struct;
    lag = (*NSQ).lagPrev;
    let vla = (*psEncC).nStatesDelayedDecision as usize;
    let mut psDelDec: Vec<NSQ_del_dec_struct> = ::std::vec::from_elem(
        NSQ_del_dec_struct {
            sLPC_Q14: [0; 96],
            RandState: [0; 40],
            Q_Q10: [0; 40],
            Xq_Q14: [0; 40],
            Pred_Q15: [0; 40],
            Shape_Q14: [0; 40],
            sAR2_Q14: [0; 24],
            LF_AR_Q14: 0,
            Diff_Q14: 0,
            Seed: 0,
            SeedInit: 0,
            RD_Q10: 0,
        },
        vla,
    );
    memset(
        psDelDec.as_mut_ptr() as *mut core::ffi::c_void,
        0,
        ((*psEncC).nStatesDelayedDecision as u64)
            .wrapping_mul(::core::mem::size_of::<NSQ_del_dec_struct>() as u64),
    );
    k = 0;
    while k < (*psEncC).nStatesDelayedDecision {
        psDD = &mut *psDelDec.as_mut_ptr().offset(k as isize) as *mut NSQ_del_dec_struct;
        (*psDD).Seed = k + (*psIndices).Seed as i32 & 3;
        (*psDD).SeedInit = (*psDD).Seed;
        (*psDD).RD_Q10 = 0;
        (*psDD).LF_AR_Q14 = (*NSQ).sLF_AR_shp_Q14;
        (*psDD).Diff_Q14 = (*NSQ).sDiff_shp_Q14;
        (*psDD).Shape_Q14[0 as usize] =
            (*NSQ).sLTP_shp_Q14[((*psEncC).ltp_mem_length - 1) as usize];
        memcpy(
            ((*psDD).sLPC_Q14).as_mut_ptr() as *mut core::ffi::c_void,
            ((*NSQ).sLPC_Q14).as_mut_ptr() as *const core::ffi::c_void,
            16_u64.wrapping_mul(::core::mem::size_of::<i32>() as u64),
        );
        memcpy(
            ((*psDD).sAR2_Q14).as_mut_ptr() as *mut core::ffi::c_void,
            ((*NSQ).sAR2_Q14).as_mut_ptr() as *const core::ffi::c_void,
            ::core::mem::size_of::<[i32; 24]>() as u64,
        );
        k += 1;
    }
    offset_Q10 = silk_Quantization_Offsets_Q10[((*psIndices).signalType as i32 >> 1) as usize]
        [(*psIndices).quantOffsetType as usize] as i32;
    smpl_buf_idx = 0;
    decisionDelay = silk_min_int(DECISION_DELAY, (*psEncC).subfr_length);
    if (*psIndices).signalType as i32 == TYPE_VOICED {
        k = 0;
        while k < (*psEncC).nb_subfr {
            decisionDelay = silk_min_int(
                decisionDelay,
                *pitchL.offset(k as isize) - LTP_ORDER / 2 - 1,
            );
            k += 1;
        }
    } else if lag > 0 {
        decisionDelay = silk_min_int(decisionDelay, lag - LTP_ORDER / 2 - 1);
    }
    if (*psIndices).NLSFInterpCoef_Q2 as i32 == 4 {
        LSF_interpolation_flag = 0;
    } else {
        LSF_interpolation_flag = 1;
    }
    let vla_0 = ((*psEncC).ltp_mem_length + (*psEncC).frame_length) as usize;
    let mut sLTP_Q15: Vec<i32> = ::std::vec::from_elem(0, vla_0);
    let vla_1 = ((*psEncC).ltp_mem_length + (*psEncC).frame_length) as usize;
    let mut sLTP: Vec<i16> = ::std::vec::from_elem(0, vla_1);
    let vla_2 = (*psEncC).subfr_length as usize;
    let mut x_sc_Q10: Vec<i32> = ::std::vec::from_elem(0, vla_2);
    let mut delayedGain_Q10: [i32; 40] = [0; 40];
    pxq = &mut *((*NSQ).xq)
        .as_mut_ptr()
        .offset((*psEncC).ltp_mem_length as isize) as *mut i16;
    (*NSQ).sLTP_shp_buf_idx = (*psEncC).ltp_mem_length;
    (*NSQ).sLTP_buf_idx = (*psEncC).ltp_mem_length;
    subfr = 0;
    k = 0;
    while k < (*psEncC).nb_subfr {
        A_Q12 = &*PredCoef_Q12
            .offset(((k >> 1 | 1 - LSF_interpolation_flag) * MAX_LPC_ORDER) as isize)
            as *const i16;
        B_Q14 = &*LTPCoef_Q14.offset((k * LTP_ORDER) as isize) as *const i16;
        AR_shp_Q13 = &*AR_Q13.offset((k * MAX_SHAPE_LPC_ORDER) as isize) as *const i16;
        HarmShapeFIRPacked_Q14 = *HarmShapeGain_Q14.offset(k as isize) >> 2;
        HarmShapeFIRPacked_Q14 |=
            (((*HarmShapeGain_Q14.offset(k as isize) >> 1) as u32) << 16) as i32;
        (*NSQ).rewhite_flag = 0;
        if (*psIndices).signalType as i32 == TYPE_VOICED {
            lag = *pitchL.offset(k as isize);
            if k & 3 - ((LSF_interpolation_flag as u32) << 1) as i32 == 0 {
                if k == 2 {
                    RDmin_Q10 = (*psDelDec.as_mut_ptr().offset(0 as isize)).RD_Q10;
                    Winner_ind = 0;
                    i = 1;
                    while i < (*psEncC).nStatesDelayedDecision {
                        if (*psDelDec.as_mut_ptr().offset(i as isize)).RD_Q10 < RDmin_Q10 {
                            RDmin_Q10 = (*psDelDec.as_mut_ptr().offset(i as isize)).RD_Q10;
                            Winner_ind = i;
                        }
                        i += 1;
                    }
                    i = 0;
                    while i < (*psEncC).nStatesDelayedDecision {
                        if i != Winner_ind {
                            let ref mut fresh0 = (*psDelDec.as_mut_ptr().offset(i as isize)).RD_Q10;
                            *fresh0 += silk_int32_MAX >> 4;
                        }
                        i += 1;
                    }
                    psDD = &mut *psDelDec.as_mut_ptr().offset(Winner_ind as isize)
                        as *mut NSQ_del_dec_struct;
                    last_smple_idx = smpl_buf_idx + decisionDelay;
                    i = 0;
                    while i < decisionDelay {
                        last_smple_idx = (last_smple_idx - 1) % DECISION_DELAY;
                        if last_smple_idx < 0 {
                            last_smple_idx += DECISION_DELAY;
                        }
                        *pulses.offset((i - decisionDelay) as isize) = (if 10 == 1 {
                            ((*psDD).Q_Q10[last_smple_idx as usize] >> 1)
                                + ((*psDD).Q_Q10[last_smple_idx as usize] & 1)
                        } else {
                            ((*psDD).Q_Q10[last_smple_idx as usize] >> 10 - 1) + 1 >> 1
                        })
                            as i8;
                        *pxq.offset((i - decisionDelay) as isize) = (if (if 14 == 1 {
                            (((*psDD).Xq_Q14[last_smple_idx as usize] as i64
                                * *Gains_Q16.offset(1 as isize) as i64
                                >> 16) as i32
                                >> 1)
                                + (((*psDD).Xq_Q14[last_smple_idx as usize] as i64
                                    * *Gains_Q16.offset(1 as isize) as i64
                                    >> 16) as i32
                                    & 1)
                        } else {
                            (((*psDD).Xq_Q14[last_smple_idx as usize] as i64
                                * *Gains_Q16.offset(1 as isize) as i64
                                >> 16) as i32
                                >> 14 - 1)
                                + 1
                                >> 1
                        }) > silk_int16_MAX
                        {
                            silk_int16_MAX
                        } else if (if 14 == 1 {
                            (((*psDD).Xq_Q14[last_smple_idx as usize] as i64
                                * *Gains_Q16.offset(1 as isize) as i64
                                >> 16) as i32
                                >> 1)
                                + (((*psDD).Xq_Q14[last_smple_idx as usize] as i64
                                    * *Gains_Q16.offset(1 as isize) as i64
                                    >> 16) as i32
                                    & 1)
                        } else {
                            (((*psDD).Xq_Q14[last_smple_idx as usize] as i64
                                * *Gains_Q16.offset(1 as isize) as i64
                                >> 16) as i32
                                >> 14 - 1)
                                + 1
                                >> 1
                        }) < silk_int16_MIN
                        {
                            silk_int16_MIN
                        } else if 14 == 1 {
                            (((*psDD).Xq_Q14[last_smple_idx as usize] as i64
                                * *Gains_Q16.offset(1 as isize) as i64
                                >> 16) as i32
                                >> 1)
                                + (((*psDD).Xq_Q14[last_smple_idx as usize] as i64
                                    * *Gains_Q16.offset(1 as isize) as i64
                                    >> 16) as i32
                                    & 1)
                        } else {
                            (((*psDD).Xq_Q14[last_smple_idx as usize] as i64
                                * *Gains_Q16.offset(1 as isize) as i64
                                >> 16) as i32
                                >> 14 - 1)
                                + 1
                                >> 1
                        })
                            as i16;
                        (*NSQ).sLTP_shp_Q14
                            [((*NSQ).sLTP_shp_buf_idx - decisionDelay + i) as usize] =
                            (*psDD).Shape_Q14[last_smple_idx as usize];
                        i += 1;
                    }
                    subfr = 0;
                }
                start_idx =
                    (*psEncC).ltp_mem_length - lag - (*psEncC).predictLPCOrder - LTP_ORDER / 2;
                assert!(start_idx > 0);
                silk_LPC_analysis_filter(
                    &mut *sLTP.as_mut_ptr().offset(start_idx as isize),
                    &mut *((*NSQ).xq)
                        .as_mut_ptr()
                        .offset((start_idx + k * (*psEncC).subfr_length) as isize),
                    A_Q12,
                    (*psEncC).ltp_mem_length - start_idx,
                    (*psEncC).predictLPCOrder,
                    (*psEncC).arch,
                );
                (*NSQ).sLTP_buf_idx = (*psEncC).ltp_mem_length;
                (*NSQ).rewhite_flag = 1;
            }
        }
        silk_nsq_del_dec_scale_states(
            psEncC,
            NSQ,
            psDelDec.as_mut_ptr(),
            x16,
            x_sc_Q10.as_mut_ptr(),
            sLTP.as_mut_ptr() as *const i16,
            sLTP_Q15.as_mut_ptr(),
            k,
            (*psEncC).nStatesDelayedDecision,
            LTP_scale_Q14,
            Gains_Q16,
            pitchL,
            (*psIndices).signalType as i32,
            decisionDelay,
        );
        let fresh1 = subfr;
        subfr = subfr + 1;
        silk_noise_shape_quantizer_del_dec(
            NSQ,
            psDelDec.as_mut_ptr(),
            (*psIndices).signalType as i32,
            x_sc_Q10.as_mut_ptr() as *const i32,
            pulses,
            pxq,
            sLTP_Q15.as_mut_ptr(),
            delayedGain_Q10.as_mut_ptr(),
            A_Q12,
            B_Q14,
            AR_shp_Q13,
            lag,
            HarmShapeFIRPacked_Q14,
            *Tilt_Q14.offset(k as isize),
            *LF_shp_Q14.offset(k as isize),
            *Gains_Q16.offset(k as isize),
            Lambda_Q10,
            offset_Q10,
            (*psEncC).subfr_length,
            fresh1,
            (*psEncC).shapingLPCOrder,
            (*psEncC).predictLPCOrder,
            (*psEncC).warping_Q16,
            (*psEncC).nStatesDelayedDecision,
            &mut smpl_buf_idx,
            decisionDelay,
            (*psEncC).arch,
        );
        x16 = x16.offset((*psEncC).subfr_length as isize);
        pulses = pulses.offset((*psEncC).subfr_length as isize);
        pxq = pxq.offset((*psEncC).subfr_length as isize);
        k += 1;
    }
    RDmin_Q10 = (*psDelDec.as_mut_ptr().offset(0 as isize)).RD_Q10;
    Winner_ind = 0;
    k = 1;
    while k < (*psEncC).nStatesDelayedDecision {
        if (*psDelDec.as_mut_ptr().offset(k as isize)).RD_Q10 < RDmin_Q10 {
            RDmin_Q10 = (*psDelDec.as_mut_ptr().offset(k as isize)).RD_Q10;
            Winner_ind = k;
        }
        k += 1;
    }
    psDD = &mut *psDelDec.as_mut_ptr().offset(Winner_ind as isize) as *mut NSQ_del_dec_struct;
    (*psIndices).Seed = (*psDD).SeedInit as i8;
    last_smple_idx = smpl_buf_idx + decisionDelay;
    Gain_Q10 = *Gains_Q16.offset(((*psEncC).nb_subfr - 1) as isize) >> 6;
    i = 0;
    while i < decisionDelay {
        last_smple_idx = (last_smple_idx - 1) % DECISION_DELAY;
        if last_smple_idx < 0 {
            last_smple_idx += DECISION_DELAY;
        }
        *pulses.offset((i - decisionDelay) as isize) = (if 10 == 1 {
            ((*psDD).Q_Q10[last_smple_idx as usize] >> 1)
                + ((*psDD).Q_Q10[last_smple_idx as usize] & 1)
        } else {
            ((*psDD).Q_Q10[last_smple_idx as usize] >> 10 - 1) + 1 >> 1
        }) as i8;
        *pxq.offset((i - decisionDelay) as isize) = (if (if 8 == 1 {
            (((*psDD).Xq_Q14[last_smple_idx as usize] as i64 * Gain_Q10 as i64 >> 16) as i32 >> 1)
                + (((*psDD).Xq_Q14[last_smple_idx as usize] as i64 * Gain_Q10 as i64 >> 16) as i32
                    & 1)
        } else {
            (((*psDD).Xq_Q14[last_smple_idx as usize] as i64 * Gain_Q10 as i64 >> 16) as i32
                >> 8 - 1)
                + 1
                >> 1
        }) > silk_int16_MAX
        {
            silk_int16_MAX
        } else if (if 8 == 1 {
            (((*psDD).Xq_Q14[last_smple_idx as usize] as i64 * Gain_Q10 as i64 >> 16) as i32 >> 1)
                + (((*psDD).Xq_Q14[last_smple_idx as usize] as i64 * Gain_Q10 as i64 >> 16) as i32
                    & 1)
        } else {
            (((*psDD).Xq_Q14[last_smple_idx as usize] as i64 * Gain_Q10 as i64 >> 16) as i32
                >> 8 - 1)
                + 1
                >> 1
        }) < silk_int16_MIN
        {
            silk_int16_MIN
        } else if 8 == 1 {
            (((*psDD).Xq_Q14[last_smple_idx as usize] as i64 * Gain_Q10 as i64 >> 16) as i32 >> 1)
                + (((*psDD).Xq_Q14[last_smple_idx as usize] as i64 * Gain_Q10 as i64 >> 16) as i32
                    & 1)
        } else {
            (((*psDD).Xq_Q14[last_smple_idx as usize] as i64 * Gain_Q10 as i64 >> 16) as i32
                >> 8 - 1)
                + 1
                >> 1
        }) as i16;
        (*NSQ).sLTP_shp_Q14[((*NSQ).sLTP_shp_buf_idx - decisionDelay + i) as usize] =
            (*psDD).Shape_Q14[last_smple_idx as usize];
        i += 1;
    }
    memcpy(
        ((*NSQ).sLPC_Q14).as_mut_ptr() as *mut core::ffi::c_void,
        &mut *((*psDD).sLPC_Q14)
            .as_mut_ptr()
            .offset((*psEncC).subfr_length as isize) as *mut i32
            as *const core::ffi::c_void,
        16_u64.wrapping_mul(::core::mem::size_of::<i32>() as u64),
    );
    memcpy(
        ((*NSQ).sAR2_Q14).as_mut_ptr() as *mut core::ffi::c_void,
        ((*psDD).sAR2_Q14).as_mut_ptr() as *const core::ffi::c_void,
        ::core::mem::size_of::<[i32; 24]>() as u64,
    );
    (*NSQ).sLF_AR_shp_Q14 = (*psDD).LF_AR_Q14;
    (*NSQ).sDiff_shp_Q14 = (*psDD).Diff_Q14;
    (*NSQ).lagPrev = *pitchL.offset(((*psEncC).nb_subfr - 1) as isize);
    memmove(
        ((*NSQ).xq).as_mut_ptr() as *mut core::ffi::c_void,
        &mut *((*NSQ).xq)
            .as_mut_ptr()
            .offset((*psEncC).frame_length as isize) as *mut i16
            as *const core::ffi::c_void,
        ((*psEncC).ltp_mem_length as u64).wrapping_mul(::core::mem::size_of::<i16>() as u64),
    );
    memmove(
        ((*NSQ).sLTP_shp_Q14).as_mut_ptr() as *mut core::ffi::c_void,
        &mut *((*NSQ).sLTP_shp_Q14)
            .as_mut_ptr()
            .offset((*psEncC).frame_length as isize) as *mut i32
            as *const core::ffi::c_void,
        ((*psEncC).ltp_mem_length as u64).wrapping_mul(::core::mem::size_of::<i32>() as u64),
    );
}
#[inline]
unsafe fn silk_noise_shape_quantizer_del_dec(
    NSQ: *mut silk_nsq_state,
    psDelDec: *mut NSQ_del_dec_struct,
    signalType: i32,
    x_Q10: *const i32,
    pulses: *mut i8,
    xq: *mut i16,
    sLTP_Q15: *mut i32,
    delayedGain_Q10: *mut i32,
    a_Q12: *const i16,
    b_Q14: *const i16,
    AR_shp_Q13: *const i16,
    lag: i32,
    HarmShapeFIRPacked_Q14: i32,
    Tilt_Q14: i32,
    LF_shp_Q14: i32,
    Gain_Q16: i32,
    Lambda_Q10: i32,
    offset_Q10: i32,
    length: i32,
    subfr: i32,
    shapingLPCOrder: i32,
    predictLPCOrder: i32,
    warping_Q16: i32,
    nStatesDelayedDecision: i32,
    smpl_buf_idx: *mut i32,
    decisionDelay: i32,
    _arch: i32,
) {
    let mut i: i32 = 0;
    let mut j: i32 = 0;
    let mut k: i32 = 0;
    let mut Winner_ind: i32 = 0;
    let mut RDmin_ind: i32 = 0;
    let mut RDmax_ind: i32 = 0;
    let mut last_smple_idx: i32 = 0;
    let mut Winner_rand_state: i32 = 0;
    let mut LTP_pred_Q14: i32 = 0;
    let mut LPC_pred_Q14: i32 = 0;
    let mut n_AR_Q14: i32 = 0;
    let mut n_LTP_Q14: i32 = 0;
    let mut n_LF_Q14: i32 = 0;
    let mut r_Q10: i32 = 0;
    let mut rr_Q10: i32 = 0;
    let mut rd1_Q10: i32 = 0;
    let mut rd2_Q10: i32 = 0;
    let mut RDmin_Q10: i32 = 0;
    let mut RDmax_Q10: i32 = 0;
    let mut q1_Q0: i32 = 0;
    let mut q1_Q10: i32 = 0;
    let mut q2_Q10: i32 = 0;
    let mut exc_Q14: i32 = 0;
    let mut LPC_exc_Q14: i32 = 0;
    let mut xq_Q14: i32 = 0;
    let mut Gain_Q10: i32 = 0;
    let mut tmp1: i32 = 0;
    let mut tmp2: i32 = 0;
    let mut sLF_AR_shp_Q14: i32 = 0;
    let mut pred_lag_ptr: *mut i32 = 0 as *mut i32;
    let mut shp_lag_ptr: *mut i32 = 0 as *mut i32;
    let mut psLPC_Q14: *mut i32 = 0 as *mut i32;
    let mut psDD: *mut NSQ_del_dec_struct = 0 as *mut NSQ_del_dec_struct;
    let mut psSS: *mut NSQ_sample_struct = 0 as *mut NSQ_sample_struct;
    assert!(nStatesDelayedDecision > 0);
    let vla = nStatesDelayedDecision as usize;
    let mut psSampleState: Vec<NSQ_sample_pair> = ::std::vec::from_elem(
        [NSQ_sample_struct {
            Q_Q10: 0,
            RD_Q10: 0,
            xq_Q14: 0,
            LF_AR_Q14: 0,
            Diff_Q14: 0,
            sLTP_shp_Q14: 0,
            LPC_exc_Q14: 0,
        }; 2],
        vla,
    );
    shp_lag_ptr = &mut *((*NSQ).sLTP_shp_Q14)
        .as_mut_ptr()
        .offset(((*NSQ).sLTP_shp_buf_idx - lag + HARM_SHAPE_FIR_TAPS / 2) as isize)
        as *mut i32;
    pred_lag_ptr =
        &mut *sLTP_Q15.offset(((*NSQ).sLTP_buf_idx - lag + LTP_ORDER / 2) as isize) as *mut i32;
    Gain_Q10 = Gain_Q16 >> 6;
    i = 0;
    while i < length {
        if signalType == TYPE_VOICED {
            LTP_pred_Q14 = 2;
            LTP_pred_Q14 = (LTP_pred_Q14 as i64
                + (*pred_lag_ptr.offset(0 as isize) as i64 * *b_Q14.offset(0 as isize) as i64
                    >> 16)) as i32;
            LTP_pred_Q14 = (LTP_pred_Q14 as i64
                + (*pred_lag_ptr.offset(-1 as isize) as i64 * *b_Q14.offset(1 as isize) as i64
                    >> 16)) as i32;
            LTP_pred_Q14 = (LTP_pred_Q14 as i64
                + (*pred_lag_ptr.offset(-(2) as isize) as i64 * *b_Q14.offset(2 as isize) as i64
                    >> 16)) as i32;
            LTP_pred_Q14 = (LTP_pred_Q14 as i64
                + (*pred_lag_ptr.offset(-(3) as isize) as i64 * *b_Q14.offset(3 as isize) as i64
                    >> 16)) as i32;
            LTP_pred_Q14 = (LTP_pred_Q14 as i64
                + (*pred_lag_ptr.offset(-(4) as isize) as i64 * *b_Q14.offset(4 as isize) as i64
                    >> 16)) as i32;
            LTP_pred_Q14 = ((LTP_pred_Q14 as u32) << 1) as i32;
            pred_lag_ptr = pred_lag_ptr.offset(1);
        } else {
            LTP_pred_Q14 = 0;
        }
        if lag > 0 {
            n_LTP_Q14 = ((*shp_lag_ptr.offset(0 as isize) + *shp_lag_ptr.offset(-(2) as isize))
                as i64
                * HarmShapeFIRPacked_Q14 as i16 as i64
                >> 16) as i32;
            n_LTP_Q14 = (n_LTP_Q14 as i64
                + (*shp_lag_ptr.offset(-1 as isize) as i64 * (HarmShapeFIRPacked_Q14 as i64 >> 16)
                    >> 16)) as i32;
            n_LTP_Q14 = LTP_pred_Q14 - ((n_LTP_Q14 as u32) << 2) as i32;
            shp_lag_ptr = shp_lag_ptr.offset(1);
        } else {
            n_LTP_Q14 = 0;
        }
        k = 0;
        while k < nStatesDelayedDecision {
            psDD = &mut *psDelDec.offset(k as isize) as *mut NSQ_del_dec_struct;
            psSS = (*psSampleState.as_mut_ptr().offset(k as isize)).as_mut_ptr();
            (*psDD).Seed =
                907633515_u32.wrapping_add(((*psDD).Seed as u32).wrapping_mul(196314165)) as i32;
            psLPC_Q14 = &mut *((*psDD).sLPC_Q14)
                .as_mut_ptr()
                .offset((NSQ_LPC_BUF_LENGTH - 1 + i) as isize) as *mut i32;
            LPC_pred_Q14 =
                silk_noise_shape_quantizer_short_prediction_c(psLPC_Q14, a_Q12, predictLPCOrder);
            LPC_pred_Q14 = ((LPC_pred_Q14 as u32) << 4) as i32;
            assert!(shapingLPCOrder & 1 == 0);
            tmp2 = ((*psDD).Diff_Q14 as i64
                + ((*psDD).sAR2_Q14[0 as usize] as i64 * warping_Q16 as i16 as i64 >> 16))
                as i32;
            tmp1 = ((*psDD).sAR2_Q14[0 as usize] as i64
                + (((*psDD).sAR2_Q14[1 as usize] - tmp2) as i64 * warping_Q16 as i16 as i64 >> 16))
                as i32;
            (*psDD).sAR2_Q14[0 as usize] = tmp2;
            n_AR_Q14 = shapingLPCOrder >> 1;
            n_AR_Q14 = (n_AR_Q14 as i64
                + (tmp2 as i64 * *AR_shp_Q13.offset(0 as isize) as i64 >> 16))
                as i32;
            j = 2;
            while j < shapingLPCOrder {
                tmp2 = ((*psDD).sAR2_Q14[(j - 1) as usize] as i64
                    + (((*psDD).sAR2_Q14[(j + 0) as usize] - tmp1) as i64
                        * warping_Q16 as i16 as i64
                        >> 16)) as i32;
                (*psDD).sAR2_Q14[(j - 1) as usize] = tmp1;
                n_AR_Q14 = (n_AR_Q14 as i64
                    + (tmp1 as i64 * *AR_shp_Q13.offset((j - 1) as isize) as i64 >> 16))
                    as i32;
                tmp1 = ((*psDD).sAR2_Q14[(j + 0) as usize] as i64
                    + (((*psDD).sAR2_Q14[(j + 1) as usize] - tmp2) as i64
                        * warping_Q16 as i16 as i64
                        >> 16)) as i32;
                (*psDD).sAR2_Q14[(j + 0) as usize] = tmp2;
                n_AR_Q14 = (n_AR_Q14 as i64
                    + (tmp2 as i64 * *AR_shp_Q13.offset(j as isize) as i64 >> 16))
                    as i32;
                j += 2;
            }
            (*psDD).sAR2_Q14[(shapingLPCOrder - 1) as usize] = tmp1;
            n_AR_Q14 = (n_AR_Q14 as i64
                + (tmp1 as i64 * *AR_shp_Q13.offset((shapingLPCOrder - 1) as isize) as i64 >> 16))
                as i32;
            n_AR_Q14 = ((n_AR_Q14 as u32) << 1) as i32;
            n_AR_Q14 = (n_AR_Q14 as i64 + ((*psDD).LF_AR_Q14 as i64 * Tilt_Q14 as i16 as i64 >> 16))
                as i32;
            n_AR_Q14 = ((n_AR_Q14 as u32) << 2) as i32;
            n_LF_Q14 = ((*psDD).Shape_Q14[*smpl_buf_idx as usize] as i64 * LF_shp_Q14 as i16 as i64
                >> 16) as i32;
            n_LF_Q14 = (n_LF_Q14 as i64
                + ((*psDD).LF_AR_Q14 as i64 * (LF_shp_Q14 as i64 >> 16) >> 16))
                as i32;
            n_LF_Q14 = ((n_LF_Q14 as u32) << 2) as i32;
            tmp1 = n_AR_Q14 + n_LF_Q14;
            tmp2 = n_LTP_Q14 + LPC_pred_Q14;
            tmp1 = tmp2 - tmp1;
            tmp1 = if 4 == 1 {
                (tmp1 >> 1) + (tmp1 & 1)
            } else {
                (tmp1 >> 4 - 1) + 1 >> 1
            };
            r_Q10 = *x_Q10.offset(i as isize) - tmp1;
            if (*psDD).Seed < 0 {
                r_Q10 = -r_Q10;
            }
            r_Q10 = if -((31) << 10) > (30) << 10 {
                if r_Q10 > -((31) << 10) {
                    -((31) << 10)
                } else if r_Q10 < (30) << 10 {
                    (30) << 10
                } else {
                    r_Q10
                }
            } else if r_Q10 > (30) << 10 {
                (30) << 10
            } else if r_Q10 < -((31) << 10) {
                -((31) << 10)
            } else {
                r_Q10
            };
            q1_Q10 = r_Q10 - offset_Q10;
            q1_Q0 = q1_Q10 >> 10;
            if Lambda_Q10 > 2048 {
                let rdo_offset: i32 = Lambda_Q10 / 2 - 512;
                if q1_Q10 > rdo_offset {
                    q1_Q0 = q1_Q10 - rdo_offset >> 10;
                } else if q1_Q10 < -rdo_offset {
                    q1_Q0 = q1_Q10 + rdo_offset >> 10;
                } else if q1_Q10 < 0 {
                    q1_Q0 = -1;
                } else {
                    q1_Q0 = 0;
                }
            }
            if q1_Q0 > 0 {
                q1_Q10 = ((q1_Q0 as u32) << 10) as i32 - 80;
                q1_Q10 = q1_Q10 + offset_Q10;
                q2_Q10 = q1_Q10 + 1024;
                rd1_Q10 = q1_Q10 as i16 as i32 * Lambda_Q10 as i16 as i32;
                rd2_Q10 = q2_Q10 as i16 as i32 * Lambda_Q10 as i16 as i32;
            } else if q1_Q0 == 0 {
                q1_Q10 = offset_Q10;
                q2_Q10 = q1_Q10 + (1024 - 80);
                rd1_Q10 = q1_Q10 as i16 as i32 * Lambda_Q10 as i16 as i32;
                rd2_Q10 = q2_Q10 as i16 as i32 * Lambda_Q10 as i16 as i32;
            } else if q1_Q0 == -1 {
                q2_Q10 = offset_Q10;
                q1_Q10 = q2_Q10 - (1024 - 80);
                rd1_Q10 = -q1_Q10 as i16 as i32 * Lambda_Q10 as i16 as i32;
                rd2_Q10 = q2_Q10 as i16 as i32 * Lambda_Q10 as i16 as i32;
            } else {
                q1_Q10 = ((q1_Q0 as u32) << 10) as i32 + 80;
                q1_Q10 = q1_Q10 + offset_Q10;
                q2_Q10 = q1_Q10 + 1024;
                rd1_Q10 = -q1_Q10 as i16 as i32 * Lambda_Q10 as i16 as i32;
                rd2_Q10 = -q2_Q10 as i16 as i32 * Lambda_Q10 as i16 as i32;
            }
            rr_Q10 = r_Q10 - q1_Q10;
            rd1_Q10 = rd1_Q10 + rr_Q10 as i16 as i32 * rr_Q10 as i16 as i32 >> 10;
            rr_Q10 = r_Q10 - q2_Q10;
            rd2_Q10 = rd2_Q10 + rr_Q10 as i16 as i32 * rr_Q10 as i16 as i32 >> 10;
            if rd1_Q10 < rd2_Q10 {
                (*psSS.offset(0 as isize)).RD_Q10 = (*psDD).RD_Q10 + rd1_Q10;
                (*psSS.offset(1 as isize)).RD_Q10 = (*psDD).RD_Q10 + rd2_Q10;
                (*psSS.offset(0 as isize)).Q_Q10 = q1_Q10;
                (*psSS.offset(1 as isize)).Q_Q10 = q2_Q10;
            } else {
                (*psSS.offset(0 as isize)).RD_Q10 = (*psDD).RD_Q10 + rd2_Q10;
                (*psSS.offset(1 as isize)).RD_Q10 = (*psDD).RD_Q10 + rd1_Q10;
                (*psSS.offset(0 as isize)).Q_Q10 = q2_Q10;
                (*psSS.offset(1 as isize)).Q_Q10 = q1_Q10;
            }
            exc_Q14 = (((*psSS.offset(0 as isize)).Q_Q10 as u32) << 4) as i32;
            if (*psDD).Seed < 0 {
                exc_Q14 = -exc_Q14;
            }
            LPC_exc_Q14 = exc_Q14 + LTP_pred_Q14;
            xq_Q14 = LPC_exc_Q14 + LPC_pred_Q14;
            (*psSS.offset(0 as isize)).Diff_Q14 =
                xq_Q14 - ((*x_Q10.offset(i as isize) as u32) << 4) as i32;
            sLF_AR_shp_Q14 = (*psSS.offset(0 as isize)).Diff_Q14 - n_AR_Q14;
            (*psSS.offset(0 as isize)).sLTP_shp_Q14 = sLF_AR_shp_Q14 - n_LF_Q14;
            (*psSS.offset(0 as isize)).LF_AR_Q14 = sLF_AR_shp_Q14;
            (*psSS.offset(0 as isize)).LPC_exc_Q14 = LPC_exc_Q14;
            (*psSS.offset(0 as isize)).xq_Q14 = xq_Q14;
            exc_Q14 = (((*psSS.offset(1 as isize)).Q_Q10 as u32) << 4) as i32;
            if (*psDD).Seed < 0 {
                exc_Q14 = -exc_Q14;
            }
            LPC_exc_Q14 = exc_Q14 + LTP_pred_Q14;
            xq_Q14 = LPC_exc_Q14 + LPC_pred_Q14;
            (*psSS.offset(1 as isize)).Diff_Q14 =
                xq_Q14 - ((*x_Q10.offset(i as isize) as u32) << 4) as i32;
            sLF_AR_shp_Q14 = (*psSS.offset(1 as isize)).Diff_Q14 - n_AR_Q14;
            (*psSS.offset(1 as isize)).sLTP_shp_Q14 = sLF_AR_shp_Q14 - n_LF_Q14;
            (*psSS.offset(1 as isize)).LF_AR_Q14 = sLF_AR_shp_Q14;
            (*psSS.offset(1 as isize)).LPC_exc_Q14 = LPC_exc_Q14;
            (*psSS.offset(1 as isize)).xq_Q14 = xq_Q14;
            k += 1;
        }
        *smpl_buf_idx = (*smpl_buf_idx - 1) % DECISION_DELAY;
        if *smpl_buf_idx < 0 {
            *smpl_buf_idx += DECISION_DELAY;
        }
        last_smple_idx = (*smpl_buf_idx + decisionDelay) % DECISION_DELAY;
        RDmin_Q10 = (*psSampleState.as_mut_ptr().offset(0 as isize))[0 as usize].RD_Q10;
        Winner_ind = 0;
        k = 1;
        while k < nStatesDelayedDecision {
            if (*psSampleState.as_mut_ptr().offset(k as isize))[0 as usize].RD_Q10 < RDmin_Q10 {
                RDmin_Q10 = (*psSampleState.as_mut_ptr().offset(k as isize))[0 as usize].RD_Q10;
                Winner_ind = k;
            }
            k += 1;
        }
        Winner_rand_state =
            (*psDelDec.offset(Winner_ind as isize)).RandState[last_smple_idx as usize];
        k = 0;
        while k < nStatesDelayedDecision {
            if (*psDelDec.offset(k as isize)).RandState[last_smple_idx as usize]
                != Winner_rand_state
            {
                (*psSampleState.as_mut_ptr().offset(k as isize))[0 as usize].RD_Q10 =
                    (*psSampleState.as_mut_ptr().offset(k as isize))[0 as usize].RD_Q10
                        + (0x7fffffff >> 4);
                (*psSampleState.as_mut_ptr().offset(k as isize))[1 as usize].RD_Q10 =
                    (*psSampleState.as_mut_ptr().offset(k as isize))[1 as usize].RD_Q10
                        + (0x7fffffff >> 4);
            }
            k += 1;
        }
        RDmax_Q10 = (*psSampleState.as_mut_ptr().offset(0 as isize))[0 as usize].RD_Q10;
        RDmin_Q10 = (*psSampleState.as_mut_ptr().offset(0 as isize))[1 as usize].RD_Q10;
        RDmax_ind = 0;
        RDmin_ind = 0;
        k = 1;
        while k < nStatesDelayedDecision {
            if (*psSampleState.as_mut_ptr().offset(k as isize))[0 as usize].RD_Q10 > RDmax_Q10 {
                RDmax_Q10 = (*psSampleState.as_mut_ptr().offset(k as isize))[0 as usize].RD_Q10;
                RDmax_ind = k;
            }
            if (*psSampleState.as_mut_ptr().offset(k as isize))[1 as usize].RD_Q10 < RDmin_Q10 {
                RDmin_Q10 = (*psSampleState.as_mut_ptr().offset(k as isize))[1 as usize].RD_Q10;
                RDmin_ind = k;
            }
            k += 1;
        }
        if RDmin_Q10 < RDmax_Q10 {
            memcpy(
                (&mut *psDelDec.offset(RDmax_ind as isize) as *mut NSQ_del_dec_struct as *mut i32)
                    .offset(i as isize) as *mut core::ffi::c_void,
                (&mut *psDelDec.offset(RDmin_ind as isize) as *mut NSQ_del_dec_struct as *mut i32)
                    .offset(i as isize) as *const core::ffi::c_void,
                (::core::mem::size_of::<NSQ_del_dec_struct>() as u64)
                    .wrapping_sub((i as u64).wrapping_mul(::core::mem::size_of::<i32>() as u64)),
            );
            memcpy(
                &mut *(*psSampleState.as_mut_ptr().offset(RDmax_ind as isize))
                    .as_mut_ptr()
                    .offset(0 as isize) as *mut NSQ_sample_struct
                    as *mut core::ffi::c_void,
                &mut *(*psSampleState.as_mut_ptr().offset(RDmin_ind as isize))
                    .as_mut_ptr()
                    .offset(1 as isize) as *mut NSQ_sample_struct
                    as *const core::ffi::c_void,
                ::core::mem::size_of::<NSQ_sample_struct>() as u64,
            );
        }
        psDD = &mut *psDelDec.offset(Winner_ind as isize) as *mut NSQ_del_dec_struct;
        if subfr > 0 || i >= decisionDelay {
            *pulses.offset((i - decisionDelay) as isize) = (if 10 == 1 {
                ((*psDD).Q_Q10[last_smple_idx as usize] >> 1)
                    + ((*psDD).Q_Q10[last_smple_idx as usize] & 1)
            } else {
                ((*psDD).Q_Q10[last_smple_idx as usize] >> 10 - 1) + 1 >> 1
            }) as i8;
            *xq.offset((i - decisionDelay) as isize) = (if (if 8 == 1 {
                (((*psDD).Xq_Q14[last_smple_idx as usize] as i64
                    * *delayedGain_Q10.offset(last_smple_idx as isize) as i64
                    >> 16) as i32
                    >> 1)
                    + (((*psDD).Xq_Q14[last_smple_idx as usize] as i64
                        * *delayedGain_Q10.offset(last_smple_idx as isize) as i64
                        >> 16) as i32
                        & 1)
            } else {
                (((*psDD).Xq_Q14[last_smple_idx as usize] as i64
                    * *delayedGain_Q10.offset(last_smple_idx as isize) as i64
                    >> 16) as i32
                    >> 8 - 1)
                    + 1
                    >> 1
            }) > silk_int16_MAX
            {
                silk_int16_MAX
            } else if (if 8 == 1 {
                (((*psDD).Xq_Q14[last_smple_idx as usize] as i64
                    * *delayedGain_Q10.offset(last_smple_idx as isize) as i64
                    >> 16) as i32
                    >> 1)
                    + (((*psDD).Xq_Q14[last_smple_idx as usize] as i64
                        * *delayedGain_Q10.offset(last_smple_idx as isize) as i64
                        >> 16) as i32
                        & 1)
            } else {
                (((*psDD).Xq_Q14[last_smple_idx as usize] as i64
                    * *delayedGain_Q10.offset(last_smple_idx as isize) as i64
                    >> 16) as i32
                    >> 8 - 1)
                    + 1
                    >> 1
            }) < silk_int16_MIN
            {
                silk_int16_MIN
            } else if 8 == 1 {
                (((*psDD).Xq_Q14[last_smple_idx as usize] as i64
                    * *delayedGain_Q10.offset(last_smple_idx as isize) as i64
                    >> 16) as i32
                    >> 1)
                    + (((*psDD).Xq_Q14[last_smple_idx as usize] as i64
                        * *delayedGain_Q10.offset(last_smple_idx as isize) as i64
                        >> 16) as i32
                        & 1)
            } else {
                (((*psDD).Xq_Q14[last_smple_idx as usize] as i64
                    * *delayedGain_Q10.offset(last_smple_idx as isize) as i64
                    >> 16) as i32
                    >> 8 - 1)
                    + 1
                    >> 1
            }) as i16;
            (*NSQ).sLTP_shp_Q14[((*NSQ).sLTP_shp_buf_idx - decisionDelay) as usize] =
                (*psDD).Shape_Q14[last_smple_idx as usize];
            *sLTP_Q15.offset(((*NSQ).sLTP_buf_idx - decisionDelay) as isize) =
                (*psDD).Pred_Q15[last_smple_idx as usize];
        }
        (*NSQ).sLTP_shp_buf_idx += 1;
        (*NSQ).sLTP_buf_idx += 1;
        k = 0;
        while k < nStatesDelayedDecision {
            psDD = &mut *psDelDec.offset(k as isize) as *mut NSQ_del_dec_struct;
            psSS = &mut *(*psSampleState.as_mut_ptr().offset(k as isize))
                .as_mut_ptr()
                .offset(0 as isize) as *mut NSQ_sample_struct;
            (*psDD).LF_AR_Q14 = (*psSS).LF_AR_Q14;
            (*psDD).Diff_Q14 = (*psSS).Diff_Q14;
            (*psDD).sLPC_Q14[(NSQ_LPC_BUF_LENGTH + i) as usize] = (*psSS).xq_Q14;
            (*psDD).Xq_Q14[*smpl_buf_idx as usize] = (*psSS).xq_Q14;
            (*psDD).Q_Q10[*smpl_buf_idx as usize] = (*psSS).Q_Q10;
            (*psDD).Pred_Q15[*smpl_buf_idx as usize] = (((*psSS).LPC_exc_Q14 as u32) << 1) as i32;
            (*psDD).Shape_Q14[*smpl_buf_idx as usize] = (*psSS).sLTP_shp_Q14;
            (*psDD).Seed = ((*psDD).Seed as u32).wrapping_add(
                (if 10 == 1 {
                    ((*psSS).Q_Q10 >> 1) + ((*psSS).Q_Q10 & 1)
                } else {
                    ((*psSS).Q_Q10 >> 10 - 1) + 1 >> 1
                }) as u32,
            ) as i32;
            (*psDD).RandState[*smpl_buf_idx as usize] = (*psDD).Seed;
            (*psDD).RD_Q10 = (*psSS).RD_Q10;
            k += 1;
        }
        *delayedGain_Q10.offset(*smpl_buf_idx as isize) = Gain_Q10;
        i += 1;
    }
    k = 0;
    while k < nStatesDelayedDecision {
        psDD = &mut *psDelDec.offset(k as isize) as *mut NSQ_del_dec_struct;
        memcpy(
            ((*psDD).sLPC_Q14).as_mut_ptr() as *mut core::ffi::c_void,
            &mut *((*psDD).sLPC_Q14).as_mut_ptr().offset(length as isize) as *mut i32
                as *const core::ffi::c_void,
            16_u64.wrapping_mul(::core::mem::size_of::<i32>() as u64),
        );
        k += 1;
    }
}
#[inline]
unsafe fn silk_nsq_del_dec_scale_states(
    psEncC: *const silk_encoder_state,
    NSQ: *mut silk_nsq_state,
    psDelDec: *mut NSQ_del_dec_struct,
    x16: *const i16,
    x_sc_Q10: *mut i32,
    sLTP: *const i16,
    sLTP_Q15: *mut i32,
    subfr: i32,
    nStatesDelayedDecision: i32,
    LTP_scale_Q14: i32,
    Gains_Q16: *const i32,
    pitchL: *const i32,
    signal_type: i32,
    decisionDelay: i32,
) {
    let mut i: i32 = 0;
    let mut k: i32 = 0;
    let mut lag: i32 = 0;
    let mut gain_adj_Q16: i32 = 0;
    let mut inv_gain_Q31: i32 = 0;
    let mut inv_gain_Q26: i32 = 0;
    let mut psDD: *mut NSQ_del_dec_struct = 0 as *mut NSQ_del_dec_struct;
    lag = *pitchL.offset(subfr as isize);
    inv_gain_Q31 = silk_INVERSE32_varQ(
        if *Gains_Q16.offset(subfr as isize) > 1 {
            *Gains_Q16.offset(subfr as isize)
        } else {
            1
        },
        47,
    );
    inv_gain_Q26 = if 5 == 1 {
        (inv_gain_Q31 >> 1) + (inv_gain_Q31 & 1)
    } else {
        (inv_gain_Q31 >> 5 - 1) + 1 >> 1
    };
    i = 0;
    while i < (*psEncC).subfr_length {
        *x_sc_Q10.offset(i as isize) =
            (*x16.offset(i as isize) as i64 * inv_gain_Q26 as i64 >> 16) as i32;
        i += 1;
    }
    if (*NSQ).rewhite_flag != 0 {
        if subfr == 0 {
            inv_gain_Q31 = (((inv_gain_Q31 as i64 * LTP_scale_Q14 as i16 as i64 >> 16) as i32
                as u32)
                << 2) as i32;
        }
        i = (*NSQ).sLTP_buf_idx - lag - LTP_ORDER / 2;
        while i < (*NSQ).sLTP_buf_idx {
            *sLTP_Q15.offset(i as isize) =
                (inv_gain_Q31 as i64 * *sLTP.offset(i as isize) as i64 >> 16) as i32;
            i += 1;
        }
    }
    if *Gains_Q16.offset(subfr as isize) != (*NSQ).prev_gain_Q16 {
        gain_adj_Q16 = silk_DIV32_varQ((*NSQ).prev_gain_Q16, *Gains_Q16.offset(subfr as isize), 16);
        i = (*NSQ).sLTP_shp_buf_idx - (*psEncC).ltp_mem_length;
        while i < (*NSQ).sLTP_shp_buf_idx {
            (*NSQ).sLTP_shp_Q14[i as usize] =
                (gain_adj_Q16 as i64 * (*NSQ).sLTP_shp_Q14[i as usize] as i64 >> 16) as i32;
            i += 1;
        }
        if signal_type == TYPE_VOICED && (*NSQ).rewhite_flag == 0 {
            i = (*NSQ).sLTP_buf_idx - lag - LTP_ORDER / 2;
            while i < (*NSQ).sLTP_buf_idx - decisionDelay {
                *sLTP_Q15.offset(i as isize) =
                    (gain_adj_Q16 as i64 * *sLTP_Q15.offset(i as isize) as i64 >> 16) as i32;
                i += 1;
            }
        }
        k = 0;
        while k < nStatesDelayedDecision {
            psDD = &mut *psDelDec.offset(k as isize) as *mut NSQ_del_dec_struct;
            (*psDD).LF_AR_Q14 = (gain_adj_Q16 as i64 * (*psDD).LF_AR_Q14 as i64 >> 16) as i32;
            (*psDD).Diff_Q14 = (gain_adj_Q16 as i64 * (*psDD).Diff_Q14 as i64 >> 16) as i32;
            i = 0;
            while i < NSQ_LPC_BUF_LENGTH {
                (*psDD).sLPC_Q14[i as usize] =
                    (gain_adj_Q16 as i64 * (*psDD).sLPC_Q14[i as usize] as i64 >> 16) as i32;
                i += 1;
            }
            i = 0;
            while i < MAX_SHAPE_LPC_ORDER {
                (*psDD).sAR2_Q14[i as usize] =
                    (gain_adj_Q16 as i64 * (*psDD).sAR2_Q14[i as usize] as i64 >> 16) as i32;
                i += 1;
            }
            i = 0;
            while i < DECISION_DELAY {
                (*psDD).Pred_Q15[i as usize] =
                    (gain_adj_Q16 as i64 * (*psDD).Pred_Q15[i as usize] as i64 >> 16) as i32;
                (*psDD).Shape_Q14[i as usize] =
                    (gain_adj_Q16 as i64 * (*psDD).Shape_Q14[i as usize] as i64 >> 16) as i32;
                i += 1;
            }
            k += 1;
        }
        (*NSQ).prev_gain_Q16 = *Gains_Q16.offset(subfr as isize);
    }
}
