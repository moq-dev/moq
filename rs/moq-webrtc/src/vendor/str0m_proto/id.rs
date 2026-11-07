use std::fmt;
use std::str::from_utf8;

use crate::NonCryptographicRng;

// deliberate subset of ice-char, etc that are "safe"
const CHARS: &[u8] = b"abcdefghijklmnopqrstuvxyzABCDEFGHIJKLMNOPQRSTUVXYZ0123456789";

pub struct Id<const L: usize>([u8; L]);

impl<const L: usize> Id<L> {
    pub fn random() -> Id<L> {
        let mut x = [0; L];
        for val in x.iter_mut().take(L) {
            let y: f32 = NonCryptographicRng::f32();
            let idx = (CHARS.len() as f32 * y).floor() as usize;
            *val = CHARS[idx];
        }
        Id(x)
    }

    pub fn into_array(self) -> [u8; L] {
        self.0
    }
}

impl<const L: usize> Default for Id<L> {
    fn default() -> Self {
        Id::random()
    }
}

impl<const L: usize> fmt::Display for Id<L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = from_utf8(&self.0).expect("ascii characters");
        write!(f, "{s}")
    }
}
