pub unsafe fn silk_k2a_FLP(A: *mut f32, rc: *const f32, order: i32) {
    let mut k: i32 = 0;
    let mut n: i32 = 0;
    let mut rck: f32 = 0.;
    let mut tmp1: f32 = 0.;
    let mut tmp2: f32 = 0.;
    k = 0;
    while k < order {
        rck = *rc.offset(k as isize);
        n = 0;
        while n < k + 1 >> 1 {
            tmp1 = *A.offset(n as isize);
            tmp2 = *A.offset((k - n - 1) as isize);
            *A.offset(n as isize) = tmp1 + tmp2 * rck;
            *A.offset((k - n - 1) as isize) = tmp2 + tmp1 * rck;
            n += 1;
        }
        *A.offset(k as isize) = -rck;
        k += 1;
    }
}
