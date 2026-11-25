pub unsafe fn silk_inner_prod_aligned_scale(
    inVec1: *const i16,
    inVec2: *const i16,
    scale: i32,
    len: i32,
) -> i32 {
    let mut i: i32 = 0;
    let mut sum: i32 = 0;
    i = 0;
    while i < len {
        sum =
            sum + (*inVec1.offset(i as isize) as i32 * *inVec2.offset(i as isize) as i32 >> scale);
        i += 1;
    }
    return sum;
}
