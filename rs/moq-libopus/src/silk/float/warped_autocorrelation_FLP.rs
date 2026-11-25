pub unsafe fn silk_warped_autocorrelation_FLP(
    corr: *mut f32,
    input: *const f32,
    warping: f32,
    length: i32,
    order: i32,
) {
    let mut n: i32 = 0;
    let mut i: i32 = 0;
    let mut tmp1: f64 = 0.;
    let mut tmp2: f64 = 0.;
    let mut state: [f64; 25] = [
        0 as f64, 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0.,
        0., 0., 0., 0.,
    ];
    let mut C: [f64; 25] = [
        0 as f64, 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0., 0.,
        0., 0., 0., 0.,
    ];
    assert!(order & 1 == 0);
    n = 0;
    while n < length {
        tmp1 = *input.offset(n as isize) as f64;
        i = 0;
        while i < order {
            tmp2 = state[i as usize] + warping as f64 * (state[(i + 1) as usize] - tmp1);
            state[i as usize] = tmp1;
            C[i as usize] += state[0 as usize] * tmp1;
            tmp1 = state[(i + 1) as usize] + warping as f64 * (state[(i + 2) as usize] - tmp2);
            state[(i + 1) as usize] = tmp2;
            C[(i + 1) as usize] += state[0 as usize] * tmp2;
            i += 2;
        }
        state[order as usize] = tmp1;
        C[order as usize] += state[0 as usize] * tmp1;
        n += 1;
    }
    i = 0;
    while i < order + 1 {
        *corr.offset(i as isize) = C[i as usize] as f32;
        i += 1;
    }
}
