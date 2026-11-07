/// One byte bit pattern to allow matching against specific bits.
#[derive(Copy, Clone)]
pub(crate) struct BitPattern {
    mask: u8,
    masked_value: u8,
}

impl BitPattern {
    pub(crate) fn matches(&self, profile_iop: u8) -> bool {
        ((profile_iop ^ self.masked_value) & self.mask) == 0x0
    }

    /// Create a bit pattern. Use `1` for bits that must be set, `0` for bits that must not be
    /// set, and `x` for bits that can take any value.
    ///
    /// Panics if any other byte values than ASCII `1`, `0`, or `x` is used.
    pub(crate) const fn new(pattern: [u8; 8]) -> Self {
        const fn bit_to_mask_bit(pattern: [u8; 8], i: usize) -> u8 {
            let bit = pattern[7 - i];
            match bit {
                b'1' | b'0' => 0x1 << i,
                b'x' => 0x0 << i,
                _ => panic!("Invalid bit pattern in IOPPattern only ASCII 1, 0, and x are allowed"),
            }
        }

        const fn to_mask(pattern: [u8; 8]) -> u8 {
            bit_to_mask_bit(pattern, 7)
                | bit_to_mask_bit(pattern, 6)
                | bit_to_mask_bit(pattern, 5)
                | bit_to_mask_bit(pattern, 4)
                | bit_to_mask_bit(pattern, 3)
                | bit_to_mask_bit(pattern, 2)
                | bit_to_mask_bit(pattern, 1)
                | bit_to_mask_bit(pattern, 0)
        }

        const fn bit_to_mask_value_bit(pattern: [u8; 8], i: usize) -> u8 {
            let bit = pattern[7 - i];
            match bit {
                b'1' => 0x1 << i,
                b'x' | b'0' => 0x0 << i,
                _ => panic!("Invalid bit pattern in IOPPattern only ASCII 1, 0, and x are allowed"),
            }
        }

        const fn to_mask_value(pattern: [u8; 8]) -> u8 {
            bit_to_mask_value_bit(pattern, 7)
                | bit_to_mask_value_bit(pattern, 6)
                | bit_to_mask_value_bit(pattern, 5)
                | bit_to_mask_value_bit(pattern, 4)
                | bit_to_mask_value_bit(pattern, 3)
                | bit_to_mask_value_bit(pattern, 2)
                | bit_to_mask_value_bit(pattern, 1)
                | bit_to_mask_value_bit(pattern, 0)
        }
        let mask = to_mask(pattern);
        let masked_value = to_mask_value(pattern);

        Self { mask, masked_value }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_iop_pattern_matching() {
        let all_any = BitPattern::new(*b"xxxxxxxx");
        for x in 0..255 {
            assert!(all_any.matches(x));
        }

        let stripes = BitPattern::new(*b"10101010");
        assert!(stripes.matches(0b1010_1010));
        assert!(!stripes.matches(0b1011_1010));

        let inverse_stripes = BitPattern::new(*b"01010101");
        assert!(inverse_stripes.matches(0b0101_0101));
        assert!(!inverse_stripes.matches(0b0111_0001));

        let high_bits = BitPattern::new(*b"1101xxxx");
        assert!(high_bits.matches(0b1101_0101));
        assert!(!high_bits.matches(0b1001_0101));

        let mid_bits = BitPattern::new(*b"xx0110xx");
        assert!(mid_bits.matches(0b0101_1001));
        assert!(!mid_bits.matches(0b1000_1001));

        let only_ones = BitPattern::new(*b"11111111");
        assert!(only_ones.matches(0b1111_1111));
        assert!(!only_ones.matches(0b1110_1111));

        let only_zeros = BitPattern::new(*b"00000000");
        assert!(only_zeros.matches(0b0000_0000));
        assert!(!only_zeros.matches(0b0000_0010));

        let mixed_pattern = BitPattern::new(*b"1x0x1x01");
        assert!(mixed_pattern.matches(0b11011001));
        assert!(!mixed_pattern.matches(0b11011011));

        let complex_pattern = BitPattern::new(*b"1xx01x0x");
        assert!(complex_pattern.matches(0b10001001));
        assert!(!complex_pattern.matches(0b10101010));
    }
}
