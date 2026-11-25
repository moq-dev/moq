pub mod typedef_h {
    pub const silk_int16_MIN: i32 = i16::MIN as i32;
    pub const silk_int16_MAX: i32 = i16::MAX as i32;
}
pub use self::typedef_h::{silk_int16_MAX, silk_int16_MIN};
use crate::externs::memcpy;
use crate::silk::define::{LA_SHAPE_MS, STEREO_INTERP_LEN_MS};
use crate::silk::stereo_find_predictor::silk_stereo_find_predictor;
use crate::silk::stereo_quant_pred::silk_stereo_quant_pred;
use crate::silk::structs::stereo_enc_state;
use crate::silk::Inlines::silk_DIV32_varQ;
use crate::silk::SigProc_FIX::silk_max_int;

pub unsafe fn silk_stereo_LR_to_MS(
    state: *mut stereo_enc_state,
    x1: *mut i16,
    x2: *mut i16,
    ix: *mut [i8; 3],
    mid_only_flag: *mut i8,
    mid_side_rates_bps: *mut i32,
    mut total_rate_bps: i32,
    prev_speech_act_Q8: i32,
    toMono: i32,
    fs_kHz: i32,
    frame_length: i32,
) {
    let mut n: i32 = 0;
    let mut is10msFrame: i32 = 0;
    let mut denom_Q16: i32 = 0;
    let mut delta0_Q13: i32 = 0;
    let mut delta1_Q13: i32 = 0;
    let mut sum: i32 = 0;
    let mut diff: i32 = 0;
    let mut smooth_coef_Q16: i32 = 0;
    let mut pred_Q13: [i32; 2] = [0; 2];
    let mut pred0_Q13: i32 = 0;
    let mut pred1_Q13: i32 = 0;
    let mut LP_ratio_Q14: i32 = 0;
    let mut HP_ratio_Q14: i32 = 0;
    let mut frac_Q16: i32 = 0;
    let mut frac_3_Q16: i32 = 0;
    let mut min_mid_rate_bps: i32 = 0;
    let mut width_Q14: i32 = 0;
    let mut w_Q24: i32 = 0;
    let mut deltaw_Q24: i32 = 0;
    let mid: *mut i16 = &mut *x1.offset(-(2) as isize) as *mut i16;
    let vla = (frame_length + 2) as usize;
    let mut side: Vec<i16> = ::std::vec::from_elem(0, vla);
    n = 0;
    while n < frame_length + 2 {
        sum = *x1.offset((n - 2) as isize) as i32 + *x2.offset((n - 2) as isize) as i32;
        diff = *x1.offset((n - 2) as isize) as i32 - *x2.offset((n - 2) as isize) as i32;
        *mid.offset(n as isize) = (if 1 == 1 {
            (sum >> 1) + (sum & 1)
        } else {
            (sum >> 1 - 1) + 1 >> 1
        }) as i16;
        *side.as_mut_ptr().offset(n as isize) = (if (if 1 == 1 {
            (diff >> 1) + (diff & 1)
        } else {
            (diff >> 1 - 1) + 1 >> 1
        }) > silk_int16_MAX
        {
            silk_int16_MAX
        } else if (if 1 == 1 {
            (diff >> 1) + (diff & 1)
        } else {
            (diff >> 1 - 1) + 1 >> 1
        }) < silk_int16_MIN
        {
            silk_int16_MIN
        } else if 1 == 1 {
            (diff >> 1) + (diff & 1)
        } else {
            (diff >> 1 - 1) + 1 >> 1
        }) as i16;
        n += 1;
    }
    memcpy(
        mid as *mut core::ffi::c_void,
        ((*state).sMid).as_mut_ptr() as *const core::ffi::c_void,
        2_u64.wrapping_mul(::core::mem::size_of::<i16>() as u64),
    );
    memcpy(
        side.as_mut_ptr() as *mut core::ffi::c_void,
        ((*state).sSide).as_mut_ptr() as *const core::ffi::c_void,
        2_u64.wrapping_mul(::core::mem::size_of::<i16>() as u64),
    );
    memcpy(
        ((*state).sMid).as_mut_ptr() as *mut core::ffi::c_void,
        &mut *mid.offset(frame_length as isize) as *mut i16 as *const core::ffi::c_void,
        2_u64.wrapping_mul(::core::mem::size_of::<i16>() as u64),
    );
    memcpy(
        ((*state).sSide).as_mut_ptr() as *mut core::ffi::c_void,
        &mut *side.as_mut_ptr().offset(frame_length as isize) as *mut i16
            as *const core::ffi::c_void,
        2_u64.wrapping_mul(::core::mem::size_of::<i16>() as u64),
    );
    let vla_0 = frame_length as usize;
    let mut LP_mid: Vec<i16> = ::std::vec::from_elem(0, vla_0);
    let vla_1 = frame_length as usize;
    let mut HP_mid: Vec<i16> = ::std::vec::from_elem(0, vla_1);
    n = 0;
    while n < frame_length {
        sum = if 2 == 1 {
            (*mid.offset(n as isize) as i32
                + *mid.offset((n + 2) as isize) as i32
                + ((*mid.offset((n + 1) as isize) as u32) << 1) as i32
                >> 1)
                + (*mid.offset(n as isize) as i32
                    + *mid.offset((n + 2) as isize) as i32
                    + ((*mid.offset((n + 1) as isize) as u32) << 1) as i32
                    & 1)
        } else {
            (*mid.offset(n as isize) as i32
                + *mid.offset((n + 2) as isize) as i32
                + ((*mid.offset((n + 1) as isize) as u32) << 1) as i32
                >> 2 - 1)
                + 1
                >> 1
        };
        *LP_mid.as_mut_ptr().offset(n as isize) = sum as i16;
        *HP_mid.as_mut_ptr().offset(n as isize) =
            (*mid.offset((n + 1) as isize) as i32 - sum) as i16;
        n += 1;
    }
    let vla_2 = frame_length as usize;
    let mut LP_side: Vec<i16> = ::std::vec::from_elem(0, vla_2);
    let vla_3 = frame_length as usize;
    let mut HP_side: Vec<i16> = ::std::vec::from_elem(0, vla_3);
    n = 0;
    while n < frame_length {
        sum = if 2 == 1 {
            (*side.as_mut_ptr().offset(n as isize) as i32
                + *side.as_mut_ptr().offset((n + 2) as isize) as i32
                + ((*side.as_mut_ptr().offset((n + 1) as isize) as u32) << 1) as i32
                >> 1)
                + (*side.as_mut_ptr().offset(n as isize) as i32
                    + *side.as_mut_ptr().offset((n + 2) as isize) as i32
                    + ((*side.as_mut_ptr().offset((n + 1) as isize) as u32) << 1) as i32
                    & 1)
        } else {
            (*side.as_mut_ptr().offset(n as isize) as i32
                + *side.as_mut_ptr().offset((n + 2) as isize) as i32
                + ((*side.as_mut_ptr().offset((n + 1) as isize) as u32) << 1) as i32
                >> 2 - 1)
                + 1
                >> 1
        };
        *LP_side.as_mut_ptr().offset(n as isize) = sum as i16;
        *HP_side.as_mut_ptr().offset(n as isize) =
            (*side.as_mut_ptr().offset((n + 1) as isize) as i32 - sum) as i16;
        n += 1;
    }
    is10msFrame = (frame_length == 10 * fs_kHz) as i32;
    smooth_coef_Q16 = if is10msFrame != 0 {
        (0.01f64 / 2 as f64 * ((1) << 16) as f64 + 0.5f64) as i32
    } else {
        (0.01f64 * ((1) << 16) as f64 + 0.5f64) as i32
    };
    smooth_coef_Q16 = ((prev_speech_act_Q8 as i16 as i32 * prev_speech_act_Q8 as i16 as i32) as i64
        * smooth_coef_Q16 as i16 as i64
        >> 16) as i32;
    pred_Q13[0 as usize] = silk_stereo_find_predictor(
        &mut LP_ratio_Q14,
        LP_mid.as_mut_ptr() as *const i16,
        LP_side.as_mut_ptr() as *const i16,
        &mut *((*state).mid_side_amp_Q0).as_mut_ptr().offset(0 as isize),
        frame_length,
        smooth_coef_Q16,
    );
    pred_Q13[1 as usize] = silk_stereo_find_predictor(
        &mut HP_ratio_Q14,
        HP_mid.as_mut_ptr() as *const i16,
        HP_side.as_mut_ptr() as *const i16,
        &mut *((*state).mid_side_amp_Q0).as_mut_ptr().offset(2 as isize),
        frame_length,
        smooth_coef_Q16,
    );
    frac_Q16 = HP_ratio_Q14 + LP_ratio_Q14 as i16 as i32 * 3;
    frac_Q16 = if frac_Q16 < ((1 * ((1) << 16)) as f64 + 0.5f64) as i32 {
        frac_Q16
    } else {
        ((1 * ((1) << 16)) as f64 + 0.5f64) as i32
    };
    total_rate_bps -= if is10msFrame != 0 { 1200 } else { 600 };
    if total_rate_bps < 1 {
        total_rate_bps = 1;
    }
    min_mid_rate_bps = 2000 + fs_kHz as i16 as i32 * 600;
    frac_3_Q16 = 3 * frac_Q16;
    *mid_side_rates_bps.offset(0 as isize) = silk_DIV32_varQ(
        total_rate_bps,
        (((8 + 5) as i64 * ((1) << 16)) as f64 + 0.5f64) as i32 + frac_3_Q16,
        16 + 3,
    );
    if *mid_side_rates_bps.offset(0 as isize) < min_mid_rate_bps {
        *mid_side_rates_bps.offset(0 as isize) = min_mid_rate_bps;
        *mid_side_rates_bps.offset(1 as isize) =
            total_rate_bps - *mid_side_rates_bps.offset(0 as isize);
        width_Q14 = silk_DIV32_varQ(
            ((*mid_side_rates_bps.offset(1 as isize) as u32) << 1) as i32 - min_mid_rate_bps,
            ((((1 * ((1) << 16)) as f64 + 0.5f64) as i32 + frac_3_Q16) as i64
                * min_mid_rate_bps as i16 as i64
                >> 16) as i32,
            14 + 2,
        );
        width_Q14 = if 0 > ((1 * ((1) << 14)) as f64 + 0.5f64) as i32 {
            if width_Q14 > 0 {
                0
            } else if width_Q14 < ((1 * ((1) << 14)) as f64 + 0.5f64) as i32 {
                ((1 * ((1) << 14)) as f64 + 0.5f64) as i32
            } else {
                width_Q14
            }
        } else if width_Q14 > ((1 * ((1) << 14)) as f64 + 0.5f64) as i32 {
            ((1 * ((1) << 14)) as f64 + 0.5f64) as i32
        } else if width_Q14 < 0 {
            0
        } else {
            width_Q14
        };
    } else {
        *mid_side_rates_bps.offset(1 as isize) =
            total_rate_bps - *mid_side_rates_bps.offset(0 as isize);
        width_Q14 = ((1 * ((1) << 14)) as f64 + 0.5f64) as i32;
    }
    (*state).smth_width_Q14 = ((*state).smth_width_Q14 as i64
        + ((width_Q14 - (*state).smth_width_Q14 as i32) as i64 * smooth_coef_Q16 as i16 as i64
            >> 16)) as i32 as i16;
    *mid_only_flag = 0;
    if toMono != 0 {
        width_Q14 = 0;
        pred_Q13[0 as usize] = 0;
        pred_Q13[1 as usize] = 0;
        silk_stereo_quant_pred(pred_Q13.as_mut_ptr(), ix);
    } else if (*state).width_prev_Q14 as i32 == 0
        && (8 * total_rate_bps < 13 * min_mid_rate_bps
            || ((frac_Q16 as i64 * (*state).smth_width_Q14 as i64 >> 16) as i32)
                < (0.05f64 * ((1) << 14) as f64 + 0.5f64) as i32)
    {
        pred_Q13[0 as usize] =
            (*state).smth_width_Q14 as i32 * pred_Q13[0 as usize] as i16 as i32 >> 14;
        pred_Q13[1 as usize] =
            (*state).smth_width_Q14 as i32 * pred_Q13[1 as usize] as i16 as i32 >> 14;
        silk_stereo_quant_pred(pred_Q13.as_mut_ptr(), ix);
        width_Q14 = 0;
        pred_Q13[0 as usize] = 0;
        pred_Q13[1 as usize] = 0;
        *mid_side_rates_bps.offset(0 as isize) = total_rate_bps;
        *mid_side_rates_bps.offset(1 as isize) = 0;
        *mid_only_flag = 1;
    } else if (*state).width_prev_Q14 as i32 != 0
        && (8 * total_rate_bps < 11 * min_mid_rate_bps
            || ((frac_Q16 as i64 * (*state).smth_width_Q14 as i64 >> 16) as i32)
                < (0.02f64 * ((1) << 14) as f64 + 0.5f64) as i32)
    {
        pred_Q13[0 as usize] =
            (*state).smth_width_Q14 as i32 * pred_Q13[0 as usize] as i16 as i32 >> 14;
        pred_Q13[1 as usize] =
            (*state).smth_width_Q14 as i32 * pred_Q13[1 as usize] as i16 as i32 >> 14;
        silk_stereo_quant_pred(pred_Q13.as_mut_ptr(), ix);
        width_Q14 = 0;
        pred_Q13[0 as usize] = 0;
        pred_Q13[1 as usize] = 0;
    } else if (*state).smth_width_Q14 as i32 > (0.95f64 * ((1) << 14) as f64 + 0.5f64) as i32 {
        silk_stereo_quant_pred(pred_Q13.as_mut_ptr(), ix);
        width_Q14 = ((1 * ((1) << 14)) as f64 + 0.5f64) as i32;
    } else {
        pred_Q13[0 as usize] =
            (*state).smth_width_Q14 as i32 * pred_Q13[0 as usize] as i16 as i32 >> 14;
        pred_Q13[1 as usize] =
            (*state).smth_width_Q14 as i32 * pred_Q13[1 as usize] as i16 as i32 >> 14;
        silk_stereo_quant_pred(pred_Q13.as_mut_ptr(), ix);
        width_Q14 = (*state).smth_width_Q14 as i32;
    }
    if *mid_only_flag as i32 == 1 {
        (*state).silent_side_len = ((*state).silent_side_len as i32
            + (frame_length - STEREO_INTERP_LEN_MS * fs_kHz))
            as i16;
        if ((*state).silent_side_len as i32) < LA_SHAPE_MS * fs_kHz {
            *mid_only_flag = 0;
        } else {
            (*state).silent_side_len = 10000;
        }
    } else {
        (*state).silent_side_len = 0;
    }
    if *mid_only_flag as i32 == 0 && *mid_side_rates_bps.offset(1 as isize) < 1 {
        *mid_side_rates_bps.offset(1 as isize) = 1;
        *mid_side_rates_bps.offset(0 as isize) =
            silk_max_int(1, total_rate_bps - *mid_side_rates_bps.offset(1 as isize));
    }
    pred0_Q13 = -((*state).pred_prev_Q13[0 as usize] as i32);
    pred1_Q13 = -((*state).pred_prev_Q13[1 as usize] as i32);
    w_Q24 = (((*state).width_prev_Q14 as u32) << 10) as i32;
    denom_Q16 = ((1) << 16) / (8 * fs_kHz);
    delta0_Q13 = -if 16 == 1 {
        ((pred_Q13[0 as usize] - (*state).pred_prev_Q13[0 as usize] as i32) as i16 as i32
            * denom_Q16 as i16 as i32
            >> 1)
            + ((pred_Q13[0 as usize] - (*state).pred_prev_Q13[0 as usize] as i32) as i16 as i32
                * denom_Q16 as i16 as i32
                & 1)
    } else {
        ((pred_Q13[0 as usize] - (*state).pred_prev_Q13[0 as usize] as i32) as i16 as i32
            * denom_Q16 as i16 as i32
            >> 16 - 1)
            + 1
            >> 1
    };
    delta1_Q13 = -if 16 == 1 {
        ((pred_Q13[1 as usize] - (*state).pred_prev_Q13[1 as usize] as i32) as i16 as i32
            * denom_Q16 as i16 as i32
            >> 1)
            + ((pred_Q13[1 as usize] - (*state).pred_prev_Q13[1 as usize] as i32) as i16 as i32
                * denom_Q16 as i16 as i32
                & 1)
    } else {
        ((pred_Q13[1 as usize] - (*state).pred_prev_Q13[1 as usize] as i32) as i16 as i32
            * denom_Q16 as i16 as i32
            >> 16 - 1)
            + 1
            >> 1
    };
    deltaw_Q24 = ((((width_Q14 - (*state).width_prev_Q14 as i32) as i64 * denom_Q16 as i16 as i64
        >> 16) as i32 as u32)
        << 10) as i32;
    n = 0;
    while n < STEREO_INTERP_LEN_MS * fs_kHz {
        pred0_Q13 += delta0_Q13;
        pred1_Q13 += delta1_Q13;
        w_Q24 += deltaw_Q24;
        sum = (((*mid.offset(n as isize) as i32
            + *mid.offset((n + 2) as isize) as i32
            + ((*mid.offset((n + 1) as isize) as u32) << 1) as i32) as u32)
            << 9) as i32;
        sum = ((w_Q24 as i64 * *side.as_mut_ptr().offset((n + 1) as isize) as i64 >> 16) as i32
            as i64
            + (sum as i64 * pred0_Q13 as i16 as i64 >> 16)) as i32;
        sum = (sum as i64
            + (((*mid.offset((n + 1) as isize) as i32 as u32) << 11) as i32 as i64
                * pred1_Q13 as i16 as i64
                >> 16)) as i32;
        *x2.offset((n - 1) as isize) = (if (if 8 == 1 {
            (sum >> 1) + (sum & 1)
        } else {
            (sum >> 8 - 1) + 1 >> 1
        }) > silk_int16_MAX
        {
            silk_int16_MAX
        } else if (if 8 == 1 {
            (sum >> 1) + (sum & 1)
        } else {
            (sum >> 8 - 1) + 1 >> 1
        }) < silk_int16_MIN
        {
            silk_int16_MIN
        } else if 8 == 1 {
            (sum >> 1) + (sum & 1)
        } else {
            (sum >> 8 - 1) + 1 >> 1
        }) as i16;
        n += 1;
    }
    pred0_Q13 = -pred_Q13[0 as usize];
    pred1_Q13 = -pred_Q13[1 as usize];
    w_Q24 = ((width_Q14 as u32) << 10) as i32;
    n = STEREO_INTERP_LEN_MS * fs_kHz;
    while n < frame_length {
        sum = (((*mid.offset(n as isize) as i32
            + *mid.offset((n + 2) as isize) as i32
            + ((*mid.offset((n + 1) as isize) as u32) << 1) as i32) as u32)
            << 9) as i32;
        sum = ((w_Q24 as i64 * *side.as_mut_ptr().offset((n + 1) as isize) as i64 >> 16) as i32
            as i64
            + (sum as i64 * pred0_Q13 as i16 as i64 >> 16)) as i32;
        sum = (sum as i64
            + (((*mid.offset((n + 1) as isize) as i32 as u32) << 11) as i32 as i64
                * pred1_Q13 as i16 as i64
                >> 16)) as i32;
        *x2.offset((n - 1) as isize) = (if (if 8 == 1 {
            (sum >> 1) + (sum & 1)
        } else {
            (sum >> 8 - 1) + 1 >> 1
        }) > silk_int16_MAX
        {
            silk_int16_MAX
        } else if (if 8 == 1 {
            (sum >> 1) + (sum & 1)
        } else {
            (sum >> 8 - 1) + 1 >> 1
        }) < silk_int16_MIN
        {
            silk_int16_MIN
        } else if 8 == 1 {
            (sum >> 1) + (sum & 1)
        } else {
            (sum >> 8 - 1) + 1 >> 1
        }) as i16;
        n += 1;
    }
    (*state).pred_prev_Q13[0 as usize] = pred_Q13[0 as usize] as i16;
    (*state).pred_prev_Q13[1 as usize] = pred_Q13[1 as usize] as i16;
    (*state).width_prev_Q14 = width_Q14 as i16;
}
