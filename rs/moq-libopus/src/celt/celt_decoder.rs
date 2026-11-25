use crate::celt::bands::{
    anti_collapse, celt_lcg_rand, denormalise_bands, quant_all_bands, SPREAD_NORMAL,
};
use crate::opus_custom_decoder_ctl;

pub mod arch_h {
    pub type opus_val16 = f32;
    pub type opus_val32 = f32;
    pub type celt_sig = f32;
    pub type celt_norm = f32;
    pub type celt_ener = f32;
    pub const Q15ONE: f32 = 1.0f32;
    pub const VERY_SMALL: f32 = 1e-30f32;
    pub const CELT_SIG_SCALE: f32 = 32768.0f32;
}

pub mod cpu_support_h {
    #[inline]
    pub unsafe fn opus_select_arch() -> i32 {
        return 0;
    }
}
pub mod stddef_h {
    pub const NULL: i32 = 0;
}
pub use self::arch_h::{
    celt_ener, celt_norm, celt_sig, opus_val16, opus_val32, CELT_SIG_SCALE, Q15ONE, VERY_SMALL,
};
pub use self::cpu_support_h::opus_select_arch;
pub use self::stddef_h::NULL;
use crate::celt::celt::{
    comb_filter, init_caps, resampling_factor, spread_icdf, tapset_icdf, tf_select_table, trim_icdf,
};
use crate::celt::celt::{
    CELT_GET_AND_CLEAR_ERROR_REQUEST, CELT_GET_MODE_REQUEST, CELT_SET_CHANNELS_REQUEST,
    CELT_SET_END_BAND_REQUEST, CELT_SET_SIGNALLING_REQUEST, CELT_SET_START_BAND_REQUEST,
};
use crate::celt::celt_lpc::{_celt_autocorr, _celt_lpc, celt_fir_c, celt_iir, LPC_ORDER};
use crate::celt::entcode::{ec_get_error, ec_tell, ec_tell_frac, BITRES};
use crate::celt::entdec::{
    ec_dec, ec_dec_bit_logp, ec_dec_bits, ec_dec_icdf, ec_dec_init, ec_dec_uint,
};
use crate::celt::mathops::celt_sqrt;
use crate::celt::mdct::clt_mdct_backward_c;
use crate::celt::modes::{opus_custom_mode_create, OpusCustomMode, MAX_PERIOD};
use crate::celt::pitch::{pitch_downsample, pitch_search};
use crate::celt::quant_bands::{
    unquant_coarse_energy, unquant_energy_finalise, unquant_fine_energy,
};
use crate::celt::rate::clt_compute_allocation;
use crate::celt::vq::renormalise_vector;
use crate::externs::{memcpy, memmove, memset};
use crate::src::opus_defines::{
    OPUS_ALLOC_FAIL, OPUS_BAD_ARG, OPUS_GET_FINAL_RANGE_REQUEST, OPUS_GET_LOOKAHEAD_REQUEST,
    OPUS_GET_PHASE_INVERSION_DISABLED_REQUEST, OPUS_GET_PITCH_REQUEST, OPUS_INTERNAL_ERROR,
    OPUS_OK, OPUS_RESET_STATE, OPUS_SET_PHASE_INVERSION_DISABLED_REQUEST, OPUS_UNIMPLEMENTED,
};
use crate::varargs::VarArgs;

#[derive(Copy, Clone)]
#[repr(C)]
pub struct OpusCustomDecoder {
    pub mode: *const OpusCustomMode,
    pub overlap: i32,
    pub channels: i32,
    pub stream_channels: i32,
    pub downsample: i32,
    pub start: i32,
    pub end: i32,
    pub signalling: i32,
    pub disable_inv: i32,
    pub arch: i32,
    pub rng: u32,
    pub error: i32,
    pub last_pitch_index: i32,
    pub loss_count: i32,
    pub skip_plc: i32,
    pub postfilter_period: i32,
    pub postfilter_period_old: i32,
    pub postfilter_gain: opus_val16,
    pub postfilter_gain_old: opus_val16,
    pub postfilter_tapset: i32,
    pub postfilter_tapset_old: i32,
    pub preemph_memD: [celt_sig; 2],
    pub _decode_mem: [celt_sig; 1],
}
pub const PLC_PITCH_LAG_MAX: i32 = 720;
pub const PLC_PITCH_LAG_MIN: i32 = 100;
pub const DECODE_BUFFER_SIZE: i32 = 2048;
pub unsafe fn validate_celt_decoder(st: *mut OpusCustomDecoder) {
    assert!(
        (*st).mode == opus_custom_mode_create(48000, 960, 0 as *mut i32) as *const OpusCustomMode
    );
    assert!((*st).overlap == 120);
    assert!((*st).channels == 1 || (*st).channels == 2);
    assert!((*st).stream_channels == 1 || (*st).stream_channels == 2);
    assert!((*st).downsample > 0);
    assert!((*st).start == 0 || (*st).start == 17);
    assert!((*st).start < (*st).end);
    assert!((*st).end <= 21);
    assert!((*st).arch >= 0);
    assert!((*st).arch <= 0);
    assert!((*st).last_pitch_index <= 720);
    assert!((*st).last_pitch_index >= 100 || (*st).last_pitch_index == 0);
    assert!((*st).postfilter_period < 1024);
    assert!((*st).postfilter_period >= 15 || (*st).postfilter_period == 0);
    assert!((*st).postfilter_period_old < 1024);
    assert!((*st).postfilter_period_old >= 15 || (*st).postfilter_period_old == 0);
    assert!((*st).postfilter_tapset <= 2);
    assert!((*st).postfilter_tapset >= 0);
    assert!((*st).postfilter_tapset_old <= 2);
    assert!((*st).postfilter_tapset_old >= 0);
}
pub unsafe fn celt_decoder_get_size(channels: i32) -> i32 {
    let mode: *const OpusCustomMode = opus_custom_mode_create(48000, 960, NULL as *mut i32);
    return opus_custom_decoder_get_size(mode, channels);
}
#[inline]
unsafe fn opus_custom_decoder_get_size(mode: *const OpusCustomMode, channels: i32) -> i32 {
    let size: i32 = (::core::mem::size_of::<OpusCustomDecoder>() as u64)
        .wrapping_add(
            ((channels * (DECODE_BUFFER_SIZE + (*mode).overlap) - 1) as u64)
                .wrapping_mul(::core::mem::size_of::<celt_sig>() as u64),
        )
        .wrapping_add(
            ((channels * LPC_ORDER) as u64)
                .wrapping_mul(::core::mem::size_of::<opus_val16>() as u64),
        )
        .wrapping_add(
            ((4 * 2 * (*mode).nbEBands) as u64)
                .wrapping_mul(::core::mem::size_of::<opus_val16>() as u64),
        ) as i32;
    return size;
}
pub unsafe fn celt_decoder_init(
    st: *mut OpusCustomDecoder,
    sampling_rate: i32,
    channels: i32,
) -> i32 {
    let mut ret: i32 = 0;
    ret = opus_custom_decoder_init(
        st,
        opus_custom_mode_create(48000, 960, NULL as *mut i32),
        channels,
    );
    if ret != OPUS_OK {
        return ret;
    }
    (*st).downsample = resampling_factor(sampling_rate);
    if (*st).downsample == 0 {
        return OPUS_BAD_ARG;
    } else {
        return OPUS_OK;
    };
}
#[inline]
unsafe fn opus_custom_decoder_init(
    st: *mut OpusCustomDecoder,
    mode: *const OpusCustomMode,
    channels: i32,
) -> i32 {
    if channels < 0 || channels > 2 {
        return OPUS_BAD_ARG;
    }
    if st.is_null() {
        return OPUS_ALLOC_FAIL;
    }
    memset(
        st as *mut i8 as *mut core::ffi::c_void,
        0,
        (opus_custom_decoder_get_size(mode, channels) as u64)
            .wrapping_mul(::core::mem::size_of::<i8>() as u64),
    );
    (*st).mode = mode;
    (*st).overlap = (*mode).overlap;
    (*st).channels = channels;
    (*st).stream_channels = (*st).channels;
    (*st).downsample = 1;
    (*st).start = 0;
    (*st).end = (*(*st).mode).effEBands;
    (*st).signalling = 1;
    (*st).disable_inv = (channels == 1) as i32;
    (*st).arch = opus_select_arch();
    opus_custom_decoder_ctl!(st, OPUS_RESET_STATE);
    return OPUS_OK;
}
unsafe fn deemphasis_stereo_simple(
    in_0: *mut *mut celt_sig,
    pcm: *mut opus_val16,
    N: i32,
    coef0: opus_val16,
    mem: *mut celt_sig,
) {
    let mut x0: *mut celt_sig = 0 as *mut celt_sig;
    let mut x1: *mut celt_sig = 0 as *mut celt_sig;
    let mut m0: celt_sig = 0.;
    let mut m1: celt_sig = 0.;
    let mut j: i32 = 0;
    x0 = *in_0.offset(0 as isize);
    x1 = *in_0.offset(1 as isize);
    m0 = *mem.offset(0 as isize);
    m1 = *mem.offset(1 as isize);
    j = 0;
    while j < N {
        let mut tmp0: celt_sig = 0.;
        let mut tmp1: celt_sig = 0.;
        tmp0 = *x0.offset(j as isize) + VERY_SMALL + m0;
        tmp1 = *x1.offset(j as isize) + VERY_SMALL + m1;
        m0 = coef0 * tmp0;
        m1 = coef0 * tmp1;
        *pcm.offset((2 * j) as isize) = tmp0 * (1 as f32 / CELT_SIG_SCALE);
        *pcm.offset((2 * j + 1) as isize) = tmp1 * (1 as f32 / CELT_SIG_SCALE);
        j += 1;
    }
    *mem.offset(0 as isize) = m0;
    *mem.offset(1 as isize) = m1;
}
unsafe fn deemphasis(
    in_0: *mut *mut celt_sig,
    pcm: *mut opus_val16,
    N: i32,
    C: i32,
    downsample: i32,
    coef: *const opus_val16,
    mem: *mut celt_sig,
    accum: i32,
) {
    let mut c: i32 = 0;
    let mut Nd: i32 = 0;
    let mut apply_downsampling: i32 = 0;
    let mut coef0: opus_val16 = 0.;
    if downsample == 1 && C == 2 && accum == 0 {
        deemphasis_stereo_simple(in_0, pcm, N, *coef.offset(0 as isize), mem);
        return;
    }
    assert!(accum == 0);
    let vla = N as usize;
    let mut scratch: Vec<celt_sig> = ::std::vec::from_elem(0., vla);
    coef0 = *coef.offset(0 as isize);
    Nd = N / downsample;
    c = 0;
    loop {
        let mut j: i32 = 0;
        let mut x: *mut celt_sig = 0 as *mut celt_sig;
        let mut y: *mut opus_val16 = 0 as *mut opus_val16;
        let mut m: celt_sig = *mem.offset(c as isize);
        x = *in_0.offset(c as isize);
        y = pcm.offset(c as isize);
        if downsample > 1 {
            j = 0;
            while j < N {
                let tmp: celt_sig = *x.offset(j as isize) + VERY_SMALL + m;
                m = coef0 * tmp;
                *scratch.as_mut_ptr().offset(j as isize) = tmp;
                j += 1;
            }
            apply_downsampling = 1;
        } else {
            j = 0;
            while j < N {
                let tmp_0: celt_sig = *x.offset(j as isize) + VERY_SMALL + m;
                m = coef0 * tmp_0;
                *y.offset((j * C) as isize) = tmp_0 * (1 as f32 / CELT_SIG_SCALE);
                j += 1;
            }
        }
        *mem.offset(c as isize) = m;
        if apply_downsampling != 0 {
            j = 0;
            while j < Nd {
                *y.offset((j * C) as isize) =
                    *scratch.as_mut_ptr().offset((j * downsample) as isize)
                        * (1 as f32 / CELT_SIG_SCALE);
                j += 1;
            }
        }
        c += 1;
        if !(c < C) {
            break;
        }
    }
}
unsafe fn celt_synthesis(
    mode: *const OpusCustomMode,
    X: *mut celt_norm,
    out_syn: *mut *mut celt_sig,
    oldBandE: *mut opus_val16,
    start: i32,
    effEnd: i32,
    C: i32,
    CC: i32,
    isTransient: i32,
    LM: i32,
    downsample: i32,
    silence: i32,
    arch: i32,
) {
    let mut c: i32 = 0;
    let mut i: i32 = 0;
    let mut M: i32 = 0;
    let mut b: i32 = 0;
    let mut B: i32 = 0;
    let mut N: i32 = 0;
    let mut NB: i32 = 0;
    let mut shift: i32 = 0;
    let mut nbEBands: i32 = 0;
    let mut overlap: i32 = 0;
    overlap = (*mode).overlap;
    nbEBands = (*mode).nbEBands;
    N = (*mode).shortMdctSize << LM;
    let vla = N as usize;
    let mut freq: Vec<celt_sig> = ::std::vec::from_elem(0., vla);
    M = (1) << LM;
    if isTransient != 0 {
        B = M;
        NB = (*mode).shortMdctSize;
        shift = (*mode).maxLM;
    } else {
        B = 1;
        NB = (*mode).shortMdctSize << LM;
        shift = (*mode).maxLM - LM;
    }
    if CC == 2 && C == 1 {
        let mut freq2: *mut celt_sig = 0 as *mut celt_sig;
        denormalise_bands(
            mode,
            X,
            freq.as_mut_ptr(),
            oldBandE,
            start,
            effEnd,
            M,
            downsample,
            silence,
        );
        freq2 = (*out_syn.offset(1 as isize)).offset((overlap / 2) as isize);
        memcpy(
            freq2 as *mut core::ffi::c_void,
            freq.as_mut_ptr() as *const core::ffi::c_void,
            (N as u64)
                .wrapping_mul(::core::mem::size_of::<celt_sig>() as u64)
                .wrapping_add((0 * freq2.offset_from(freq.as_mut_ptr()) as i64) as u64),
        );
        b = 0;
        while b < B {
            clt_mdct_backward_c(
                &(*mode).mdct,
                &mut *freq2.offset(b as isize),
                (*out_syn.offset(0 as isize)).offset((NB * b) as isize),
                (*mode).window,
                overlap,
                shift,
                B,
                arch,
            );
            b += 1;
        }
        b = 0;
        while b < B {
            clt_mdct_backward_c(
                &(*mode).mdct,
                &mut *freq.as_mut_ptr().offset(b as isize),
                (*out_syn.offset(1 as isize)).offset((NB * b) as isize),
                (*mode).window,
                overlap,
                shift,
                B,
                arch,
            );
            b += 1;
        }
    } else if CC == 1 && C == 2 {
        let mut freq2_0: *mut celt_sig = 0 as *mut celt_sig;
        freq2_0 = (*out_syn.offset(0 as isize)).offset((overlap / 2) as isize);
        denormalise_bands(
            mode,
            X,
            freq.as_mut_ptr(),
            oldBandE,
            start,
            effEnd,
            M,
            downsample,
            silence,
        );
        denormalise_bands(
            mode,
            X.offset(N as isize),
            freq2_0,
            oldBandE.offset(nbEBands as isize),
            start,
            effEnd,
            M,
            downsample,
            silence,
        );
        i = 0;
        while i < N {
            *freq.as_mut_ptr().offset(i as isize) = 0.5f32 * *freq.as_mut_ptr().offset(i as isize)
                + 0.5f32 * *freq2_0.offset(i as isize);
            i += 1;
        }
        b = 0;
        while b < B {
            clt_mdct_backward_c(
                &(*mode).mdct,
                &mut *freq.as_mut_ptr().offset(b as isize),
                (*out_syn.offset(0 as isize)).offset((NB * b) as isize),
                (*mode).window,
                overlap,
                shift,
                B,
                arch,
            );
            b += 1;
        }
    } else {
        c = 0;
        loop {
            denormalise_bands(
                mode,
                X.offset((c * N) as isize),
                freq.as_mut_ptr(),
                oldBandE.offset((c * nbEBands) as isize),
                start,
                effEnd,
                M,
                downsample,
                silence,
            );
            b = 0;
            while b < B {
                clt_mdct_backward_c(
                    &(*mode).mdct,
                    &mut *freq.as_mut_ptr().offset(b as isize),
                    (*out_syn.offset(c as isize)).offset((NB * b) as isize),
                    (*mode).window,
                    overlap,
                    shift,
                    B,
                    arch,
                );
                b += 1;
            }
            c += 1;
            if !(c < CC) {
                break;
            }
        }
    }
    c = 0;
    loop {
        i = 0;
        while i < N {
            *(*out_syn.offset(c as isize)).offset(i as isize) =
                *(*out_syn.offset(c as isize)).offset(i as isize);
            i += 1;
        }
        c += 1;
        if !(c < CC) {
            break;
        }
    }
}
unsafe fn tf_decode(
    start: i32,
    end: i32,
    isTransient: i32,
    tf_res: *mut i32,
    LM: i32,
    dec: &mut ec_dec,
) {
    let mut i: i32 = 0;
    let mut curr: i32 = 0;
    let mut tf_select: i32 = 0;
    let mut tf_select_rsv: i32 = 0;
    let mut tf_changed: i32 = 0;
    let mut logp: i32 = 0;
    let mut budget: u32 = 0;
    let mut tell: u32 = 0;
    budget = dec.storage.wrapping_mul(8);
    tell = ec_tell(dec) as u32;
    logp = if isTransient != 0 { 2 } else { 4 };
    tf_select_rsv = (LM > 0 && tell.wrapping_add(logp as u32).wrapping_add(1) <= budget) as i32;
    budget = budget.wrapping_sub(tf_select_rsv as u32);
    curr = 0;
    tf_changed = curr;
    i = start;
    while i < end {
        if tell.wrapping_add(logp as u32) <= budget {
            curr ^= ec_dec_bit_logp(dec, logp as u32);
            tell = ec_tell(dec) as u32;
            tf_changed |= curr;
        }
        *tf_res.offset(i as isize) = curr;
        logp = if isTransient != 0 { 4 } else { 5 };
        i += 1;
    }
    tf_select = 0;
    if tf_select_rsv != 0
        && tf_select_table[LM as usize][(4 * isTransient + 0 + tf_changed) as usize] as i32
            != tf_select_table[LM as usize][(4 * isTransient + 2 + tf_changed) as usize] as i32
    {
        tf_select = ec_dec_bit_logp(dec, 1);
    }
    i = start;
    while i < end {
        *tf_res.offset(i as isize) = tf_select_table[LM as usize]
            [(4 * isTransient + 2 * tf_select + *tf_res.offset(i as isize)) as usize]
            as i32;
        i += 1;
    }
}
unsafe fn celt_plc_pitch_search(decode_mem: *mut *mut celt_sig, C: i32, arch: i32) -> i32 {
    let mut pitch_index: i32 = 0;
    let mut lp_pitch_buf: [opus_val16; 1024] = [0.; 1024];
    pitch_downsample(
        decode_mem as *mut *mut celt_sig,
        lp_pitch_buf.as_mut_ptr(),
        DECODE_BUFFER_SIZE,
        C,
        arch,
    );
    pitch_search(
        lp_pitch_buf
            .as_mut_ptr()
            .offset((PLC_PITCH_LAG_MAX >> 1) as isize),
        lp_pitch_buf.as_mut_ptr(),
        DECODE_BUFFER_SIZE - PLC_PITCH_LAG_MAX,
        PLC_PITCH_LAG_MAX - PLC_PITCH_LAG_MIN,
        &mut pitch_index,
        arch,
    );
    pitch_index = PLC_PITCH_LAG_MAX - pitch_index;
    return pitch_index;
}
unsafe fn celt_decode_lost(st: *mut OpusCustomDecoder, N: i32, LM: i32) {
    let mut c: i32 = 0;
    let mut i: i32 = 0;
    let C: i32 = (*st).channels;
    let mut decode_mem: [*mut celt_sig; 2] = [0 as *mut celt_sig; 2];
    let mut out_syn: [*mut celt_sig; 2] = [0 as *mut celt_sig; 2];
    let mut lpc: *mut opus_val16 = 0 as *mut opus_val16;
    let mut oldBandE: *mut opus_val16 = 0 as *mut opus_val16;
    let mut oldLogE: *mut opus_val16 = 0 as *mut opus_val16;
    let mut oldLogE2: *mut opus_val16 = 0 as *mut opus_val16;
    let mut backgroundLogE: *mut opus_val16 = 0 as *mut opus_val16;
    let mut mode: *const OpusCustomMode = 0 as *const OpusCustomMode;
    let mut nbEBands: i32 = 0;
    let mut overlap: i32 = 0;
    let mut start: i32 = 0;
    let mut loss_count: i32 = 0;
    let mut noise_based: i32 = 0;
    let mut eBands: *const i16 = 0 as *const i16;
    mode = (*st).mode;
    nbEBands = (*mode).nbEBands;
    overlap = (*mode).overlap;
    eBands = (*mode).eBands;
    c = 0;
    loop {
        decode_mem[c as usize] = ((*st)._decode_mem)
            .as_mut_ptr()
            .offset((c * (DECODE_BUFFER_SIZE + overlap)) as isize);
        out_syn[c as usize] = (decode_mem[c as usize])
            .offset(DECODE_BUFFER_SIZE as isize)
            .offset(-(N as isize));
        c += 1;
        if !(c < C) {
            break;
        }
    }
    lpc = ((*st)._decode_mem)
        .as_mut_ptr()
        .offset(((DECODE_BUFFER_SIZE + overlap) * C) as isize) as *mut opus_val16;
    oldBandE = lpc.offset((C * LPC_ORDER) as isize);
    oldLogE = oldBandE.offset((2 * nbEBands) as isize);
    oldLogE2 = oldLogE.offset((2 * nbEBands) as isize);
    backgroundLogE = oldLogE2.offset((2 * nbEBands) as isize);
    loss_count = (*st).loss_count;
    start = (*st).start;
    noise_based = (loss_count >= 5 || start != 0 || (*st).skip_plc != 0) as i32;
    if noise_based != 0 {
        let mut seed: u32 = 0;
        let mut end: i32 = 0;
        let mut effEnd: i32 = 0;
        let mut decay: opus_val16 = 0.;
        end = (*st).end;
        effEnd = if start
            > (if end < (*mode).effEBands {
                end
            } else {
                (*mode).effEBands
            }) {
            start
        } else if end < (*mode).effEBands {
            end
        } else {
            (*mode).effEBands
        };
        let vla = (C * N) as usize;
        let mut X: Vec<celt_norm> = ::std::vec::from_elem(0., vla);
        decay = if loss_count == 0 { 1.5f32 } else { 0.5f32 };
        c = 0;
        loop {
            i = start;
            while i < end {
                *oldBandE.offset((c * nbEBands + i) as isize) = if *backgroundLogE
                    .offset((c * nbEBands + i) as isize)
                    > *oldBandE.offset((c * nbEBands + i) as isize) - decay
                {
                    *backgroundLogE.offset((c * nbEBands + i) as isize)
                } else {
                    *oldBandE.offset((c * nbEBands + i) as isize) - decay
                };
                i += 1;
            }
            c += 1;
            if !(c < C) {
                break;
            }
        }
        seed = (*st).rng;
        c = 0;
        while c < C {
            i = start;
            while i < effEnd {
                let mut j: i32 = 0;
                let mut boffs: i32 = 0;
                let mut blen: i32 = 0;
                boffs = N * c + ((*eBands.offset(i as isize) as i32) << LM);
                blen = (*eBands.offset((i + 1) as isize) as i32
                    - *eBands.offset(i as isize) as i32)
                    << LM;
                j = 0;
                while j < blen {
                    seed = celt_lcg_rand(seed);
                    *X.as_mut_ptr().offset((boffs + j) as isize) = (seed as i32 >> 20) as celt_norm;
                    j += 1;
                }
                renormalise_vector(
                    X.as_mut_ptr().offset(boffs as isize),
                    blen,
                    Q15ONE,
                    (*st).arch,
                );
                i += 1;
            }
            c += 1;
        }
        (*st).rng = seed;
        c = 0;
        loop {
            memmove(
                decode_mem[c as usize] as *mut core::ffi::c_void,
                (decode_mem[c as usize]).offset(N as isize) as *const core::ffi::c_void,
                ((2048 - N + (overlap >> 1)) as u64)
                    .wrapping_mul(::core::mem::size_of::<celt_sig>() as u64)
                    .wrapping_add(
                        (0 * (decode_mem[c as usize])
                            .offset_from((decode_mem[c as usize]).offset(N as isize))
                            as i64) as u64,
                    ),
            );
            c += 1;
            if !(c < C) {
                break;
            }
        }
        celt_synthesis(
            mode,
            X.as_mut_ptr(),
            out_syn.as_mut_ptr(),
            oldBandE,
            start,
            effEnd,
            C,
            C,
            0,
            LM,
            (*st).downsample,
            0,
            (*st).arch,
        );
    } else {
        let mut exc_length: i32 = 0;
        let mut window: *const opus_val16 = 0 as *const opus_val16;
        let mut exc: *mut opus_val16 = 0 as *mut opus_val16;
        let mut fade: opus_val16 = Q15ONE;
        let mut pitch_index: i32 = 0;
        if loss_count == 0 {
            pitch_index = celt_plc_pitch_search(decode_mem.as_mut_ptr(), C, (*st).arch);
            (*st).last_pitch_index = pitch_index;
        } else {
            pitch_index = (*st).last_pitch_index;
            fade = 0.8f32;
        }
        exc_length = if 2 * pitch_index < 1024 {
            2 * pitch_index
        } else {
            1024
        };
        let vla_0 = overlap as usize;
        let mut etmp: Vec<opus_val32> = ::std::vec::from_elem(0., vla_0);
        let mut _exc: [opus_val16; 1048] = [0.; 1048];
        let vla_1 = exc_length as usize;
        let mut fir_tmp: Vec<opus_val16> = ::std::vec::from_elem(0., vla_1);
        exc = _exc.as_mut_ptr().offset(LPC_ORDER as isize);
        window = (*mode).window;
        c = 0;
        loop {
            let mut decay_0: opus_val16 = 0.;
            let mut attenuation: opus_val16 = 0.;
            let mut S1: opus_val32 = 0 as opus_val32;
            let mut buf: *mut celt_sig = 0 as *mut celt_sig;
            let mut extrapolation_offset: i32 = 0;
            let mut extrapolation_len: i32 = 0;
            let mut j_0: i32 = 0;
            buf = decode_mem[c as usize];
            i = 0;
            while i < MAX_PERIOD + LPC_ORDER {
                *exc.offset((i - LPC_ORDER) as isize) =
                    *buf.offset((2048 - 1024 - 24 + i) as isize);
                i += 1;
            }
            if loss_count == 0 {
                let mut ac: [opus_val32; 25] = [0.; 25];
                _celt_autocorr(
                    exc,
                    ac.as_mut_ptr(),
                    window,
                    overlap,
                    LPC_ORDER,
                    MAX_PERIOD,
                    (*st).arch,
                );
                ac[0 as usize] *= 1.0001f32;
                i = 1;
                while i <= LPC_ORDER {
                    ac[i as usize] -= ac[i as usize] * (0.008f32 * 0.008f32) * i as f32 * i as f32;
                    i += 1;
                }
                _celt_lpc(
                    lpc.offset((c * LPC_ORDER) as isize),
                    ac.as_mut_ptr(),
                    LPC_ORDER,
                );
            }
            celt_fir_c(
                exc.offset(1024 as isize).offset(-(exc_length as isize)),
                lpc.offset((c * 24) as isize),
                fir_tmp.as_mut_ptr(),
                exc_length,
                24,
                (*st).arch,
            );
            memcpy(
                exc.offset(1024 as isize).offset(-(exc_length as isize)) as *mut core::ffi::c_void,
                fir_tmp.as_mut_ptr() as *const core::ffi::c_void,
                (exc_length as u64)
                    .wrapping_mul(::core::mem::size_of::<opus_val16>() as u64)
                    .wrapping_add(
                        (0 * exc
                            .offset(1024 as isize)
                            .offset(-(exc_length as isize))
                            .offset_from(fir_tmp.as_mut_ptr()) as i64)
                            as u64,
                    ),
            );
            let mut E1: opus_val32 = 1 as opus_val32;
            let mut E2: opus_val32 = 1 as opus_val32;
            let mut decay_length: i32 = 0;
            decay_length = exc_length >> 1;
            i = 0;
            while i < decay_length {
                let mut e: opus_val16 = 0.;
                e = *exc.offset((MAX_PERIOD - decay_length + i) as isize);
                E1 += e * e;
                e = *exc.offset((MAX_PERIOD - 2 * decay_length + i) as isize);
                E2 += e * e;
                i += 1;
            }
            E1 = if E1 < E2 { E1 } else { E2 };
            decay_0 = celt_sqrt(E1 / E2);
            memmove(
                buf as *mut core::ffi::c_void,
                buf.offset(N as isize) as *const core::ffi::c_void,
                ((2048 - N) as u64)
                    .wrapping_mul(::core::mem::size_of::<celt_sig>() as u64)
                    .wrapping_add((0 * buf.offset_from(buf.offset(N as isize)) as i64) as u64),
            );
            extrapolation_offset = MAX_PERIOD - pitch_index;
            extrapolation_len = N + overlap;
            attenuation = fade * decay_0;
            j_0 = 0;
            i = j_0;
            while i < extrapolation_len {
                let mut tmp: opus_val16 = 0.;
                if j_0 >= pitch_index {
                    j_0 -= pitch_index;
                    attenuation = attenuation * decay_0;
                }
                *buf.offset((DECODE_BUFFER_SIZE - N + i) as isize) =
                    attenuation * *exc.offset((extrapolation_offset + j_0) as isize);
                tmp = *buf.offset((2048 - 1024 - N + extrapolation_offset + j_0) as isize);
                S1 += tmp * tmp;
                i += 1;
                j_0 += 1;
            }
            let mut lpc_mem: [opus_val16; 24] = [0.; 24];
            i = 0;
            while i < LPC_ORDER {
                lpc_mem[i as usize] = *buf.offset((2048 - N - 1 - i) as isize);
                i += 1;
            }
            celt_iir(
                buf.offset(DECODE_BUFFER_SIZE as isize)
                    .offset(-(N as isize)),
                lpc.offset((c * LPC_ORDER) as isize),
                buf.offset(DECODE_BUFFER_SIZE as isize)
                    .offset(-(N as isize)),
                extrapolation_len,
                LPC_ORDER,
                lpc_mem.as_mut_ptr(),
                (*st).arch,
            );
            let mut S2: opus_val32 = 0 as opus_val32;
            i = 0;
            while i < extrapolation_len {
                let tmp_0: opus_val16 = *buf.offset((2048 - N + i) as isize);
                S2 += tmp_0 * tmp_0;
                i += 1;
            }
            if !(S1 > 0.2f32 * S2) {
                i = 0;
                while i < extrapolation_len {
                    *buf.offset((DECODE_BUFFER_SIZE - N + i) as isize) = 0 as celt_sig;
                    i += 1;
                }
            } else if S1 < S2 {
                let ratio: opus_val16 = celt_sqrt((S1 + 1 as f32) / (S2 + 1 as f32));
                i = 0;
                while i < overlap {
                    let tmp_g: opus_val16 = Q15ONE - *window.offset(i as isize) * (1.0f32 - ratio);
                    *buf.offset((DECODE_BUFFER_SIZE - N + i) as isize) =
                        tmp_g * *buf.offset((2048 - N + i) as isize);
                    i += 1;
                }
                i = overlap;
                while i < extrapolation_len {
                    *buf.offset((DECODE_BUFFER_SIZE - N + i) as isize) =
                        ratio * *buf.offset((2048 - N + i) as isize);
                    i += 1;
                }
            }
            comb_filter(
                etmp.as_mut_ptr(),
                buf.offset(DECODE_BUFFER_SIZE as isize),
                (*st).postfilter_period,
                (*st).postfilter_period,
                overlap,
                -(*st).postfilter_gain,
                -(*st).postfilter_gain,
                (*st).postfilter_tapset,
                (*st).postfilter_tapset,
                NULL as *const opus_val16,
                0,
                (*st).arch,
            );
            i = 0;
            while i < overlap / 2 {
                *buf.offset((DECODE_BUFFER_SIZE + i) as isize) = *window.offset(i as isize)
                    * *etmp.as_mut_ptr().offset((overlap - 1 - i) as isize)
                    + *window.offset((overlap - i - 1) as isize)
                        * *etmp.as_mut_ptr().offset(i as isize);
                i += 1;
            }
            c += 1;
            if !(c < C) {
                break;
            }
        }
    }
    (*st).loss_count = loss_count + 1;
}
pub unsafe fn celt_decode_with_ec(
    st: *mut OpusCustomDecoder,
    data: *const u8,
    len: i32,
    pcm: *mut opus_val16,
    mut frame_size: i32,
    dec: Option<&mut ec_dec>,
    accum: i32,
) -> i32 {
    let mut c: i32 = 0;
    let mut i: i32 = 0;
    let mut N: i32 = 0;
    let mut spread_decision: i32 = 0;
    let mut bits: i32 = 0;
    let mut _dec: ec_dec = ec_dec {
        buf: &mut [],
        storage: 0,
        end_offs: 0,
        end_window: 0,
        nend_bits: 0,
        nbits_total: 0,
        offs: 0,
        rng: 0,
        val: 0,
        ext: 0,
        rem: 0,
        error: 0,
    };
    let mut decode_mem: [*mut celt_sig; 2] = [0 as *mut celt_sig; 2];
    let mut out_syn: [*mut celt_sig; 2] = [0 as *mut celt_sig; 2];
    let mut lpc: *mut opus_val16 = 0 as *mut opus_val16;
    let mut oldBandE: *mut opus_val16 = 0 as *mut opus_val16;
    let mut oldLogE: *mut opus_val16 = 0 as *mut opus_val16;
    let mut oldLogE2: *mut opus_val16 = 0 as *mut opus_val16;
    let mut backgroundLogE: *mut opus_val16 = 0 as *mut opus_val16;
    let mut shortBlocks: i32 = 0;
    let mut isTransient: i32 = 0;
    let mut intra_ener: i32 = 0;
    let CC: i32 = (*st).channels;
    let mut LM: i32 = 0;
    let mut M: i32 = 0;
    let mut start: i32 = 0;
    let mut end: i32 = 0;
    let mut effEnd: i32 = 0;
    let mut codedBands: i32 = 0;
    let mut alloc_trim: i32 = 0;
    let mut postfilter_pitch: i32 = 0;
    let mut postfilter_gain: opus_val16 = 0.;
    let mut intensity: i32 = 0;
    let mut dual_stereo: i32 = 0;
    let mut total_bits: i32 = 0;
    let mut balance: i32 = 0;
    let mut tell: i32 = 0;
    let mut dynalloc_logp: i32 = 0;
    let mut postfilter_tapset: i32 = 0;
    let mut anti_collapse_rsv: i32 = 0;
    let mut anti_collapse_on: i32 = 0;
    let mut silence: i32 = 0;
    let C: i32 = (*st).stream_channels;
    let mut mode: *const OpusCustomMode = 0 as *const OpusCustomMode;
    let mut nbEBands: i32 = 0;
    let mut overlap: i32 = 0;
    let mut eBands: *const i16 = 0 as *const i16;
    validate_celt_decoder(st);
    mode = (*st).mode;
    nbEBands = (*mode).nbEBands;
    overlap = (*mode).overlap;
    eBands = (*mode).eBands;
    start = (*st).start;
    end = (*st).end;
    frame_size *= (*st).downsample;
    lpc = ((*st)._decode_mem)
        .as_mut_ptr()
        .offset(((DECODE_BUFFER_SIZE + overlap) * CC) as isize) as *mut opus_val16;
    oldBandE = lpc.offset((CC * LPC_ORDER) as isize);
    oldLogE = oldBandE.offset((2 * nbEBands) as isize);
    oldLogE2 = oldLogE.offset((2 * nbEBands) as isize);
    backgroundLogE = oldLogE2.offset((2 * nbEBands) as isize);
    LM = 0;
    while LM <= (*mode).maxLM {
        if (*mode).shortMdctSize << LM == frame_size {
            break;
        }
        LM += 1;
    }
    if LM > (*mode).maxLM {
        return OPUS_BAD_ARG;
    }
    M = (1) << LM;
    if len < 0 || len > 1275 || pcm.is_null() {
        return OPUS_BAD_ARG;
    }
    N = M * (*mode).shortMdctSize;
    c = 0;
    loop {
        decode_mem[c as usize] = ((*st)._decode_mem)
            .as_mut_ptr()
            .offset((c * (DECODE_BUFFER_SIZE + overlap)) as isize);
        out_syn[c as usize] = (decode_mem[c as usize])
            .offset(DECODE_BUFFER_SIZE as isize)
            .offset(-(N as isize));
        c += 1;
        if !(c < CC) {
            break;
        }
    }
    effEnd = end;
    if effEnd > (*mode).effEBands {
        effEnd = (*mode).effEBands;
    }
    if data.is_null() || len <= 1 {
        celt_decode_lost(st, N, LM);
        deemphasis(
            out_syn.as_mut_ptr(),
            pcm,
            N,
            CC,
            (*st).downsample,
            ((*mode).preemph).as_ptr(),
            ((*st).preemph_memD).as_mut_ptr(),
            accum,
        );
        return frame_size / (*st).downsample;
    }
    (*st).skip_plc = ((*st).loss_count != 0) as i32;
    let dec = if let Some(dec) = dec {
        dec
    } else {
        _dec = ec_dec_init(std::slice::from_raw_parts_mut(
            data as *mut u8,
            len as usize,
        ));
        &mut _dec
    };
    if C == 1 {
        i = 0;
        while i < nbEBands {
            *oldBandE.offset(i as isize) =
                if *oldBandE.offset(i as isize) > *oldBandE.offset((nbEBands + i) as isize) {
                    *oldBandE.offset(i as isize)
                } else {
                    *oldBandE.offset((nbEBands + i) as isize)
                };
            i += 1;
        }
    }
    total_bits = len * 8;
    tell = ec_tell(dec);
    if tell >= total_bits {
        silence = 1;
    } else if tell == 1 {
        silence = ec_dec_bit_logp(dec, 15);
    } else {
        silence = 0;
    }
    if silence != 0 {
        tell = len * 8;
        (*dec).nbits_total += tell - ec_tell(dec);
    }
    postfilter_gain = 0 as opus_val16;
    postfilter_pitch = 0;
    postfilter_tapset = 0;
    if start == 0 && tell + 16 <= total_bits {
        if ec_dec_bit_logp(dec, 1) != 0 {
            let mut qg: i32 = 0;
            let mut octave: i32 = 0;
            octave = ec_dec_uint(dec, 6) as i32;
            postfilter_pitch = (((16) << octave) as u32)
                .wrapping_add(ec_dec_bits(dec, (4 + octave) as u32))
                .wrapping_sub(1) as i32;
            qg = ec_dec_bits(dec, 3) as i32;
            if ec_tell(dec) + 2 <= total_bits {
                postfilter_tapset = ec_dec_icdf(dec, &tapset_icdf, 2);
            }
            postfilter_gain = 0.09375f32 * (qg + 1) as f32;
        }
        tell = ec_tell(dec);
    }
    if LM > 0 && tell + 3 <= total_bits {
        isTransient = ec_dec_bit_logp(dec, 3);
        tell = ec_tell(dec);
    } else {
        isTransient = 0;
    }
    if isTransient != 0 {
        shortBlocks = M;
    } else {
        shortBlocks = 0;
    }
    intra_ener = if tell + 3 <= total_bits {
        ec_dec_bit_logp(dec, 3)
    } else {
        0
    };
    unquant_coarse_energy(mode, start, end, oldBandE, intra_ener, dec, C, LM);
    let vla = nbEBands as usize;
    let mut tf_res: Vec<i32> = ::std::vec::from_elem(0, vla);
    tf_decode(start, end, isTransient, tf_res.as_mut_ptr(), LM, dec);
    tell = ec_tell(dec);
    spread_decision = SPREAD_NORMAL;
    if tell + 4 <= total_bits {
        spread_decision = ec_dec_icdf(dec, &spread_icdf, 5);
    }
    let vla_0 = nbEBands as usize;
    let mut cap: Vec<i32> = ::std::vec::from_elem(0, vla_0);
    init_caps(mode, cap.as_mut_ptr(), LM, C);
    let vla_1 = nbEBands as usize;
    let mut offsets: Vec<i32> = ::std::vec::from_elem(0, vla_1);
    dynalloc_logp = 6;
    total_bits <<= BITRES;
    tell = ec_tell_frac(dec) as i32;
    i = start;
    while i < end {
        let mut width: i32 = 0;
        let mut quanta: i32 = 0;
        let mut dynalloc_loop_logp: i32 = 0;
        let mut boost: i32 = 0;
        width =
            C * (*eBands.offset((i + 1) as isize) as i32 - *eBands.offset(i as isize) as i32) << LM;
        quanta = if (width << 3) < (if (6) << 3 > width { (6) << 3 } else { width }) {
            width << 3
        } else if (6) << 3 > width {
            (6) << 3
        } else {
            width
        };
        dynalloc_loop_logp = dynalloc_logp;
        boost = 0;
        while tell + (dynalloc_loop_logp << BITRES) < total_bits
            && boost < *cap.as_mut_ptr().offset(i as isize)
        {
            let mut flag: i32 = 0;
            flag = ec_dec_bit_logp(dec, dynalloc_loop_logp as u32);
            tell = ec_tell_frac(dec) as i32;
            if flag == 0 {
                break;
            }
            boost += quanta;
            total_bits -= quanta;
            dynalloc_loop_logp = 1;
        }
        *offsets.as_mut_ptr().offset(i as isize) = boost;
        if boost > 0 {
            dynalloc_logp = if 2 > dynalloc_logp - 1 {
                2
            } else {
                dynalloc_logp - 1
            };
        }
        i += 1;
    }
    let vla_2 = nbEBands as usize;
    let mut fine_quant: Vec<i32> = ::std::vec::from_elem(0, vla_2);
    alloc_trim = if tell + ((6) << BITRES) <= total_bits {
        ec_dec_icdf(dec, &trim_icdf, 7)
    } else {
        5
    };
    bits = (((len * 8) << BITRES) as u32)
        .wrapping_sub(ec_tell_frac(dec))
        .wrapping_sub(1) as i32;
    anti_collapse_rsv = if isTransient != 0 && LM >= 2 && bits >= (LM + 2) << BITRES {
        (1) << BITRES
    } else {
        0
    };
    bits -= anti_collapse_rsv;
    let vla_3 = nbEBands as usize;
    let mut pulses: Vec<i32> = ::std::vec::from_elem(0, vla_3);
    let vla_4 = nbEBands as usize;
    let mut fine_priority: Vec<i32> = ::std::vec::from_elem(0, vla_4);
    codedBands = clt_compute_allocation(
        mode,
        start,
        end,
        offsets.as_mut_ptr(),
        cap.as_mut_ptr(),
        alloc_trim,
        &mut intensity,
        &mut dual_stereo,
        bits,
        &mut balance,
        pulses.as_mut_ptr(),
        fine_quant.as_mut_ptr(),
        fine_priority.as_mut_ptr(),
        C,
        LM,
        dec,
        0,
        0,
        0,
    );
    unquant_fine_energy(mode, start, end, oldBandE, fine_quant.as_mut_ptr(), dec, C);
    c = 0;
    loop {
        memmove(
            decode_mem[c as usize] as *mut core::ffi::c_void,
            (decode_mem[c as usize]).offset(N as isize) as *const core::ffi::c_void,
            ((2048 - N + overlap / 2) as u64)
                .wrapping_mul(::core::mem::size_of::<celt_sig>() as u64)
                .wrapping_add(
                    (0 * (decode_mem[c as usize])
                        .offset_from((decode_mem[c as usize]).offset(N as isize))
                        as i64) as u64,
                ),
        );
        c += 1;
        if !(c < CC) {
            break;
        }
    }
    let vla_5 = (C * nbEBands) as usize;
    let mut collapse_masks: Vec<u8> = ::std::vec::from_elem(0, vla_5);
    let vla_6 = (C * N) as usize;
    let mut X: Vec<celt_norm> = ::std::vec::from_elem(0., vla_6);
    quant_all_bands(
        0,
        mode,
        start,
        end,
        X.as_mut_ptr(),
        if C == 2 {
            X.as_mut_ptr().offset(N as isize)
        } else {
            NULL as *mut celt_norm
        },
        collapse_masks.as_mut_ptr(),
        NULL as *const celt_ener,
        pulses.as_mut_ptr(),
        shortBlocks,
        spread_decision,
        dual_stereo,
        intensity,
        tf_res.as_mut_ptr(),
        len * ((8) << BITRES) - anti_collapse_rsv,
        balance,
        dec,
        LM,
        codedBands,
        &mut (*st).rng,
        0,
        (*st).arch,
        (*st).disable_inv,
    );
    if anti_collapse_rsv > 0 {
        anti_collapse_on = ec_dec_bits(dec, 1) as i32;
    }
    unquant_energy_finalise(
        mode,
        start,
        end,
        oldBandE,
        fine_quant.as_mut_ptr(),
        fine_priority.as_mut_ptr(),
        len * 8 - ec_tell(dec),
        dec,
        C,
    );
    if anti_collapse_on != 0 {
        anti_collapse(
            mode,
            X.as_mut_ptr(),
            collapse_masks.as_mut_ptr(),
            LM,
            C,
            N,
            start,
            end,
            oldBandE,
            oldLogE,
            oldLogE2,
            pulses.as_mut_ptr(),
            (*st).rng,
            (*st).arch,
        );
    }
    if silence != 0 {
        i = 0;
        while i < C * nbEBands {
            *oldBandE.offset(i as isize) = -28.0f32;
            i += 1;
        }
    }
    celt_synthesis(
        mode,
        X.as_mut_ptr(),
        out_syn.as_mut_ptr(),
        oldBandE,
        start,
        effEnd,
        C,
        CC,
        isTransient,
        LM,
        (*st).downsample,
        silence,
        (*st).arch,
    );
    c = 0;
    loop {
        (*st).postfilter_period = if (*st).postfilter_period > 15 {
            (*st).postfilter_period
        } else {
            15
        };
        (*st).postfilter_period_old = if (*st).postfilter_period_old > 15 {
            (*st).postfilter_period_old
        } else {
            15
        };
        comb_filter(
            out_syn[c as usize],
            out_syn[c as usize],
            (*st).postfilter_period_old,
            (*st).postfilter_period,
            (*mode).shortMdctSize,
            (*st).postfilter_gain_old,
            (*st).postfilter_gain,
            (*st).postfilter_tapset_old,
            (*st).postfilter_tapset,
            (*mode).window,
            overlap,
            (*st).arch,
        );
        if LM != 0 {
            comb_filter(
                (out_syn[c as usize]).offset((*mode).shortMdctSize as isize),
                (out_syn[c as usize]).offset((*mode).shortMdctSize as isize),
                (*st).postfilter_period,
                postfilter_pitch,
                N - (*mode).shortMdctSize,
                (*st).postfilter_gain,
                postfilter_gain,
                (*st).postfilter_tapset,
                postfilter_tapset,
                (*mode).window,
                overlap,
                (*st).arch,
            );
        }
        c += 1;
        if !(c < CC) {
            break;
        }
    }
    (*st).postfilter_period_old = (*st).postfilter_period;
    (*st).postfilter_gain_old = (*st).postfilter_gain;
    (*st).postfilter_tapset_old = (*st).postfilter_tapset;
    (*st).postfilter_period = postfilter_pitch;
    (*st).postfilter_gain = postfilter_gain;
    (*st).postfilter_tapset = postfilter_tapset;
    if LM != 0 {
        (*st).postfilter_period_old = (*st).postfilter_period;
        (*st).postfilter_gain_old = (*st).postfilter_gain;
        (*st).postfilter_tapset_old = (*st).postfilter_tapset;
    }
    if C == 1 {
        memcpy(
            &mut *oldBandE.offset(nbEBands as isize) as *mut opus_val16 as *mut core::ffi::c_void,
            oldBandE as *const core::ffi::c_void,
            (nbEBands as u64)
                .wrapping_mul(::core::mem::size_of::<opus_val16>() as u64)
                .wrapping_add(
                    (0 * (&mut *oldBandE.offset(nbEBands as isize) as *mut opus_val16)
                        .offset_from(oldBandE) as i64) as u64,
                ),
        );
    }
    if isTransient == 0 {
        let mut max_background_increase: opus_val16 = 0.;
        memcpy(
            oldLogE2 as *mut core::ffi::c_void,
            oldLogE as *const core::ffi::c_void,
            ((2 * nbEBands) as u64)
                .wrapping_mul(::core::mem::size_of::<opus_val16>() as u64)
                .wrapping_add((0 * oldLogE2.offset_from(oldLogE) as i64) as u64),
        );
        memcpy(
            oldLogE as *mut core::ffi::c_void,
            oldBandE as *const core::ffi::c_void,
            ((2 * nbEBands) as u64)
                .wrapping_mul(::core::mem::size_of::<opus_val16>() as u64)
                .wrapping_add((0 * oldLogE.offset_from(oldBandE) as i64) as u64),
        );
        if (*st).loss_count < 10 {
            max_background_increase = M as f32 * 0.001f32;
        } else {
            max_background_increase = 1.0f32;
        }
        i = 0;
        while i < 2 * nbEBands {
            *backgroundLogE.offset(i as isize) = if *backgroundLogE.offset(i as isize)
                + max_background_increase
                < *oldBandE.offset(i as isize)
            {
                *backgroundLogE.offset(i as isize) + max_background_increase
            } else {
                *oldBandE.offset(i as isize)
            };
            i += 1;
        }
    } else {
        i = 0;
        while i < 2 * nbEBands {
            *oldLogE.offset(i as isize) =
                if *oldLogE.offset(i as isize) < *oldBandE.offset(i as isize) {
                    *oldLogE.offset(i as isize)
                } else {
                    *oldBandE.offset(i as isize)
                };
            i += 1;
        }
    }
    c = 0;
    loop {
        i = 0;
        while i < start {
            *oldBandE.offset((c * nbEBands + i) as isize) = 0 as opus_val16;
            let ref mut fresh0 = *oldLogE2.offset((c * nbEBands + i) as isize);
            *fresh0 = -28.0f32;
            *oldLogE.offset((c * nbEBands + i) as isize) = *fresh0;
            i += 1;
        }
        i = end;
        while i < nbEBands {
            *oldBandE.offset((c * nbEBands + i) as isize) = 0 as opus_val16;
            let ref mut fresh1 = *oldLogE2.offset((c * nbEBands + i) as isize);
            *fresh1 = -28.0f32;
            *oldLogE.offset((c * nbEBands + i) as isize) = *fresh1;
            i += 1;
        }
        c += 1;
        if !(c < 2) {
            break;
        }
    }
    (*st).rng = (*dec).rng;
    deemphasis(
        out_syn.as_mut_ptr(),
        pcm,
        N,
        CC,
        (*st).downsample,
        ((*mode).preemph).as_ptr(),
        ((*st).preemph_memD).as_mut_ptr(),
        accum,
    );
    (*st).loss_count = 0;
    if ec_tell(dec) > 8 * len {
        return OPUS_INTERNAL_ERROR;
    }
    if ec_get_error(dec) != 0 {
        (*st).error = 1;
    }
    return frame_size / (*st).downsample;
}
pub unsafe fn opus_custom_decoder_ctl_impl(
    st: *mut OpusCustomDecoder,
    request: i32,
    args: VarArgs,
) -> i32 {
    let current_block: u64;
    let mut ap = args;
    match request {
        CELT_SET_START_BAND_REQUEST => {
            let value: i32 = ap.arg::<i32>();
            if value < 0 || value >= (*(*st).mode).nbEBands {
                current_block = 7990025728955927862;
            } else {
                (*st).start = value;
                current_block = 3689906465960840878;
            }
        }
        CELT_SET_END_BAND_REQUEST => {
            let value_0: i32 = ap.arg::<i32>();
            if value_0 < 1 || value_0 > (*(*st).mode).nbEBands {
                current_block = 7990025728955927862;
            } else {
                (*st).end = value_0;
                current_block = 3689906465960840878;
            }
        }
        CELT_SET_CHANNELS_REQUEST => {
            let value_1: i32 = ap.arg::<i32>();
            if value_1 < 1 || value_1 > 2 {
                current_block = 7990025728955927862;
            } else {
                (*st).stream_channels = value_1;
                current_block = 3689906465960840878;
            }
        }
        CELT_GET_AND_CLEAR_ERROR_REQUEST => {
            let value_2: &mut i32 = ap.arg::<&mut i32>();
            *value_2 = (*st).error;
            (*st).error = 0;
            current_block = 3689906465960840878;
        }
        OPUS_GET_LOOKAHEAD_REQUEST => {
            let value_3 = ap.arg::<&mut i32>();
            *value_3 = (*st).overlap / (*st).downsample;
            current_block = 3689906465960840878;
        }
        OPUS_RESET_STATE => {
            let mut i: i32 = 0;
            let mut lpc: *mut opus_val16 = 0 as *mut opus_val16;
            let mut oldBandE: *mut opus_val16 = 0 as *mut opus_val16;
            let mut oldLogE: *mut opus_val16 = 0 as *mut opus_val16;
            let mut oldLogE2: *mut opus_val16 = 0 as *mut opus_val16;
            lpc = ((*st)._decode_mem)
                .as_mut_ptr()
                .offset(((DECODE_BUFFER_SIZE + (*st).overlap) * (*st).channels) as isize)
                as *mut opus_val16;
            oldBandE = lpc.offset(((*st).channels * LPC_ORDER) as isize);
            oldLogE = oldBandE.offset((2 * (*(*st).mode).nbEBands) as isize);
            oldLogE2 = oldLogE.offset((2 * (*(*st).mode).nbEBands) as isize);
            memset(
                &mut (*st).rng as *mut u32 as *mut i8 as *mut core::ffi::c_void,
                0,
                ((opus_custom_decoder_get_size((*st).mode, (*st).channels) as i64
                    - (&mut (*st).rng as *mut u32 as *mut i8).offset_from(st as *mut i8) as i64)
                    as u64)
                    .wrapping_mul(::core::mem::size_of::<i8>() as u64),
            );
            i = 0;
            while i < 2 * (*(*st).mode).nbEBands {
                let ref mut fresh2 = *oldLogE2.offset(i as isize);
                *fresh2 = -28.0f32;
                *oldLogE.offset(i as isize) = *fresh2;
                i += 1;
            }
            (*st).skip_plc = 1;
            current_block = 3689906465960840878;
        }
        OPUS_GET_PITCH_REQUEST => {
            let value_4 = ap.arg::<&mut i32>();
            *value_4 = (*st).postfilter_period;
            current_block = 3689906465960840878;
        }
        CELT_GET_MODE_REQUEST => {
            let value_5 = ap.arg::<&mut *const OpusCustomMode>();
            *value_5 = (*st).mode;
            current_block = 3689906465960840878;
        }
        CELT_SET_SIGNALLING_REQUEST => {
            let value_6: i32 = ap.arg::<i32>();
            (*st).signalling = value_6;
            current_block = 3689906465960840878;
        }
        OPUS_GET_FINAL_RANGE_REQUEST => {
            let value_7 = ap.arg::<&mut u32>();
            *value_7 = (*st).rng;
            current_block = 3689906465960840878;
        }
        OPUS_SET_PHASE_INVERSION_DISABLED_REQUEST => {
            let value_8: i32 = ap.arg::<i32>();
            if value_8 < 0 || value_8 > 1 {
                current_block = 7990025728955927862;
            } else {
                (*st).disable_inv = value_8;
                current_block = 3689906465960840878;
            }
        }
        OPUS_GET_PHASE_INVERSION_DISABLED_REQUEST => {
            let value_9 = ap.arg::<&mut i32>();
            *value_9 = (*st).disable_inv;
            current_block = 3689906465960840878;
        }
        _ => return OPUS_UNIMPLEMENTED,
    }
    match current_block {
        3689906465960840878 => return OPUS_OK,
        _ => return OPUS_BAD_ARG,
    };
}
#[macro_export]
macro_rules! opus_custom_decoder_ctl {
    ($st:expr, $request:expr, $($arg:expr),*) => {
        $crate::opus_custom_decoder_ctl_impl($st, $request, $crate::varargs!($($arg),*))
    };
    ($st:expr, $request:expr) => {
        opus_custom_decoder_ctl!($st, $request,)
    };
    ($st:expr, $request:expr, $($arg:expr),*,) => {
        opus_custom_decoder_ctl!($st, $request, $($arg),*)
    };
}
