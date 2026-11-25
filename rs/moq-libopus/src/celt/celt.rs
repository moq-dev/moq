use crate::celt::modes::OpusCustomMode;

pub mod arch_h {
    pub type opus_val16 = f32;
    pub type opus_val32 = f32;
}

pub use self::arch_h::{opus_val16, opus_val32};
use crate::externs::memmove;

pub static trim_icdf: [u8; 11] = [126, 124, 119, 109, 87, 41, 19, 9, 4, 2, 0];
pub static spread_icdf: [u8; 4] = [25, 23, 2, 0];
pub static tapset_icdf: [u8; 3] = [2, 1, 0];
pub static tf_select_table: [[i8; 8]; 4] = [
    [0, -1, 0, -1, 0, -1, 0, -1],
    [0, -1, 0, -2, 1, 0, 1, -1],
    [0, -2, 0, -3, 2, 0, 1, -1],
    [0, -2, 0, -3, 3, 0, 1, -1],
];

pub const OPUS_SET_ENERGY_MASK_REQUEST: i32 = 10026;
pub const CELT_GET_MODE_REQUEST: i32 = 10015;
pub const CELT_SET_SILK_INFO_REQUEST: i32 = 10028;
pub const CELT_SET_ANALYSIS_REQUEST: i32 = 10022;
pub const CELT_SET_SIGNALLING_REQUEST: i32 = 10016;
pub const OPUS_SET_LFE_REQUEST: i32 = 10024;
pub const CELT_SET_START_BAND_REQUEST: i32 = 10010;
pub const CELT_SET_CHANNELS_REQUEST: i32 = 10008;
pub const CELT_SET_END_BAND_REQUEST: i32 = 10012;
pub const CELT_SET_PREDICTION_REQUEST: i32 = 10002;
pub const CELT_GET_AND_CLEAR_ERROR_REQUEST: i32 = 10007;

pub const COMBFILTER_MAXPERIOD: i32 = 1024;
pub const COMBFILTER_MINPERIOD: i32 = 15;

pub fn resampling_factor(rate: i32) -> i32 {
    match rate {
        48000 => 1,
        24000 => 2,
        16000 => 3,
        12000 => 4,
        8000 => 6,
        _ => panic!("Unsupported sampling rate: {}", rate),
    }
}
unsafe fn comb_filter_const_c(
    y: *mut opus_val32,
    x: *mut opus_val32,
    T: i32,
    N: i32,
    g10: opus_val16,
    g11: opus_val16,
    g12: opus_val16,
) {
    let mut x0: opus_val32 = 0.;
    let mut x1: opus_val32 = 0.;
    let mut x2: opus_val32 = 0.;
    let mut x3: opus_val32 = 0.;
    let mut x4: opus_val32 = 0.;
    let mut i: i32 = 0;
    x4 = *x.offset((-T - 2) as isize);
    x3 = *x.offset((-T - 1) as isize);
    x2 = *x.offset(-T as isize);
    x1 = *x.offset((-T + 1) as isize);
    i = 0;
    while i < N {
        x0 = *x.offset((i - T + 2) as isize);
        *y.offset(i as isize) =
            *x.offset(i as isize) + g10 * x2 + g11 * (x1 + x3) + g12 * (x0 + x4);
        *y.offset(i as isize) = *y.offset(i as isize);
        x4 = x3;
        x3 = x2;
        x2 = x1;
        x1 = x0;
        i += 1;
    }
}
pub unsafe fn comb_filter(
    y: *mut opus_val32,
    x: *mut opus_val32,
    mut T0: i32,
    mut T1: i32,
    N: i32,
    g0: opus_val16,
    g1: opus_val16,
    tapset0: i32,
    tapset1: i32,
    window: *const opus_val16,
    mut overlap: i32,
    _arch: i32,
) {
    let mut i: i32 = 0;
    let mut g00: opus_val16 = 0.;
    let mut g01: opus_val16 = 0.;
    let mut g02: opus_val16 = 0.;
    let mut g10: opus_val16 = 0.;
    let mut g11: opus_val16 = 0.;
    let mut g12: opus_val16 = 0.;
    let mut x0: opus_val32 = 0.;
    let mut x1: opus_val32 = 0.;
    let mut x2: opus_val32 = 0.;
    let mut x3: opus_val32 = 0.;
    let mut x4: opus_val32 = 0.;
    static mut gains: [[opus_val16; 3]; 3] = [
        [0.3066406250f32, 0.2170410156f32, 0.1296386719f32],
        [0.4638671875f32, 0.2680664062f32, 0.0f32],
        [0.7998046875f32, 0.1000976562f32, 0.0f32],
    ];
    if g0 == 0 as f32 && g1 == 0 as f32 {
        if x != y {
            memmove(
                y as *mut core::ffi::c_void,
                x as *const core::ffi::c_void,
                (N as u64)
                    .wrapping_mul(::core::mem::size_of::<opus_val32>() as u64)
                    .wrapping_add((0 * y.offset_from(x) as i64) as u64),
            );
        }
        return;
    }
    T0 = if T0 > 15 { T0 } else { 15 };
    T1 = if T1 > 15 { T1 } else { 15 };
    g00 = g0 * gains[tapset0 as usize][0 as usize];
    g01 = g0 * gains[tapset0 as usize][1 as usize];
    g02 = g0 * gains[tapset0 as usize][2 as usize];
    g10 = g1 * gains[tapset1 as usize][0 as usize];
    g11 = g1 * gains[tapset1 as usize][1 as usize];
    g12 = g1 * gains[tapset1 as usize][2 as usize];
    x1 = *x.offset((-T1 + 1) as isize);
    x2 = *x.offset(-T1 as isize);
    x3 = *x.offset((-T1 - 1) as isize);
    x4 = *x.offset((-T1 - 2) as isize);
    if g0 == g1 && T0 == T1 && tapset0 == tapset1 {
        overlap = 0;
    }
    i = 0;
    while i < overlap {
        let mut f: opus_val16 = 0.;
        x0 = *x.offset((i - T1 + 2) as isize);
        f = *window.offset(i as isize) * *window.offset(i as isize);
        *y.offset(i as isize) = *x.offset(i as isize)
            + (1.0f32 - f) * g00 * *x.offset((i - T0) as isize)
            + (1.0f32 - f)
                * g01
                * (*x.offset((i - T0 + 1) as isize) + *x.offset((i - T0 - 1) as isize))
            + (1.0f32 - f)
                * g02
                * (*x.offset((i - T0 + 2) as isize) + *x.offset((i - T0 - 2) as isize))
            + f * g10 * x2
            + f * g11 * (x1 + x3)
            + f * g12 * (x0 + x4);
        *y.offset(i as isize) = *y.offset(i as isize);
        x4 = x3;
        x3 = x2;
        x2 = x1;
        x1 = x0;
        i += 1;
    }
    if g1 == 0 as f32 {
        if x != y {
            memmove(
                y.offset(overlap as isize) as *mut core::ffi::c_void,
                x.offset(overlap as isize) as *const core::ffi::c_void,
                ((N - overlap) as u64)
                    .wrapping_mul(::core::mem::size_of::<opus_val32>() as u64)
                    .wrapping_add(
                        (0 * y
                            .offset(overlap as isize)
                            .offset_from(x.offset(overlap as isize))
                            as i64) as u64,
                    ),
            );
        }
        return;
    }
    comb_filter_const_c(
        y.offset(i as isize),
        x.offset(i as isize),
        T1,
        N - i,
        g10,
        g11,
        g12,
    );
}
pub unsafe fn init_caps(m: *const OpusCustomMode, cap: *mut i32, LM: i32, C: i32) {
    let mut i: i32 = 0;
    i = 0;
    while i < (*m).nbEBands {
        let mut N: i32 = 0;
        N = (*((*m).eBands).offset((i + 1) as isize) as i32
            - *((*m).eBands).offset(i as isize) as i32)
            << LM;
        *cap.offset(i as isize) =
            (*((*m).cache.caps).offset(((*m).nbEBands * (2 * LM + C - 1) + i) as isize) as i32
                + 64)
                * C
                * N
                >> 2;
        i += 1;
    }
}

pub fn opus_strerror(error: i32) -> &'static str {
    static error_strings: [&str; 8] = [
        "success (0)",
        "invalid argument (-1)",
        "buffer too small (-2)",
        "internal error (-3)",
        "corrupted stream (-4)",
        "request not implemented (-5)",
        "invalid state (-6)",
        "memory allocation failed (-7)",
    ];
    if error > 0 || error < -7 {
        "unknown error"
    } else {
        error_strings[-error as usize]
    }
}

pub fn opus_get_version_string() -> &'static str {
    "unsafe-libopus (rust port) 1.3.1"
}
