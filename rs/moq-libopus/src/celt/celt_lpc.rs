pub mod arch_h {
    pub type opus_val16 = f32;
    pub type opus_val32 = f32;
}
pub use self::arch_h::{opus_val16, opus_val32};
use crate::celt::pitch::{celt_pitch_xcorr_c, xcorr_kernel_c};
use crate::externs::memset;

pub const LPC_ORDER: i32 = 24;

pub unsafe fn _celt_lpc(mut _lpc: *mut opus_val16, ac: *const opus_val32, p: i32) {
    let mut i: i32 = 0;
    let mut j: i32 = 0;
    let mut r: opus_val32 = 0.;
    let mut error: opus_val32 = *ac.offset(0 as isize);
    let lpc: *mut f32 = _lpc;
    memset(
        lpc as *mut core::ffi::c_void,
        0,
        (p as u64).wrapping_mul(::core::mem::size_of::<f32>() as u64),
    );
    if *ac.offset(0 as isize) != 0 as f32 {
        i = 0;
        while i < p {
            let mut rr: opus_val32 = 0 as opus_val32;
            j = 0;
            while j < i {
                rr += *lpc.offset(j as isize) * *ac.offset((i - j) as isize);
                j += 1;
            }
            rr += *ac.offset((i + 1) as isize);
            r = -(rr / error);
            *lpc.offset(i as isize) = r;
            j = 0;
            while j < i + 1 >> 1 {
                let mut tmp1: opus_val32 = 0.;
                let mut tmp2: opus_val32 = 0.;
                tmp1 = *lpc.offset(j as isize);
                tmp2 = *lpc.offset((i - 1 - j) as isize);
                *lpc.offset(j as isize) = tmp1 + r * tmp2;
                *lpc.offset((i - 1 - j) as isize) = tmp2 + r * tmp1;
                j += 1;
            }
            error = error - r * r * error;
            if error < 0.001f32 * *ac.offset(0 as isize) {
                break;
            }
            i += 1;
        }
    }
}
pub unsafe fn celt_fir_c(
    x: *const opus_val16,
    num: *const opus_val16,
    y: *mut opus_val16,
    N: i32,
    ord: i32,
    _arch: i32,
) {
    let mut i: i32 = 0;
    let mut j: i32 = 0;
    assert!(x != y as *const opus_val16);
    let vla = ord as usize;
    let mut rnum: Vec<opus_val16> = ::std::vec::from_elem(0., vla);
    i = 0;
    while i < ord {
        *rnum.as_mut_ptr().offset(i as isize) = *num.offset((ord - i - 1) as isize);
        i += 1;
    }
    i = 0;
    while i < N - 3 {
        let mut sum: [opus_val32; 4] = [0.; 4];
        sum[0 as usize] = *x.offset(i as isize);
        sum[1 as usize] = *x.offset((i + 1) as isize);
        sum[2 as usize] = *x.offset((i + 2) as isize);
        sum[3 as usize] = *x.offset((i + 3) as isize);
        xcorr_kernel_c(
            rnum.as_mut_ptr(),
            x.offset(i as isize).offset(-(ord as isize)),
            sum.as_mut_ptr(),
            ord,
        );
        *y.offset(i as isize) = sum[0 as usize];
        *y.offset((i + 1) as isize) = sum[1 as usize];
        *y.offset((i + 2) as isize) = sum[2 as usize];
        *y.offset((i + 3) as isize) = sum[3 as usize];
        i += 4;
    }
    while i < N {
        let mut sum_0: opus_val32 = *x.offset(i as isize);
        j = 0;
        while j < ord {
            sum_0 =
                sum_0 + *rnum.as_mut_ptr().offset(j as isize) * *x.offset((i + j - ord) as isize);
            j += 1;
        }
        *y.offset(i as isize) = sum_0;
        i += 1;
    }
}
pub unsafe fn celt_iir(
    mut _x: *const opus_val32,
    den: *const opus_val16,
    mut _y: *mut opus_val32,
    N: i32,
    ord: i32,
    mem: *mut opus_val16,
    _arch: i32,
) {
    let mut i: i32 = 0;
    let mut j: i32 = 0;
    assert!(ord & 3 == 0);
    let vla = ord as usize;
    let mut rden: Vec<opus_val16> = ::std::vec::from_elem(0., vla);
    let vla_0 = (N + ord) as usize;
    let mut y: Vec<opus_val16> = ::std::vec::from_elem(0., vla_0);
    i = 0;
    while i < ord {
        *rden.as_mut_ptr().offset(i as isize) = *den.offset((ord - i - 1) as isize);
        i += 1;
    }
    i = 0;
    while i < ord {
        *y.as_mut_ptr().offset(i as isize) = -*mem.offset((ord - i - 1) as isize);
        i += 1;
    }
    while i < N + ord {
        *y.as_mut_ptr().offset(i as isize) = 0 as opus_val16;
        i += 1;
    }
    i = 0;
    while i < N - 3 {
        let mut sum: [opus_val32; 4] = [0.; 4];
        sum[0 as usize] = *_x.offset(i as isize);
        sum[1 as usize] = *_x.offset((i + 1) as isize);
        sum[2 as usize] = *_x.offset((i + 2) as isize);
        sum[3 as usize] = *_x.offset((i + 3) as isize);
        xcorr_kernel_c(
            rden.as_mut_ptr(),
            y.as_mut_ptr().offset(i as isize),
            sum.as_mut_ptr(),
            ord,
        );
        *y.as_mut_ptr().offset((i + ord) as isize) = -sum[0 as usize];
        *_y.offset(i as isize) = sum[0 as usize];
        sum[1 as usize] =
            sum[1 as usize] + *y.as_mut_ptr().offset((i + ord) as isize) * *den.offset(0 as isize);
        *y.as_mut_ptr().offset((i + ord + 1) as isize) = -sum[1 as usize];
        *_y.offset((i + 1) as isize) = sum[1 as usize];
        sum[2 as usize] = sum[2 as usize]
            + *y.as_mut_ptr().offset((i + ord + 1) as isize) * *den.offset(0 as isize);
        sum[2 as usize] =
            sum[2 as usize] + *y.as_mut_ptr().offset((i + ord) as isize) * *den.offset(1 as isize);
        *y.as_mut_ptr().offset((i + ord + 2) as isize) = -sum[2 as usize];
        *_y.offset((i + 2) as isize) = sum[2 as usize];
        sum[3 as usize] = sum[3 as usize]
            + *y.as_mut_ptr().offset((i + ord + 2) as isize) * *den.offset(0 as isize);
        sum[3 as usize] = sum[3 as usize]
            + *y.as_mut_ptr().offset((i + ord + 1) as isize) * *den.offset(1 as isize);
        sum[3 as usize] =
            sum[3 as usize] + *y.as_mut_ptr().offset((i + ord) as isize) * *den.offset(2 as isize);
        *y.as_mut_ptr().offset((i + ord + 3) as isize) = -sum[3 as usize];
        *_y.offset((i + 3) as isize) = sum[3 as usize];
        i += 4;
    }
    while i < N {
        let mut sum_0: opus_val32 = *_x.offset(i as isize);
        j = 0;
        while j < ord {
            sum_0 -=
                *rden.as_mut_ptr().offset(j as isize) * *y.as_mut_ptr().offset((i + j) as isize);
            j += 1;
        }
        *y.as_mut_ptr().offset((i + ord) as isize) = sum_0;
        *_y.offset(i as isize) = sum_0;
        i += 1;
    }
    i = 0;
    while i < ord {
        *mem.offset(i as isize) = *_y.offset((N - i - 1) as isize);
        i += 1;
    }
}
pub unsafe fn _celt_autocorr(
    x: *const opus_val16,
    ac: *mut opus_val32,
    window: *const opus_val16,
    overlap: i32,
    lag: i32,
    n: i32,
    arch: i32,
) -> i32 {
    let mut d: opus_val32 = 0.;
    let mut i: i32 = 0;
    let mut k: i32 = 0;
    let fastN: i32 = n - lag;
    let mut shift: i32 = 0;
    let mut xptr: *const opus_val16 = 0 as *const opus_val16;
    let vla = n as usize;
    let mut xx: Vec<opus_val16> = ::std::vec::from_elem(0., vla);
    assert!(n > 0);
    assert!(overlap >= 0);
    if overlap == 0 {
        xptr = x;
    } else {
        i = 0;
        while i < n {
            *xx.as_mut_ptr().offset(i as isize) = *x.offset(i as isize);
            i += 1;
        }
        i = 0;
        while i < overlap {
            *xx.as_mut_ptr().offset(i as isize) =
                *x.offset(i as isize) * *window.offset(i as isize);
            *xx.as_mut_ptr().offset((n - i - 1) as isize) =
                *x.offset((n - i - 1) as isize) * *window.offset(i as isize);
            i += 1;
        }
        xptr = xx.as_mut_ptr();
    }
    shift = 0;
    celt_pitch_xcorr_c(xptr, xptr, ac, fastN, lag + 1, arch);
    k = 0;
    while k <= lag {
        i = k + fastN;
        d = 0 as opus_val32;
        while i < n {
            d = d + *xptr.offset(i as isize) * *xptr.offset((i - k) as isize);
            i += 1;
        }
        let ref mut fresh19 = *ac.offset(k as isize);
        *fresh19 += d;
        k += 1;
    }
    return shift;
}
