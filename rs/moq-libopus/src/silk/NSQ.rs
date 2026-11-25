pub mod typedef_h {
    pub const silk_int16_MAX: i32 = i16::MAX as i32;
    pub const silk_int16_MIN: i32 = i16::MIN as i32;
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
    #[inline]
    pub unsafe fn silk_NSQ_noise_shape_feedback_loop_c(
        data0: *const i32,
        data1: *mut i32,
        coef: *const i16,
        order: i32,
    ) -> i32 {
        let mut out: i32 = 0;
        let mut tmp1: i32 = 0;
        let mut tmp2: i32 = 0;
        let mut j: i32 = 0;
        tmp2 = *data0.offset(0 as isize);
        tmp1 = *data1.offset(0 as isize);
        *data1.offset(0 as isize) = tmp2;
        out = order >> 1;
        out = (out as i64 + (tmp2 as i64 * *coef.offset(0 as isize) as i64 >> 16)) as i32;
        j = 2;
        while j < order {
            tmp2 = *data1.offset((j - 1) as isize);
            *data1.offset((j - 1) as isize) = tmp1;
            out = (out as i64 + (tmp1 as i64 * *coef.offset((j - 1) as isize) as i64 >> 16)) as i32;
            tmp1 = *data1.offset((j + 0) as isize);
            *data1.offset((j + 0) as isize) = tmp2;
            out = (out as i64 + (tmp2 as i64 * *coef.offset(j as isize) as i64 >> 16)) as i32;
            j += 2;
        }
        *data1.offset((order - 1) as isize) = tmp1;
        out = (out as i64 + (tmp1 as i64 * *coef.offset((order - 1) as isize) as i64 >> 16)) as i32;
        out = ((out as u32) << 1) as i32;
        return out;
    }
}

pub use self::typedef_h::{silk_int16_MAX, silk_int16_MIN};
pub use self::NSQ_h::{
    silk_NSQ_noise_shape_feedback_loop_c, silk_noise_shape_quantizer_short_prediction_c,
};
use crate::externs::{memcpy, memmove};
use crate::silk::define::{
    HARM_SHAPE_FIR_TAPS, LTP_ORDER, MAX_LPC_ORDER, MAX_SHAPE_LPC_ORDER, NSQ_LPC_BUF_LENGTH,
    TYPE_VOICED,
};
use crate::silk::structs::{silk_encoder_state, silk_nsq_state, SideInfoIndices};
use crate::silk::tables_other::silk_Quantization_Offsets_Q10;
use crate::silk::Inlines::{silk_DIV32_varQ, silk_INVERSE32_varQ};
use crate::silk::LPC_analysis_filter::silk_LPC_analysis_filter;

pub unsafe fn silk_NSQ_c(
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
    let mut k: i32 = 0;
    let mut lag: i32 = 0;
    let mut start_idx: i32 = 0;
    let mut LSF_interpolation_flag: i32 = 0;
    let mut A_Q12: *const i16 = 0 as *const i16;
    let mut B_Q14: *const i16 = 0 as *const i16;
    let mut AR_shp_Q13: *const i16 = 0 as *const i16;
    let mut pxq: *mut i16 = 0 as *mut i16;
    let mut HarmShapeFIRPacked_Q14: i32 = 0;
    let mut offset_Q10: i32 = 0;
    (*NSQ).rand_seed = (*psIndices).Seed as i32;
    lag = (*NSQ).lagPrev;
    offset_Q10 = silk_Quantization_Offsets_Q10[((*psIndices).signalType as i32 >> 1) as usize]
        [(*psIndices).quantOffsetType as usize] as i32;
    if (*psIndices).NLSFInterpCoef_Q2 as i32 == 4 {
        LSF_interpolation_flag = 0;
    } else {
        LSF_interpolation_flag = 1;
    }
    let vla = ((*psEncC).ltp_mem_length + (*psEncC).frame_length) as usize;
    let mut sLTP_Q15: Vec<i32> = ::std::vec::from_elem(0, vla);
    let vla_0 = ((*psEncC).ltp_mem_length + (*psEncC).frame_length) as usize;
    let mut sLTP: Vec<i16> = ::std::vec::from_elem(0, vla_0);
    let vla_1 = (*psEncC).subfr_length as usize;
    let mut x_sc_Q10: Vec<i32> = ::std::vec::from_elem(0, vla_1);
    (*NSQ).sLTP_shp_buf_idx = (*psEncC).ltp_mem_length;
    (*NSQ).sLTP_buf_idx = (*psEncC).ltp_mem_length;
    pxq = &mut *((*NSQ).xq)
        .as_mut_ptr()
        .offset((*psEncC).ltp_mem_length as isize) as *mut i16;
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
                (*NSQ).rewhite_flag = 1;
                (*NSQ).sLTP_buf_idx = (*psEncC).ltp_mem_length;
            }
        }
        silk_nsq_scale_states(
            psEncC,
            NSQ,
            x16,
            x_sc_Q10.as_mut_ptr(),
            sLTP.as_mut_ptr() as *const i16,
            sLTP_Q15.as_mut_ptr(),
            k,
            LTP_scale_Q14,
            Gains_Q16,
            pitchL,
            (*psIndices).signalType as i32,
        );
        silk_noise_shape_quantizer(
            NSQ,
            (*psIndices).signalType as i32,
            x_sc_Q10.as_mut_ptr() as *const i32,
            pulses,
            pxq,
            sLTP_Q15.as_mut_ptr(),
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
            (*psEncC).shapingLPCOrder,
            (*psEncC).predictLPCOrder,
            (*psEncC).arch,
        );
        x16 = x16.offset((*psEncC).subfr_length as isize);
        pulses = pulses.offset((*psEncC).subfr_length as isize);
        pxq = pxq.offset((*psEncC).subfr_length as isize);
        k += 1;
    }
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
unsafe fn silk_noise_shape_quantizer(
    NSQ: *mut silk_nsq_state,
    signalType: i32,
    x_sc_Q10: *const i32,
    pulses: *mut i8,
    xq: *mut i16,
    sLTP_Q15: *mut i32,
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
    shapingLPCOrder: i32,
    predictLPCOrder: i32,
    _arch: i32,
) {
    let mut i: i32 = 0;
    let mut LTP_pred_Q13: i32 = 0;
    let mut LPC_pred_Q10: i32 = 0;
    let mut n_AR_Q12: i32 = 0;
    let mut n_LTP_Q13: i32 = 0;
    let mut n_LF_Q12: i32 = 0;
    let mut r_Q10: i32 = 0;
    let mut rr_Q10: i32 = 0;
    let mut q1_Q0: i32 = 0;
    let mut q1_Q10: i32 = 0;
    let mut q2_Q10: i32 = 0;
    let mut rd1_Q20: i32 = 0;
    let mut rd2_Q20: i32 = 0;
    let mut exc_Q14: i32 = 0;
    let mut LPC_exc_Q14: i32 = 0;
    let mut xq_Q14: i32 = 0;
    let mut Gain_Q10: i32 = 0;
    let mut tmp1: i32 = 0;
    let mut tmp2: i32 = 0;
    let mut sLF_AR_shp_Q14: i32 = 0;
    let mut psLPC_Q14: *mut i32 = 0 as *mut i32;
    let mut shp_lag_ptr: *mut i32 = 0 as *mut i32;
    let mut pred_lag_ptr: *mut i32 = 0 as *mut i32;
    shp_lag_ptr = &mut *((*NSQ).sLTP_shp_Q14)
        .as_mut_ptr()
        .offset(((*NSQ).sLTP_shp_buf_idx - lag + HARM_SHAPE_FIR_TAPS / 2) as isize)
        as *mut i32;
    pred_lag_ptr =
        &mut *sLTP_Q15.offset(((*NSQ).sLTP_buf_idx - lag + LTP_ORDER / 2) as isize) as *mut i32;
    Gain_Q10 = Gain_Q16 >> 6;
    psLPC_Q14 = &mut *((*NSQ).sLPC_Q14)
        .as_mut_ptr()
        .offset((NSQ_LPC_BUF_LENGTH - 1) as isize) as *mut i32;
    i = 0;
    while i < length {
        (*NSQ).rand_seed =
            907633515_u32.wrapping_add(((*NSQ).rand_seed as u32).wrapping_mul(196314165)) as i32;
        LPC_pred_Q10 =
            silk_noise_shape_quantizer_short_prediction_c(psLPC_Q14, a_Q12, predictLPCOrder);
        if signalType == TYPE_VOICED {
            LTP_pred_Q13 = 2;
            LTP_pred_Q13 = (LTP_pred_Q13 as i64
                + (*pred_lag_ptr.offset(0 as isize) as i64 * *b_Q14.offset(0 as isize) as i64
                    >> 16)) as i32;
            LTP_pred_Q13 = (LTP_pred_Q13 as i64
                + (*pred_lag_ptr.offset(-1 as isize) as i64 * *b_Q14.offset(1 as isize) as i64
                    >> 16)) as i32;
            LTP_pred_Q13 = (LTP_pred_Q13 as i64
                + (*pred_lag_ptr.offset(-(2) as isize) as i64 * *b_Q14.offset(2 as isize) as i64
                    >> 16)) as i32;
            LTP_pred_Q13 = (LTP_pred_Q13 as i64
                + (*pred_lag_ptr.offset(-(3) as isize) as i64 * *b_Q14.offset(3 as isize) as i64
                    >> 16)) as i32;
            LTP_pred_Q13 = (LTP_pred_Q13 as i64
                + (*pred_lag_ptr.offset(-(4) as isize) as i64 * *b_Q14.offset(4 as isize) as i64
                    >> 16)) as i32;
            pred_lag_ptr = pred_lag_ptr.offset(1);
        } else {
            LTP_pred_Q13 = 0;
        }
        assert!(shapingLPCOrder & 1 == 0);
        n_AR_Q12 = silk_NSQ_noise_shape_feedback_loop_c(
            &mut (*NSQ).sDiff_shp_Q14,
            ((*NSQ).sAR2_Q14).as_mut_ptr(),
            AR_shp_Q13,
            shapingLPCOrder,
        );
        n_AR_Q12 = (n_AR_Q12 as i64 + ((*NSQ).sLF_AR_shp_Q14 as i64 * Tilt_Q14 as i16 as i64 >> 16))
            as i32;
        n_LF_Q12 = ((*NSQ).sLTP_shp_Q14[((*NSQ).sLTP_shp_buf_idx - 1) as usize] as i64
            * LF_shp_Q14 as i16 as i64
            >> 16) as i32;
        n_LF_Q12 = (n_LF_Q12 as i64
            + ((*NSQ).sLF_AR_shp_Q14 as i64 * (LF_shp_Q14 as i64 >> 16) >> 16))
            as i32;
        assert!(lag > 0 || signalType != 2);
        tmp1 = ((LPC_pred_Q10 as u32) << 2) as i32 - n_AR_Q12;
        tmp1 = tmp1 - n_LF_Q12;
        if lag > 0 {
            n_LTP_Q13 = ((*shp_lag_ptr.offset(0 as isize) + *shp_lag_ptr.offset(-(2) as isize))
                as i64
                * HarmShapeFIRPacked_Q14 as i16 as i64
                >> 16) as i32;
            n_LTP_Q13 = (n_LTP_Q13 as i64
                + (*shp_lag_ptr.offset(-1 as isize) as i64 * (HarmShapeFIRPacked_Q14 as i64 >> 16)
                    >> 16)) as i32;
            n_LTP_Q13 = ((n_LTP_Q13 as u32) << 1) as i32;
            shp_lag_ptr = shp_lag_ptr.offset(1);
            tmp2 = LTP_pred_Q13 - n_LTP_Q13;
            tmp1 = tmp2 + ((tmp1 as u32) << 1) as i32;
            tmp1 = if 3 == 1 {
                (tmp1 >> 1) + (tmp1 & 1)
            } else {
                (tmp1 >> 3 - 1) + 1 >> 1
            };
        } else {
            tmp1 = if 2 == 1 {
                (tmp1 >> 1) + (tmp1 & 1)
            } else {
                (tmp1 >> 2 - 1) + 1 >> 1
            };
        }
        r_Q10 = *x_sc_Q10.offset(i as isize) - tmp1;
        if (*NSQ).rand_seed < 0 {
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
            rd1_Q20 = q1_Q10 as i16 as i32 * Lambda_Q10 as i16 as i32;
            rd2_Q20 = q2_Q10 as i16 as i32 * Lambda_Q10 as i16 as i32;
        } else if q1_Q0 == 0 {
            q1_Q10 = offset_Q10;
            q2_Q10 = q1_Q10 + (1024 - 80);
            rd1_Q20 = q1_Q10 as i16 as i32 * Lambda_Q10 as i16 as i32;
            rd2_Q20 = q2_Q10 as i16 as i32 * Lambda_Q10 as i16 as i32;
        } else if q1_Q0 == -1 {
            q2_Q10 = offset_Q10;
            q1_Q10 = q2_Q10 - (1024 - 80);
            rd1_Q20 = -q1_Q10 as i16 as i32 * Lambda_Q10 as i16 as i32;
            rd2_Q20 = q2_Q10 as i16 as i32 * Lambda_Q10 as i16 as i32;
        } else {
            q1_Q10 = ((q1_Q0 as u32) << 10) as i32 + 80;
            q1_Q10 = q1_Q10 + offset_Q10;
            q2_Q10 = q1_Q10 + 1024;
            rd1_Q20 = -q1_Q10 as i16 as i32 * Lambda_Q10 as i16 as i32;
            rd2_Q20 = -q2_Q10 as i16 as i32 * Lambda_Q10 as i16 as i32;
        }
        rr_Q10 = r_Q10 - q1_Q10;
        rd1_Q20 = rd1_Q20 + rr_Q10 as i16 as i32 * rr_Q10 as i16 as i32;
        rr_Q10 = r_Q10 - q2_Q10;
        rd2_Q20 = rd2_Q20 + rr_Q10 as i16 as i32 * rr_Q10 as i16 as i32;
        if rd2_Q20 < rd1_Q20 {
            q1_Q10 = q2_Q10;
        }
        *pulses.offset(i as isize) = (if 10 == 1 {
            (q1_Q10 >> 1) + (q1_Q10 & 1)
        } else {
            (q1_Q10 >> 10 - 1) + 1 >> 1
        }) as i8;
        exc_Q14 = ((q1_Q10 as u32) << 4) as i32;
        if (*NSQ).rand_seed < 0 {
            exc_Q14 = -exc_Q14;
        }
        LPC_exc_Q14 = exc_Q14 + ((LTP_pred_Q13 as u32) << 1) as i32;
        xq_Q14 = LPC_exc_Q14 + ((LPC_pred_Q10 as u32) << 4) as i32;
        *xq.offset(i as isize) = (if (if 8 == 1 {
            ((xq_Q14 as i64 * Gain_Q10 as i64 >> 16) as i32 >> 1)
                + ((xq_Q14 as i64 * Gain_Q10 as i64 >> 16) as i32 & 1)
        } else {
            ((xq_Q14 as i64 * Gain_Q10 as i64 >> 16) as i32 >> 8 - 1) + 1 >> 1
        }) > silk_int16_MAX
        {
            silk_int16_MAX
        } else if (if 8 == 1 {
            ((xq_Q14 as i64 * Gain_Q10 as i64 >> 16) as i32 >> 1)
                + ((xq_Q14 as i64 * Gain_Q10 as i64 >> 16) as i32 & 1)
        } else {
            ((xq_Q14 as i64 * Gain_Q10 as i64 >> 16) as i32 >> 8 - 1) + 1 >> 1
        }) < silk_int16_MIN
        {
            silk_int16_MIN
        } else if 8 == 1 {
            ((xq_Q14 as i64 * Gain_Q10 as i64 >> 16) as i32 >> 1)
                + ((xq_Q14 as i64 * Gain_Q10 as i64 >> 16) as i32 & 1)
        } else {
            ((xq_Q14 as i64 * Gain_Q10 as i64 >> 16) as i32 >> 8 - 1) + 1 >> 1
        }) as i16;
        psLPC_Q14 = psLPC_Q14.offset(1);
        *psLPC_Q14 = xq_Q14;
        (*NSQ).sDiff_shp_Q14 = xq_Q14 - ((*x_sc_Q10.offset(i as isize) as u32) << 4) as i32;
        sLF_AR_shp_Q14 = (*NSQ).sDiff_shp_Q14 - ((n_AR_Q12 as u32) << 2) as i32;
        (*NSQ).sLF_AR_shp_Q14 = sLF_AR_shp_Q14;
        (*NSQ).sLTP_shp_Q14[(*NSQ).sLTP_shp_buf_idx as usize] =
            sLF_AR_shp_Q14 - ((n_LF_Q12 as u32) << 2) as i32;
        *sLTP_Q15.offset((*NSQ).sLTP_buf_idx as isize) = ((LPC_exc_Q14 as u32) << 1) as i32;
        (*NSQ).sLTP_shp_buf_idx += 1;
        (*NSQ).sLTP_buf_idx += 1;
        (*NSQ).rand_seed =
            ((*NSQ).rand_seed as u32).wrapping_add(*pulses.offset(i as isize) as u32) as i32;
        i += 1;
    }
    memcpy(
        ((*NSQ).sLPC_Q14).as_mut_ptr() as *mut core::ffi::c_void,
        &mut *((*NSQ).sLPC_Q14).as_mut_ptr().offset(length as isize) as *mut i32
            as *const core::ffi::c_void,
        16_u64.wrapping_mul(::core::mem::size_of::<i32>() as u64),
    );
}
#[inline]
unsafe fn silk_nsq_scale_states(
    psEncC: *const silk_encoder_state,
    NSQ: *mut silk_nsq_state,
    x16: *const i16,
    x_sc_Q10: *mut i32,
    sLTP: *const i16,
    sLTP_Q15: *mut i32,
    subfr: i32,
    LTP_scale_Q14: i32,
    Gains_Q16: *const i32,
    pitchL: *const i32,
    signal_type: i32,
) {
    let mut i: i32 = 0;
    let mut lag: i32 = 0;
    let mut gain_adj_Q16: i32 = 0;
    let mut inv_gain_Q31: i32 = 0;
    let mut inv_gain_Q26: i32 = 0;
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
            while i < (*NSQ).sLTP_buf_idx {
                *sLTP_Q15.offset(i as isize) =
                    (gain_adj_Q16 as i64 * *sLTP_Q15.offset(i as isize) as i64 >> 16) as i32;
                i += 1;
            }
        }
        (*NSQ).sLF_AR_shp_Q14 = (gain_adj_Q16 as i64 * (*NSQ).sLF_AR_shp_Q14 as i64 >> 16) as i32;
        (*NSQ).sDiff_shp_Q14 = (gain_adj_Q16 as i64 * (*NSQ).sDiff_shp_Q14 as i64 >> 16) as i32;
        i = 0;
        while i < NSQ_LPC_BUF_LENGTH {
            (*NSQ).sLPC_Q14[i as usize] =
                (gain_adj_Q16 as i64 * (*NSQ).sLPC_Q14[i as usize] as i64 >> 16) as i32;
            i += 1;
        }
        i = 0;
        while i < MAX_SHAPE_LPC_ORDER {
            (*NSQ).sAR2_Q14[i as usize] =
                (gain_adj_Q16 as i64 * (*NSQ).sAR2_Q14[i as usize] as i64 >> 16) as i32;
            i += 1;
        }
        (*NSQ).prev_gain_Q16 = *Gains_Q16.offset(subfr as isize);
    }
}
