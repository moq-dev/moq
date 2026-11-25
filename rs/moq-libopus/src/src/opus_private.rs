pub const MODE_SILK_ONLY: i32 = 1000;
pub const MODE_HYBRID: i32 = 1001;
pub const MODE_CELT_ONLY: i32 = 1002;

pub const OPUS_SET_VOICE_RATIO_REQUEST: i32 = 11018;
pub const OPUS_GET_VOICE_RATIO_REQUEST: i32 = 11019;
pub const OPUS_SET_FORCE_MODE_REQUEST: i32 = 11002;

#[inline]
pub fn align(i: i32) -> i32 {
    let alignment: u32 = 8 as u64 as u32;
    return (i as u32)
        .wrapping_add(alignment)
        .wrapping_sub(1)
        .wrapping_div(alignment)
        .wrapping_mul(alignment) as i32;
}
