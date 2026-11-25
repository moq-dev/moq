use crate::celt::entcode::{ec_ctx_saved, ec_tell, ec_tell_frac};
use crate::celt::entdec::{ec_dec, ec_dec_bit_logp, ec_dec_bits, ec_dec_icdf};
use crate::celt::entenc::{ec_enc, ec_enc_bit_logp, ec_enc_bits, ec_enc_icdf};
use crate::celt::laplace::{ec_laplace_decode, ec_laplace_encode};
use crate::celt::mathops::celt_log2;
use crate::celt::modes::OpusCustomMode;
use crate::celt::rate::MAX_FINE_BITS;

pub mod arch_h {
    pub type opus_val16 = f32;
    pub type opus_val32 = f32;
    pub type celt_ener = f32;
}

pub mod stack_alloc_h {
    pub const ALLOC_NONE: i32 = 1;
}
pub use self::arch_h::{celt_ener, opus_val16, opus_val32};
pub use self::stack_alloc_h::ALLOC_NONE;

use crate::externs::memcpy;
pub static eMeans: [opus_val16; 25] = [
    6.437500f32,
    6.250000f32,
    5.750000f32,
    5.312500f32,
    5.062500f32,
    4.812500f32,
    4.500000f32,
    4.375000f32,
    4.875000f32,
    4.687500f32,
    4.562500f32,
    4.437500f32,
    4.875000f32,
    4.625000f32,
    4.312500f32,
    4.500000f32,
    4.375000f32,
    4.625000f32,
    4.750000f32,
    4.437500f32,
    3.750000f32,
    3.750000f32,
    3.750000f32,
    3.750000f32,
    3.750000f32,
];
static mut pred_coef: [opus_val16; 4] = [
    (29440 as f64 / 32768.0f64) as opus_val16,
    (26112 as f64 / 32768.0f64) as opus_val16,
    (21248 as f64 / 32768.0f64) as opus_val16,
    (16384 as f64 / 32768.0f64) as opus_val16,
];
static mut beta_coef: [opus_val16; 4] = [
    (30147 as f64 / 32768.0f64) as opus_val16,
    (22282 as f64 / 32768.0f64) as opus_val16,
    (12124 as f64 / 32768.0f64) as opus_val16,
    (6554 as f64 / 32768.0f64) as opus_val16,
];
static mut beta_intra: opus_val16 = (4915 as f64 / 32768.0f64) as opus_val16;
static mut e_prob_model: [[[u8; 42]; 2]; 4] = [
    [
        [
            72, 127, 65, 129, 66, 128, 65, 128, 64, 128, 62, 128, 64, 128, 64, 128, 92, 78, 92, 79,
            92, 78, 90, 79, 116, 41, 115, 40, 114, 40, 132, 26, 132, 26, 145, 17, 161, 12, 176, 10,
            177, 11,
        ],
        [
            24, 179, 48, 138, 54, 135, 54, 132, 53, 134, 56, 133, 55, 132, 55, 132, 61, 114, 70,
            96, 74, 88, 75, 88, 87, 74, 89, 66, 91, 67, 100, 59, 108, 50, 120, 40, 122, 37, 97, 43,
            78, 50,
        ],
    ],
    [
        [
            83, 78, 84, 81, 88, 75, 86, 74, 87, 71, 90, 73, 93, 74, 93, 74, 109, 40, 114, 36, 117,
            34, 117, 34, 143, 17, 145, 18, 146, 19, 162, 12, 165, 10, 178, 7, 189, 6, 190, 8, 177,
            9,
        ],
        [
            23, 178, 54, 115, 63, 102, 66, 98, 69, 99, 74, 89, 71, 91, 73, 91, 78, 89, 86, 80, 92,
            66, 93, 64, 102, 59, 103, 60, 104, 60, 117, 52, 123, 44, 138, 35, 133, 31, 97, 38, 77,
            45,
        ],
    ],
    [
        [
            61, 90, 93, 60, 105, 42, 107, 41, 110, 45, 116, 38, 113, 38, 112, 38, 124, 26, 132, 27,
            136, 19, 140, 20, 155, 14, 159, 16, 158, 18, 170, 13, 177, 10, 187, 8, 192, 6, 175, 9,
            159, 10,
        ],
        [
            21, 178, 59, 110, 71, 86, 75, 85, 84, 83, 91, 66, 88, 73, 87, 72, 92, 75, 98, 72, 105,
            58, 107, 54, 115, 52, 114, 55, 112, 56, 129, 51, 132, 40, 150, 33, 140, 29, 98, 35, 77,
            42,
        ],
    ],
    [
        [
            42, 121, 96, 66, 108, 43, 111, 40, 117, 44, 123, 32, 120, 36, 119, 33, 127, 33, 134,
            34, 139, 21, 147, 23, 152, 20, 158, 25, 154, 26, 166, 21, 173, 16, 184, 13, 184, 10,
            150, 13, 139, 15,
        ],
        [
            22, 178, 63, 114, 74, 82, 84, 83, 92, 82, 103, 62, 96, 72, 96, 67, 101, 73, 107, 72,
            113, 55, 118, 52, 125, 52, 118, 52, 117, 55, 135, 49, 137, 39, 157, 32, 145, 29, 97,
            33, 77, 40,
        ],
    ],
];
static mut small_energy_icdf: [u8; 3] = [2, 1, 0];
unsafe fn loss_distortion(
    eBands: *const opus_val16,
    oldEBands: *mut opus_val16,
    start: i32,
    end: i32,
    len: i32,
    C: i32,
) -> opus_val32 {
    let mut c: i32 = 0;
    let mut i: i32 = 0;
    let mut dist: opus_val32 = 0 as opus_val32;
    c = 0;
    loop {
        i = start;
        while i < end {
            let d: opus_val16 =
                *eBands.offset((i + c * len) as isize) - *oldEBands.offset((i + c * len) as isize);
            dist = dist + d * d;
            i += 1;
        }
        c += 1;
        if !(c < C) {
            break;
        }
    }
    return if (200 as f32) < dist {
        200 as f32
    } else {
        dist
    };
}
unsafe fn quant_coarse_energy_impl(
    m: *const OpusCustomMode,
    start: i32,
    end: i32,
    eBands: *const opus_val16,
    oldEBands: *mut opus_val16,
    budget: i32,
    mut tell: i32,
    prob_model: *const u8,
    error: *mut opus_val16,
    enc: &mut ec_enc,
    C: i32,
    LM: i32,
    intra: i32,
    max_decay: opus_val16,
    lfe: i32,
) -> i32 {
    let mut i: i32 = 0;
    let mut c: i32 = 0;
    let mut badness: i32 = 0;
    let mut prev: [opus_val32; 2] = [0 as opus_val32, 0 as opus_val32];
    let mut coef: opus_val16 = 0.;
    let mut beta: opus_val16 = 0.;
    if tell + 3 <= budget {
        ec_enc_bit_logp(enc, intra, 3);
    }
    if intra != 0 {
        coef = 0 as opus_val16;
        beta = beta_intra;
    } else {
        beta = beta_coef[LM as usize];
        coef = pred_coef[LM as usize];
    }
    i = start;
    while i < end {
        c = 0;
        loop {
            let mut bits_left: i32 = 0;
            let mut qi: i32 = 0;
            let mut qi0: i32 = 0;
            let mut q: opus_val32 = 0.;
            let mut x: opus_val16 = 0.;
            let mut f: opus_val32 = 0.;
            let mut tmp: opus_val32 = 0.;
            let mut oldE: opus_val16 = 0.;
            let mut decay_bound: opus_val16 = 0.;
            x = *eBands.offset((i + c * (*m).nbEBands) as isize);
            oldE = if -9.0f32 > *oldEBands.offset((i + c * (*m).nbEBands) as isize) {
                -9.0f32
            } else {
                *oldEBands.offset((i + c * (*m).nbEBands) as isize)
            };
            f = x - coef * oldE - prev[c as usize];
            qi = (0.5f32 + f).floor() as i32;
            decay_bound = (if -28.0f32 > *oldEBands.offset((i + c * (*m).nbEBands) as isize) {
                -28.0f32
            } else {
                *oldEBands.offset((i + c * (*m).nbEBands) as isize)
            }) - max_decay;
            if qi < 0 && x < decay_bound {
                qi += (decay_bound - x) as i32;
                if qi > 0 {
                    qi = 0;
                }
            }
            qi0 = qi;
            tell = ec_tell(enc);
            bits_left = budget - tell - 3 * C * (end - i);
            if i != start && bits_left < 30 {
                if bits_left < 24 {
                    qi = if (1) < qi { 1 } else { qi };
                }
                if bits_left < 16 {
                    qi = if -1 > qi { -1 } else { qi };
                }
            }
            if lfe != 0 && i >= 2 {
                qi = if qi < 0 { qi } else { 0 };
            }
            if budget - tell >= 15 {
                let mut pi: i32 = 0;
                pi = 2 * (if i < 20 { i } else { 20 });
                ec_laplace_encode(
                    enc,
                    &mut qi,
                    ((*prob_model.offset(pi as isize) as i32) << 7) as u32,
                    (*prob_model.offset((pi + 1) as isize) as i32) << 6,
                );
            } else if budget - tell >= 2 {
                qi = if -1 > (if qi < 1 { qi } else { 1 }) {
                    -1
                } else if qi < 1 {
                    qi
                } else {
                    1
                };
                ec_enc_icdf(enc, 2 * qi ^ -((qi < 0) as i32), &small_energy_icdf, 2);
            } else if budget - tell >= 1 {
                qi = if (0) < qi { 0 } else { qi };
                ec_enc_bit_logp(enc, -qi, 1);
            } else {
                qi = -1;
            }
            *error.offset((i + c * (*m).nbEBands) as isize) = f - qi as f32;
            badness += (qi0 - qi).abs();
            q = qi as opus_val32;
            tmp = coef * oldE + prev[c as usize] + q;
            *oldEBands.offset((i + c * (*m).nbEBands) as isize) = tmp;
            prev[c as usize] = prev[c as usize] + q - beta * q;
            c += 1;
            if !(c < C) {
                break;
            }
        }
        i += 1;
    }
    return if lfe != 0 { 0 } else { badness };
}
pub unsafe fn quant_coarse_energy(
    m: *const OpusCustomMode,
    start: i32,
    end: i32,
    effEnd: i32,
    eBands: *const opus_val16,
    oldEBands: *mut opus_val16,
    budget: u32,
    error: *mut opus_val16,
    enc: &mut ec_enc,
    C: i32,
    LM: i32,
    nbAvailableBytes: i32,
    force_intra: i32,
    delayedIntra: *mut opus_val32,
    mut two_pass: i32,
    loss_rate: i32,
    lfe: i32,
) {
    let mut intra: i32 = 0;
    let mut max_decay: opus_val16 = 0.;
    let mut enc_start_state = ec_ctx_saved::default();
    let mut tell: u32 = 0;
    let mut badness1: i32 = 0;
    let mut intra_bias: i32 = 0;
    let mut new_distortion: opus_val32 = 0.;
    intra = (force_intra != 0
        || two_pass == 0
            && *delayedIntra > (2 * C * (end - start)) as f32
            && nbAvailableBytes > (end - start) * C) as i32;
    intra_bias = (budget as f32 * *delayedIntra * loss_rate as f32 / (C * 512) as f32) as i32;
    new_distortion = loss_distortion(eBands, oldEBands, start, effEnd, (*m).nbEBands, C);
    tell = ec_tell(enc) as u32;
    if tell.wrapping_add(3) > budget {
        intra = 0;
        two_pass = intra;
    }
    max_decay = 16.0f32;
    if end - start > 10 {
        max_decay = if max_decay < 0.125f32 * nbAvailableBytes as f32 {
            max_decay
        } else {
            0.125f32 * nbAvailableBytes as f32
        };
    }
    if lfe != 0 {
        max_decay = 3.0f32;
    }
    enc_start_state = enc.save();
    let vla = (C * (*m).nbEBands) as usize;
    let mut oldEBands_intra: Vec<opus_val16> = ::std::vec::from_elem(0., vla);
    let vla_0 = (C * (*m).nbEBands) as usize;
    let mut error_intra: Vec<opus_val16> = ::std::vec::from_elem(0., vla_0);
    memcpy(
        oldEBands_intra.as_mut_ptr() as *mut core::ffi::c_void,
        oldEBands as *const core::ffi::c_void,
        ((C * (*m).nbEBands) as u64)
            .wrapping_mul(::core::mem::size_of::<opus_val16>() as u64)
            .wrapping_add((0 * oldEBands_intra.as_mut_ptr().offset_from(oldEBands) as i64) as u64),
    );
    if two_pass != 0 || intra != 0 {
        badness1 = quant_coarse_energy_impl(
            m,
            start,
            end,
            eBands,
            oldEBands_intra.as_mut_ptr(),
            budget as i32,
            tell as i32,
            (e_prob_model[LM as usize][1]).as_ptr(),
            error_intra.as_mut_ptr(),
            enc,
            C,
            LM,
            1,
            max_decay,
            lfe,
        );
    }
    if intra == 0 {
        let mut intra_buf: *mut u8 = 0 as *mut u8;
        let mut enc_intra_state = ec_ctx_saved::default();
        let mut tell_intra: i32 = 0;
        let mut nstart_bytes: u32 = 0;
        let mut nintra_bytes: u32 = 0;
        let mut save_bytes: u32 = 0;
        let mut badness2: i32 = 0;
        tell_intra = ec_tell_frac(enc) as i32;
        enc_intra_state = enc.save();
        nstart_bytes = enc_start_state.offs;
        nintra_bytes = enc_intra_state.offs;
        intra_buf = (enc.buf.as_mut_ptr()).offset(nstart_bytes as isize);
        save_bytes = nintra_bytes.wrapping_sub(nstart_bytes);
        if save_bytes == 0 {
            save_bytes = ALLOC_NONE as u32;
        }
        let vla_1 = save_bytes as usize;
        let mut intra_bits: Vec<u8> = ::std::vec::from_elem(0, vla_1);
        memcpy(
            intra_bits.as_mut_ptr() as *mut core::ffi::c_void,
            intra_buf as *const core::ffi::c_void,
            (nintra_bytes.wrapping_sub(nstart_bytes) as u64)
                .wrapping_mul(::core::mem::size_of::<u8>() as u64)
                .wrapping_add((0 * intra_bits.as_mut_ptr().offset_from(intra_buf) as i64) as u64),
        );
        enc.restore(enc_start_state);
        badness2 = quant_coarse_energy_impl(
            m,
            start,
            end,
            eBands,
            oldEBands,
            budget as i32,
            tell as i32,
            (e_prob_model[LM as usize][intra as usize]).as_ptr(),
            error,
            enc,
            C,
            LM,
            0,
            max_decay,
            lfe,
        );
        if two_pass != 0
            && (badness1 < badness2
                || badness1 == badness2 && ec_tell_frac(enc) as i32 + intra_bias > tell_intra)
        {
            enc.restore(enc_intra_state);
            memcpy(
                intra_buf as *mut core::ffi::c_void,
                intra_bits.as_mut_ptr() as *const core::ffi::c_void,
                (nintra_bytes.wrapping_sub(nstart_bytes) as u64)
                    .wrapping_mul(::core::mem::size_of::<u8>() as u64)
                    .wrapping_add(
                        (0 * intra_buf.offset_from(intra_bits.as_mut_ptr()) as i64) as u64,
                    ),
            );
            memcpy(
                oldEBands as *mut core::ffi::c_void,
                oldEBands_intra.as_mut_ptr() as *const core::ffi::c_void,
                ((C * (*m).nbEBands) as u64)
                    .wrapping_mul(::core::mem::size_of::<opus_val16>() as u64)
                    .wrapping_add(
                        (0 * oldEBands.offset_from(oldEBands_intra.as_mut_ptr()) as i64) as u64,
                    ),
            );
            memcpy(
                error as *mut core::ffi::c_void,
                error_intra.as_mut_ptr() as *const core::ffi::c_void,
                ((C * (*m).nbEBands) as u64)
                    .wrapping_mul(::core::mem::size_of::<opus_val16>() as u64)
                    .wrapping_add((0 * error.offset_from(error_intra.as_mut_ptr()) as i64) as u64),
            );
            intra = 1;
        }
    } else {
        memcpy(
            oldEBands as *mut core::ffi::c_void,
            oldEBands_intra.as_mut_ptr() as *const core::ffi::c_void,
            ((C * (*m).nbEBands) as u64)
                .wrapping_mul(::core::mem::size_of::<opus_val16>() as u64)
                .wrapping_add(
                    (0 * oldEBands.offset_from(oldEBands_intra.as_mut_ptr()) as i64) as u64,
                ),
        );
        memcpy(
            error as *mut core::ffi::c_void,
            error_intra.as_mut_ptr() as *const core::ffi::c_void,
            ((C * (*m).nbEBands) as u64)
                .wrapping_mul(::core::mem::size_of::<opus_val16>() as u64)
                .wrapping_add((0 * error.offset_from(error_intra.as_mut_ptr()) as i64) as u64),
        );
    }
    if intra != 0 {
        *delayedIntra = new_distortion;
    } else {
        *delayedIntra =
            pred_coef[LM as usize] * pred_coef[LM as usize] * *delayedIntra + new_distortion;
    };
}
pub unsafe fn quant_fine_energy(
    m: *const OpusCustomMode,
    start: i32,
    end: i32,
    oldEBands: *mut opus_val16,
    error: *mut opus_val16,
    fine_quant: *mut i32,
    enc: &mut ec_enc,
    C: i32,
) {
    let mut i: i32 = 0;
    let mut c: i32 = 0;
    i = start;
    while i < end {
        let frac: i16 = ((1) << *fine_quant.offset(i as isize)) as i16;
        if !(*fine_quant.offset(i as isize) <= 0) {
            c = 0;
            loop {
                let mut q2: i32 = 0;
                let mut offset: opus_val16 = 0.;
                q2 = ((*error.offset((i + c * (*m).nbEBands) as isize) + 0.5f32)
                    * frac as i32 as f32)
                    .floor() as i32;
                if q2 > frac as i32 - 1 {
                    q2 = frac as i32 - 1;
                }
                if q2 < 0 {
                    q2 = 0;
                }
                ec_enc_bits(enc, q2 as u32, *fine_quant.offset(i as isize) as u32);
                offset = (q2 as f32 + 0.5f32)
                    * ((1) << 14 - *fine_quant.offset(i as isize)) as f32
                    * (1.0f32 / 16384 as f32)
                    - 0.5f32;
                let ref mut fresh0 = *oldEBands.offset((i + c * (*m).nbEBands) as isize);
                *fresh0 += offset;
                let ref mut fresh1 = *error.offset((i + c * (*m).nbEBands) as isize);
                *fresh1 -= offset;
                c += 1;
                if !(c < C) {
                    break;
                }
            }
        }
        i += 1;
    }
}
pub unsafe fn quant_energy_finalise(
    m: *const OpusCustomMode,
    start: i32,
    end: i32,
    oldEBands: *mut opus_val16,
    error: *mut opus_val16,
    fine_quant: *mut i32,
    fine_priority: *mut i32,
    mut bits_left: i32,
    enc: &mut ec_enc,
    C: i32,
) {
    let mut i: i32 = 0;
    let mut prio: i32 = 0;
    let mut c: i32 = 0;
    prio = 0;
    while prio < 2 {
        i = start;
        while i < end && bits_left >= C {
            if !(*fine_quant.offset(i as isize) >= MAX_FINE_BITS
                || *fine_priority.offset(i as isize) != prio)
            {
                c = 0;
                loop {
                    let mut q2: i32 = 0;
                    let mut offset: opus_val16 = 0.;
                    q2 = if *error.offset((i + c * (*m).nbEBands) as isize) < 0 as f32 {
                        0
                    } else {
                        1
                    };
                    ec_enc_bits(enc, q2 as u32, 1);
                    offset = (q2 as f32 - 0.5f32)
                        * ((1) << 14 - *fine_quant.offset(i as isize) - 1) as f32
                        * (1.0f32 / 16384 as f32);
                    let ref mut fresh2 = *oldEBands.offset((i + c * (*m).nbEBands) as isize);
                    *fresh2 += offset;
                    let ref mut fresh3 = *error.offset((i + c * (*m).nbEBands) as isize);
                    *fresh3 -= offset;
                    bits_left -= 1;
                    c += 1;
                    if !(c < C) {
                        break;
                    }
                }
            }
            i += 1;
        }
        prio += 1;
    }
}
pub unsafe fn unquant_coarse_energy(
    m: *const OpusCustomMode,
    start: i32,
    end: i32,
    oldEBands: *mut opus_val16,
    intra: i32,
    dec: &mut ec_dec,
    C: i32,
    LM: i32,
) {
    let prob_model: *const u8 = (e_prob_model[LM as usize][intra as usize]).as_ptr();
    let mut i: i32 = 0;
    let mut c: i32 = 0;
    let mut prev: [opus_val32; 2] = [0 as opus_val32, 0 as opus_val32];
    let mut coef: opus_val16 = 0.;
    let mut beta: opus_val16 = 0.;
    let mut budget: i32 = 0;
    let mut tell: i32 = 0;
    if intra != 0 {
        coef = 0 as opus_val16;
        beta = beta_intra;
    } else {
        beta = beta_coef[LM as usize];
        coef = pred_coef[LM as usize];
    }
    budget = ((*dec).storage).wrapping_mul(8) as i32;
    i = start;
    while i < end {
        c = 0;
        loop {
            let mut qi: i32 = 0;
            let mut q: opus_val32 = 0.;
            let mut tmp: opus_val32 = 0.;
            tell = ec_tell(dec);
            if budget - tell >= 15 {
                let mut pi: i32 = 0;
                pi = 2 * (if i < 20 { i } else { 20 });
                qi = ec_laplace_decode(
                    dec,
                    ((*prob_model.offset(pi as isize) as i32) << 7) as u32,
                    (*prob_model.offset((pi + 1) as isize) as i32) << 6,
                );
            } else if budget - tell >= 2 {
                qi = ec_dec_icdf(dec, &small_energy_icdf, 2);
                qi = qi >> 1 ^ -(qi & 1);
            } else if budget - tell >= 1 {
                qi = -ec_dec_bit_logp(dec, 1);
            } else {
                qi = -1;
            }
            q = qi as opus_val32;
            *oldEBands.offset((i + c * (*m).nbEBands) as isize) =
                if -9.0f32 > *oldEBands.offset((i + c * (*m).nbEBands) as isize) {
                    -9.0f32
                } else {
                    *oldEBands.offset((i + c * (*m).nbEBands) as isize)
                };
            tmp = coef * *oldEBands.offset((i + c * (*m).nbEBands) as isize) + prev[c as usize] + q;
            *oldEBands.offset((i + c * (*m).nbEBands) as isize) = tmp;
            prev[c as usize] = prev[c as usize] + q - beta * q;
            c += 1;
            if !(c < C) {
                break;
            }
        }
        i += 1;
    }
}
pub unsafe fn unquant_fine_energy(
    m: *const OpusCustomMode,
    start: i32,
    end: i32,
    oldEBands: *mut opus_val16,
    fine_quant: *mut i32,
    dec: &mut ec_dec,
    C: i32,
) {
    let mut i: i32 = 0;
    let mut c: i32 = 0;
    i = start;
    while i < end {
        if !(*fine_quant.offset(i as isize) <= 0) {
            c = 0;
            loop {
                let mut q2: i32 = 0;
                let mut offset: opus_val16 = 0.;
                q2 = ec_dec_bits(dec, *fine_quant.offset(i as isize) as u32) as i32;
                offset = (q2 as f32 + 0.5f32)
                    * ((1) << 14 - *fine_quant.offset(i as isize)) as f32
                    * (1.0f32 / 16384 as f32)
                    - 0.5f32;
                let ref mut fresh4 = *oldEBands.offset((i + c * (*m).nbEBands) as isize);
                *fresh4 += offset;
                c += 1;
                if !(c < C) {
                    break;
                }
            }
        }
        i += 1;
    }
}
pub unsafe fn unquant_energy_finalise(
    m: *const OpusCustomMode,
    start: i32,
    end: i32,
    oldEBands: *mut opus_val16,
    fine_quant: *mut i32,
    fine_priority: *mut i32,
    mut bits_left: i32,
    dec: &mut ec_dec,
    C: i32,
) {
    let mut i: i32 = 0;
    let mut prio: i32 = 0;
    let mut c: i32 = 0;
    prio = 0;
    while prio < 2 {
        i = start;
        while i < end && bits_left >= C {
            if !(*fine_quant.offset(i as isize) >= MAX_FINE_BITS
                || *fine_priority.offset(i as isize) != prio)
            {
                c = 0;
                loop {
                    let mut q2: i32 = 0;
                    let mut offset: opus_val16 = 0.;
                    q2 = ec_dec_bits(dec, 1) as i32;
                    offset = (q2 as f32 - 0.5f32)
                        * ((1) << 14 - *fine_quant.offset(i as isize) - 1) as f32
                        * (1.0f32 / 16384 as f32);
                    let ref mut fresh5 = *oldEBands.offset((i + c * (*m).nbEBands) as isize);
                    *fresh5 += offset;
                    bits_left -= 1;
                    c += 1;
                    if !(c < C) {
                        break;
                    }
                }
            }
            i += 1;
        }
        prio += 1;
    }
}
pub unsafe fn amp2Log2(
    m: *const OpusCustomMode,
    effEnd: i32,
    end: i32,
    bandE: *mut celt_ener,
    bandLogE: *mut opus_val16,
    C: i32,
) {
    let mut c: i32 = 0;
    let mut i: i32 = 0;
    c = 0;
    loop {
        i = 0;
        while i < effEnd {
            *bandLogE.offset((i + c * (*m).nbEBands) as isize) =
                celt_log2(*bandE.offset((i + c * (*m).nbEBands) as isize)) - eMeans[i as usize];
            i += 1;
        }
        i = effEnd;
        while i < end {
            *bandLogE.offset((c * (*m).nbEBands + i) as isize) = -14.0f32;
            i += 1;
        }
        c += 1;
        if !(c < C) {
            break;
        }
    }
}
