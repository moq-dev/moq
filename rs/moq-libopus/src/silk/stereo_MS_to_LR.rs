pub mod typedef_h {
    pub const silk_int16_MIN: i32 = i16::MIN as i32;
    pub const silk_int16_MAX: i32 = i16::MAX as i32;
}
use crate::silk::define::STEREO_INTERP_LEN_MS;

pub use self::typedef_h::{silk_int16_MAX, silk_int16_MIN};
use crate::silk::structs::stereo_dec_state;

use crate::externs::memcpy;
pub unsafe fn silk_stereo_MS_to_LR(
    state: *mut stereo_dec_state,
    x1: *mut i16,
    x2: *mut i16,
    pred_Q13: *const i32,
    fs_kHz: i32,
    frame_length: i32,
) {
    let mut n: i32 = 0;
    let mut denom_Q16: i32 = 0;
    let mut delta0_Q13: i32 = 0;
    let mut delta1_Q13: i32 = 0;
    let mut sum: i32 = 0;
    let mut diff: i32 = 0;
    let mut pred0_Q13: i32 = 0;
    let mut pred1_Q13: i32 = 0;
    memcpy(
        x1 as *mut core::ffi::c_void,
        ((*state).sMid).as_mut_ptr() as *const core::ffi::c_void,
        2_u64.wrapping_mul(::core::mem::size_of::<i16>() as u64),
    );
    memcpy(
        x2 as *mut core::ffi::c_void,
        ((*state).sSide).as_mut_ptr() as *const core::ffi::c_void,
        2_u64.wrapping_mul(::core::mem::size_of::<i16>() as u64),
    );
    memcpy(
        ((*state).sMid).as_mut_ptr() as *mut core::ffi::c_void,
        &mut *x1.offset(frame_length as isize) as *mut i16 as *const core::ffi::c_void,
        2_u64.wrapping_mul(::core::mem::size_of::<i16>() as u64),
    );
    memcpy(
        ((*state).sSide).as_mut_ptr() as *mut core::ffi::c_void,
        &mut *x2.offset(frame_length as isize) as *mut i16 as *const core::ffi::c_void,
        2_u64.wrapping_mul(::core::mem::size_of::<i16>() as u64),
    );
    pred0_Q13 = (*state).pred_prev_Q13[0 as usize] as i32;
    pred1_Q13 = (*state).pred_prev_Q13[1 as usize] as i32;
    denom_Q16 = ((1) << 16) / (8 * fs_kHz);
    delta0_Q13 = if 16 == 1 {
        ((*pred_Q13.offset(0 as isize) - (*state).pred_prev_Q13[0 as usize] as i32) as i16 as i32
            * denom_Q16 as i16 as i32
            >> 1)
            + ((*pred_Q13.offset(0 as isize) - (*state).pred_prev_Q13[0 as usize] as i32) as i16
                as i32
                * denom_Q16 as i16 as i32
                & 1)
    } else {
        ((*pred_Q13.offset(0 as isize) - (*state).pred_prev_Q13[0 as usize] as i32) as i16 as i32
            * denom_Q16 as i16 as i32
            >> 16 - 1)
            + 1
            >> 1
    };
    delta1_Q13 = if 16 == 1 {
        ((*pred_Q13.offset(1 as isize) - (*state).pred_prev_Q13[1 as usize] as i32) as i16 as i32
            * denom_Q16 as i16 as i32
            >> 1)
            + ((*pred_Q13.offset(1 as isize) - (*state).pred_prev_Q13[1 as usize] as i32) as i16
                as i32
                * denom_Q16 as i16 as i32
                & 1)
    } else {
        ((*pred_Q13.offset(1 as isize) - (*state).pred_prev_Q13[1 as usize] as i32) as i16 as i32
            * denom_Q16 as i16 as i32
            >> 16 - 1)
            + 1
            >> 1
    };
    n = 0;
    while n < STEREO_INTERP_LEN_MS * fs_kHz {
        pred0_Q13 += delta0_Q13;
        pred1_Q13 += delta1_Q13;
        sum = (((*x1.offset(n as isize) as i32
            + *x1.offset((n + 2) as isize) as i32
            + ((*x1.offset((n + 1) as isize) as u32) << 1) as i32) as u32)
            << 9) as i32;
        sum = (((*x2.offset((n + 1) as isize) as i32 as u32) << 8) as i32 as i64
            + (sum as i64 * pred0_Q13 as i16 as i64 >> 16)) as i32;
        sum = (sum as i64
            + (((*x1.offset((n + 1) as isize) as i32 as u32) << 11) as i32 as i64
                * pred1_Q13 as i16 as i64
                >> 16)) as i32;
        *x2.offset((n + 1) as isize) = (if (if 8 == 1 {
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
    pred0_Q13 = *pred_Q13.offset(0 as isize);
    pred1_Q13 = *pred_Q13.offset(1 as isize);
    n = STEREO_INTERP_LEN_MS * fs_kHz;
    while n < frame_length {
        sum = (((*x1.offset(n as isize) as i32
            + *x1.offset((n + 2) as isize) as i32
            + ((*x1.offset((n + 1) as isize) as u32) << 1) as i32) as u32)
            << 9) as i32;
        sum = (((*x2.offset((n + 1) as isize) as i32 as u32) << 8) as i32 as i64
            + (sum as i64 * pred0_Q13 as i16 as i64 >> 16)) as i32;
        sum = (sum as i64
            + (((*x1.offset((n + 1) as isize) as i32 as u32) << 11) as i32 as i64
                * pred1_Q13 as i16 as i64
                >> 16)) as i32;
        *x2.offset((n + 1) as isize) = (if (if 8 == 1 {
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
    (*state).pred_prev_Q13[0 as usize] = *pred_Q13.offset(0 as isize) as i16;
    (*state).pred_prev_Q13[1 as usize] = *pred_Q13.offset(1 as isize) as i16;
    n = 0;
    while n < frame_length {
        sum = *x1.offset((n + 1) as isize) as i32 + *x2.offset((n + 1) as isize) as i32;
        diff = *x1.offset((n + 1) as isize) as i32 - *x2.offset((n + 1) as isize) as i32;
        *x1.offset((n + 1) as isize) = (if sum > silk_int16_MAX {
            silk_int16_MAX
        } else if sum < silk_int16_MIN {
            silk_int16_MIN
        } else {
            sum
        }) as i16;
        *x2.offset((n + 1) as isize) = (if diff > silk_int16_MAX {
            silk_int16_MAX
        } else if diff < silk_int16_MIN {
            silk_int16_MIN
        } else {
            diff
        }) as i16;
        n += 1;
    }
}
