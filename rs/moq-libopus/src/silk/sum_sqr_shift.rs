use crate::silk::macros::silk_CLZ32;
use crate::silk::SigProc_FIX::silk_max_32;

pub unsafe fn silk_sum_sqr_shift(energy: *mut i32, shift: *mut i32, x: *const i16, len: i32) {
    let mut i: i32 = 0;
    let mut shft: i32 = 0;
    let mut nrg_tmp: u32 = 0;
    let mut nrg: i32 = 0;
    shft = 31 - silk_CLZ32(len);
    nrg = len;
    i = 0;
    while i < len - 1 {
        nrg_tmp = (*x.offset(i as isize) as i32 * *x.offset(i as isize) as i32) as u32;
        nrg_tmp = nrg_tmp.wrapping_add(
            (*x.offset((i + 1) as isize) as i32 * *x.offset((i + 1) as isize) as i32) as u32,
        ) as i32 as u32;
        nrg = (nrg as u32).wrapping_add(nrg_tmp >> shft) as i32;
        i += 2;
    }
    if i < len {
        nrg_tmp = (*x.offset(i as isize) as i32 * *x.offset(i as isize) as i32) as u32;
        nrg = (nrg as u32).wrapping_add(nrg_tmp >> shft) as i32;
    }
    shft = silk_max_32(0, shft + 3 - silk_CLZ32(nrg));
    nrg = 0;
    i = 0;
    while i < len - 1 {
        nrg_tmp = (*x.offset(i as isize) as i32 * *x.offset(i as isize) as i32) as u32;
        nrg_tmp = nrg_tmp.wrapping_add(
            (*x.offset((i + 1) as isize) as i32 * *x.offset((i + 1) as isize) as i32) as u32,
        ) as i32 as u32;
        nrg = (nrg as u32).wrapping_add(nrg_tmp >> shft) as i32;
        i += 2;
    }
    if i < len {
        nrg_tmp = (*x.offset(i as isize) as i32 * *x.offset(i as isize) as i32) as u32;
        nrg = (nrg as u32).wrapping_add(nrg_tmp >> shft) as i32;
    }
    *shift = shft;
    *energy = nrg;
}
