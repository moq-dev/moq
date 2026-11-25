#![forbid(unsafe_code)]

mod ar2;
mod down_fir;
mod iir_fir;
mod rom;
mod up2_hq;

mod down2;
mod down2_3;

use down_fir::{silk_resampler_private_down_FIR, ResamplerDownFirParams, ResamplerDownFirState};
use iir_fir::{silk_resampler_private_IIR_FIR, ResamplerIirFirState};
use rom::{
    silk_Resampler_1_2_COEFS, silk_Resampler_1_3_COEFS, silk_Resampler_1_4_COEFS,
    silk_Resampler_1_6_COEFS, silk_Resampler_2_3_COEFS, silk_Resampler_3_4_COEFS,
    RESAMPLER_DOWN_ORDER_FIR0, RESAMPLER_DOWN_ORDER_FIR1, RESAMPLER_DOWN_ORDER_FIR2,
};
use std::cmp::Ordering;
use up2_hq::{silk_resampler_private_up2_HQ, ResamplerUp2HqState};

pub use down2::silk_resampler_down2;
pub use down2_3::silk_resampler_down2_3;

const RESAMPLER_MAX_BATCH_SIZE_MS: i32 = 10;
const RESAMPLER_MAX_FS_KHZ: usize = 48;
const RESAMPLER_MAX_BATCH_SIZE_IN: usize =
    RESAMPLER_MAX_BATCH_SIZE_MS as usize * RESAMPLER_MAX_FS_KHZ;

/*
 * Matrix of resampling methods used:
 *                                 Fs_out (kHz)
 *                        8      12     16     24     48
 *
 *               8        C      UF     U      UF     UF
 *              12        AF     C      UF     U      UF
 * Fs_in (kHz)  16        D      AF     C      UF     UF
 *              24        AF     D      AF     C      U
 *              48        AF     AF     AF     D      C
 *
 * C   -> Copy (no resampling)
 * D   -> Allpass-based 2x downsampling
 * U   -> Allpass-based 2x upsampling
 * UF  -> Allpass-based 2x upsampling followed by FIR interpolation
 * AF  -> AR2 filter followed by FIR interpolation
 */

#[rustfmt::skip]
static delay_matrix_enc: [[i8; 3]; 5] = [
    /* in  \ out  8  12  16 */
    /*  8 */   [  6,  0,  3 ],
    /* 12 */   [  0,  7,  3 ],
    /* 16 */   [  0,  1, 10 ],
    /* 24 */   [  0,  2,  6 ],
    /* 48 */   [ 18, 10, 12 ],
];
#[rustfmt::skip]
static delay_matrix_dec: [[i8; 5]; 3] = [
    /* in  \ out  8  12  16  24  48 */
    /*  8 */   [  4,  0,  2,  0,  0 ],
    /* 12 */   [  0,  9,  4,  7,  4 ],
    /* 16 */   [  0,  3, 12,  7,  7 ],
];

/* Simple way to make [8000, 12000, 16000, 24000, 48000] to [0, 1, 2, 3, 4] */
fn rate_id(r: i32) -> usize {
    match r {
        8000 => 0,
        12000 => 1,
        16000 => 2,
        24000 => 3,
        48000 => 4,
        _ => unreachable!("unsupported sampling rate"),
    }
}

const SILK_RESAMPLER_MAX_FIR_ORDER: usize = 36;

#[derive(Copy, Clone)]
pub struct ResamplerState {
    params: ResamplerParams,
    mode: ResamplerMode,
    delay_buf: [i16; 48],
}

#[derive(Copy, Clone)]
struct ResamplerParams {
    pub batch_size: usize,
    pub inv_ratio_q16: i32,
    pub fs_in_khz: usize,
    pub fs_out_khz: usize,
    pub input_delay: usize,
}

/// Includes the resampler mode, as well as the necessary params and state
#[derive(Copy, Clone)]
enum ResamplerMode {
    Copy,
    Up2Hq(ResamplerUp2HqState),
    IirFir(ResamplerIirFirState),
    DownFir(ResamplerDownFirParams, ResamplerDownFirState),
}

#[derive(Copy, Clone)]
#[repr(C)]
pub union sFIR_union {
    pub i32_0: [i32; SILK_RESAMPLER_MAX_FIR_ORDER],
    pub i16_0: [i16; SILK_RESAMPLER_MAX_FIR_ORDER],
}

pub fn silk_resampler_init(Fs_Hz_in: i32, Fs_Hz_out: i32, forEnc: i32) -> ResamplerState {
    let inputDelay = if forEnc != 0 {
        if !matches!(Fs_Hz_in, 8000 | 12000 | 16000 | 24000 | 48000)
            || !matches!(Fs_Hz_out, 8000 | 12000 | 16000)
        {
            // see comments in `[unsafe_libopus::silk::check_control_input]`
            // TODO: we should probably make this function return a Result..
            panic!("libopus: assert(0) called");
            // return -1;
        }

        delay_matrix_enc[rate_id(Fs_Hz_in)][rate_id(Fs_Hz_out)] as i32
    } else {
        if !matches!(Fs_Hz_in, 8000 | 12000 | 16000)
            || !matches!(Fs_Hz_out, 8000 | 12000 | 16000 | 24000 | 48000)
        {
            // see comments in `[unsafe_libopus::silk::check_control_input]`
            // TODO: we should probably make this function return a Result..
            panic!("libopus: assert(0) called");
            // return -1;
        }

        delay_matrix_dec[rate_id(Fs_Hz_in)][rate_id(Fs_Hz_out)] as i32
    };

    let Fs_in_kHz = Fs_Hz_in / 1000;
    let Fs_out_kHz = Fs_Hz_out / 1000;
    let batchSize = Fs_in_kHz * RESAMPLER_MAX_BATCH_SIZE_MS;

    let mut up2x = 0;
    let mode = match Fs_Hz_out.cmp(&Fs_Hz_in) {
        Ordering::Greater => {
            // Upsample
            // Fs_out : Fs_in = 2 : 1
            if Fs_Hz_out == Fs_Hz_in * 2 {
                // Special case: directly use 2x upsampler
                ResamplerMode::Up2Hq(ResamplerUp2HqState::default())
            } else {
                // Default resampler
                up2x = 1;
                ResamplerMode::IirFir(ResamplerIirFirState::default())
            }
        }
        Ordering::Less => {
            // downsample
            let params = if Fs_Hz_out * 4 == Fs_Hz_in * 3 {
                // Fs_out : Fs_in = 3 : 4
                ResamplerDownFirParams {
                    fir_order: RESAMPLER_DOWN_ORDER_FIR0,
                    fir_fracs: 3,
                    coefs: &silk_Resampler_3_4_COEFS,
                }
            } else if Fs_Hz_out * 3 == Fs_Hz_in * 2 {
                // Fs_out : Fs_in = 2 : 3
                ResamplerDownFirParams {
                    fir_order: RESAMPLER_DOWN_ORDER_FIR0,
                    fir_fracs: 2,
                    coefs: &silk_Resampler_2_3_COEFS,
                }
            } else if Fs_Hz_out * 2 == Fs_Hz_in {
                // Fs_out : Fs_in = 1 : 2
                ResamplerDownFirParams {
                    fir_order: RESAMPLER_DOWN_ORDER_FIR1,
                    fir_fracs: 1,
                    coefs: &silk_Resampler_1_2_COEFS,
                }
            } else if Fs_Hz_out * 3 == Fs_Hz_in {
                // Fs_out : Fs_in = 1 : 3
                ResamplerDownFirParams {
                    fir_order: RESAMPLER_DOWN_ORDER_FIR2,
                    fir_fracs: 1,
                    coefs: &silk_Resampler_1_3_COEFS,
                }
            } else if Fs_Hz_out * 4 == Fs_Hz_in {
                // Fs_out : Fs_in = 1 : 4
                ResamplerDownFirParams {
                    fir_order: RESAMPLER_DOWN_ORDER_FIR2,
                    fir_fracs: 1,
                    coefs: &silk_Resampler_1_4_COEFS,
                }
            } else if Fs_Hz_out * 6 == Fs_Hz_in {
                // Fs_out : Fs_in = 1 : 6
                ResamplerDownFirParams {
                    fir_order: RESAMPLER_DOWN_ORDER_FIR2,
                    fir_fracs: 1,
                    coefs: &silk_Resampler_1_6_COEFS,
                }
            } else {
                unreachable!("Unsupported resampling ratio: {} : {}", Fs_Hz_out, Fs_Hz_in);
            };

            ResamplerMode::DownFir(params, ResamplerDownFirState::default())
        }
        Ordering::Equal => ResamplerMode::Copy,
    };

    /* Ratio of input/output samples */
    let mut invRatio_Q16 =
        (((((Fs_Hz_in as u32) << (14 + up2x)) as i32 / Fs_Hz_out) as u32) << 2) as i32;
    /* Make sure the ratio is rounded up */
    while (((invRatio_Q16 as i64 * Fs_Hz_out as i64) >> 16) as i32)
        < ((Fs_Hz_in as u32) << up2x) as i32
    {
        invRatio_Q16 += 1;
    }

    let params = ResamplerParams {
        batch_size: batchSize as usize,
        inv_ratio_q16: invRatio_Q16,
        fs_in_khz: Fs_in_kHz as usize,
        fs_out_khz: Fs_out_kHz as usize,
        input_delay: inputDelay as usize,
    };

    ResamplerState {
        params,
        mode,
        delay_buf: [0; 48],
    }
}

/* Resampler: convert from one sampling rate to another */
/* Input and output sampling rate are at most 48000 Hz  */
pub fn silk_resampler(S: &mut ResamplerState, out: &mut [i16], in_0: &[i16]) -> i32 {
    /* Need at least 1 ms of input data */
    assert!(in_0.len() >= S.params.fs_in_khz);
    /* Delay can't exceed the 1 ms of buffering */
    assert!(S.params.input_delay <= S.params.fs_in_khz);

    let nSamples = S.params.fs_in_khz - S.params.input_delay;

    /* Copy to delay buffer */
    S.delay_buf[S.params.input_delay..][..nSamples].copy_from_slice(&in_0[..nSamples]);

    let delay_in = &S.delay_buf[..S.params.fs_in_khz];
    let rest_in = &in_0[nSamples..][..in_0.len() - S.params.fs_in_khz];
    let (delay_out, rest_out) = out.split_at_mut(S.params.fs_out_khz);

    // ensure we have exactly the right amount of space in the out buffer
    let rest_out = &mut rest_out[..rest_in.len() * S.params.fs_out_khz / S.params.fs_in_khz];

    match &mut S.mode {
        ResamplerMode::Up2Hq(state) => {
            silk_resampler_private_up2_HQ(state, delay_out, delay_in);
            silk_resampler_private_up2_HQ(state, rest_out, rest_in);
        }
        ResamplerMode::IirFir(state) => {
            silk_resampler_private_IIR_FIR(&S.params, state, delay_out, delay_in);
            silk_resampler_private_IIR_FIR(&S.params, state, rest_out, rest_in);
        }
        ResamplerMode::DownFir(ref params, state) => {
            silk_resampler_private_down_FIR(&S.params, params, state, delay_out, delay_in);
            silk_resampler_private_down_FIR(&S.params, params, state, rest_out, rest_in);
        }
        ResamplerMode::Copy => {
            delay_out.copy_from_slice(delay_in);
            rest_out.copy_from_slice(rest_in);
        }
    }

    /* Copy to delay buffer */
    S.delay_buf[..S.params.input_delay].copy_from_slice(&in_0[in_0.len() - S.params.input_delay..]);

    0
}
