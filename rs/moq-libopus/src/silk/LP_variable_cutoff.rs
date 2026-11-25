use crate::externs::memcpy;
use crate::silk::biquad_alt::silk_biquad_alt_stride1;
use crate::silk::define::{TRANSITION_INT_NUM, TRANSITION_NA, TRANSITION_NB};
use crate::silk::structs::silk_LP_state;
use crate::silk::tables_other::{silk_Transition_LP_A_Q28, silk_Transition_LP_B_Q28};

#[inline]
unsafe fn silk_LP_interpolate_filter_taps(
    B_Q28: *mut i32,
    A_Q28: *mut i32,
    ind: i32,
    fac_Q16: i32,
) {
    let mut nb: i32 = 0;
    let mut na: i32 = 0;
    if ind < TRANSITION_INT_NUM - 1 {
        if fac_Q16 > 0 {
            if fac_Q16 < 32768 {
                nb = 0;
                while nb < TRANSITION_NB {
                    *B_Q28.offset(nb as isize) = (silk_Transition_LP_B_Q28[ind as usize]
                        [nb as usize] as i64
                        + ((silk_Transition_LP_B_Q28[(ind + 1) as usize][nb as usize]
                            - silk_Transition_LP_B_Q28[ind as usize][nb as usize])
                            as i64
                            * fac_Q16 as i16 as i64
                            >> 16)) as i32;
                    nb += 1;
                }
                na = 0;
                while na < TRANSITION_NA {
                    *A_Q28.offset(na as isize) = (silk_Transition_LP_A_Q28[ind as usize]
                        [na as usize] as i64
                        + ((silk_Transition_LP_A_Q28[(ind + 1) as usize][na as usize]
                            - silk_Transition_LP_A_Q28[ind as usize][na as usize])
                            as i64
                            * fac_Q16 as i16 as i64
                            >> 16)) as i32;
                    na += 1;
                }
            } else {
                nb = 0;
                while nb < TRANSITION_NB {
                    *B_Q28.offset(nb as isize) = (silk_Transition_LP_B_Q28[(ind + 1) as usize]
                        [nb as usize] as i64
                        + ((silk_Transition_LP_B_Q28[(ind + 1) as usize][nb as usize]
                            - silk_Transition_LP_B_Q28[ind as usize][nb as usize])
                            as i64
                            * (fac_Q16 - ((1) << 16)) as i16 as i64
                            >> 16)) as i32;
                    nb += 1;
                }
                na = 0;
                while na < TRANSITION_NA {
                    *A_Q28.offset(na as isize) = (silk_Transition_LP_A_Q28[(ind + 1) as usize]
                        [na as usize] as i64
                        + ((silk_Transition_LP_A_Q28[(ind + 1) as usize][na as usize]
                            - silk_Transition_LP_A_Q28[ind as usize][na as usize])
                            as i64
                            * (fac_Q16 - ((1) << 16)) as i16 as i64
                            >> 16)) as i32;
                    na += 1;
                }
            }
        } else {
            memcpy(
                B_Q28 as *mut core::ffi::c_void,
                (silk_Transition_LP_B_Q28[ind as usize]).as_ptr() as *const core::ffi::c_void,
                3_u64.wrapping_mul(::core::mem::size_of::<i32>() as u64),
            );
            memcpy(
                A_Q28 as *mut core::ffi::c_void,
                (silk_Transition_LP_A_Q28[ind as usize]).as_ptr() as *const core::ffi::c_void,
                2_u64.wrapping_mul(::core::mem::size_of::<i32>() as u64),
            );
        }
    } else {
        memcpy(
            B_Q28 as *mut core::ffi::c_void,
            (silk_Transition_LP_B_Q28[(5 - 1) as usize]).as_ptr() as *const core::ffi::c_void,
            3_u64.wrapping_mul(::core::mem::size_of::<i32>() as u64),
        );
        memcpy(
            A_Q28 as *mut core::ffi::c_void,
            (silk_Transition_LP_A_Q28[(5 - 1) as usize]).as_ptr() as *const core::ffi::c_void,
            2_u64.wrapping_mul(::core::mem::size_of::<i32>() as u64),
        );
    };
}
pub unsafe fn silk_LP_variable_cutoff(
    psLP: *mut silk_LP_state,
    frame: *mut i16,
    frame_length: i32,
) {
    let mut B_Q28: [i32; 3] = [0; 3];
    let mut A_Q28: [i32; 2] = [0; 2];
    let mut fac_Q16: i32 = 0;
    let mut ind: i32 = 0;
    if (*psLP).mode != 0 {
        fac_Q16 = (((5120 / (5 * 4) - (*psLP).transition_frame_no) as u32) << 16 - 6) as i32;
        ind = fac_Q16 >> 16;
        fac_Q16 -= ((ind as u32) << 16) as i32;
        silk_LP_interpolate_filter_taps(B_Q28.as_mut_ptr(), A_Q28.as_mut_ptr(), ind, fac_Q16);
        (*psLP).transition_frame_no = if 0 > 5120 / (5 * 4) {
            if (*psLP).transition_frame_no + (*psLP).mode > 0 {
                0
            } else if (*psLP).transition_frame_no + (*psLP).mode < 5120 / (5 * 4) {
                5120 / (5 * 4)
            } else {
                (*psLP).transition_frame_no + (*psLP).mode
            }
        } else if (*psLP).transition_frame_no + (*psLP).mode > 5120 / (5 * 4) {
            5120 / (5 * 4)
        } else if (*psLP).transition_frame_no + (*psLP).mode < 0 {
            0
        } else {
            (*psLP).transition_frame_no + (*psLP).mode
        };
        silk_biquad_alt_stride1(
            frame,
            B_Q28.as_mut_ptr(),
            A_Q28.as_mut_ptr(),
            ((*psLP).In_LP_State).as_mut_ptr(),
            frame,
            frame_length,
        );
    }
}
