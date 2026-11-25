pub unsafe fn silk_bwexpander_32(ar: *mut i32, d: i32, mut chirp_Q16: i32) {
    let mut i: i32 = 0;
    let chirp_minus_one_Q16: i32 = chirp_Q16 - 65536;
    i = 0;
    while i < d - 1 {
        *ar.offset(i as isize) = (chirp_Q16 as i64 * *ar.offset(i as isize) as i64 >> 16) as i32;
        chirp_Q16 += if 16 == 1 {
            (chirp_Q16 * chirp_minus_one_Q16 >> 1) + (chirp_Q16 * chirp_minus_one_Q16 & 1)
        } else {
            (chirp_Q16 * chirp_minus_one_Q16 >> 16 - 1) + 1 >> 1
        };
        i += 1;
    }
    *ar.offset((d - 1) as isize) =
        (chirp_Q16 as i64 * *ar.offset((d - 1) as isize) as i64 >> 16) as i32;
}
