//!  NLSF stabilizer:
//!
//!  - Moves NLSFs further apart if they are too close
//!  - Moves NLSFs away from borders if they are too close
//!  - High effort to achieve a modification with minimum
//!      Euclidean distance to input vector
//!  - Output are sorted NLSF coefficients
//!

use crate::silk::sort::silk_insertion_sort_increasing_all_values_int16;
use crate::silk::SigProc_FIX::{silk_max_int, silk_min_int};

pub mod typedef_h {
    pub const silk_int16_MIN: i32 = i16::MIN as i32;
    pub const silk_int16_MAX: i32 = i16::MAX as i32;
}

pub use self::typedef_h::{silk_int16_MAX, silk_int16_MIN};

pub const MAX_LOOPS: i32 = 20;

/// NLSF stabilizer, for a single input data vector
pub fn silk_NLSF_stabilize(NLSF_Q15: &mut [i16], NDeltaMin_Q15: &[i16]) {
    let mut i: usize = 0;
    let mut I: usize = 0;
    let mut k: usize = 0;
    let mut loops: i32 = 0;
    let mut center_freq_Q15: i16 = 0;
    let mut diff_Q15: i32 = 0;
    let mut min_diff_Q15: i32 = 0;
    let mut min_center_Q15: i32 = 0;
    let mut max_center_Q15: i32 = 0;

    let L = NLSF_Q15.len();

    /* This is necessary to ensure an output within range of a opus_int16 */
    assert!(NDeltaMin_Q15[L] >= 1);

    loops = 0;
    while loops < MAX_LOOPS {
        /**************************/
        /* Find smallest distance */
        /**************************/
        /* First element */
        min_diff_Q15 = NLSF_Q15[0] as i32 - NDeltaMin_Q15[0] as i32;
        I = 0;
        /* Middle elements */
        i = 1;
        while i < L {
            diff_Q15 = NLSF_Q15[i] as i32 - (NLSF_Q15[i - 1] as i32 + NDeltaMin_Q15[i] as i32);
            if diff_Q15 < min_diff_Q15 {
                min_diff_Q15 = diff_Q15;
                I = i;
            }
            i += 1;
        }
        /* Last element */
        diff_Q15 = ((1) << 15) - (NLSF_Q15[L - 1] as i32 + NDeltaMin_Q15[L] as i32);
        if diff_Q15 < min_diff_Q15 {
            min_diff_Q15 = diff_Q15;
            I = L;
        }

        /***************************************************/
        /* Now check if the smallest distance non-negative */
        /***************************************************/
        if min_diff_Q15 >= 0 {
            return;
        }
        if I == 0 {
            /* Move away from lower limit */
            NLSF_Q15[0] = NDeltaMin_Q15[0];
        } else if I == L {
            /* Move away from higher limit */
            NLSF_Q15[L - 1] = (((1) << 15) - NDeltaMin_Q15[L] as i32) as i16;
        } else {
            /* Find the lower extreme for the location of the current center frequency */
            min_center_Q15 = 0;
            k = 0;
            while k < I {
                min_center_Q15 += NDeltaMin_Q15[k] as i32;
                k += 1;
            }
            min_center_Q15 += NDeltaMin_Q15[I] as i32 >> 1;

            /* Find the upper extreme for the location of the current center frequency */
            max_center_Q15 = (1) << 15;
            k = L;
            while k > I {
                max_center_Q15 -= NDeltaMin_Q15[k] as i32;
                k -= 1;
            }
            max_center_Q15 -= NDeltaMin_Q15[I] as i32 >> 1;

            /* Move apart, sorted by value, keeping the same center frequency */
            center_freq_Q15 = (if min_center_Q15 > max_center_Q15 {
                if (if 1 == 1 {
                    (NLSF_Q15[I - 1] as i32 + NLSF_Q15[I] as i32 >> 1)
                        + (NLSF_Q15[I - 1] as i32 + NLSF_Q15[I] as i32 & 1)
                } else {
                    (NLSF_Q15[I - 1] as i32 + NLSF_Q15[I] as i32 >> 1 - 1) + 1 >> 1
                }) > min_center_Q15
                {
                    min_center_Q15
                } else if (if 1 == 1 {
                    (NLSF_Q15[I - 1] as i32 + NLSF_Q15[I] as i32 >> 1)
                        + (NLSF_Q15[I - 1] as i32 + NLSF_Q15[I] as i32 & 1)
                } else {
                    (NLSF_Q15[I - 1] as i32 + NLSF_Q15[I] as i32 >> 1 - 1) + 1 >> 1
                }) < max_center_Q15
                {
                    max_center_Q15
                } else if 1 == 1 {
                    (NLSF_Q15[I - 1] as i32 + NLSF_Q15[I] as i32 >> 1)
                        + (NLSF_Q15[I - 1] as i32 + NLSF_Q15[I] as i32 & 1)
                } else {
                    (NLSF_Q15[I - 1] as i32 + NLSF_Q15[I] as i32 >> 1 - 1) + 1 >> 1
                }
            } else if (if 1 == 1 {
                (NLSF_Q15[I - 1] as i32 + NLSF_Q15[I] as i32 >> 1)
                    + (NLSF_Q15[I - 1] as i32 + NLSF_Q15[I] as i32 & 1)
            } else {
                (NLSF_Q15[I - 1] as i32 + NLSF_Q15[I] as i32 >> 1 - 1) + 1 >> 1
            }) > max_center_Q15
            {
                max_center_Q15
            } else if (if 1 == 1 {
                (NLSF_Q15[I - 1] as i32 + NLSF_Q15[I] as i32 >> 1)
                    + (NLSF_Q15[I - 1] as i32 + NLSF_Q15[I] as i32 & 1)
            } else {
                (NLSF_Q15[I - 1] as i32 + NLSF_Q15[I] as i32 >> 1 - 1) + 1 >> 1
            }) < min_center_Q15
            {
                min_center_Q15
            } else if 1 == 1 {
                (NLSF_Q15[I - 1] as i32 + NLSF_Q15[I] as i32 >> 1)
                    + (NLSF_Q15[I - 1] as i32 + NLSF_Q15[I] as i32 & 1)
            } else {
                (NLSF_Q15[I - 1] as i32 + NLSF_Q15[I] as i32 >> 1 - 1) + 1 >> 1
            }) as i16;
            NLSF_Q15[I - 1] = (center_freq_Q15 as i32 - (NDeltaMin_Q15[I] as i32 >> 1)) as i16;
            NLSF_Q15[I] = (NLSF_Q15[I - 1] as i32 + NDeltaMin_Q15[I] as i32) as i16;
        }
        loops += 1;
    }

    /* Safe and simple fall back method, which is less ideal than the above */
    if loops == MAX_LOOPS {
        /* Insertion sort (fast for already almost sorted arrays):   */
        /* Best case:  O(n)   for an already sorted array            */
        /* Worst case: O(n^2) for an inversely sorted array          */
        silk_insertion_sort_increasing_all_values_int16(NLSF_Q15);

        /* First NLSF should be no less than NDeltaMin[0] */
        NLSF_Q15[0] = silk_max_int(NLSF_Q15[0] as i32, NDeltaMin_Q15[0 as usize] as i32) as i16;

        /* Keep delta_min distance between the NLSFs */
        i = 1;
        while i < L {
            NLSF_Q15[i] = silk_max_int(
                NLSF_Q15[i] as i32,
                (if NLSF_Q15[i - 1] as i32 + NDeltaMin_Q15[i] as i32 > silk_int16_MAX {
                    silk_int16_MAX
                } else if (NLSF_Q15[i - 1] as i32 + NDeltaMin_Q15[i] as i32) < silk_int16_MIN {
                    silk_int16_MIN
                } else {
                    NLSF_Q15[i - 1] as i32 + NDeltaMin_Q15[i] as i32
                }) as i16 as i32,
            ) as i16;
            i += 1;
        }

        /* Last NLSF should be no higher than 1 - NDeltaMin[L] */
        NLSF_Q15[L - 1] = silk_min_int(
            NLSF_Q15[L - 1] as i32,
            ((1) << 15) - NDeltaMin_Q15[L] as i32,
        ) as i16;

        /* Keep NDeltaMin distance between the NLSFs */

        for i in (0..=L - 2).rev() {
            NLSF_Q15[i] = silk_min_int(
                NLSF_Q15[i] as i32,
                NLSF_Q15[i + 1] as i32 - NDeltaMin_Q15[i + 1] as i32,
            ) as i16;
        }
    }
}
