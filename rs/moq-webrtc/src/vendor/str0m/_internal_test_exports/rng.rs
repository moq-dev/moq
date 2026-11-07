pub struct Rng<'a>(&'a [u8], usize);

macro_rules! rng_primary {
    ($unit:tt, $above:tt) => {
        pub fn $unit(&mut self, max: $unit) -> Option<$unit> {
            const X: usize = std::mem::size_of::<$unit>();
            let bytes = self.array::<X>()?;
            let u = $unit::from_be_bytes(bytes);
            Some(if max == $unit::MAX {
                u
            } else {
                ((u as $above * max as $above) / $unit::MAX as $above) as $unit
            })
        }
    };
}

impl<'a> Rng<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self(data, 0)
    }

    pub fn slice(&mut self, n: usize) -> Option<&[u8]> {
        let start = self.1;
        let end = self.1 + n;

        if end > self.0.len() {
            // First that hits limit ends anymore data.
            self.1 = self.0.len();
            return None;
        }

        self.1 = end;

        Some(&self.0[start..end])
    }

    pub fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        let slice = self.slice(N)?;

        let mut arr = [0_u8; N];
        arr.copy_from_slice(slice);

        Some(arr)
    }

    pub fn bool(&mut self) -> Option<bool> {
        Some(self.array::<1>()?[0] < 128)
    }

    rng_primary!(u8, u16);
    rng_primary!(u32, u64);
    rng_primary!(u64, u128);
    rng_primary!(usize, u128);
}
