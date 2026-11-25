use crate::silk::bwexpander_32::silk_bwexpander_32;
use crate::silk::define::MAX_LPC_STABILIZE_ITERATIONS;
use crate::silk::table_LSF_cos::silk_LSFCosTab_FIX_Q12;
use crate::silk::LPC_fit::silk_LPC_fit;
use crate::silk::LPC_inv_pred_gain::silk_LPC_inverse_pred_gain_c;

pub const QA: i32 = 16;
#[inline]
unsafe fn silk_NLSF2A_find_poly(out: *mut i32, cLSF: *const i32, dd: i32) {
    let mut k: i32 = 0;
    let mut n: i32 = 0;
    let mut ftmp: i32 = 0;
    *out.offset(0 as isize) = ((1) << 16) as i32;
    *out.offset(1 as isize) = -*cLSF.offset(0 as isize);
    k = 1;
    while k < dd {
        ftmp = *cLSF.offset((2 * k) as isize);
        *out.offset((k + 1) as isize) = ((*out.offset((k - 1) as isize) as u32) << 1) as i32
            - (if 16 == 1 {
                (ftmp as i64 * *out.offset(k as isize) as i64 >> 1)
                    + (ftmp as i64 * *out.offset(k as isize) as i64 & 1)
            } else {
                (ftmp as i64 * *out.offset(k as isize) as i64 >> 16 - 1) + 1 >> 1
            }) as i32;
        n = k;
        while n > 1 {
            let ref mut fresh0 = *out.offset(n as isize);
            *fresh0 += *out.offset((n - 2) as isize)
                - (if 16 == 1 {
                    (ftmp as i64 * *out.offset((n - 1) as isize) as i64 >> 1)
                        + (ftmp as i64 * *out.offset((n - 1) as isize) as i64 & 1)
                } else {
                    (ftmp as i64 * *out.offset((n - 1) as isize) as i64 >> 16 - 1) + 1 >> 1
                }) as i32;
            n -= 1;
        }
        let ref mut fresh1 = *out.offset(1 as isize);
        *fresh1 -= ftmp;
        k += 1;
    }
}

// compute whitening filter coefficients from normalized line spectral frequencies
pub unsafe fn silk_NLSF2A(a_Q12: *mut i16, NLSF: *const i16, d: i32, _arch: i32) {
    static ordering16: [u8; 16] = [0, 15, 8, 7, 4, 11, 12, 3, 2, 13, 10, 5, 6, 9, 14, 1];
    static ordering10: [u8; 10] = [0, 9, 6, 3, 4, 5, 8, 1, 2, 7];
    let mut ordering: *const u8 = 0 as *const u8;
    let mut k: i32 = 0;
    let mut i: i32 = 0;
    let mut dd: i32 = 0;
    let mut cos_LSF_QA: [i32; 24] = [0; 24];
    let mut P: [i32; 13] = [0; 13];
    let mut Q: [i32; 13] = [0; 13];
    let mut Ptmp: i32 = 0;
    let mut Qtmp: i32 = 0;
    let mut f_int: i32 = 0;
    let mut f_frac: i32 = 0;
    let mut cos_val: i32 = 0;
    let mut delta: i32 = 0;
    let mut a32_QA1: [i32; 24] = [0; 24];
    assert!(d == 10 || d == 16);
    ordering = if d == 16 {
        ordering16.as_ptr()
    } else {
        ordering10.as_ptr()
    };
    k = 0;
    while k < d {
        f_int = *NLSF.offset(k as isize) as i32 >> 15 - 7;
        f_frac = *NLSF.offset(k as isize) as i32 - ((f_int as u32) << 15 - 7) as i32;
        cos_val = silk_LSFCosTab_FIX_Q12[f_int as usize] as i32;
        delta = silk_LSFCosTab_FIX_Q12[(f_int + 1) as usize] as i32 - cos_val;
        cos_LSF_QA[*ordering.offset(k as isize) as usize] = if 20 - 16 == 1 {
            (((cos_val as u32) << 8) as i32 + delta * f_frac >> 1)
                + (((cos_val as u32) << 8) as i32 + delta * f_frac & 1)
        } else {
            (((cos_val as u32) << 8) as i32 + delta * f_frac >> 20 - 16 - 1) + 1 >> 1
        };
        k += 1;
    }
    dd = d >> 1;
    silk_NLSF2A_find_poly(
        P.as_mut_ptr(),
        &mut *cos_LSF_QA.as_mut_ptr().offset(0 as isize),
        dd,
    );
    silk_NLSF2A_find_poly(
        Q.as_mut_ptr(),
        &mut *cos_LSF_QA.as_mut_ptr().offset(1 as isize),
        dd,
    );
    k = 0;
    while k < dd {
        Ptmp = P[(k + 1) as usize] + P[k as usize];
        Qtmp = Q[(k + 1) as usize] - Q[k as usize];
        a32_QA1[k as usize] = -Qtmp - Ptmp;
        a32_QA1[(d - k - 1) as usize] = Qtmp - Ptmp;
        k += 1;
    }
    silk_LPC_fit(a_Q12, a32_QA1.as_mut_ptr(), 12, QA + 1, d);
    i = 0;
    while silk_LPC_inverse_pred_gain_c(a_Q12, d) == 0 && i < MAX_LPC_STABILIZE_ITERATIONS {
        silk_bwexpander_32(a32_QA1.as_mut_ptr(), d, 65536 - ((2) << i) as i32);
        k = 0;
        while k < d {
            *a_Q12.offset(k as isize) = (if 16 + 1 - 12 == 1 {
                (a32_QA1[k as usize] >> 1) + (a32_QA1[k as usize] & 1)
            } else {
                (a32_QA1[k as usize] >> 16 + 1 - 12 - 1) + 1 >> 1
            }) as i16;
            k += 1;
        }
        i += 1;
    }
}
