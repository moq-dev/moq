pub unsafe fn silk_schur_FLP(refl_coef: *mut f32, auto_corr: *const f32, order: i32) -> f32 {
    let mut k: i32 = 0;
    let mut n: i32 = 0;
    let mut C: [[f64; 2]; 25] = [[0.; 2]; 25];
    let mut Ctmp1: f64 = 0.;
    let mut Ctmp2: f64 = 0.;
    let mut rc_tmp: f64 = 0.;
    assert!(order >= 0 && order <= 24);
    k = 0;
    loop {
        C[k as usize][1 as usize] = *auto_corr.offset(k as isize) as f64;
        C[k as usize][0 as usize] = C[k as usize][1 as usize];
        k += 1;
        if !(k <= order) {
            break;
        }
    }
    k = 0;
    while k < order {
        rc_tmp = -C[(k + 1) as usize][0 as usize]
            / (if C[0 as usize][1 as usize] > 1e-9f32 as f64 {
                C[0 as usize][1 as usize]
            } else {
                1e-9f32 as f64
            });
        *refl_coef.offset(k as isize) = rc_tmp as f32;
        n = 0;
        while n < order - k {
            Ctmp1 = C[(n + k + 1) as usize][0 as usize];
            Ctmp2 = C[n as usize][1 as usize];
            C[(n + k + 1) as usize][0 as usize] = Ctmp1 + Ctmp2 * rc_tmp;
            C[n as usize][1 as usize] = Ctmp2 + Ctmp1 * rc_tmp;
            n += 1;
        }
        k += 1;
    }
    return C[0 as usize][1 as usize] as f32;
}
