use crate::celt::bands::SPREAD_NONE;
use std::f32::consts::PI;

pub mod arch_h {
    pub type opus_val16 = f32;
    pub type opus_val32 = f32;
    pub type celt_norm = f32;
    pub const EPSILON: f32 = 1e-15f32;
}
use self::arch_h::{celt_norm, opus_val16, opus_val32, EPSILON};
use crate::celt::cwrs::{decode_pulses, encode_pulses};
use crate::celt::entcode::celt_udiv;
use crate::celt::entdec::ec_dec;
use crate::celt::entenc::ec_enc;
use crate::celt::mathops::{celt_cos_norm, celt_rsqrt_norm, celt_sqrt, fast_atan2f};
use crate::celt::pitch::celt_inner_prod_c;

unsafe fn exp_rotation1(X: *mut celt_norm, len: i32, stride: i32, c: opus_val16, s: opus_val16) {
    let mut i: i32 = 0;
    let mut ms: opus_val16 = 0.;
    let mut Xptr: *mut celt_norm = 0 as *mut celt_norm;
    Xptr = X;
    ms = -s;
    i = 0;
    while i < len - stride {
        let mut x1: celt_norm = 0.;
        let mut x2: celt_norm = 0.;
        x1 = *Xptr.offset(0 as isize);
        x2 = *Xptr.offset(stride as isize);
        *Xptr.offset(stride as isize) = c * x2 + s * x1;
        let fresh0 = Xptr;
        Xptr = Xptr.offset(1);
        *fresh0 = c * x1 + ms * x2;
        i += 1;
    }
    Xptr = &mut *X.offset((len - 2 * stride - 1) as isize) as *mut celt_norm;
    i = len - 2 * stride - 1;
    while i >= 0 {
        let mut x1_0: celt_norm = 0.;
        let mut x2_0: celt_norm = 0.;
        x1_0 = *Xptr.offset(0 as isize);
        x2_0 = *Xptr.offset(stride as isize);
        *Xptr.offset(stride as isize) = c * x2_0 + s * x1_0;
        let fresh1 = Xptr;
        Xptr = Xptr.offset(-1);
        *fresh1 = c * x1_0 + ms * x2_0;
        i -= 1;
    }
}
pub unsafe fn exp_rotation(
    X: *mut celt_norm,
    mut len: i32,
    dir: i32,
    stride: i32,
    K: i32,
    spread: i32,
) {
    static SPREAD_FACTOR: [i32; 3] = [15, 10, 5];
    let mut i: i32 = 0;
    let mut c: opus_val16 = 0.;
    let mut s: opus_val16 = 0.;
    let mut gain: opus_val16 = 0.;
    let mut theta: opus_val16 = 0.;
    let mut stride2: i32 = 0;
    let mut factor: i32 = 0;
    if 2 * K >= len || spread == SPREAD_NONE {
        return;
    }
    factor = SPREAD_FACTOR[(spread - 1) as usize];
    gain = 1.0f32 * len as opus_val32 / (len + factor * K) as opus_val32;
    theta = 0.5f32 * (gain * gain);
    c = celt_cos_norm(theta);
    s = celt_cos_norm(1.0f32 - theta);
    if len >= 8 * stride {
        stride2 = 1;
        while (stride2 * stride2 + stride2) * stride + (stride >> 2) < len {
            stride2 += 1;
        }
    }
    len = celt_udiv(len as u32, stride as u32) as i32;
    i = 0;
    while i < stride {
        if dir < 0 {
            if stride2 != 0 {
                exp_rotation1(X.offset((i * len) as isize), len, stride2, s, c);
            }
            exp_rotation1(X.offset((i * len) as isize), len, 1, c, s);
        } else {
            exp_rotation1(X.offset((i * len) as isize), len, 1, c, -s);
            if stride2 != 0 {
                exp_rotation1(X.offset((i * len) as isize), len, stride2, s, -c);
            }
        }
        i += 1;
    }
}
unsafe fn normalise_residual(
    iy: *mut i32,
    X: *mut celt_norm,
    N: i32,
    Ryy: opus_val32,
    gain: opus_val16,
) {
    let mut i: i32 = 0;
    let mut t: opus_val32 = 0.;
    let mut g: opus_val16 = 0.;
    t = Ryy;
    g = celt_rsqrt_norm(t) * gain;
    i = 0;
    loop {
        *X.offset(i as isize) = g * *iy.offset(i as isize) as opus_val32;
        i += 1;
        if !(i < N) {
            break;
        }
    }
}
unsafe fn extract_collapse_mask(iy: *mut i32, N: i32, B: i32) -> u32 {
    let mut collapse_mask: u32 = 0;
    let mut N0: i32 = 0;
    let mut i: i32 = 0;
    if B <= 1 {
        return 1;
    }
    N0 = celt_udiv(N as u32, B as u32) as i32;
    collapse_mask = 0;
    i = 0;
    loop {
        let mut j: i32 = 0;
        let mut tmp: u32 = 0;
        j = 0;
        loop {
            tmp |= *iy.offset((i * N0 + j) as isize) as u32;
            j += 1;
            if !(j < N0) {
                break;
            }
        }
        collapse_mask |= (((tmp != 0) as i32) << i) as u32;
        i += 1;
        if !(i < B) {
            break;
        }
    }
    return collapse_mask;
}
pub unsafe fn op_pvq_search_c(
    X: *mut celt_norm,
    iy: *mut i32,
    K: i32,
    N: i32,
    _arch: i32,
) -> opus_val16 {
    let mut i: i32 = 0;
    let mut j: i32 = 0;
    let mut pulsesLeft: i32 = 0;
    let mut sum: opus_val32 = 0.;
    let mut xy: opus_val32 = 0.;
    let mut yy: opus_val16 = 0.;
    let vla = N as usize;
    let mut y: Vec<celt_norm> = ::std::vec::from_elem(0., vla);
    let vla_0 = N as usize;
    let mut signx: Vec<i32> = ::std::vec::from_elem(0, vla_0);
    sum = 0 as opus_val32;
    j = 0;
    loop {
        *signx.as_mut_ptr().offset(j as isize) = (*X.offset(j as isize) < 0 as f32) as i32;
        *X.offset(j as isize) = (*X.offset(j as isize)).abs();
        *iy.offset(j as isize) = 0;
        *y.as_mut_ptr().offset(j as isize) = 0 as celt_norm;
        j += 1;
        if !(j < N) {
            break;
        }
    }
    yy = 0 as opus_val16;
    xy = yy;
    pulsesLeft = K;
    if K > N >> 1 {
        let mut rcp: opus_val16 = 0.;
        j = 0;
        loop {
            sum += *X.offset(j as isize);
            j += 1;
            if !(j < N) {
                break;
            }
        }
        if !(sum > EPSILON && sum < 64 as f32) {
            *X.offset(0 as isize) = 1.0f32;
            j = 1;
            loop {
                *X.offset(j as isize) = 0 as celt_norm;
                j += 1;
                if !(j < N) {
                    break;
                }
            }
            sum = 1.0f32;
        }
        rcp = (K as f32 + 0.8f32) * (1.0f32 / sum);
        j = 0;
        loop {
            *iy.offset(j as isize) = (rcp * *X.offset(j as isize)).floor() as i32;
            *y.as_mut_ptr().offset(j as isize) = *iy.offset(j as isize) as celt_norm;
            yy = yy + *y.as_mut_ptr().offset(j as isize) * *y.as_mut_ptr().offset(j as isize);
            xy = xy + *X.offset(j as isize) * *y.as_mut_ptr().offset(j as isize);
            let ref mut fresh2 = *y.as_mut_ptr().offset(j as isize);
            *fresh2 *= 2 as f32;
            pulsesLeft -= *iy.offset(j as isize);
            j += 1;
            if !(j < N) {
                break;
            }
        }
    }
    if pulsesLeft > N + 3 {
        let tmp: opus_val16 = pulsesLeft as opus_val16;
        yy = yy + tmp * tmp;
        yy = yy + tmp * *y.as_mut_ptr().offset(0 as isize);
        *iy.offset(0 as isize) += pulsesLeft;
        pulsesLeft = 0;
    }
    i = 0;
    while i < pulsesLeft {
        let mut Rxy: opus_val16 = 0.;
        let mut Ryy: opus_val16 = 0.;
        let mut best_id: i32 = 0;
        let mut best_num: opus_val32 = 0.;
        let mut best_den: opus_val16 = 0.;
        best_id = 0;
        yy = yy + 1 as f32;
        Rxy = xy + *X.offset(0 as isize);
        Ryy = yy + *y.as_mut_ptr().offset(0 as isize);
        Rxy = Rxy * Rxy;
        best_den = Ryy;
        best_num = Rxy;
        j = 1;
        loop {
            Rxy = xy + *X.offset(j as isize);
            Ryy = yy + *y.as_mut_ptr().offset(j as isize);
            Rxy = Rxy * Rxy;
            if (best_den * Rxy > Ryy * best_num) as i32 as i64 != 0 {
                best_den = Ryy;
                best_num = Rxy;
                best_id = j;
            }
            j += 1;
            if !(j < N) {
                break;
            }
        }
        xy = xy + *X.offset(best_id as isize);
        yy = yy + *y.as_mut_ptr().offset(best_id as isize);
        let ref mut fresh3 = *y.as_mut_ptr().offset(best_id as isize);
        *fresh3 += 2 as f32;
        let ref mut fresh4 = *iy.offset(best_id as isize);
        *fresh4 += 1;
        i += 1;
    }
    j = 0;
    loop {
        *iy.offset(j as isize) = (*iy.offset(j as isize) ^ -*signx.as_mut_ptr().offset(j as isize))
            + *signx.as_mut_ptr().offset(j as isize);
        j += 1;
        if !(j < N) {
            break;
        }
    }
    return yy;
}
pub unsafe fn alg_quant(
    X: *mut celt_norm,
    N: i32,
    K: i32,
    spread: i32,
    B: i32,
    enc: &mut ec_enc,
    gain: opus_val16,
    resynth: i32,
    arch: i32,
) -> u32 {
    let mut yy: opus_val16 = 0.;
    let mut collapse_mask: u32 = 0;
    assert!(K > 0);
    assert!(N > 1);
    let vla = (N + 3) as usize;
    let mut iy: Vec<i32> = ::std::vec::from_elem(0, vla);
    exp_rotation(X, N, 1, B, K, spread);
    yy = op_pvq_search_c(X, iy.as_mut_ptr(), K, N, arch);
    encode_pulses(iy.as_mut_ptr(), N, K, enc);
    if resynth != 0 {
        normalise_residual(iy.as_mut_ptr(), X, N, yy, gain);
        exp_rotation(X, N, -1, B, K, spread);
    }
    collapse_mask = extract_collapse_mask(iy.as_mut_ptr(), N, B);
    return collapse_mask;
}
pub unsafe fn alg_unquant(
    X: *mut celt_norm,
    N: i32,
    K: i32,
    spread: i32,
    B: i32,
    dec: &mut ec_dec,
    gain: opus_val16,
) -> u32 {
    let mut Ryy: opus_val32 = 0.;
    let mut collapse_mask: u32 = 0;
    assert!(K > 0);
    assert!(N > 1);
    let vla = N as usize;
    let mut iy: Vec<i32> = ::std::vec::from_elem(0, vla);
    Ryy = decode_pulses(iy.as_mut_ptr(), N, K, dec);
    normalise_residual(iy.as_mut_ptr(), X, N, Ryy, gain);
    exp_rotation(X, N, -1, B, K, spread);
    collapse_mask = extract_collapse_mask(iy.as_mut_ptr(), N, B);
    return collapse_mask;
}
pub unsafe fn renormalise_vector(X: *mut celt_norm, N: i32, gain: opus_val16, _arch: i32) {
    let mut i: i32 = 0;
    let mut E: opus_val32 = 0.;
    let mut g: opus_val16 = 0.;
    let mut t: opus_val32 = 0.;
    let mut xptr: *mut celt_norm = 0 as *mut celt_norm;
    E = EPSILON + celt_inner_prod_c(X, X, N);
    t = E;
    g = celt_rsqrt_norm(t) * gain;
    xptr = X;
    i = 0;
    while i < N {
        *xptr = g * *xptr;
        xptr = xptr.offset(1);
        i += 1;
    }
}
pub unsafe fn stereo_itheta(
    X: *const celt_norm,
    Y: *const celt_norm,
    stereo: i32,
    N: i32,
    _arch: i32,
) -> i32 {
    let mut i: i32 = 0;
    let mut itheta: i32 = 0;
    let mut mid: opus_val16 = 0.;
    let mut side: opus_val16 = 0.;
    let mut Emid: opus_val32 = 0.;
    let mut Eside: opus_val32 = 0.;
    Eside = EPSILON;
    Emid = Eside;
    if stereo != 0 {
        i = 0;
        while i < N {
            let mut m: celt_norm = 0.;
            let mut s: celt_norm = 0.;
            m = *X.offset(i as isize) + *Y.offset(i as isize);
            s = *X.offset(i as isize) - *Y.offset(i as isize);
            Emid = Emid + m * m;
            Eside = Eside + s * s;
            i += 1;
        }
    } else {
        Emid += celt_inner_prod_c(X, X, N);
        Eside += celt_inner_prod_c(Y, Y, N);
    }
    mid = celt_sqrt(Emid);
    side = celt_sqrt(Eside);
    itheta = (0.5f32 + 16384 as f32 * 0.63662f32 * fast_atan2f(side, mid)).floor() as i32;
    return itheta;
}
