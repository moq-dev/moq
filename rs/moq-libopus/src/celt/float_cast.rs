pub const CELT_SIG_SCALE: f32 = 32768.0f32;

#[inline]
pub fn FLOAT2INT16(x: f32) -> i16 {
    let x = x * CELT_SIG_SCALE;
    let x = x.max(-32768.0);
    let x = x.min(32767.0);
    float2int(x) as i16
}
#[inline]
pub fn float2int(x: f32) -> i32 {
    (x + 0.5).floor() as i32
}
