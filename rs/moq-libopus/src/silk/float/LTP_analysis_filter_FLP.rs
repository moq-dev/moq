use crate::silk::define::LTP_ORDER;
pub unsafe fn silk_LTP_analysis_filter_FLP(
    LTP_res: *mut f32,
    x: *const f32,
    B: *const f32,
    pitchL: *const i32,
    invGains: *const f32,
    subfr_length: i32,
    nb_subfr: i32,
    pre_length: i32,
) {
    let mut x_ptr: *const f32 = 0 as *const f32;
    let mut x_lag_ptr: *const f32 = 0 as *const f32;
    let mut Btmp: [f32; 5] = [0.; 5];
    let mut LTP_res_ptr: *mut f32 = 0 as *mut f32;
    let mut inv_gain: f32 = 0.;
    let mut k: i32 = 0;
    let mut i: i32 = 0;
    let mut j: i32 = 0;
    x_ptr = x;
    LTP_res_ptr = LTP_res;
    k = 0;
    while k < nb_subfr {
        x_lag_ptr = x_ptr.offset(-(*pitchL.offset(k as isize) as isize));
        inv_gain = *invGains.offset(k as isize);
        i = 0;
        while i < LTP_ORDER {
            Btmp[i as usize] = *B.offset((k * LTP_ORDER + i) as isize);
            i += 1;
        }
        i = 0;
        while i < subfr_length + pre_length {
            *LTP_res_ptr.offset(i as isize) = *x_ptr.offset(i as isize);
            j = 0;
            while j < LTP_ORDER {
                *LTP_res_ptr.offset(i as isize) -=
                    Btmp[j as usize] * *x_lag_ptr.offset((LTP_ORDER / 2 - j) as isize);
                j += 1;
            }
            *LTP_res_ptr.offset(i as isize) *= inv_gain;
            x_lag_ptr = x_lag_ptr.offset(1);
            i += 1;
        }
        LTP_res_ptr = LTP_res_ptr.offset((subfr_length + pre_length) as isize);
        x_ptr = x_ptr.offset(subfr_length as isize);
        k += 1;
    }
}
