pub mod arch_h {
    pub type opus_val16 = f32;
    pub type opus_val32 = f32;
    pub type celt_sig = f32;
    pub type celt_norm = f32;
    pub type celt_ener = f32;
    pub const Q15ONE: f32 = 1.0f32;
    pub const NORM_SCALING: f32 = 1.0f32;
    pub const EPSILON: f32 = 1e-15f32;
}
pub mod stack_alloc_h {
    pub const ALLOC_NONE: i32 = 1;
}
pub mod stddef_h {
    pub const NULL: i32 = 0;
}
pub use self::arch_h::{
    celt_ener, celt_norm, celt_sig, opus_val16, opus_val32, EPSILON, NORM_SCALING, Q15ONE,
};
pub use self::stack_alloc_h::ALLOC_NONE;
pub use self::stddef_h::NULL;
use crate::celt::entcode::{celt_sudiv, celt_udiv, ec_ctx, ec_ctx_saved, ec_tell_frac, BITRES};
use crate::celt::entdec::{ec_dec_bit_logp, ec_dec_bits, ec_dec_uint, ec_dec_update, ec_decode};
use crate::celt::entenc::{ec_enc_bit_logp, ec_enc_bits, ec_enc_uint, ec_encode};
use crate::celt::mathops::{celt_exp2, celt_rsqrt, celt_rsqrt_norm, celt_sqrt, isqrt32};
use crate::celt::modes::OpusCustomMode;
use crate::celt::pitch::{celt_inner_prod_c, dual_inner_prod_c};
use crate::celt::quant_bands::eMeans;
use crate::celt::rate::{
    bits2pulses, get_pulses, pulses2bits, QTHETA_OFFSET, QTHETA_OFFSET_TWOPHASE,
};
use crate::celt::vq::{alg_quant, alg_unquant, renormalise_vector, stereo_itheta};
use crate::externs::{memcpy, memset};
use crate::silk::macros::EC_CLZ0;

pub const SPREAD_NONE: i32 = 0;
pub const SPREAD_LIGHT: i32 = 1;
pub const SPREAD_NORMAL: i32 = 2;
pub const SPREAD_AGGRESSIVE: i32 = 3;

#[derive(Copy, Clone)]
#[repr(C)]
pub struct band_ctx<'a> {
    pub encode: i32,
    pub resynth: i32,
    pub m: *const OpusCustomMode,
    pub i: i32,
    pub intensity: i32,
    pub spread: i32,
    pub tf_change: i32,
    pub ec: *mut ec_ctx<'a>,
    pub remaining_bits: i32,
    pub bandE: *const celt_ener,
    pub seed: u32,
    pub arch: i32,
    pub theta_round: i32,
    pub disable_inv: i32,
    pub avoid_split_noise: i32,
}
#[derive(Copy, Clone)]
#[repr(C)]
pub struct split_ctx {
    pub inv: i32,
    pub imid: i32,
    pub iside: i32,
    pub delta: i32,
    pub itheta: i32,
    pub qalloc: i32,
}
pub unsafe fn hysteresis_decision(
    val: opus_val16,
    thresholds: *const opus_val16,
    hysteresis: *const opus_val16,
    N: i32,
    prev: i32,
) -> i32 {
    let mut i: i32 = 0;
    while i < N {
        if val < *thresholds.offset(i as isize) {
            break;
        }
        i += 1;
    }
    if i > prev && val < *thresholds.offset(prev as isize) + *hysteresis.offset(prev as isize) {
        i = prev;
    }
    if i < prev
        && val > *thresholds.offset((prev - 1) as isize) - *hysteresis.offset((prev - 1) as isize)
    {
        i = prev;
    }
    return i;
}
pub unsafe fn celt_lcg_rand(seed: u32) -> u32 {
    return (1664525_u32).wrapping_mul(seed).wrapping_add(1013904223);
}
pub unsafe fn bitexact_cos(x: i16) -> i16 {
    let mut tmp: i32 = 0;
    let mut x2: i16 = 0;
    tmp = 4096 + x as i32 * x as i32 >> 13;
    x2 = tmp as i16;
    x2 = (32767 - x2 as i32
        + (16384
            + x2 as i32
                * (-(7651)
                    + (16384
                        + x2 as i32
                            * (8277 + (16384 + -(626) as i16 as i32 * x2 as i32 >> 15)) as i16
                                as i32
                        >> 15)) as i16 as i32
            >> 15)) as i16;
    return (1 + x2 as i32) as i16;
}
pub unsafe fn bitexact_log2tan(mut isin: i32, mut icos: i32) -> i32 {
    let mut lc: i32 = 0;
    let mut ls: i32 = 0;
    lc = EC_CLZ0 - (icos as u32).leading_zeros() as i32;
    ls = EC_CLZ0 - (isin as u32).leading_zeros() as i32;
    icos <<= 15 - lc;
    isin <<= 15 - ls;
    return (ls - lc) * ((1) << 11)
        + (16384
            + isin as i16 as i32
                * ((16384 + isin as i16 as i32 * -(2597) as i16 as i32 >> 15) + 7932) as i16
                    as i32
            >> 15)
        - (16384
            + icos as i16 as i32
                * ((16384 + icos as i16 as i32 * -(2597) as i16 as i32 >> 15) + 7932) as i16
                    as i32
            >> 15);
}
pub unsafe fn compute_band_energies(
    m: *const OpusCustomMode,
    X: *const celt_sig,
    bandE: *mut celt_ener,
    end: i32,
    C: i32,
    LM: i32,
    _arch: i32,
) {
    let mut i: i32 = 0;
    let mut c: i32 = 0;
    let mut N: i32 = 0;
    let eBands: *const i16 = (*m).eBands;
    N = (*m).shortMdctSize << LM;
    c = 0;
    loop {
        i = 0;
        while i < end {
            let mut sum: opus_val32 = 0.;
            sum = 1e-27f32
                + celt_inner_prod_c(
                    &*X.offset((c * N + ((*eBands.offset(i as isize) as i32) << LM)) as isize),
                    &*X.offset((c * N + ((*eBands.offset(i as isize) as i32) << LM)) as isize),
                    (*eBands.offset((i + 1) as isize) as i32 - *eBands.offset(i as isize) as i32)
                        << LM,
                );
            *bandE.offset((i + c * (*m).nbEBands) as isize) = celt_sqrt(sum);
            i += 1;
        }
        c += 1;
        if !(c < C) {
            break;
        }
    }
}
pub unsafe fn normalise_bands(
    m: *const OpusCustomMode,
    freq: *const celt_sig,
    X: *mut celt_norm,
    bandE: *const celt_ener,
    end: i32,
    C: i32,
    M: i32,
) {
    let mut i: i32 = 0;
    let mut c: i32 = 0;
    let mut N: i32 = 0;
    let eBands: *const i16 = (*m).eBands;
    N = M * (*m).shortMdctSize;
    c = 0;
    loop {
        i = 0;
        while i < end {
            let mut j: i32 = 0;
            let g: opus_val16 =
                1.0f32 / (1e-27f32 + *bandE.offset((i + c * (*m).nbEBands) as isize));
            j = M * *eBands.offset(i as isize) as i32;
            while j < M * *eBands.offset((i + 1) as isize) as i32 {
                *X.offset((j + c * N) as isize) = *freq.offset((j + c * N) as isize) * g;
                j += 1;
            }
            i += 1;
        }
        c += 1;
        if !(c < C) {
            break;
        }
    }
}
pub unsafe fn denormalise_bands(
    m: *const OpusCustomMode,
    X: *const celt_norm,
    freq: *mut celt_sig,
    bandLogE: *const opus_val16,
    mut start: i32,
    mut end: i32,
    M: i32,
    downsample: i32,
    silence: i32,
) {
    let mut i: i32 = 0;
    let mut N: i32 = 0;
    let mut bound: i32 = 0;
    let mut f: *mut celt_sig = 0 as *mut celt_sig;
    let mut x: *const celt_norm = 0 as *const celt_norm;
    let eBands: *const i16 = (*m).eBands;
    N = M * (*m).shortMdctSize;
    bound = M * *eBands.offset(end as isize) as i32;
    if downsample != 1 {
        bound = if bound < N / downsample {
            bound
        } else {
            N / downsample
        };
    }
    if silence != 0 {
        bound = 0;
        end = 0;
        start = end;
    }
    f = freq;
    x = X.offset((M * *eBands.offset(start as isize) as i32) as isize);
    i = 0;
    while i < M * *eBands.offset(start as isize) as i32 {
        let fresh0 = f;
        f = f.offset(1);
        *fresh0 = 0 as celt_sig;
        i += 1;
    }
    i = start;
    while i < end {
        let mut j: i32 = 0;
        let mut band_end: i32 = 0;
        let mut g: opus_val16 = 0.;
        let mut lg: opus_val16 = 0.;
        j = M * *eBands.offset(i as isize) as i32;
        band_end = M * *eBands.offset((i + 1) as isize) as i32;
        lg = *bandLogE.offset(i as isize) + eMeans[i as usize];
        g = celt_exp2(if 32.0 < lg { 32.0f32 } else { lg });
        loop {
            let fresh1 = x;
            x = x.offset(1);
            let fresh2 = f;
            f = f.offset(1);
            *fresh2 = *fresh1 * g;
            j += 1;
            if !(j < band_end) {
                break;
            }
        }
        i += 1;
    }
    assert!(start <= end);
    memset(
        &mut *freq.offset(bound as isize) as *mut celt_sig as *mut core::ffi::c_void,
        0,
        ((N - bound) as u64).wrapping_mul(::core::mem::size_of::<celt_sig>() as u64),
    );
}
pub unsafe fn anti_collapse(
    m: *const OpusCustomMode,
    X_: *mut celt_norm,
    collapse_masks: *mut u8,
    LM: i32,
    C: i32,
    size: i32,
    start: i32,
    end: i32,
    logE: *const opus_val16,
    prev1logE: *const opus_val16,
    prev2logE: *const opus_val16,
    pulses: *const i32,
    mut seed: u32,
    arch: i32,
) {
    let mut c: i32 = 0;
    let mut i: i32 = 0;
    let mut j: i32 = 0;
    let mut k: i32 = 0;
    i = start;
    while i < end {
        let mut N0: i32 = 0;
        let mut thresh: opus_val16 = 0.;
        let mut sqrt_1: opus_val16 = 0.;
        let mut depth: i32 = 0;
        N0 = *((*m).eBands).offset((i + 1) as isize) as i32
            - *((*m).eBands).offset(i as isize) as i32;
        depth = (celt_udiv(
            (1 + *pulses.offset(i as isize)) as u32,
            (*((*m).eBands).offset((i + 1) as isize) as i32
                - *((*m).eBands).offset(i as isize) as i32) as u32,
        ) >> LM) as i32;
        thresh = 0.5f32 * celt_exp2(-0.125f32 * depth as f32);
        sqrt_1 = celt_rsqrt((N0 << LM) as f32);
        c = 0;
        loop {
            let mut X: *mut celt_norm = 0 as *mut celt_norm;
            let mut prev1: opus_val16 = 0.;
            let mut prev2: opus_val16 = 0.;
            let mut Ediff: opus_val32 = 0.;
            let mut r: opus_val16 = 0.;
            let mut renormalize: i32 = 0;
            prev1 = *prev1logE.offset((c * (*m).nbEBands + i) as isize);
            prev2 = *prev2logE.offset((c * (*m).nbEBands + i) as isize);
            if C == 1 {
                prev1 = if prev1 > *prev1logE.offset(((*m).nbEBands + i) as isize) {
                    prev1
                } else {
                    *prev1logE.offset(((*m).nbEBands + i) as isize)
                };
                prev2 = if prev2 > *prev2logE.offset(((*m).nbEBands + i) as isize) {
                    prev2
                } else {
                    *prev2logE.offset(((*m).nbEBands + i) as isize)
                };
            }
            Ediff = *logE.offset((c * (*m).nbEBands + i) as isize)
                - (if prev1 < prev2 { prev1 } else { prev2 });
            Ediff = if 0 as f32 > Ediff { 0 as f32 } else { Ediff };
            r = 2.0f32 * celt_exp2(-Ediff);
            if LM == 3 {
                r *= 1.41421356f32;
            }
            r = if thresh < r { thresh } else { r };
            r = r * sqrt_1;
            X = X_
                .offset((c * size) as isize)
                .offset(((*((*m).eBands).offset(i as isize) as i32) << LM) as isize);
            k = 0;
            while k < (1) << LM {
                if *collapse_masks.offset((i * C + c) as isize) as i32 & (1) << k == 0 {
                    j = 0;
                    while j < N0 {
                        seed = celt_lcg_rand(seed);
                        *X.offset(((j << LM) + k) as isize) =
                            if seed & 0x8000 != 0 { r } else { -r };
                        j += 1;
                    }
                    renormalize = 1;
                }
                k += 1;
            }
            if renormalize != 0 {
                renormalise_vector(X, N0 << LM, Q15ONE, arch);
            }
            c += 1;
            if !(c < C) {
                break;
            }
        }
        i += 1;
    }
}
unsafe fn compute_channel_weights(mut Ex: celt_ener, mut Ey: celt_ener, w: *mut opus_val16) {
    let mut minE: celt_ener = 0.;
    minE = if Ex < Ey { Ex } else { Ey };
    Ex = Ex + minE / 3 as f32;
    Ey = Ey + minE / 3 as f32;
    *w.offset(0 as isize) = Ex;
    *w.offset(1 as isize) = Ey;
}
unsafe fn intensity_stereo(
    m: *const OpusCustomMode,
    X: *mut celt_norm,
    Y: *const celt_norm,
    bandE: *const celt_ener,
    bandID: i32,
    N: i32,
) {
    let i: i32 = bandID;
    let mut j: i32 = 0;
    let mut a1: opus_val16 = 0.;
    let mut a2: opus_val16 = 0.;
    let mut left: opus_val16 = 0.;
    let mut right: opus_val16 = 0.;
    let mut norm: opus_val16 = 0.;
    left = *bandE.offset(i as isize);
    right = *bandE.offset((i + (*m).nbEBands) as isize);
    norm = EPSILON + celt_sqrt(1e-15f32 + left * left + right * right);
    a1 = left / norm;
    a2 = right / norm;
    j = 0;
    while j < N {
        let mut r: celt_norm = 0.;
        let mut l: celt_norm = 0.;
        l = *X.offset(j as isize);
        r = *Y.offset(j as isize);
        *X.offset(j as isize) = a1 * l + a2 * r;
        j += 1;
    }
}
unsafe fn stereo_split(X: *mut celt_norm, Y: *mut celt_norm, N: i32) {
    let mut j: i32 = 0;
    j = 0;
    while j < N {
        let mut r: opus_val32 = 0.;
        let mut l: opus_val32 = 0.;
        l = 0.70710678f32 * *X.offset(j as isize);
        r = 0.70710678f32 * *Y.offset(j as isize);
        *X.offset(j as isize) = l + r;
        *Y.offset(j as isize) = r - l;
        j += 1;
    }
}
unsafe fn stereo_merge(X: *mut celt_norm, Y: *mut celt_norm, mid: opus_val16, N: i32, _arch: i32) {
    let mut j: i32 = 0;
    let mut xp: opus_val32 = 0 as opus_val32;
    let mut side: opus_val32 = 0 as opus_val32;
    let mut El: opus_val32 = 0.;
    let mut Er: opus_val32 = 0.;
    let mut mid2: opus_val16 = 0.;
    let mut t: opus_val32 = 0.;
    let mut lgain: opus_val32 = 0.;
    let mut rgain: opus_val32 = 0.;
    dual_inner_prod_c(Y, X, Y, N, &mut xp, &mut side);
    xp = mid * xp;
    mid2 = mid;
    El = mid2 * mid2 + side - 2 as f32 * xp;
    Er = mid2 * mid2 + side + 2 as f32 * xp;
    if Er < 6e-4f32 || El < 6e-4f32 {
        memcpy(
            Y as *mut core::ffi::c_void,
            X as *const core::ffi::c_void,
            (N as u64)
                .wrapping_mul(::core::mem::size_of::<celt_norm>() as u64)
                .wrapping_add((0 * Y.offset_from(X) as i64) as u64),
        );
        return;
    }
    t = El;
    lgain = celt_rsqrt_norm(t);
    t = Er;
    rgain = celt_rsqrt_norm(t);
    j = 0;
    while j < N {
        let mut r: celt_norm = 0.;
        let mut l: celt_norm = 0.;
        l = mid * *X.offset(j as isize);
        r = *Y.offset(j as isize);
        *X.offset(j as isize) = lgain * (l - r);
        *Y.offset(j as isize) = rgain * (l + r);
        j += 1;
    }
}
pub unsafe fn spreading_decision(
    m: *const OpusCustomMode,
    X: *const celt_norm,
    average: *mut i32,
    last_decision: i32,
    hf_average: *mut i32,
    tapset_decision: *mut i32,
    update_hf: i32,
    end: i32,
    C: i32,
    M: i32,
    spread_weight: *const i32,
) -> i32 {
    let mut i: i32 = 0;
    let mut c: i32 = 0;
    let mut N0: i32 = 0;
    let mut sum: i32 = 0;
    let mut nbBands: i32 = 0;
    let eBands: *const i16 = (*m).eBands;
    let mut decision: i32 = 0;
    let mut hf_sum: i32 = 0;
    assert!(end > 0);
    N0 = M * (*m).shortMdctSize;
    if M * (*eBands.offset(end as isize) as i32 - *eBands.offset((end - 1) as isize) as i32) <= 8 {
        return SPREAD_NONE;
    }
    c = 0;
    loop {
        i = 0;
        while i < end {
            let mut j: i32 = 0;
            let mut N: i32 = 0;
            let mut tmp: i32 = 0;
            let mut tcount: [i32; 3] = [0, 0, 0];
            let x: *const celt_norm = X
                .offset((M * *eBands.offset(i as isize) as i32) as isize)
                .offset((c * N0) as isize);
            N = M * (*eBands.offset((i + 1) as isize) as i32 - *eBands.offset(i as isize) as i32);
            if !(N <= 8) {
                j = 0;
                while j < N {
                    let mut x2N: opus_val32 = 0.;
                    x2N = *x.offset(j as isize) * *x.offset(j as isize) * N as opus_val32;
                    if x2N < 0.25f32 {
                        tcount[0 as usize] += 1;
                    }
                    if x2N < 0.0625f32 {
                        tcount[1 as usize] += 1;
                    }
                    if x2N < 0.015625f32 {
                        tcount[2 as usize] += 1;
                    }
                    j += 1;
                }
                if i > (*m).nbEBands - 4 {
                    hf_sum = (hf_sum as u32).wrapping_add(celt_udiv(
                        (32 * (tcount[1 as usize] + tcount[0 as usize])) as u32,
                        N as u32,
                    )) as i32 as i32;
                }
                tmp = (2 * tcount[2 as usize] >= N) as i32
                    + (2 * tcount[1 as usize] >= N) as i32
                    + (2 * tcount[0 as usize] >= N) as i32;
                sum += tmp * *spread_weight.offset(i as isize);
                nbBands += *spread_weight.offset(i as isize);
            }
            i += 1;
        }
        c += 1;
        if !(c < C) {
            break;
        }
    }
    if update_hf != 0 {
        if hf_sum != 0 {
            hf_sum = celt_udiv(hf_sum as u32, (C * (4 - (*m).nbEBands + end)) as u32) as i32;
        }
        *hf_average = *hf_average + hf_sum >> 1;
        hf_sum = *hf_average;
        if *tapset_decision == 2 {
            hf_sum += 4;
        } else if *tapset_decision == 0 {
            hf_sum -= 4;
        }
        if hf_sum > 22 {
            *tapset_decision = 2;
        } else if hf_sum > 18 {
            *tapset_decision = 1;
        } else {
            *tapset_decision = 0;
        }
    }
    assert!(nbBands > 0);
    assert!(sum >= 0);
    sum = celt_udiv((sum << 8) as u32, nbBands as u32) as i32;
    sum = sum + *average >> 1;
    *average = sum;
    sum = 3 * sum + ((3 - last_decision << 7) + 64) + 2 >> 2;
    if sum < 80 {
        decision = SPREAD_AGGRESSIVE;
    } else if sum < 256 {
        decision = SPREAD_NORMAL;
    } else if sum < 384 {
        decision = SPREAD_LIGHT;
    } else {
        decision = SPREAD_NONE;
    }
    return decision;
}
static mut ordery_table: [i32; 30] = [
    1, 0, 3, 0, 2, 1, 7, 0, 4, 3, 6, 1, 5, 2, 15, 0, 8, 7, 12, 3, 11, 4, 14, 1, 9, 6, 13, 2, 10, 5,
];
unsafe fn deinterleave_hadamard(X: *mut celt_norm, N0: i32, stride: i32, hadamard: i32) {
    let mut i: i32 = 0;
    let mut j: i32 = 0;
    let mut N: i32 = 0;
    N = N0 * stride;
    let vla = N as usize;
    let mut tmp: Vec<celt_norm> = ::std::vec::from_elem(0., vla);
    assert!(stride > 0);
    if hadamard != 0 {
        let ordery: *const i32 = ordery_table
            .as_ptr()
            .offset(stride as isize)
            .offset(-(2 as isize));
        i = 0;
        while i < stride {
            j = 0;
            while j < N0 {
                *tmp.as_mut_ptr()
                    .offset((*ordery.offset(i as isize) * N0 + j) as isize) =
                    *X.offset((j * stride + i) as isize);
                j += 1;
            }
            i += 1;
        }
    } else {
        i = 0;
        while i < stride {
            j = 0;
            while j < N0 {
                *tmp.as_mut_ptr().offset((i * N0 + j) as isize) =
                    *X.offset((j * stride + i) as isize);
                j += 1;
            }
            i += 1;
        }
    }
    memcpy(
        X as *mut core::ffi::c_void,
        tmp.as_mut_ptr() as *const core::ffi::c_void,
        (N as u64)
            .wrapping_mul(::core::mem::size_of::<celt_norm>() as u64)
            .wrapping_add((0 * X.offset_from(tmp.as_mut_ptr()) as i64) as u64),
    );
}
unsafe fn interleave_hadamard(X: *mut celt_norm, N0: i32, stride: i32, hadamard: i32) {
    let mut i: i32 = 0;
    let mut j: i32 = 0;
    let mut N: i32 = 0;
    N = N0 * stride;
    let vla = N as usize;
    let mut tmp: Vec<celt_norm> = ::std::vec::from_elem(0., vla);
    if hadamard != 0 {
        let ordery: *const i32 = ordery_table
            .as_ptr()
            .offset(stride as isize)
            .offset(-(2 as isize));
        i = 0;
        while i < stride {
            j = 0;
            while j < N0 {
                *tmp.as_mut_ptr().offset((j * stride + i) as isize) =
                    *X.offset((*ordery.offset(i as isize) * N0 + j) as isize);
                j += 1;
            }
            i += 1;
        }
    } else {
        i = 0;
        while i < stride {
            j = 0;
            while j < N0 {
                *tmp.as_mut_ptr().offset((j * stride + i) as isize) =
                    *X.offset((i * N0 + j) as isize);
                j += 1;
            }
            i += 1;
        }
    }
    memcpy(
        X as *mut core::ffi::c_void,
        tmp.as_mut_ptr() as *const core::ffi::c_void,
        (N as u64)
            .wrapping_mul(::core::mem::size_of::<celt_norm>() as u64)
            .wrapping_add((0 * X.offset_from(tmp.as_mut_ptr()) as i64) as u64),
    );
}
pub unsafe fn haar1(X: *mut celt_norm, mut N0: i32, stride: i32) {
    let mut i: i32 = 0;
    let mut j: i32 = 0;
    N0 >>= 1;
    i = 0;
    while i < stride {
        j = 0;
        while j < N0 {
            let mut tmp1: opus_val32 = 0.;
            let mut tmp2: opus_val32 = 0.;
            tmp1 = std::f32::consts::FRAC_1_SQRT_2 * *X.offset((stride * 2 * j + i) as isize);
            tmp2 = std::f32::consts::FRAC_1_SQRT_2 * *X.offset((stride * (2 * j + 1) + i) as isize);
            *X.offset((stride * 2 * j + i) as isize) = tmp1 + tmp2;
            *X.offset((stride * (2 * j + 1) + i) as isize) = tmp1 - tmp2;
            j += 1;
        }
        i += 1;
    }
}
unsafe fn compute_qn(N: i32, b: i32, offset: i32, pulse_cap: i32, stereo: i32) -> i32 {
    static mut exp2_table8: [i16; 8] = [16384, 17866, 19483, 21247, 23170, 25267, 27554, 30048];
    let mut qn: i32 = 0;
    let mut qb: i32 = 0;
    let mut N2: i32 = 2 * N - 1;
    if stereo != 0 && N == 2 {
        N2 -= 1;
    }
    qb = celt_sudiv(b + N2 * offset, N2);
    qb = if b - pulse_cap - ((4) << 3) < qb {
        b - pulse_cap - ((4) << 3)
    } else {
        qb
    };
    qb = if ((8) << 3) < qb { (8) << 3 } else { qb };
    if qb < (1) << BITRES >> 1 {
        qn = 1;
    } else {
        qn = exp2_table8[(qb & 0x7) as usize] as i32 >> 14 - (qb >> BITRES);
        qn = (qn + 1 >> 1) << 1;
    }
    assert!(qn <= 256);
    return qn;
}
unsafe fn compute_theta(
    ctx: *mut band_ctx,
    sctx: *mut split_ctx,
    X: *mut celt_norm,
    Y: *mut celt_norm,
    N: i32,
    b: *mut i32,
    B: i32,
    B0: i32,
    LM: i32,
    stereo: i32,
    fill: *mut i32,
) {
    let mut qn: i32 = 0;
    let mut itheta: i32 = 0;
    let mut delta: i32 = 0;
    let mut imid: i32 = 0;
    let mut iside: i32 = 0;
    let mut qalloc: i32 = 0;
    let mut pulse_cap: i32 = 0;
    let mut offset: i32 = 0;
    let mut tell: i32 = 0;
    let mut inv: i32 = 0;
    let mut encode: i32 = 0;
    let mut m: *const OpusCustomMode = 0 as *const OpusCustomMode;
    let mut i: i32 = 0;
    let mut intensity: i32 = 0;
    let mut bandE: *const celt_ener = 0 as *const celt_ener;
    encode = (*ctx).encode;
    m = (*ctx).m;
    i = (*ctx).i;
    intensity = (*ctx).intensity;
    let ec = &mut *(*ctx).ec;
    bandE = (*ctx).bandE;
    pulse_cap = *((*m).logN).offset(i as isize) as i32 + LM * ((1) << BITRES);
    offset = (pulse_cap >> 1)
        - (if stereo != 0 && N == 2 {
            QTHETA_OFFSET_TWOPHASE
        } else {
            QTHETA_OFFSET
        });
    qn = compute_qn(N, *b, offset, pulse_cap, stereo);
    if stereo != 0 && i >= intensity {
        qn = 1;
    }
    if encode != 0 {
        itheta = stereo_itheta(X, Y, stereo, N, (*ctx).arch);
    }
    tell = ec_tell_frac(ec) as i32;
    if qn != 1 {
        if encode != 0 {
            if stereo == 0 || (*ctx).theta_round == 0 {
                itheta = itheta * qn + 8192 >> 14;
                if stereo == 0 && (*ctx).avoid_split_noise != 0 && itheta > 0 && itheta < qn {
                    let unquantized: i32 = celt_udiv((itheta * 16384) as u32, qn as u32) as i32;
                    imid = bitexact_cos(unquantized as i16) as i32;
                    iside = bitexact_cos((16384 - unquantized) as i16) as i32;
                    delta = 16384
                        + ((N - 1) << 7) as i16 as i32
                            * bitexact_log2tan(iside, imid) as i16 as i32
                        >> 15;
                    if delta > *b {
                        itheta = qn;
                    } else if delta < -*b {
                        itheta = 0;
                    }
                }
            } else {
                let mut down: i32 = 0;
                let bias: i32 = if itheta > 8192 {
                    32767 / qn
                } else {
                    -(32767) / qn
                };
                down = if (qn - 1)
                    < (if 0 > itheta * qn + bias >> 14 {
                        0
                    } else {
                        itheta * qn + bias >> 14
                    }) {
                    qn - 1
                } else if 0 > itheta * qn + bias >> 14 {
                    0
                } else {
                    itheta * qn + bias >> 14
                };
                if (*ctx).theta_round < 0 {
                    itheta = down;
                } else {
                    itheta = down + 1;
                }
            }
        }
        if stereo != 0 && N > 2 {
            let p0: i32 = 3;
            let mut x: i32 = itheta;
            let x0: i32 = qn / 2;
            let ft: i32 = p0 * (x0 + 1) + x0;
            if encode != 0 {
                ec_encode(
                    ec,
                    (if x <= x0 {
                        p0 * x
                    } else {
                        x - 1 - x0 + (x0 + 1) * p0
                    }) as u32,
                    (if x <= x0 {
                        p0 * (x + 1)
                    } else {
                        x - x0 + (x0 + 1) * p0
                    }) as u32,
                    ft as u32,
                );
            } else {
                let mut fs: i32 = 0;
                fs = ec_decode(ec, ft as u32) as i32;
                if fs < (x0 + 1) * p0 {
                    x = fs / p0;
                } else {
                    x = x0 + 1 + (fs - (x0 + 1) * p0);
                }
                ec_dec_update(
                    ec,
                    (if x <= x0 {
                        p0 * x
                    } else {
                        x - 1 - x0 + (x0 + 1) * p0
                    }) as u32,
                    (if x <= x0 {
                        p0 * (x + 1)
                    } else {
                        x - x0 + (x0 + 1) * p0
                    }) as u32,
                    ft as u32,
                );
                itheta = x;
            }
        } else if B0 > 1 || stereo != 0 {
            if encode != 0 {
                ec_enc_uint(ec, itheta as u32, (qn + 1) as u32);
            } else {
                itheta = ec_dec_uint(ec, (qn + 1) as u32) as i32;
            }
        } else {
            let mut fs_0: i32 = 1;
            let mut ft_0: i32 = 0;
            ft_0 = ((qn >> 1) + 1) * ((qn >> 1) + 1);
            if encode != 0 {
                let mut fl: i32 = 0;
                fs_0 = if itheta <= qn >> 1 {
                    itheta + 1
                } else {
                    qn + 1 - itheta
                };
                fl = if itheta <= qn >> 1 {
                    itheta * (itheta + 1) >> 1
                } else {
                    ft_0 - ((qn + 1 - itheta) * (qn + 2 - itheta) >> 1)
                };
                ec_encode(ec, fl as u32, (fl + fs_0) as u32, ft_0 as u32);
            } else {
                let mut fl_0: i32 = 0;
                let mut fm: i32 = 0;
                fm = ec_decode(ec, ft_0 as u32) as i32;
                if fm < (qn >> 1) * ((qn >> 1) + 1) >> 1 {
                    itheta = ((isqrt32((8_u32).wrapping_mul(fm as u32).wrapping_add(1)))
                        .wrapping_sub(1)
                        >> 1) as i32;
                    fs_0 = itheta + 1;
                    fl_0 = itheta * (itheta + 1) >> 1;
                } else {
                    itheta = (((2 * (qn + 1)) as u32).wrapping_sub(isqrt32(
                        (8_u32).wrapping_mul((ft_0 - fm - 1) as u32).wrapping_add(1),
                    )) >> 1) as i32;
                    fs_0 = qn + 1 - itheta;
                    fl_0 = ft_0 - ((qn + 1 - itheta) * (qn + 2 - itheta) >> 1);
                }
                ec_dec_update(ec, fl_0 as u32, (fl_0 + fs_0) as u32, ft_0 as u32);
            }
        }
        assert!(itheta >= 0);
        itheta = celt_udiv((itheta * 16384) as u32, qn as u32) as i32;
        if encode != 0 && stereo != 0 {
            if itheta == 0 {
                intensity_stereo(m, X, Y, bandE, i, N);
            } else {
                stereo_split(X, Y, N);
            }
        }
    } else if stereo != 0 {
        if encode != 0 {
            inv = (itheta > 8192 && (*ctx).disable_inv == 0) as i32;
            if inv != 0 {
                let mut j: i32 = 0;
                j = 0;
                while j < N {
                    *Y.offset(j as isize) = -*Y.offset(j as isize);
                    j += 1;
                }
            }
            intensity_stereo(m, X, Y, bandE, i, N);
        }
        if *b > (2) << BITRES && (*ctx).remaining_bits > (2) << BITRES {
            if encode != 0 {
                ec_enc_bit_logp(ec, inv, 2);
            } else {
                inv = ec_dec_bit_logp(ec, 2);
            }
        } else {
            inv = 0;
        }
        if (*ctx).disable_inv != 0 {
            inv = 0;
        }
        itheta = 0;
    }
    qalloc = (ec_tell_frac(ec)).wrapping_sub(tell as u32) as i32;
    *b -= qalloc;
    if itheta == 0 {
        imid = 32767;
        iside = 0;
        *fill &= ((1) << B) - 1;
        delta = -(16384);
    } else if itheta == 16384 {
        imid = 0;
        iside = 32767;
        *fill &= (((1) << B) - 1) << B;
        delta = 16384;
    } else {
        imid = bitexact_cos(itheta as i16) as i32;
        iside = bitexact_cos((16384 - itheta) as i16) as i32;
        delta = 16384 + ((N - 1) << 7) as i16 as i32 * bitexact_log2tan(iside, imid) as i16 as i32
            >> 15;
    }
    (*sctx).inv = inv;
    (*sctx).imid = imid;
    (*sctx).iside = iside;
    (*sctx).delta = delta;
    (*sctx).itheta = itheta;
    (*sctx).qalloc = qalloc;
}
unsafe fn quant_band_n1(
    ctx: *mut band_ctx,
    X: *mut celt_norm,
    Y: *mut celt_norm,
    mut _b: i32,
    lowband_out: *mut celt_norm,
) -> u32 {
    let mut c: i32 = 0;
    let mut stereo: i32 = 0;
    let mut x: *mut celt_norm = X;
    let mut encode: i32 = 0;
    encode = (*ctx).encode;
    let ec = &mut *(*ctx).ec;
    stereo = (Y != NULL as *mut celt_norm) as i32;
    c = 0;
    loop {
        let mut sign: i32 = 0;
        if (*ctx).remaining_bits >= (1) << BITRES {
            if encode != 0 {
                sign = (*x.offset(0 as isize) < 0 as f32) as i32;
                ec_enc_bits(ec, sign as u32, 1);
            } else {
                sign = ec_dec_bits(ec, 1) as i32;
            }
            (*ctx).remaining_bits -= (1) << BITRES;
            _b -= (1) << BITRES;
        }
        if (*ctx).resynth != 0 {
            *x.offset(0 as isize) = if sign != 0 {
                -NORM_SCALING
            } else {
                NORM_SCALING
            };
        }
        x = Y;
        c += 1;
        if !(c < 1 + stereo) {
            break;
        }
    }
    if !lowband_out.is_null() {
        *lowband_out.offset(0 as isize) = *X.offset(0 as isize);
    }
    return 1;
}
unsafe fn quant_partition(
    ctx: *mut band_ctx,
    X: *mut celt_norm,
    mut N: i32,
    mut b: i32,
    mut B: i32,
    lowband: *mut celt_norm,
    mut LM: i32,
    gain: opus_val16,
    mut fill: i32,
) -> u32 {
    let mut cache: *const u8 = 0 as *const u8;
    let mut q: i32 = 0;
    let mut curr_bits: i32 = 0;
    let mut imid: i32 = 0;
    let mut iside: i32 = 0;
    let B0: i32 = B;
    let mut mid: opus_val16 = 0 as opus_val16;
    let mut side: opus_val16 = 0 as opus_val16;
    let mut cm: u32 = 0;
    let mut Y: *mut celt_norm = NULL as *mut celt_norm;
    let mut encode: i32 = 0;
    let mut m: *const OpusCustomMode = 0 as *const OpusCustomMode;
    let mut i: i32 = 0;
    let mut spread: i32 = 0;
    encode = (*ctx).encode;
    m = (*ctx).m;
    i = (*ctx).i;
    spread = (*ctx).spread;
    let ec = &mut *(*ctx).ec;
    cache = ((*m).cache.bits).offset(
        *((*m).cache.index).offset(((LM + 1) * (*m).nbEBands + i) as isize) as i32 as isize,
    );
    if LM != -1 && b > *cache.offset(*cache.offset(0 as isize) as isize) as i32 + 12 && N > 2 {
        let mut mbits: i32 = 0;
        let mut sbits: i32 = 0;
        let mut delta: i32 = 0;
        let mut itheta: i32 = 0;
        let mut qalloc: i32 = 0;
        let mut sctx: split_ctx = split_ctx {
            inv: 0,
            imid: 0,
            iside: 0,
            delta: 0,
            itheta: 0,
            qalloc: 0,
        };
        let mut next_lowband2: *mut celt_norm = NULL as *mut celt_norm;
        let mut rebalance: i32 = 0;
        N >>= 1;
        Y = X.offset(N as isize);
        LM -= 1;
        if B == 1 {
            fill = fill & 1 | fill << 1;
        }
        B = B + 1 >> 1;
        compute_theta(ctx, &mut sctx, X, Y, N, &mut b, B, B0, LM, 0, &mut fill);
        imid = sctx.imid;
        iside = sctx.iside;
        delta = sctx.delta;
        itheta = sctx.itheta;
        qalloc = sctx.qalloc;
        mid = 1.0f32 / 32768 as f32 * imid as f32;
        side = 1.0f32 / 32768 as f32 * iside as f32;
        if B0 > 1 && itheta & 0x3fff != 0 {
            if itheta > 8192 {
                delta -= delta >> 4 - LM;
            } else {
                delta = if (0) < delta + (N << 3 >> 5 - LM) {
                    0
                } else {
                    delta + (N << 3 >> 5 - LM)
                };
            }
        }
        mbits = if 0
            > (if b < (b - delta) / 2 {
                b
            } else {
                (b - delta) / 2
            }) {
            0
        } else if b < (b - delta) / 2 {
            b
        } else {
            (b - delta) / 2
        };
        sbits = b - mbits;
        (*ctx).remaining_bits -= qalloc;
        if !lowband.is_null() {
            next_lowband2 = lowband.offset(N as isize);
        }
        rebalance = (*ctx).remaining_bits;
        if mbits >= sbits {
            cm = quant_partition(ctx, X, N, mbits, B, lowband, LM, gain * mid, fill);
            rebalance = mbits - (rebalance - (*ctx).remaining_bits);
            if rebalance > (3) << BITRES && itheta != 0 {
                sbits += rebalance - ((3) << BITRES);
            }
            cm |= quant_partition(
                ctx,
                Y,
                N,
                sbits,
                B,
                next_lowband2,
                LM,
                gain * side,
                fill >> B,
            ) << (B0 >> 1);
        } else {
            cm = quant_partition(
                ctx,
                Y,
                N,
                sbits,
                B,
                next_lowband2,
                LM,
                gain * side,
                fill >> B,
            ) << (B0 >> 1);
            rebalance = sbits - (rebalance - (*ctx).remaining_bits);
            if rebalance > (3) << BITRES && itheta != 16384 {
                mbits += rebalance - ((3) << BITRES);
            }
            cm |= quant_partition(ctx, X, N, mbits, B, lowband, LM, gain * mid, fill);
        }
    } else {
        q = bits2pulses(m, i, LM, b);
        curr_bits = pulses2bits(m, i, LM, q);
        (*ctx).remaining_bits -= curr_bits;
        while (*ctx).remaining_bits < 0 && q > 0 {
            (*ctx).remaining_bits += curr_bits;
            q -= 1;
            curr_bits = pulses2bits(m, i, LM, q);
            (*ctx).remaining_bits -= curr_bits;
        }
        if q != 0 {
            let K: i32 = get_pulses(q);
            if encode != 0 {
                cm = alg_quant(
                    X,
                    N,
                    K,
                    spread,
                    B,
                    &mut *ec,
                    gain,
                    (*ctx).resynth,
                    (*ctx).arch,
                );
            } else {
                cm = alg_unquant(X, N, K, spread, B, ec, gain);
            }
        } else {
            let mut j: i32 = 0;
            if (*ctx).resynth != 0 {
                let mut cm_mask: u32 = 0;
                cm_mask = (((1 as u64) << B) as u32).wrapping_sub(1);
                fill = (fill as u32 & cm_mask) as i32;
                if fill == 0 {
                    memset(
                        X as *mut core::ffi::c_void,
                        0,
                        (N as u64).wrapping_mul(::core::mem::size_of::<celt_norm>() as u64),
                    );
                } else {
                    if lowband.is_null() {
                        j = 0;
                        while j < N {
                            (*ctx).seed = celt_lcg_rand((*ctx).seed);
                            *X.offset(j as isize) = ((*ctx).seed as i32 >> 20) as celt_norm;
                            j += 1;
                        }
                        cm = cm_mask;
                    } else {
                        j = 0;
                        while j < N {
                            let mut tmp: opus_val16 = 0.;
                            (*ctx).seed = celt_lcg_rand((*ctx).seed);
                            tmp = 1.0f32 / 256 as f32;
                            tmp = if (*ctx).seed & 0x8000 != 0 { tmp } else { -tmp };
                            *X.offset(j as isize) = *lowband.offset(j as isize) + tmp;
                            j += 1;
                        }
                        cm = fill as u32;
                    }
                    renormalise_vector(X, N, gain, (*ctx).arch);
                }
            }
        }
    }
    return cm;
}
unsafe fn quant_band(
    ctx: *mut band_ctx,
    X: *mut celt_norm,
    N: i32,
    b: i32,
    mut B: i32,
    mut lowband: *mut celt_norm,
    LM: i32,
    lowband_out: *mut celt_norm,
    gain: opus_val16,
    lowband_scratch: *mut celt_norm,
    mut fill: i32,
) -> u32 {
    let N0: i32 = N;
    let mut N_B: i32 = N;
    let mut N_B0: i32 = 0;
    let mut B0: i32 = B;
    let mut time_divide: i32 = 0;
    let mut recombine: i32 = 0;
    let mut longBlocks: i32 = 0;
    let mut cm: u32 = 0;
    let mut k: i32 = 0;
    let mut encode: i32 = 0;
    let mut tf_change: i32 = 0;
    encode = (*ctx).encode;
    tf_change = (*ctx).tf_change;
    longBlocks = (B0 == 1) as i32;
    N_B = celt_udiv(N_B as u32, B as u32) as i32;
    if N == 1 {
        return quant_band_n1(ctx, X, NULL as *mut celt_norm, b, lowband_out);
    }
    if tf_change > 0 {
        recombine = tf_change;
    }
    if !lowband_scratch.is_null()
        && !lowband.is_null()
        && (recombine != 0 || N_B & 1 == 0 && tf_change < 0 || B0 > 1)
    {
        memcpy(
            lowband_scratch as *mut core::ffi::c_void,
            lowband as *const core::ffi::c_void,
            (N as u64)
                .wrapping_mul(::core::mem::size_of::<celt_norm>() as u64)
                .wrapping_add((0 * lowband_scratch.offset_from(lowband) as i64) as u64),
        );
        lowband = lowband_scratch;
    }
    k = 0;
    while k < recombine {
        static mut bit_interleave_table: [u8; 16] =
            [0, 1, 1, 1, 2, 3, 3, 3, 2, 3, 3, 3, 2, 3, 3, 3];
        if encode != 0 {
            haar1(X, N >> k, (1) << k);
        }
        if !lowband.is_null() {
            haar1(lowband, N >> k, (1) << k);
        }
        fill = bit_interleave_table[(fill & 0xf) as usize] as i32
            | (bit_interleave_table[(fill >> 4) as usize] as i32) << 2;
        k += 1;
    }
    B >>= recombine;
    N_B <<= recombine;
    while N_B & 1 == 0 && tf_change < 0 {
        if encode != 0 {
            haar1(X, N_B, B);
        }
        if !lowband.is_null() {
            haar1(lowband, N_B, B);
        }
        fill |= fill << B;
        B <<= 1;
        N_B >>= 1;
        time_divide += 1;
        tf_change += 1;
    }
    B0 = B;
    N_B0 = N_B;
    if B0 > 1 {
        if encode != 0 {
            deinterleave_hadamard(X, N_B >> recombine, B0 << recombine, longBlocks);
        }
        if !lowband.is_null() {
            deinterleave_hadamard(lowband, N_B >> recombine, B0 << recombine, longBlocks);
        }
    }
    cm = quant_partition(ctx, X, N, b, B, lowband, LM, gain, fill);
    if (*ctx).resynth != 0 {
        if B0 > 1 {
            interleave_hadamard(X, N_B >> recombine, B0 << recombine, longBlocks);
        }
        N_B = N_B0;
        B = B0;
        k = 0;
        while k < time_divide {
            B >>= 1;
            N_B <<= 1;
            cm |= cm >> B;
            haar1(X, N_B, B);
            k += 1;
        }
        k = 0;
        while k < recombine {
            static mut bit_deinterleave_table: [u8; 16] = [
                0, 0x3, 0xc, 0xf, 0x30, 0x33, 0x3c, 0x3f, 0xc0, 0xc3, 0xcc, 0xcf, 0xf0, 0xf3, 0xfc,
                0xff,
            ];
            cm = bit_deinterleave_table[cm as usize] as u32;
            haar1(X, N0 >> k, (1) << k);
            k += 1;
        }
        B <<= recombine;
        if !lowband_out.is_null() {
            let mut j: i32 = 0;
            let mut n: opus_val16 = 0.;
            n = celt_sqrt(N0 as f32);
            j = 0;
            while j < N0 {
                *lowband_out.offset(j as isize) = n * *X.offset(j as isize);
                j += 1;
            }
        }
        cm &= (((1) << B) - 1) as u32;
    }
    return cm;
}
unsafe fn quant_band_stereo(
    ctx: *mut band_ctx,
    X: *mut celt_norm,
    Y: *mut celt_norm,
    N: i32,
    mut b: i32,
    B: i32,
    lowband: *mut celt_norm,
    LM: i32,
    lowband_out: *mut celt_norm,
    lowband_scratch: *mut celt_norm,
    mut fill: i32,
) -> u32 {
    let mut imid: i32 = 0;
    let mut iside: i32 = 0;
    let mut inv: i32 = 0;
    let mut mid: opus_val16 = 0 as opus_val16;
    let mut side: opus_val16 = 0 as opus_val16;
    let mut cm: u32 = 0;
    let mut mbits: i32 = 0;
    let mut sbits: i32 = 0;
    let mut delta: i32 = 0;
    let mut itheta: i32 = 0;
    let mut qalloc: i32 = 0;
    let mut sctx: split_ctx = split_ctx {
        inv: 0,
        imid: 0,
        iside: 0,
        delta: 0,
        itheta: 0,
        qalloc: 0,
    };
    let mut orig_fill: i32 = 0;
    let mut encode: i32 = 0;
    encode = (*ctx).encode;
    let ec = &mut *(*ctx).ec;
    if N == 1 {
        return quant_band_n1(ctx, X, Y, b, lowband_out);
    }
    orig_fill = fill;
    compute_theta(ctx, &mut sctx, X, Y, N, &mut b, B, B, LM, 1, &mut fill);
    inv = sctx.inv;
    imid = sctx.imid;
    iside = sctx.iside;
    delta = sctx.delta;
    itheta = sctx.itheta;
    qalloc = sctx.qalloc;
    mid = 1.0f32 / 32768 as f32 * imid as f32;
    side = 1.0f32 / 32768 as f32 * iside as f32;
    if N == 2 {
        let mut c: i32 = 0;
        let mut sign: i32 = 0;
        let mut x2: *mut celt_norm = 0 as *mut celt_norm;
        let mut y2: *mut celt_norm = 0 as *mut celt_norm;
        mbits = b;
        sbits = 0;
        if itheta != 0 && itheta != 16384 {
            sbits = (1) << BITRES;
        }
        mbits -= sbits;
        c = (itheta > 8192) as i32;
        (*ctx).remaining_bits -= qalloc + sbits;
        x2 = if c != 0 { Y } else { X };
        y2 = if c != 0 { X } else { Y };
        if sbits != 0 {
            if encode != 0 {
                sign = (*x2.offset(0 as isize) * *y2.offset(1 as isize)
                    - *x2.offset(1 as isize) * *y2.offset(0 as isize)
                    < 0 as f32) as i32;
                ec_enc_bits(ec, sign as u32, 1);
            } else {
                sign = ec_dec_bits(ec, 1) as i32;
            }
        }
        sign = 1 - 2 * sign;
        cm = quant_band(
            ctx,
            x2,
            N,
            mbits,
            B,
            lowband,
            LM,
            lowband_out,
            Q15ONE,
            lowband_scratch,
            orig_fill,
        );
        *y2.offset(0 as isize) = -sign as f32 * *x2.offset(1 as isize);
        *y2.offset(1 as isize) = sign as f32 * *x2.offset(0 as isize);
        if (*ctx).resynth != 0 {
            let mut tmp: celt_norm = 0.;
            *X.offset(0 as isize) = mid * *X.offset(0 as isize);
            *X.offset(1 as isize) = mid * *X.offset(1 as isize);
            *Y.offset(0 as isize) = side * *Y.offset(0 as isize);
            *Y.offset(1 as isize) = side * *Y.offset(1 as isize);
            tmp = *X.offset(0 as isize);
            *X.offset(0 as isize) = tmp - *Y.offset(0 as isize);
            *Y.offset(0 as isize) = tmp + *Y.offset(0 as isize);
            tmp = *X.offset(1 as isize);
            *X.offset(1 as isize) = tmp - *Y.offset(1 as isize);
            *Y.offset(1 as isize) = tmp + *Y.offset(1 as isize);
        }
    } else {
        let mut rebalance: i32 = 0;
        mbits = if 0
            > (if b < (b - delta) / 2 {
                b
            } else {
                (b - delta) / 2
            }) {
            0
        } else if b < (b - delta) / 2 {
            b
        } else {
            (b - delta) / 2
        };
        sbits = b - mbits;
        (*ctx).remaining_bits -= qalloc;
        rebalance = (*ctx).remaining_bits;
        if mbits >= sbits {
            cm = quant_band(
                ctx,
                X,
                N,
                mbits,
                B,
                lowband,
                LM,
                lowband_out,
                Q15ONE,
                lowband_scratch,
                fill,
            );
            rebalance = mbits - (rebalance - (*ctx).remaining_bits);
            if rebalance > (3) << BITRES && itheta != 0 {
                sbits += rebalance - ((3) << BITRES);
            }
            cm |= quant_band(
                ctx,
                Y,
                N,
                sbits,
                B,
                NULL as *mut celt_norm,
                LM,
                NULL as *mut celt_norm,
                side,
                NULL as *mut celt_norm,
                fill >> B,
            );
        } else {
            cm = quant_band(
                ctx,
                Y,
                N,
                sbits,
                B,
                NULL as *mut celt_norm,
                LM,
                NULL as *mut celt_norm,
                side,
                NULL as *mut celt_norm,
                fill >> B,
            );
            rebalance = sbits - (rebalance - (*ctx).remaining_bits);
            if rebalance > (3) << BITRES && itheta != 16384 {
                mbits += rebalance - ((3) << BITRES);
            }
            cm |= quant_band(
                ctx,
                X,
                N,
                mbits,
                B,
                lowband,
                LM,
                lowband_out,
                Q15ONE,
                lowband_scratch,
                fill,
            );
        }
    }
    if (*ctx).resynth != 0 {
        if N != 2 {
            stereo_merge(X, Y, mid, N, (*ctx).arch);
        }
        if inv != 0 {
            let mut j: i32 = 0;
            j = 0;
            while j < N {
                *Y.offset(j as isize) = -*Y.offset(j as isize);
                j += 1;
            }
        }
    }
    return cm;
}
unsafe fn special_hybrid_folding(
    m: *const OpusCustomMode,
    norm: *mut celt_norm,
    norm2: *mut celt_norm,
    start: i32,
    M: i32,
    dual_stereo: i32,
) {
    let mut n1: i32 = 0;
    let mut n2: i32 = 0;
    let eBands: *const i16 = (*m).eBands;
    n1 = M * (*eBands.offset((start + 1) as isize) as i32 - *eBands.offset(start as isize) as i32);
    n2 = M
        * (*eBands.offset((start + 2) as isize) as i32
            - *eBands.offset((start + 1) as isize) as i32);
    memcpy(
        &mut *norm.offset(n1 as isize) as *mut celt_norm as *mut core::ffi::c_void,
        &mut *norm.offset((2 * n1 - n2) as isize) as *mut celt_norm as *const core::ffi::c_void,
        ((n2 - n1) as u64)
            .wrapping_mul(::core::mem::size_of::<celt_norm>() as u64)
            .wrapping_add(
                (0 * (&mut *norm.offset(n1 as isize) as *mut celt_norm)
                    .offset_from(&mut *norm.offset((2 * n1 - n2) as isize))
                    as i64) as u64,
            ),
    );
    if dual_stereo != 0 {
        memcpy(
            &mut *norm2.offset(n1 as isize) as *mut celt_norm as *mut core::ffi::c_void,
            &mut *norm2.offset((2 * n1 - n2) as isize) as *mut celt_norm
                as *const core::ffi::c_void,
            ((n2 - n1) as u64)
                .wrapping_mul(::core::mem::size_of::<celt_norm>() as u64)
                .wrapping_add(
                    (0 * (&mut *norm2.offset(n1 as isize) as *mut celt_norm)
                        .offset_from(&mut *norm2.offset((2 * n1 - n2) as isize))
                        as i64) as u64,
                ),
        );
    }
}
pub unsafe fn quant_all_bands(
    encode: i32,
    m: *const OpusCustomMode,
    start: i32,
    end: i32,
    X_: *mut celt_norm,
    Y_: *mut celt_norm,
    collapse_masks: *mut u8,
    bandE: *const celt_ener,
    pulses: *mut i32,
    shortBlocks: i32,
    spread: i32,
    mut dual_stereo: i32,
    intensity: i32,
    tf_res: *mut i32,
    total_bits: i32,
    mut balance: i32,
    ec: &mut ec_ctx,
    LM: i32,
    codedBands: i32,
    seed: *mut u32,
    complexity: i32,
    arch: i32,
    disable_inv: i32,
) {
    let mut i: i32 = 0;
    let mut remaining_bits: i32 = 0;
    let eBands: *const i16 = (*m).eBands;
    let mut norm: *mut celt_norm = 0 as *mut celt_norm;
    let mut norm2: *mut celt_norm = 0 as *mut celt_norm;
    let mut resynth_alloc: i32 = 0;
    let mut lowband_scratch: *mut celt_norm = 0 as *mut celt_norm;
    let mut B: i32 = 0;
    let mut M: i32 = 0;
    let mut lowband_offset: i32 = 0;
    let mut update_lowband: i32 = 1;
    let C: i32 = if !Y_.is_null() { 2 } else { 1 };
    let mut norm_offset: i32 = 0;
    let theta_rdo: i32 =
        (encode != 0 && !Y_.is_null() && dual_stereo == 0 && complexity >= 8) as i32;
    let resynth: i32 = (encode == 0 || theta_rdo != 0) as i32;
    let mut ctx: band_ctx = band_ctx {
        encode: 0,
        resynth: 0,
        m: 0 as *const OpusCustomMode,
        i: 0,
        intensity: 0,
        spread: 0,
        tf_change: 0,
        ec: 0 as *mut ec_ctx,
        remaining_bits: 0,
        bandE: 0 as *const celt_ener,
        seed: 0,
        arch: 0,
        theta_round: 0,
        disable_inv: 0,
        avoid_split_noise: 0,
    };
    M = (1) << LM;
    B = if shortBlocks != 0 { M } else { 1 };
    norm_offset = M * *eBands.offset(start as isize) as i32;
    let vla =
        (C * (M * *eBands.offset(((*m).nbEBands - 1) as isize) as i32 - norm_offset)) as usize;
    let mut _norm: Vec<celt_norm> = ::std::vec::from_elem(0., vla);
    norm = _norm.as_mut_ptr();
    norm2 = norm
        .offset((M * *eBands.offset(((*m).nbEBands - 1) as isize) as i32) as isize)
        .offset(-(norm_offset as isize));
    if encode != 0 && resynth != 0 {
        resynth_alloc = M
            * (*eBands.offset((*m).nbEBands as isize) as i32
                - *eBands.offset(((*m).nbEBands - 1) as isize) as i32);
    } else {
        resynth_alloc = ALLOC_NONE;
    }
    let vla_0 = resynth_alloc as usize;
    let mut _lowband_scratch: Vec<celt_norm> = ::std::vec::from_elem(0., vla_0);
    if encode != 0 && resynth != 0 {
        lowband_scratch = _lowband_scratch.as_mut_ptr();
    } else {
        lowband_scratch =
            X_.offset((M * *eBands.offset(((*m).nbEBands - 1) as isize) as i32) as isize);
    }
    let vla_1 = resynth_alloc as usize;
    let mut X_save: Vec<celt_norm> = ::std::vec::from_elem(0., vla_1);
    let vla_2 = resynth_alloc as usize;
    let mut Y_save: Vec<celt_norm> = ::std::vec::from_elem(0., vla_2);
    let vla_3 = resynth_alloc as usize;
    let mut X_save2: Vec<celt_norm> = ::std::vec::from_elem(0., vla_3);
    let vla_4 = resynth_alloc as usize;
    let mut Y_save2: Vec<celt_norm> = ::std::vec::from_elem(0., vla_4);
    let vla_5 = resynth_alloc as usize;
    let mut norm_save2: Vec<celt_norm> = ::std::vec::from_elem(0., vla_5);
    lowband_offset = 0;
    ctx.bandE = bandE;
    ctx.ec = ec;
    ctx.encode = encode;
    ctx.intensity = intensity;
    ctx.m = m;
    ctx.seed = *seed;
    ctx.spread = spread;
    ctx.arch = arch;
    ctx.disable_inv = disable_inv;
    ctx.resynth = resynth;
    ctx.theta_round = 0;
    ctx.avoid_split_noise = (B > 1) as i32;
    i = start;
    while i < end {
        let mut tell: i32 = 0;
        let mut b: i32 = 0;
        let mut N: i32 = 0;
        let mut curr_balance: i32 = 0;
        let mut effective_lowband: i32 = -1;
        let mut X: *mut celt_norm = 0 as *mut celt_norm;
        let mut Y: *mut celt_norm = 0 as *mut celt_norm;
        let mut tf_change: i32 = 0;
        let mut x_cm: u32 = 0;
        let mut y_cm: u32 = 0;
        let mut last: i32 = 0;
        ctx.i = i;
        last = (i == end - 1) as i32;
        X = X_.offset((M * *eBands.offset(i as isize) as i32) as isize);
        if !Y_.is_null() {
            Y = Y_.offset((M * *eBands.offset(i as isize) as i32) as isize);
        } else {
            Y = NULL as *mut celt_norm;
        }
        N = M * *eBands.offset((i + 1) as isize) as i32 - M * *eBands.offset(i as isize) as i32;
        assert!(N > 0);
        tell = ec_tell_frac(&mut *ec) as i32;
        if i != start {
            balance -= tell;
        }
        remaining_bits = total_bits - tell - 1;
        ctx.remaining_bits = remaining_bits;
        if i <= codedBands - 1 {
            curr_balance = celt_sudiv(
                balance,
                if (3) < codedBands - i {
                    3
                } else {
                    codedBands - i
                },
            );
            b =
                if 0 > (if (16383)
                    < (if (remaining_bits + 1) < *pulses.offset(i as isize) + curr_balance {
                        remaining_bits + 1
                    } else {
                        *pulses.offset(i as isize) + curr_balance
                    }) {
                    16383
                } else {
                    if (remaining_bits + 1) < *pulses.offset(i as isize) + curr_balance {
                        remaining_bits + 1
                    } else {
                        *pulses.offset(i as isize) + curr_balance
                    }
                }) {
                    0
                } else if (16383)
                    < (if (remaining_bits + 1) < *pulses.offset(i as isize) + curr_balance {
                        remaining_bits + 1
                    } else {
                        *pulses.offset(i as isize) + curr_balance
                    })
                {
                    16383
                } else if (remaining_bits + 1) < *pulses.offset(i as isize) + curr_balance {
                    remaining_bits + 1
                } else {
                    *pulses.offset(i as isize) + curr_balance
                };
        } else {
            b = 0;
        }
        if resynth != 0
            && (M * *eBands.offset(i as isize) as i32 - N
                >= M * *eBands.offset(start as isize) as i32
                || i == start + 1)
            && (update_lowband != 0 || lowband_offset == 0)
        {
            lowband_offset = i;
        }
        if i == start + 1 {
            special_hybrid_folding(m, norm, norm2, start, M, dual_stereo);
        }
        tf_change = *tf_res.offset(i as isize);
        ctx.tf_change = tf_change;
        if i >= (*m).effEBands {
            X = norm;
            if !Y_.is_null() {
                Y = norm;
            }
            lowband_scratch = NULL as *mut celt_norm;
        }
        if last != 0 && theta_rdo == 0 {
            lowband_scratch = NULL as *mut celt_norm;
        }
        if lowband_offset != 0 && (spread != SPREAD_AGGRESSIVE || B > 1 || tf_change < 0) {
            let mut fold_start: i32 = 0;
            let mut fold_end: i32 = 0;
            let mut fold_i: i32 = 0;
            effective_lowband =
                if 0 > M * *eBands.offset(lowband_offset as isize) as i32 - norm_offset - N {
                    0
                } else {
                    M * *eBands.offset(lowband_offset as isize) as i32 - norm_offset - N
                };
            fold_start = lowband_offset;
            loop {
                fold_start -= 1;
                if !(M * *eBands.offset(fold_start as isize) as i32
                    > effective_lowband + norm_offset)
                {
                    break;
                }
            }
            fold_end = lowband_offset - 1;
            loop {
                fold_end += 1;
                if !(fold_end < i
                    && (M * *eBands.offset(fold_end as isize) as i32)
                        < effective_lowband + norm_offset + N)
                {
                    break;
                }
            }
            y_cm = 0;
            x_cm = y_cm;
            fold_i = fold_start;
            loop {
                x_cm |= *collapse_masks.offset((fold_i * C + 0) as isize) as u32;
                y_cm |= *collapse_masks.offset((fold_i * C + C - 1) as isize) as u32;
                fold_i += 1;
                if !(fold_i < fold_end) {
                    break;
                }
            }
        } else {
            y_cm = (((1) << B) - 1) as u32;
            x_cm = y_cm;
        }
        if dual_stereo != 0 && i == intensity {
            let mut j: i32 = 0;
            dual_stereo = 0;
            if resynth != 0 {
                j = 0;
                while j < M * *eBands.offset(i as isize) as i32 - norm_offset {
                    *norm.offset(j as isize) =
                        0.5f32 * (*norm.offset(j as isize) + *norm2.offset(j as isize));
                    j += 1;
                }
            }
        }
        if dual_stereo != 0 {
            x_cm = quant_band(
                &mut ctx,
                X,
                N,
                b / 2,
                B,
                if effective_lowband != -1 {
                    norm.offset(effective_lowband as isize)
                } else {
                    NULL as *mut celt_norm
                },
                LM,
                if last != 0 {
                    NULL as *mut celt_norm
                } else {
                    norm.offset((M * *eBands.offset(i as isize) as i32) as isize)
                        .offset(-(norm_offset as isize))
                },
                Q15ONE,
                lowband_scratch,
                x_cm as i32,
            );
            y_cm = quant_band(
                &mut ctx,
                Y,
                N,
                b / 2,
                B,
                if effective_lowband != -1 {
                    norm2.offset(effective_lowband as isize)
                } else {
                    NULL as *mut celt_norm
                },
                LM,
                if last != 0 {
                    NULL as *mut celt_norm
                } else {
                    norm2
                        .offset((M * *eBands.offset(i as isize) as i32) as isize)
                        .offset(-(norm_offset as isize))
                },
                Q15ONE,
                lowband_scratch,
                y_cm as i32,
            );
        } else {
            if !Y.is_null() {
                if theta_rdo != 0 && i < intensity {
                    let mut ec_save = ec_ctx_saved::default();
                    let mut ec_save2 = ec_ctx_saved::default();
                    let mut ctx_save: band_ctx = band_ctx {
                        encode: 0,
                        resynth: 0,
                        m: 0 as *const OpusCustomMode,
                        i: 0,
                        intensity: 0,
                        spread: 0,
                        tf_change: 0,
                        ec: 0 as *mut ec_ctx,
                        remaining_bits: 0,
                        bandE: 0 as *const celt_ener,
                        seed: 0,
                        arch: 0,
                        theta_round: 0,
                        disable_inv: 0,
                        avoid_split_noise: 0,
                    };
                    let mut ctx_save2: band_ctx = band_ctx {
                        encode: 0,
                        resynth: 0,
                        m: 0 as *const OpusCustomMode,
                        i: 0,
                        intensity: 0,
                        spread: 0,
                        tf_change: 0,
                        ec: 0 as *mut ec_ctx,
                        remaining_bits: 0,
                        bandE: 0 as *const celt_ener,
                        seed: 0,
                        arch: 0,
                        theta_round: 0,
                        disable_inv: 0,
                        avoid_split_noise: 0,
                    };
                    let mut dist0: opus_val32 = 0.;
                    let mut dist1: opus_val32 = 0.;
                    let mut cm: u32 = 0;
                    let mut cm2: u32 = 0;
                    let mut nstart_bytes: i32 = 0;
                    let mut nend_bytes: i32 = 0;
                    let mut save_bytes: i32 = 0;
                    let mut bytes_buf: *const u8 = 0 as *const u8;
                    let mut bytes_save: [u8; 1275] = [0; 1275];
                    let mut w: [opus_val16; 2] = [0.; 2];
                    compute_channel_weights(
                        *bandE.offset(i as isize),
                        *bandE.offset((i + (*m).nbEBands) as isize),
                        w.as_mut_ptr(),
                    );
                    cm = x_cm | y_cm;
                    ec_save = (*ec).save();
                    ctx_save = ctx;
                    memcpy(
                        X_save.as_mut_ptr() as *mut core::ffi::c_void,
                        X as *const core::ffi::c_void,
                        (N as u64)
                            .wrapping_mul(::core::mem::size_of::<celt_norm>() as u64)
                            .wrapping_add((0 * X_save.as_mut_ptr().offset_from(X) as i64) as u64),
                    );
                    memcpy(
                        Y_save.as_mut_ptr() as *mut core::ffi::c_void,
                        Y as *const core::ffi::c_void,
                        (N as u64)
                            .wrapping_mul(::core::mem::size_of::<celt_norm>() as u64)
                            .wrapping_add((0 * Y_save.as_mut_ptr().offset_from(Y) as i64) as u64),
                    );
                    ctx.theta_round = -1;
                    x_cm = quant_band_stereo(
                        &mut ctx,
                        X,
                        Y,
                        N,
                        b,
                        B,
                        if effective_lowband != -1 {
                            norm.offset(effective_lowband as isize)
                        } else {
                            NULL as *mut celt_norm
                        },
                        LM,
                        if last != 0 {
                            NULL as *mut celt_norm
                        } else {
                            norm.offset((M * *eBands.offset(i as isize) as i32) as isize)
                                .offset(-(norm_offset as isize))
                        },
                        lowband_scratch,
                        cm as i32,
                    );
                    dist0 = w[0 as usize] * celt_inner_prod_c(X_save.as_mut_ptr(), X, N)
                        + w[1 as usize] * celt_inner_prod_c(Y_save.as_mut_ptr(), Y, N);
                    cm2 = x_cm;
                    ec_save2 = (*ec).save();
                    ctx_save2 = ctx;
                    memcpy(
                        X_save2.as_mut_ptr() as *mut core::ffi::c_void,
                        X as *const core::ffi::c_void,
                        (N as u64)
                            .wrapping_mul(::core::mem::size_of::<celt_norm>() as u64)
                            .wrapping_add((0 * X_save2.as_mut_ptr().offset_from(X) as i64) as u64),
                    );
                    memcpy(
                        Y_save2.as_mut_ptr() as *mut core::ffi::c_void,
                        Y as *const core::ffi::c_void,
                        (N as u64)
                            .wrapping_mul(::core::mem::size_of::<celt_norm>() as u64)
                            .wrapping_add((0 * Y_save2.as_mut_ptr().offset_from(Y) as i64) as u64),
                    );
                    if last == 0 {
                        memcpy(
                            norm_save2.as_mut_ptr() as *mut core::ffi::c_void,
                            norm.offset((M * *eBands.offset(i as isize) as i32) as isize)
                                .offset(-(norm_offset as isize))
                                as *const core::ffi::c_void,
                            (N as u64)
                                .wrapping_mul(::core::mem::size_of::<celt_norm>() as u64)
                                .wrapping_add(
                                    (0 * norm_save2.as_mut_ptr().offset_from(
                                        norm.offset(
                                            (M * *eBands.offset(i as isize) as i32) as isize,
                                        )
                                        .offset(-(norm_offset as isize)),
                                    ) as i64) as u64,
                                ),
                        );
                    }
                    nstart_bytes = ec_save.offs as i32;
                    nend_bytes = (*ec).storage as i32;
                    bytes_buf = ec.buf.as_ptr().offset(nstart_bytes as isize);
                    save_bytes = nend_bytes - nstart_bytes;
                    memcpy(
                        bytes_save.as_mut_ptr() as *mut core::ffi::c_void,
                        bytes_buf as *const core::ffi::c_void,
                        (save_bytes as u64)
                            .wrapping_mul(::core::mem::size_of::<u8>() as u64)
                            .wrapping_add(
                                (0 * bytes_save.as_mut_ptr().offset_from(bytes_buf) as i64) as u64,
                            ),
                    );
                    (*ec).restore(ec_save);
                    ctx = ctx_save;
                    memcpy(
                        X as *mut core::ffi::c_void,
                        X_save.as_mut_ptr() as *const core::ffi::c_void,
                        (N as u64)
                            .wrapping_mul(::core::mem::size_of::<celt_norm>() as u64)
                            .wrapping_add((0 * X.offset_from(X_save.as_mut_ptr()) as i64) as u64),
                    );
                    memcpy(
                        Y as *mut core::ffi::c_void,
                        Y_save.as_mut_ptr() as *const core::ffi::c_void,
                        (N as u64)
                            .wrapping_mul(::core::mem::size_of::<celt_norm>() as u64)
                            .wrapping_add((0 * Y.offset_from(Y_save.as_mut_ptr()) as i64) as u64),
                    );
                    if i == start + 1 {
                        special_hybrid_folding(m, norm, norm2, start, M, dual_stereo);
                    }
                    ctx.theta_round = 1;
                    x_cm = quant_band_stereo(
                        &mut ctx,
                        X,
                        Y,
                        N,
                        b,
                        B,
                        if effective_lowband != -1 {
                            norm.offset(effective_lowband as isize)
                        } else {
                            NULL as *mut celt_norm
                        },
                        LM,
                        if last != 0 {
                            NULL as *mut celt_norm
                        } else {
                            norm.offset((M * *eBands.offset(i as isize) as i32) as isize)
                                .offset(-(norm_offset as isize))
                        },
                        lowband_scratch,
                        cm as i32,
                    );
                    dist1 = w[0 as usize] * celt_inner_prod_c(X_save.as_mut_ptr(), X, N)
                        + w[1 as usize] * celt_inner_prod_c(Y_save.as_mut_ptr(), Y, N);
                    if dist0 >= dist1 {
                        x_cm = cm2;
                        (*ec).restore(ec_save2);
                        ctx = ctx_save2;
                        memcpy(
                            X as *mut core::ffi::c_void,
                            X_save2.as_mut_ptr() as *const core::ffi::c_void,
                            (N as u64)
                                .wrapping_mul(::core::mem::size_of::<celt_norm>() as u64)
                                .wrapping_add(
                                    (0 * X.offset_from(X_save2.as_mut_ptr()) as i64) as u64,
                                ),
                        );
                        memcpy(
                            Y as *mut core::ffi::c_void,
                            Y_save2.as_mut_ptr() as *const core::ffi::c_void,
                            (N as u64)
                                .wrapping_mul(::core::mem::size_of::<celt_norm>() as u64)
                                .wrapping_add(
                                    (0 * Y.offset_from(Y_save2.as_mut_ptr()) as i64) as u64,
                                ),
                        );
                        if last == 0 {
                            memcpy(
                                norm.offset((M * *eBands.offset(i as isize) as i32) as isize)
                                    .offset(-(norm_offset as isize))
                                    as *mut core::ffi::c_void,
                                norm_save2.as_mut_ptr() as *const core::ffi::c_void,
                                (N as u64)
                                    .wrapping_mul(::core::mem::size_of::<celt_norm>() as u64)
                                    .wrapping_add(
                                        (0 * norm
                                            .offset(
                                                (M * *eBands.offset(i as isize) as i32) as isize,
                                            )
                                            .offset(-(norm_offset as isize))
                                            .offset_from(norm_save2.as_mut_ptr())
                                            as i64) as u64,
                                    ),
                            );
                        }
                        memcpy(
                            bytes_buf as *mut core::ffi::c_void,
                            bytes_save.as_mut_ptr() as *const core::ffi::c_void,
                            (save_bytes as u64)
                                .wrapping_mul(::core::mem::size_of::<u8>() as u64)
                                .wrapping_add(
                                    (0 * bytes_buf.offset_from(bytes_save.as_mut_ptr()) as i64)
                                        as u64,
                                ),
                        );
                    }
                } else {
                    ctx.theta_round = 0;
                    x_cm = quant_band_stereo(
                        &mut ctx,
                        X,
                        Y,
                        N,
                        b,
                        B,
                        if effective_lowband != -1 {
                            norm.offset(effective_lowband as isize)
                        } else {
                            NULL as *mut celt_norm
                        },
                        LM,
                        if last != 0 {
                            NULL as *mut celt_norm
                        } else {
                            norm.offset((M * *eBands.offset(i as isize) as i32) as isize)
                                .offset(-(norm_offset as isize))
                        },
                        lowband_scratch,
                        (x_cm | y_cm) as i32,
                    );
                }
            } else {
                x_cm = quant_band(
                    &mut ctx,
                    X,
                    N,
                    b,
                    B,
                    if effective_lowband != -1 {
                        norm.offset(effective_lowband as isize)
                    } else {
                        NULL as *mut celt_norm
                    },
                    LM,
                    if last != 0 {
                        NULL as *mut celt_norm
                    } else {
                        norm.offset((M * *eBands.offset(i as isize) as i32) as isize)
                            .offset(-(norm_offset as isize))
                    },
                    Q15ONE,
                    lowband_scratch,
                    (x_cm | y_cm) as i32,
                );
            }
            y_cm = x_cm;
        }
        *collapse_masks.offset((i * C + 0) as isize) = x_cm as u8;
        *collapse_masks.offset((i * C + C - 1) as isize) = y_cm as u8;
        balance += *pulses.offset(i as isize) + tell;
        update_lowband = (b > N << BITRES) as i32;
        ctx.avoid_split_noise = 0;
        i += 1;
    }
    *seed = ctx.seed;
}
