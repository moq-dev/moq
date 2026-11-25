#[derive(Copy, Clone)]
#[repr(C)]
pub struct OpusCustomMode {
    pub(crate) Fs: i32,
    pub(crate) overlap: i32,
    pub(crate) nbEBands: i32,
    pub(crate) effEBands: i32,
    pub(crate) preemph: [opus_val16; 4],
    pub(crate) eBands: *const i16,
    pub(crate) maxLM: i32,
    pub(crate) nbShortMdcts: i32,
    pub(crate) shortMdctSize: i32,
    pub(crate) nbAllocVectors: i32,
    pub(crate) allocVectors: *const u8,
    pub(crate) logN: *const i16,
    pub(crate) window: *const opus_val16,
    pub(crate) mdct: mdct_lookup<'static>,
    pub(crate) cache: PulseCache,
}
#[derive(Copy, Clone)]
#[repr(C)]
pub struct PulseCache {
    pub size: i32,
    pub index: *const i16,
    pub bits: *const u8,
    pub caps: *const u8,
}
pub const MAX_PERIOD: i32 = 1024;

pub mod arch_h {
    pub type opus_val16 = f32;
}
pub mod stddef_h {
    pub const NULL: i32 = 0;
}

pub mod static_modes_float_h;

pub use self::arch_h::opus_val16;
pub use self::static_modes_float_h::{
    cache_bits50, cache_caps50, cache_index50, fft_bitrev120, fft_bitrev240, fft_bitrev480,
    fft_bitrev60, fft_twiddles48000_960, logN400, static_mode_list, window120,
};
pub use self::stddef_h::NULL;
use crate::celt::mdct::mdct_lookup;
use crate::src::opus_defines::{OPUS_BAD_ARG, OPUS_OK};

static eband5ms: [i16; 22] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28, 34, 40, 48, 60, 78, 100,
];
static band_allocation: [u8; 231] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 90, 80, 75, 69, 63, 56, 49, 40,
    34, 29, 20, 18, 10, 0, 0, 0, 0, 0, 0, 0, 0, 110, 100, 90, 84, 78, 71, 65, 58, 51, 45, 39, 32,
    26, 20, 12, 0, 0, 0, 0, 0, 0, 118, 110, 103, 93, 86, 80, 75, 70, 65, 59, 53, 47, 40, 31, 23,
    15, 4, 0, 0, 0, 0, 126, 119, 112, 104, 95, 89, 83, 78, 72, 66, 60, 54, 47, 39, 32, 25, 17, 12,
    1, 0, 0, 134, 127, 120, 114, 103, 97, 91, 85, 78, 72, 66, 60, 54, 47, 41, 35, 29, 23, 16, 10,
    1, 144, 137, 130, 124, 113, 107, 101, 95, 88, 82, 76, 70, 64, 57, 51, 45, 39, 33, 26, 15, 1,
    152, 145, 138, 132, 123, 117, 111, 105, 98, 92, 86, 80, 74, 67, 61, 55, 49, 43, 36, 20, 1, 162,
    155, 148, 142, 133, 127, 121, 115, 108, 102, 96, 90, 84, 77, 71, 65, 59, 53, 46, 30, 1, 172,
    165, 158, 152, 143, 137, 131, 125, 118, 112, 106, 100, 94, 87, 81, 75, 69, 63, 56, 45, 20, 200,
    200, 200, 200, 200, 200, 200, 200, 198, 193, 188, 183, 178, 173, 168, 163, 158, 153, 148, 129,
    104,
];

pub unsafe fn opus_custom_mode_create(
    Fs: i32,
    frame_size: i32,
    error: *mut i32,
) -> *const OpusCustomMode {
    // TODO: make static_mode_list non-mutable (requires Sync)
    // TODO: maybe return Result instead of error code?
    for mode in static_mode_list {
        for j in 0..4 {
            if Fs == (*mode).Fs && frame_size << j == (*mode).shortMdctSize * (*mode).nbShortMdcts {
                if !error.is_null() {
                    *error = OPUS_OK;
                }
                return mode;
            }
        }
    }
    if !error.is_null() {
        *error = OPUS_BAD_ARG;
    }
    return NULL as *mut OpusCustomMode;
}
