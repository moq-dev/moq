pub unsafe fn silk_bwexpander_FLP(ar: *mut f32, d: i32, chirp: f32) {
    let mut i: i32 = 0;
    let mut cfac: f32 = chirp;
    i = 0;
    while i < d - 1 {
        *ar.offset(i as isize) *= cfac;
        cfac *= chirp;
        i += 1;
    }
    *ar.offset((d - 1) as isize) *= cfac;
}
