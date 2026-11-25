pub mod tuning_parameters_h {
    pub const FIND_LPC_COND_FAC: f32 = 1e-5f32;
}
pub use self::tuning_parameters_h::FIND_LPC_COND_FAC;
use crate::externs::{memcpy, memset};
use crate::silk::float::energy_FLP::silk_energy_FLP;
use crate::silk::float::inner_product_FLP::silk_inner_product_FLP;

pub unsafe fn silk_burg_modified_FLP(
    A: *mut f32,
    x: *const f32,
    minInvGain: f32,
    subfr_length: i32,
    nb_subfr: i32,
    D: i32,
) -> f32 {
    let mut k: i32 = 0;
    let mut n: i32 = 0;
    let mut s: i32 = 0;
    let mut reached_max_gain: i32 = 0;
    let mut C0: f64 = 0.;
    let mut invGain: f64 = 0.;
    let mut num: f64 = 0.;
    let mut nrg_f: f64 = 0.;
    let mut nrg_b: f64 = 0.;
    let mut rc: f64 = 0.;
    let mut Atmp: f64 = 0.;
    let mut tmp1: f64 = 0.;
    let mut tmp2: f64 = 0.;
    let mut x_ptr: *const f32 = 0 as *const f32;
    let mut C_first_row: [f64; 24] = [0.; 24];
    let mut C_last_row: [f64; 24] = [0.; 24];
    let mut CAf: [f64; 25] = [0.; 25];
    let mut CAb: [f64; 25] = [0.; 25];
    let mut Af: [f64; 24] = [0.; 24];
    assert!(subfr_length * nb_subfr <= 384);
    C0 = silk_energy_FLP(x, nb_subfr * subfr_length);
    memset(
        C_first_row.as_mut_ptr() as *mut core::ffi::c_void,
        0,
        24_u64.wrapping_mul(::core::mem::size_of::<f64>() as u64),
    );
    s = 0;
    while s < nb_subfr {
        x_ptr = x.offset((s * subfr_length) as isize);
        n = 1;
        while n < D + 1 {
            C_first_row[(n - 1) as usize] +=
                silk_inner_product_FLP(x_ptr, x_ptr.offset(n as isize), subfr_length - n);
            n += 1;
        }
        s += 1;
    }
    memcpy(
        C_last_row.as_mut_ptr() as *mut core::ffi::c_void,
        C_first_row.as_mut_ptr() as *const core::ffi::c_void,
        24_u64.wrapping_mul(::core::mem::size_of::<f64>() as u64),
    );
    CAf[0 as usize] = C0 + FIND_LPC_COND_FAC as f64 * C0 + 1e-9f32 as f64;
    CAb[0 as usize] = CAf[0 as usize];
    invGain = 1.0f32 as f64;
    reached_max_gain = 0;
    n = 0;
    while n < D {
        s = 0;
        while s < nb_subfr {
            x_ptr = x.offset((s * subfr_length) as isize);
            tmp1 = *x_ptr.offset(n as isize) as f64;
            tmp2 = *x_ptr.offset((subfr_length - n - 1) as isize) as f64;
            k = 0;
            while k < n {
                C_first_row[k as usize] -=
                    (*x_ptr.offset(n as isize) * *x_ptr.offset((n - k - 1) as isize)) as f64;
                C_last_row[k as usize] -= (*x_ptr.offset((subfr_length - n - 1) as isize)
                    * *x_ptr.offset((subfr_length - n + k) as isize))
                    as f64;
                Atmp = Af[k as usize];
                tmp1 += *x_ptr.offset((n - k - 1) as isize) as f64 * Atmp;
                tmp2 += *x_ptr.offset((subfr_length - n + k) as isize) as f64 * Atmp;
                k += 1;
            }
            k = 0;
            while k <= n {
                CAf[k as usize] -= tmp1 * *x_ptr.offset((n - k) as isize) as f64;
                CAb[k as usize] -= tmp2 * *x_ptr.offset((subfr_length - n + k - 1) as isize) as f64;
                k += 1;
            }
            s += 1;
        }
        tmp1 = C_first_row[n as usize];
        tmp2 = C_last_row[n as usize];
        k = 0;
        while k < n {
            Atmp = Af[k as usize];
            tmp1 += C_last_row[(n - k - 1) as usize] * Atmp;
            tmp2 += C_first_row[(n - k - 1) as usize] * Atmp;
            k += 1;
        }
        CAf[(n + 1) as usize] = tmp1;
        CAb[(n + 1) as usize] = tmp2;
        num = CAb[(n + 1) as usize];
        nrg_b = CAb[0 as usize];
        nrg_f = CAf[0 as usize];
        k = 0;
        while k < n {
            Atmp = Af[k as usize];
            num += CAb[(n - k) as usize] * Atmp;
            nrg_b += CAb[(k + 1) as usize] * Atmp;
            nrg_f += CAf[(k + 1) as usize] * Atmp;
            k += 1;
        }
        rc = -2.0f64 * num / (nrg_f + nrg_b);
        tmp1 = invGain * (1.0f64 - rc * rc);
        if tmp1 <= minInvGain as f64 {
            rc = (1.0f64 - minInvGain as f64 / invGain).sqrt();
            if num > 0 as f64 {
                rc = -rc;
            }
            invGain = minInvGain as f64;
            reached_max_gain = 1;
        } else {
            invGain = tmp1;
        }
        k = 0;
        while k < n + 1 >> 1 {
            tmp1 = Af[k as usize];
            tmp2 = Af[(n - k - 1) as usize];
            Af[k as usize] = tmp1 + rc * tmp2;
            Af[(n - k - 1) as usize] = tmp2 + rc * tmp1;
            k += 1;
        }
        Af[n as usize] = rc;
        if reached_max_gain != 0 {
            k = n + 1;
            while k < D {
                Af[k as usize] = 0.0f64;
                k += 1;
            }
            break;
        } else {
            k = 0;
            while k <= n + 1 {
                tmp1 = CAf[k as usize];
                CAf[k as usize] += rc * CAb[(n - k + 1) as usize];
                CAb[(n - k + 1) as usize] += rc * tmp1;
                k += 1;
            }
            n += 1;
        }
    }
    if reached_max_gain != 0 {
        k = 0;
        while k < D {
            *A.offset(k as isize) = -Af[k as usize] as f32;
            k += 1;
        }
        s = 0;
        while s < nb_subfr {
            C0 -= silk_energy_FLP(x.offset((s * subfr_length) as isize), D);
            s += 1;
        }
        nrg_f = C0 * invGain;
    } else {
        nrg_f = CAf[0 as usize];
        tmp1 = 1.0f64;
        k = 0;
        while k < D {
            Atmp = Af[k as usize];
            nrg_f += CAf[(k + 1) as usize] * Atmp;
            tmp1 += Atmp * Atmp;
            *A.offset(k as isize) = -Atmp as f32;
            k += 1;
        }
        nrg_f -= FIND_LPC_COND_FAC as f64 * C0 * tmp1;
    }
    return nrg_f as f32;
}
