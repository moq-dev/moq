use crate::externs::memset;
#[inline]
unsafe fn silk_LPC_analysis_filter16_FLP(
    r_LPC: *mut f32,
    PredCoef: *const f32,
    s: *const f32,
    length: i32,
) {
    let mut ix: i32 = 0;
    let mut LPC_pred: f32 = 0.;
    let mut s_ptr: *const f32 = 0 as *const f32;
    ix = 16;
    while ix < length {
        s_ptr = &*s.offset((ix - 1) as isize) as *const f32;
        LPC_pred = *s_ptr.offset(0 as isize) * *PredCoef.offset(0 as isize)
            + *s_ptr.offset(-1 as isize) * *PredCoef.offset(1 as isize)
            + *s_ptr.offset(-(2) as isize) * *PredCoef.offset(2 as isize)
            + *s_ptr.offset(-(3) as isize) * *PredCoef.offset(3 as isize)
            + *s_ptr.offset(-(4) as isize) * *PredCoef.offset(4 as isize)
            + *s_ptr.offset(-(5) as isize) * *PredCoef.offset(5 as isize)
            + *s_ptr.offset(-(6) as isize) * *PredCoef.offset(6 as isize)
            + *s_ptr.offset(-(7) as isize) * *PredCoef.offset(7 as isize)
            + *s_ptr.offset(-(8) as isize) * *PredCoef.offset(8 as isize)
            + *s_ptr.offset(-(9) as isize) * *PredCoef.offset(9 as isize)
            + *s_ptr.offset(-(10) as isize) * *PredCoef.offset(10 as isize)
            + *s_ptr.offset(-(11) as isize) * *PredCoef.offset(11 as isize)
            + *s_ptr.offset(-(12) as isize) * *PredCoef.offset(12 as isize)
            + *s_ptr.offset(-(13) as isize) * *PredCoef.offset(13 as isize)
            + *s_ptr.offset(-(14) as isize) * *PredCoef.offset(14 as isize)
            + *s_ptr.offset(-(15) as isize) * *PredCoef.offset(15 as isize);
        *r_LPC.offset(ix as isize) = *s_ptr.offset(1 as isize) - LPC_pred;
        ix += 1;
    }
}
#[inline]
unsafe fn silk_LPC_analysis_filter12_FLP(
    r_LPC: *mut f32,
    PredCoef: *const f32,
    s: *const f32,
    length: i32,
) {
    let mut ix: i32 = 0;
    let mut LPC_pred: f32 = 0.;
    let mut s_ptr: *const f32 = 0 as *const f32;
    ix = 12;
    while ix < length {
        s_ptr = &*s.offset((ix - 1) as isize) as *const f32;
        LPC_pred = *s_ptr.offset(0 as isize) * *PredCoef.offset(0 as isize)
            + *s_ptr.offset(-1 as isize) * *PredCoef.offset(1 as isize)
            + *s_ptr.offset(-(2) as isize) * *PredCoef.offset(2 as isize)
            + *s_ptr.offset(-(3) as isize) * *PredCoef.offset(3 as isize)
            + *s_ptr.offset(-(4) as isize) * *PredCoef.offset(4 as isize)
            + *s_ptr.offset(-(5) as isize) * *PredCoef.offset(5 as isize)
            + *s_ptr.offset(-(6) as isize) * *PredCoef.offset(6 as isize)
            + *s_ptr.offset(-(7) as isize) * *PredCoef.offset(7 as isize)
            + *s_ptr.offset(-(8) as isize) * *PredCoef.offset(8 as isize)
            + *s_ptr.offset(-(9) as isize) * *PredCoef.offset(9 as isize)
            + *s_ptr.offset(-(10) as isize) * *PredCoef.offset(10 as isize)
            + *s_ptr.offset(-(11) as isize) * *PredCoef.offset(11 as isize);
        *r_LPC.offset(ix as isize) = *s_ptr.offset(1 as isize) - LPC_pred;
        ix += 1;
    }
}
#[inline]
unsafe fn silk_LPC_analysis_filter10_FLP(
    r_LPC: *mut f32,
    PredCoef: *const f32,
    s: *const f32,
    length: i32,
) {
    let mut ix: i32 = 0;
    let mut LPC_pred: f32 = 0.;
    let mut s_ptr: *const f32 = 0 as *const f32;
    ix = 10;
    while ix < length {
        s_ptr = &*s.offset((ix - 1) as isize) as *const f32;
        LPC_pred = *s_ptr.offset(0 as isize) * *PredCoef.offset(0 as isize)
            + *s_ptr.offset(-1 as isize) * *PredCoef.offset(1 as isize)
            + *s_ptr.offset(-(2) as isize) * *PredCoef.offset(2 as isize)
            + *s_ptr.offset(-(3) as isize) * *PredCoef.offset(3 as isize)
            + *s_ptr.offset(-(4) as isize) * *PredCoef.offset(4 as isize)
            + *s_ptr.offset(-(5) as isize) * *PredCoef.offset(5 as isize)
            + *s_ptr.offset(-(6) as isize) * *PredCoef.offset(6 as isize)
            + *s_ptr.offset(-(7) as isize) * *PredCoef.offset(7 as isize)
            + *s_ptr.offset(-(8) as isize) * *PredCoef.offset(8 as isize)
            + *s_ptr.offset(-(9) as isize) * *PredCoef.offset(9 as isize);
        *r_LPC.offset(ix as isize) = *s_ptr.offset(1 as isize) - LPC_pred;
        ix += 1;
    }
}
#[inline]
unsafe fn silk_LPC_analysis_filter8_FLP(
    r_LPC: *mut f32,
    PredCoef: *const f32,
    s: *const f32,
    length: i32,
) {
    let mut ix: i32 = 0;
    let mut LPC_pred: f32 = 0.;
    let mut s_ptr: *const f32 = 0 as *const f32;
    ix = 8;
    while ix < length {
        s_ptr = &*s.offset((ix - 1) as isize) as *const f32;
        LPC_pred = *s_ptr.offset(0 as isize) * *PredCoef.offset(0 as isize)
            + *s_ptr.offset(-1 as isize) * *PredCoef.offset(1 as isize)
            + *s_ptr.offset(-(2) as isize) * *PredCoef.offset(2 as isize)
            + *s_ptr.offset(-(3) as isize) * *PredCoef.offset(3 as isize)
            + *s_ptr.offset(-(4) as isize) * *PredCoef.offset(4 as isize)
            + *s_ptr.offset(-(5) as isize) * *PredCoef.offset(5 as isize)
            + *s_ptr.offset(-(6) as isize) * *PredCoef.offset(6 as isize)
            + *s_ptr.offset(-(7) as isize) * *PredCoef.offset(7 as isize);
        *r_LPC.offset(ix as isize) = *s_ptr.offset(1 as isize) - LPC_pred;
        ix += 1;
    }
}
#[inline]
unsafe fn silk_LPC_analysis_filter6_FLP(
    r_LPC: *mut f32,
    PredCoef: *const f32,
    s: *const f32,
    length: i32,
) {
    let mut ix: i32 = 0;
    let mut LPC_pred: f32 = 0.;
    let mut s_ptr: *const f32 = 0 as *const f32;
    ix = 6;
    while ix < length {
        s_ptr = &*s.offset((ix - 1) as isize) as *const f32;
        LPC_pred = *s_ptr.offset(0 as isize) * *PredCoef.offset(0 as isize)
            + *s_ptr.offset(-1 as isize) * *PredCoef.offset(1 as isize)
            + *s_ptr.offset(-(2) as isize) * *PredCoef.offset(2 as isize)
            + *s_ptr.offset(-(3) as isize) * *PredCoef.offset(3 as isize)
            + *s_ptr.offset(-(4) as isize) * *PredCoef.offset(4 as isize)
            + *s_ptr.offset(-(5) as isize) * *PredCoef.offset(5 as isize);
        *r_LPC.offset(ix as isize) = *s_ptr.offset(1 as isize) - LPC_pred;
        ix += 1;
    }
}
pub unsafe fn silk_LPC_analysis_filter_FLP(
    r_LPC: *mut f32,
    PredCoef: *const f32,
    s: *const f32,
    length: i32,
    Order: i32,
) {
    assert!(Order <= length);
    match Order {
        6 => {
            silk_LPC_analysis_filter6_FLP(r_LPC, PredCoef, s, length);
        }
        8 => {
            silk_LPC_analysis_filter8_FLP(r_LPC, PredCoef, s, length);
        }
        10 => {
            silk_LPC_analysis_filter10_FLP(r_LPC, PredCoef, s, length);
        }
        12 => {
            silk_LPC_analysis_filter12_FLP(r_LPC, PredCoef, s, length);
        }
        16 => {
            silk_LPC_analysis_filter16_FLP(r_LPC, PredCoef, s, length);
        }
        _ => {
            panic!("libopus: assert(0) called");
        }
    }
    memset(
        r_LPC as *mut core::ffi::c_void,
        0,
        (Order as u64).wrapping_mul(::core::mem::size_of::<f32>() as u64),
    );
}
