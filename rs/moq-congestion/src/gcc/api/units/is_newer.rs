/*
 *  Copyright (c) 2013 The WebRTC project authors. All Rights Reserved.
 *
 *  Use of this source code is governed by a BSD-style license
 *  that can be found in the LICENSE file in the root of the source
 *  tree. An additional intellectual property rights grant can be found
 *  in the file PATENTS.  All contributing project authors may
 *  be found in the AUTHORS file in the root of the source tree.
 */

const fn is_newer_16(value: u16, prev_value: u16) -> bool {
    // kBreakpoint is the half-way mark for the type U. For instance, for a
    // u16 it will be 0x8000, and for a u32, it will be 0x8000000.
    const BREAKPOINT: u16 = (u16::MAX >> 1) + 1;
    // Distinguish between elements that are exactly kBreakpoint apart.
    // If t1>t2 and |t1-t2| = kBreakpoint: IsNewer(t1,t2)=true,
    // IsNewer(t2,t1)=false
    // rather than having IsNewer(t1,t2) = IsNewer(t2,t1) = false.
    match value.wrapping_sub(prev_value) {
        1..BREAKPOINT => true,
        BREAKPOINT => value > prev_value,
        _ => false,
    }
}

const fn is_newer_32(value: u32, prev_value: u32) -> bool {
    // kBreakpoint is the half-way mark for the type U. For instance, for a
    // u16 it will be 0x8000, and for a u32, it will be 0x8000000.
    const BREAKPOINT: u32 = (u32::MAX >> 1) + 1;
    // Distinguish between elements that are exactly kBreakpoint apart.
    // If t1>t2 and |t1-t2| = kBreakpoint: IsNewer(t1,t2)=true,
    // IsNewer(t2,t1)=false
    // rather than having IsNewer(t1,t2) = IsNewer(t2,t1) = false.
    match value.wrapping_sub(prev_value) {
        1..BREAKPOINT => true,
        BREAKPOINT => value > prev_value,
        _ => false,
    }
}

// NB: Doesn't fulfill strict weak ordering requirements.
//     Mustn't be used as std::map Compare function.
pub const fn is_newer_sequence_number(sequence_number: u16, prev_sequence_number: u16) -> bool {
    is_newer_16(sequence_number, prev_sequence_number)
}

// NB: Doesn't fulfill strict weak ordering requirements.
//     Mustn't be used as std::map Compare function.
pub const fn is_newer_timestamp(timestamp: u32, prev_timestamp: u32) -> bool {
    is_newer_32(timestamp, prev_timestamp)
}

pub const fn latest_sequence_number(sequence_number1: u16, sequence_number2: u16) -> u16 {
    if is_newer_sequence_number(sequence_number1, sequence_number2) {
        sequence_number1
    } else {
        sequence_number2
    }
}

pub const fn latest_timestamp(timestamp1: u32, timestamp2: u32) -> u32 {
    if is_newer_timestamp(timestamp1, timestamp2) {
        timestamp1
    } else {
        timestamp2
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn is_newer_sequence_number_equal() {
        assert!(!is_newer_sequence_number(0x0001, 0x0001));
    }

    #[test]
    fn is_newer_sequence_number_no_wrap() {
        assert!(is_newer_sequence_number(0xFFFF, 0xFFFE));
        assert!(is_newer_sequence_number(0x0001, 0x0000));
        assert!(is_newer_sequence_number(0x0100, 0x00FF));
    }

    #[test]
    fn is_newer_sequence_number_forward_wrap() {
        assert!(is_newer_sequence_number(0x0000, 0xFFFF));
        assert!(is_newer_sequence_number(0x0000, 0xFF00));
        assert!(is_newer_sequence_number(0x00FF, 0xFFFF));
        assert!(is_newer_sequence_number(0x00FF, 0xFF00));
    }

    #[test]
    fn is_newer_sequence_number_backward_wrap() {
        assert!(!is_newer_sequence_number(0xFFFF, 0x0000));
        assert!(!is_newer_sequence_number(0xFF00, 0x0000));
        assert!(!is_newer_sequence_number(0xFFFF, 0x00FF));
        assert!(!is_newer_sequence_number(0xFF00, 0x00FF));
    }

    #[test]
    fn is_newer_sequence_number_half_way_apart() {
        assert!(is_newer_sequence_number(0x8000, 0x0000));
        assert!(!is_newer_sequence_number(0x0000, 0x8000));
    }

    #[test]
    fn is_newer_timestamp_equal() {
        assert!(!is_newer_timestamp(0x00000001, 0x000000001));
    }

    #[test]
    fn is_newer_timestamp_no_wrap() {
        assert!(is_newer_timestamp(0xFFFFFFFF, 0xFFFFFFFE));
        assert!(is_newer_timestamp(0x00000001, 0x00000000));
        assert!(is_newer_timestamp(0x00010000, 0x0000FFFF));
    }

    #[test]
    fn is_newer_timestamp_forward_wrap() {
        assert!(is_newer_timestamp(0x00000000, 0xFFFFFFFF));
        assert!(is_newer_timestamp(0x00000000, 0xFFFF0000));
        assert!(is_newer_timestamp(0x0000FFFF, 0xFFFFFFFF));
        assert!(is_newer_timestamp(0x0000FFFF, 0xFFFF0000));
    }

    #[test]
    fn is_newer_timestamp_backward_wrap() {
        assert!(!is_newer_timestamp(0xFFFFFFFF, 0x00000000));
        assert!(!is_newer_timestamp(0xFFFF0000, 0x00000000));
        assert!(!is_newer_timestamp(0xFFFFFFFF, 0x0000FFFF));
        assert!(!is_newer_timestamp(0xFFFF0000, 0x0000FFFF));
    }

    #[test]
    fn is_newer_timestamp_half_way_apart() {
        assert!(is_newer_timestamp(0x80000000, 0x00000000));
        assert!(!is_newer_timestamp(0x00000000, 0x80000000));
    }

    #[test]
    fn latest_sequence_number_no_wrap() {
        assert_eq!(0xFFFF, latest_sequence_number(0xFFFF, 0xFFFE));
        assert_eq!(0x0001, latest_sequence_number(0x0001, 0x0000));
        assert_eq!(0x0100, latest_sequence_number(0x0100, 0x00FF));

        assert_eq!(0xFFFF, latest_sequence_number(0xFFFE, 0xFFFF));
        assert_eq!(0x0001, latest_sequence_number(0x0000, 0x0001));
        assert_eq!(0x0100, latest_sequence_number(0x00FF, 0x0100));
    }

    #[test]
    fn latest_sequence_number_wrap() {
        assert_eq!(0x0000, latest_sequence_number(0x0000, 0xFFFF));
        assert_eq!(0x0000, latest_sequence_number(0x0000, 0xFF00));
        assert_eq!(0x00FF, latest_sequence_number(0x00FF, 0xFFFF));
        assert_eq!(0x00FF, latest_sequence_number(0x00FF, 0xFF00));

        assert_eq!(0x0000, latest_sequence_number(0xFFFF, 0x0000));
        assert_eq!(0x0000, latest_sequence_number(0xFF00, 0x0000));
        assert_eq!(0x00FF, latest_sequence_number(0xFFFF, 0x00FF));
        assert_eq!(0x00FF, latest_sequence_number(0xFF00, 0x00FF));
    }

    #[test]
    fn latest_timestamp_no_wrap() {
        assert_eq!(0xFFFFFFFF, latest_timestamp(0xFFFFFFFF, 0xFFFFFFFE));
        assert_eq!(0x00000001, latest_timestamp(0x00000001, 0x00000000));
        assert_eq!(0x00010000, latest_timestamp(0x00010000, 0x0000FFFF));
    }

    #[test]
    fn latest_timestamp_wrap() {
        assert_eq!(0x00000000, latest_timestamp(0x00000000, 0xFFFFFFFF));
        assert_eq!(0x00000000, latest_timestamp(0x00000000, 0xFFFF0000));
        assert_eq!(0x0000FFFF, latest_timestamp(0x0000FFFF, 0xFFFFFFFF));
        assert_eq!(0x0000FFFF, latest_timestamp(0x0000FFFF, 0xFFFF0000));

        assert_eq!(0x00000000, latest_timestamp(0xFFFFFFFF, 0x00000000));
        assert_eq!(0x00000000, latest_timestamp(0xFFFF0000, 0x00000000));
        assert_eq!(0x0000FFFF, latest_timestamp(0xFFFFFFFF, 0x0000FFFF));
        assert_eq!(0x0000FFFF, latest_timestamp(0xFFFF0000, 0x0000FFFF));
    }
}
