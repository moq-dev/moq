pub mod arch_h {
    pub type opus_val16 = f32;
    pub type opus_val32 = f32;
    pub type celt_sig = f32;
    pub const Q15ONE: f32 = 1.0f32;
}
pub mod stddef_h {
    pub const NULL: i32 = 0;
}
pub use self::arch_h::{celt_sig, opus_val16, opus_val32, Q15ONE};
pub use self::stddef_h::NULL;
use crate::celt::celt_lpc::{_celt_autocorr, _celt_lpc};
use crate::celt::entcode::celt_udiv;
use crate::celt::mathops::celt_sqrt;

#[inline]
pub unsafe fn dual_inner_prod_c(
    x: *const opus_val16,
    y01: *const opus_val16,
    y02: *const opus_val16,
    N: i32,
    xy1: *mut opus_val32,
    xy2: *mut opus_val32,
) {
    let mut i: i32 = 0;
    let mut xy01: opus_val32 = 0 as opus_val32;
    let mut xy02: opus_val32 = 0 as opus_val32;
    i = 0;
    while i < N {
        xy01 = xy01 + *x.offset(i as isize) * *y01.offset(i as isize);
        xy02 = xy02 + *x.offset(i as isize) * *y02.offset(i as isize);
        i += 1;
    }
    *xy1 = xy01;
    *xy2 = xy02;
}
#[inline]
pub unsafe fn xcorr_kernel_c(
    mut x: *const opus_val16,
    mut y: *const opus_val16,
    sum: *mut opus_val32,
    len: i32,
) {
    let mut j: i32 = 0;
    let mut y_0: opus_val16 = 0.;
    let mut y_1: opus_val16 = 0.;
    let mut y_2: opus_val16 = 0.;
    let mut y_3: opus_val16 = 0.;
    assert!(len >= 3);
    y_3 = 0 as opus_val16;
    let fresh0 = y;
    y = y.offset(1);
    y_0 = *fresh0;
    let fresh1 = y;
    y = y.offset(1);
    y_1 = *fresh1;
    let fresh2 = y;
    y = y.offset(1);
    y_2 = *fresh2;
    j = 0;
    while j < len - 3 {
        let mut tmp: opus_val16 = 0.;
        let fresh3 = x;
        x = x.offset(1);
        tmp = *fresh3;
        let fresh4 = y;
        y = y.offset(1);
        y_3 = *fresh4;
        *sum.offset(0 as isize) = *sum.offset(0 as isize) + tmp * y_0;
        *sum.offset(1 as isize) = *sum.offset(1 as isize) + tmp * y_1;
        *sum.offset(2 as isize) = *sum.offset(2 as isize) + tmp * y_2;
        *sum.offset(3 as isize) = *sum.offset(3 as isize) + tmp * y_3;
        let fresh5 = x;
        x = x.offset(1);
        tmp = *fresh5;
        let fresh6 = y;
        y = y.offset(1);
        y_0 = *fresh6;
        *sum.offset(0 as isize) = *sum.offset(0 as isize) + tmp * y_1;
        *sum.offset(1 as isize) = *sum.offset(1 as isize) + tmp * y_2;
        *sum.offset(2 as isize) = *sum.offset(2 as isize) + tmp * y_3;
        *sum.offset(3 as isize) = *sum.offset(3 as isize) + tmp * y_0;
        let fresh7 = x;
        x = x.offset(1);
        tmp = *fresh7;
        let fresh8 = y;
        y = y.offset(1);
        y_1 = *fresh8;
        *sum.offset(0 as isize) = *sum.offset(0 as isize) + tmp * y_2;
        *sum.offset(1 as isize) = *sum.offset(1 as isize) + tmp * y_3;
        *sum.offset(2 as isize) = *sum.offset(2 as isize) + tmp * y_0;
        *sum.offset(3 as isize) = *sum.offset(3 as isize) + tmp * y_1;
        let fresh9 = x;
        x = x.offset(1);
        tmp = *fresh9;
        let fresh10 = y;
        y = y.offset(1);
        y_2 = *fresh10;
        *sum.offset(0 as isize) = *sum.offset(0 as isize) + tmp * y_3;
        *sum.offset(1 as isize) = *sum.offset(1 as isize) + tmp * y_0;
        *sum.offset(2 as isize) = *sum.offset(2 as isize) + tmp * y_1;
        *sum.offset(3 as isize) = *sum.offset(3 as isize) + tmp * y_2;
        j += 4;
    }
    let fresh11 = j;
    j = j + 1;
    if fresh11 < len {
        let fresh12 = x;
        x = x.offset(1);
        let tmp_0: opus_val16 = *fresh12;
        let fresh13 = y;
        y = y.offset(1);
        y_3 = *fresh13;
        *sum.offset(0 as isize) = *sum.offset(0 as isize) + tmp_0 * y_0;
        *sum.offset(1 as isize) = *sum.offset(1 as isize) + tmp_0 * y_1;
        *sum.offset(2 as isize) = *sum.offset(2 as isize) + tmp_0 * y_2;
        *sum.offset(3 as isize) = *sum.offset(3 as isize) + tmp_0 * y_3;
    }
    let fresh14 = j;
    j = j + 1;
    if fresh14 < len {
        let fresh15 = x;
        x = x.offset(1);
        let tmp_1: opus_val16 = *fresh15;
        let fresh16 = y;
        y = y.offset(1);
        y_0 = *fresh16;
        *sum.offset(0 as isize) = *sum.offset(0 as isize) + tmp_1 * y_1;
        *sum.offset(1 as isize) = *sum.offset(1 as isize) + tmp_1 * y_2;
        *sum.offset(2 as isize) = *sum.offset(2 as isize) + tmp_1 * y_3;
        *sum.offset(3 as isize) = *sum.offset(3 as isize) + tmp_1 * y_0;
    }
    if j < len {
        let fresh17 = x;
        x = x.offset(1);
        let tmp_2: opus_val16 = *fresh17;
        let fresh18 = y;
        y = y.offset(1);
        y_1 = *fresh18;
        *sum.offset(0 as isize) = *sum.offset(0 as isize) + tmp_2 * y_2;
        *sum.offset(1 as isize) = *sum.offset(1 as isize) + tmp_2 * y_3;
        *sum.offset(2 as isize) = *sum.offset(2 as isize) + tmp_2 * y_0;
        *sum.offset(3 as isize) = *sum.offset(3 as isize) + tmp_2 * y_1;
    }
}
#[inline]
pub unsafe fn celt_inner_prod_c(x: *const opus_val16, y: *const opus_val16, N: i32) -> opus_val32 {
    let mut i: i32 = 0;
    let mut xy: opus_val32 = 0 as opus_val32;
    i = 0;
    while i < N {
        xy = xy + *x.offset(i as isize) * *y.offset(i as isize);
        i += 1;
    }
    return xy;
}

unsafe fn find_best_pitch(
    xcorr: *mut opus_val32,
    y: *mut opus_val16,
    len: i32,
    max_pitch: i32,
    best_pitch: *mut i32,
) {
    let mut i: i32 = 0;
    let mut j: i32 = 0;
    let mut Syy: opus_val32 = 1 as opus_val32;
    let mut best_num: [opus_val16; 2] = [0.; 2];
    let mut best_den: [opus_val32; 2] = [0.; 2];
    best_num[0 as usize] = -1 as opus_val16;
    best_num[1 as usize] = -1 as opus_val16;
    best_den[0 as usize] = 0 as opus_val32;
    best_den[1 as usize] = 0 as opus_val32;
    *best_pitch.offset(0 as isize) = 0;
    *best_pitch.offset(1 as isize) = 1;
    j = 0;
    while j < len {
        Syy = Syy + *y.offset(j as isize) * *y.offset(j as isize);
        j += 1;
    }
    i = 0;
    while i < max_pitch {
        if *xcorr.offset(i as isize) > 0 as f32 {
            let mut num: opus_val16 = 0.;
            let mut xcorr16: opus_val32 = 0.;
            xcorr16 = *xcorr.offset(i as isize);
            xcorr16 *= 1e-12f32;
            num = xcorr16 * xcorr16;
            if num * best_den[1 as usize] > best_num[1 as usize] * Syy {
                if num * best_den[0 as usize] > best_num[0 as usize] * Syy {
                    best_num[1 as usize] = best_num[0 as usize];
                    best_den[1 as usize] = best_den[0 as usize];
                    *best_pitch.offset(1 as isize) = *best_pitch.offset(0 as isize);
                    best_num[0 as usize] = num;
                    best_den[0 as usize] = Syy;
                    *best_pitch.offset(0 as isize) = i;
                } else {
                    best_num[1 as usize] = num;
                    best_den[1 as usize] = Syy;
                    *best_pitch.offset(1 as isize) = i;
                }
            }
        }
        Syy += *y.offset((i + len) as isize) * *y.offset((i + len) as isize)
            - *y.offset(i as isize) * *y.offset(i as isize);
        Syy = if 1 as f32 > Syy { 1 as f32 } else { Syy };
        i += 1;
    }
}
unsafe fn celt_fir5(x: *mut opus_val16, num: *const opus_val16, N: i32) {
    let mut i: i32 = 0;
    let mut num0: opus_val16 = 0.;
    let mut num1: opus_val16 = 0.;
    let mut num2: opus_val16 = 0.;
    let mut num3: opus_val16 = 0.;
    let mut num4: opus_val16 = 0.;
    let mut mem0: opus_val32 = 0.;
    let mut mem1: opus_val32 = 0.;
    let mut mem2: opus_val32 = 0.;
    let mut mem3: opus_val32 = 0.;
    let mut mem4: opus_val32 = 0.;
    num0 = *num.offset(0 as isize);
    num1 = *num.offset(1 as isize);
    num2 = *num.offset(2 as isize);
    num3 = *num.offset(3 as isize);
    num4 = *num.offset(4 as isize);
    mem0 = 0 as opus_val32;
    mem1 = 0 as opus_val32;
    mem2 = 0 as opus_val32;
    mem3 = 0 as opus_val32;
    mem4 = 0 as opus_val32;
    i = 0;
    while i < N {
        let mut sum: opus_val32 = *x.offset(i as isize);
        sum = sum + num0 * mem0;
        sum = sum + num1 * mem1;
        sum = sum + num2 * mem2;
        sum = sum + num3 * mem3;
        sum = sum + num4 * mem4;
        mem4 = mem3;
        mem3 = mem2;
        mem2 = mem1;
        mem1 = mem0;
        mem0 = *x.offset(i as isize);
        *x.offset(i as isize) = sum;
        i += 1;
    }
}
pub unsafe fn pitch_downsample(
    x: *mut *mut celt_sig,
    x_lp: *mut opus_val16,
    len: i32,
    C: i32,
    arch: i32,
) {
    let mut i: i32 = 0;
    let mut ac: [opus_val32; 5] = [0.; 5];
    let mut tmp: opus_val16 = Q15ONE;
    let mut lpc: [opus_val16; 4] = [0.; 4];
    let mut lpc2: [opus_val16; 5] = [0.; 5];
    let c1: opus_val16 = 0.8f32;
    i = 1;
    while i < len >> 1 {
        *x_lp.offset(i as isize) = 0.5f32
            * (0.5f32
                * (*(*x.offset(0 as isize)).offset((2 * i - 1) as isize)
                    + *(*x.offset(0 as isize)).offset((2 * i + 1) as isize))
                + *(*x.offset(0 as isize)).offset((2 * i) as isize));
        i += 1;
    }
    *x_lp.offset(0 as isize) = 0.5f32
        * (0.5f32 * *(*x.offset(0 as isize)).offset(1 as isize)
            + *(*x.offset(0 as isize)).offset(0 as isize));
    if C == 2 {
        i = 1;
        while i < len >> 1 {
            let ref mut fresh19 = *x_lp.offset(i as isize);
            *fresh19 += 0.5f32
                * (0.5f32
                    * (*(*x.offset(1 as isize)).offset((2 * i - 1) as isize)
                        + *(*x.offset(1 as isize)).offset((2 * i + 1) as isize))
                    + *(*x.offset(1 as isize)).offset((2 * i) as isize));
            i += 1;
        }
        let ref mut fresh20 = *x_lp.offset(0 as isize);
        *fresh20 += 0.5f32
            * (0.5f32 * *(*x.offset(1 as isize)).offset(1 as isize)
                + *(*x.offset(1 as isize)).offset(0 as isize));
    }
    _celt_autocorr(
        x_lp,
        ac.as_mut_ptr(),
        NULL as *const opus_val16,
        0,
        4,
        len >> 1,
        arch,
    );
    ac[0 as usize] *= 1.0001f32;
    i = 1;
    while i <= 4 {
        ac[i as usize] -= ac[i as usize] * (0.008f32 * i as f32) * (0.008f32 * i as f32);
        i += 1;
    }
    _celt_lpc(lpc.as_mut_ptr(), ac.as_mut_ptr(), 4);
    i = 0;
    while i < 4 {
        tmp = 0.9f32 * tmp;
        lpc[i as usize] = lpc[i as usize] * tmp;
        i += 1;
    }
    lpc2[0 as usize] = lpc[0 as usize] + 0.8f32;
    lpc2[1 as usize] = lpc[1 as usize] + c1 * lpc[0 as usize];
    lpc2[2 as usize] = lpc[2 as usize] + c1 * lpc[1 as usize];
    lpc2[3 as usize] = lpc[3 as usize] + c1 * lpc[2 as usize];
    lpc2[4 as usize] = c1 * lpc[3 as usize];
    celt_fir5(x_lp, lpc2.as_mut_ptr(), len >> 1);
}
pub unsafe fn celt_pitch_xcorr_c(
    mut _x: *const opus_val16,
    mut _y: *const opus_val16,
    xcorr: *mut opus_val32,
    len: i32,
    max_pitch: i32,
    _arch: i32,
) {
    let mut i: i32 = 0;
    assert!(max_pitch > 0);
    i = 0;
    while i < max_pitch - 3 {
        let mut sum: [opus_val32; 4] = [
            0 as opus_val32,
            0 as opus_val32,
            0 as opus_val32,
            0 as opus_val32,
        ];
        xcorr_kernel_c(_x, _y.offset(i as isize), sum.as_mut_ptr(), len);
        *xcorr.offset(i as isize) = sum[0 as usize];
        *xcorr.offset((i + 1) as isize) = sum[1 as usize];
        *xcorr.offset((i + 2) as isize) = sum[2 as usize];
        *xcorr.offset((i + 3) as isize) = sum[3 as usize];
        i += 4;
    }
    while i < max_pitch {
        let mut sum_0: opus_val32 = 0.;
        sum_0 = celt_inner_prod_c(_x, _y.offset(i as isize), len);
        *xcorr.offset(i as isize) = sum_0;
        i += 1;
    }
}
pub unsafe fn pitch_search(
    x_lp: *const opus_val16,
    y: *mut opus_val16,
    len: i32,
    max_pitch: i32,
    pitch: *mut i32,
    arch: i32,
) {
    let mut i: i32 = 0;
    let mut j: i32 = 0;
    let mut lag: i32 = 0;
    let mut best_pitch: [i32; 2] = [0, 0];
    let mut offset: i32 = 0;
    assert!(len > 0);
    assert!(max_pitch > 0);
    lag = len + max_pitch;
    let vla = (len >> 2) as usize;
    let mut x_lp4: Vec<opus_val16> = ::std::vec::from_elem(0., vla);
    let vla_0 = (lag >> 2) as usize;
    let mut y_lp4: Vec<opus_val16> = ::std::vec::from_elem(0., vla_0);
    let vla_1 = (max_pitch >> 1) as usize;
    let mut xcorr: Vec<opus_val32> = ::std::vec::from_elem(0., vla_1);
    j = 0;
    while j < len >> 2 {
        *x_lp4.as_mut_ptr().offset(j as isize) = *x_lp.offset((2 * j) as isize);
        j += 1;
    }
    j = 0;
    while j < lag >> 2 {
        *y_lp4.as_mut_ptr().offset(j as isize) = *y.offset((2 * j) as isize);
        j += 1;
    }
    celt_pitch_xcorr_c(
        x_lp4.as_mut_ptr(),
        y_lp4.as_mut_ptr(),
        xcorr.as_mut_ptr(),
        len >> 2,
        max_pitch >> 2,
        arch,
    );
    find_best_pitch(
        xcorr.as_mut_ptr(),
        y_lp4.as_mut_ptr(),
        len >> 2,
        max_pitch >> 2,
        best_pitch.as_mut_ptr(),
    );
    i = 0;
    while i < max_pitch >> 1 {
        let mut sum: opus_val32 = 0.;
        *xcorr.as_mut_ptr().offset(i as isize) = 0 as opus_val32;
        if !((i - 2 * best_pitch[0 as usize]).abs() > 2
            && (i - 2 * best_pitch[1 as usize]).abs() > 2)
        {
            sum = celt_inner_prod_c(x_lp, y.offset(i as isize), len >> 1);
            *xcorr.as_mut_ptr().offset(i as isize) = if -1 as f32 > sum { -1 as f32 } else { sum };
        }
        i += 1;
    }
    find_best_pitch(
        xcorr.as_mut_ptr(),
        y,
        len >> 1,
        max_pitch >> 1,
        best_pitch.as_mut_ptr(),
    );
    if best_pitch[0 as usize] > 0 && best_pitch[0 as usize] < (max_pitch >> 1) - 1 {
        let mut a: opus_val32 = 0.;
        let mut b: opus_val32 = 0.;
        let mut c: opus_val32 = 0.;
        a = *xcorr
            .as_mut_ptr()
            .offset((best_pitch[0 as usize] - 1) as isize);
        b = *xcorr.as_mut_ptr().offset(best_pitch[0 as usize] as isize);
        c = *xcorr
            .as_mut_ptr()
            .offset((best_pitch[0 as usize] + 1) as isize);
        if c - a > 0.7f32 * (b - a) {
            offset = 1;
        } else if a - c > 0.7f32 * (b - c) {
            offset = -1;
        } else {
            offset = 0;
        }
    } else {
        offset = 0;
    }
    *pitch = 2 * best_pitch[0 as usize] - offset;
}
unsafe fn compute_pitch_gain(xy: opus_val32, xx: opus_val32, yy: opus_val32) -> opus_val16 {
    return xy / celt_sqrt(1 as f32 + xx * yy);
}
static mut second_check: [i32; 16] = [0, 0, 3, 2, 3, 2, 5, 2, 3, 2, 3, 2, 5, 2, 3, 2];
pub unsafe fn remove_doubling(
    mut x: *mut opus_val16,
    mut maxperiod: i32,
    mut minperiod: i32,
    mut N: i32,
    T0_: *mut i32,
    mut prev_period: i32,
    prev_gain: opus_val16,
    _arch: i32,
) -> opus_val16 {
    let mut k: i32 = 0;
    let mut i: i32 = 0;
    let mut T: i32 = 0;
    let mut T0: i32 = 0;
    let mut g: opus_val16 = 0.;
    let mut g0: opus_val16 = 0.;
    let mut pg: opus_val16 = 0.;
    let mut xy: opus_val32 = 0.;
    let mut xx: opus_val32 = 0.;
    let mut yy: opus_val32 = 0.;
    let mut xy2: opus_val32 = 0.;
    let mut xcorr: [opus_val32; 3] = [0.; 3];
    let mut best_xy: opus_val32 = 0.;
    let mut best_yy: opus_val32 = 0.;
    let mut offset: i32 = 0;
    let mut minperiod0: i32 = 0;
    minperiod0 = minperiod;
    maxperiod /= 2;
    minperiod /= 2;
    *T0_ /= 2;
    prev_period /= 2;
    N /= 2;
    x = x.offset(maxperiod as isize);
    if *T0_ >= maxperiod {
        *T0_ = maxperiod - 1;
    }
    T0 = *T0_;
    T = T0;
    let vla = (maxperiod + 1) as usize;
    let mut yy_lookup: Vec<opus_val32> = ::std::vec::from_elem(0., vla);
    dual_inner_prod_c(x, x, x.offset(-(T0 as isize)), N, &mut xx, &mut xy);
    *yy_lookup.as_mut_ptr().offset(0 as isize) = xx;
    yy = xx;
    i = 1;
    while i <= maxperiod {
        yy = yy + *x.offset(-i as isize) * *x.offset(-i as isize)
            - *x.offset((N - i) as isize) * *x.offset((N - i) as isize);
        *yy_lookup.as_mut_ptr().offset(i as isize) = if 0 as f32 > yy { 0 as f32 } else { yy };
        i += 1;
    }
    yy = *yy_lookup.as_mut_ptr().offset(T0 as isize);
    best_xy = xy;
    best_yy = yy;
    g0 = compute_pitch_gain(xy, xx, yy);
    g = g0;
    k = 2;
    while k <= 15 {
        let mut T1: i32 = 0;
        let mut T1b: i32 = 0;
        let mut g1: opus_val16 = 0.;
        let mut cont: opus_val16 = 0 as opus_val16;
        let mut thresh: opus_val16 = 0.;
        T1 = celt_udiv((2 * T0 + k) as u32, (2 * k) as u32) as i32;
        if T1 < minperiod {
            break;
        }
        if k == 2 {
            if T1 + T0 > maxperiod {
                T1b = T0;
            } else {
                T1b = T0 + T1;
            }
        } else {
            T1b = celt_udiv(
                (2 * second_check[k as usize] * T0 + k) as u32,
                (2 * k) as u32,
            ) as i32;
        }
        dual_inner_prod_c(
            x,
            &mut *x.offset(-T1 as isize),
            &mut *x.offset(-T1b as isize),
            N,
            &mut xy,
            &mut xy2,
        );
        xy = 0.5f32 * (xy + xy2);
        yy = 0.5f32
            * (*yy_lookup.as_mut_ptr().offset(T1 as isize)
                + *yy_lookup.as_mut_ptr().offset(T1b as isize));
        g1 = compute_pitch_gain(xy, xx, yy);
        if (T1 - prev_period).abs() <= 1 {
            cont = prev_gain;
        } else if (T1 - prev_period).abs() <= 2 && 5 * k * k < T0 {
            cont = 0.5f32 * prev_gain;
        } else {
            cont = 0 as opus_val16;
        }
        thresh = if 0.3f32 > 0.7f32 * g0 - cont {
            0.3f32
        } else {
            0.7f32 * g0 - cont
        };
        if T1 < 3 * minperiod {
            thresh = if 0.4f32 > 0.85f32 * g0 - cont {
                0.4f32
            } else {
                0.85f32 * g0 - cont
            };
        } else if T1 < 2 * minperiod {
            thresh = if 0.5f32 > 0.9f32 * g0 - cont {
                0.5f32
            } else {
                0.9f32 * g0 - cont
            };
        }
        if g1 > thresh {
            best_xy = xy;
            best_yy = yy;
            T = T1;
            g = g1;
        }
        k += 1;
    }
    best_xy = if 0 as f32 > best_xy {
        0 as f32
    } else {
        best_xy
    };
    if best_yy <= best_xy {
        pg = Q15ONE;
    } else {
        pg = best_xy / (best_yy + 1 as f32);
    }
    k = 0;
    while k < 3 {
        xcorr[k as usize] = celt_inner_prod_c(x, x.offset(-((T + k - 1) as isize)), N);
        k += 1;
    }
    if xcorr[2 as usize] - xcorr[0 as usize] > 0.7f32 * (xcorr[1 as usize] - xcorr[0 as usize]) {
        offset = 1;
    } else if xcorr[0 as usize] - xcorr[2 as usize]
        > 0.7f32 * (xcorr[1 as usize] - xcorr[2 as usize])
    {
        offset = -1;
    } else {
        offset = 0;
    }
    if pg > g {
        pg = g;
    }
    *T0_ = 2 * T + offset;
    if *T0_ < minperiod0 {
        *T0_ = minperiod0;
    }
    return pg;
}
