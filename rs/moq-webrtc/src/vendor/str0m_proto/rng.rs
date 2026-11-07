pub struct NonCryptographicRng;

impl NonCryptographicRng {
    #[inline(always)]
    pub fn u8() -> u8 {
        fastrand::u8(..)
    }

    #[inline(always)]
    pub fn u16() -> u16 {
        fastrand::u16(..)
    }

    #[inline(always)]
    pub fn u32() -> u32 {
        fastrand::u32(..)
    }

    #[inline(always)]
    pub fn u64() -> u64 {
        fastrand::u64(..)
    }

    #[inline(always)]
    pub fn f32() -> f32 {
        fastrand::f32()
    }
}
