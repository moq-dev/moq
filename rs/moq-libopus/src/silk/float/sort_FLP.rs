pub unsafe fn silk_insertion_sort_decreasing_FLP(a: *mut f32, idx: *mut i32, L: i32, K: i32) {
    let mut value: f32 = 0.;
    let mut i: i32 = 0;
    let mut j: i32 = 0;
    assert!(K > 0);
    assert!(L > 0);
    assert!(L >= K);
    i = 0;
    while i < K {
        *idx.offset(i as isize) = i;
        i += 1;
    }
    i = 1;
    while i < K {
        value = *a.offset(i as isize);
        j = i - 1;
        while j >= 0 && value > *a.offset(j as isize) {
            *a.offset((j + 1) as isize) = *a.offset(j as isize);
            *idx.offset((j + 1) as isize) = *idx.offset(j as isize);
            j -= 1;
        }
        *a.offset((j + 1) as isize) = value;
        *idx.offset((j + 1) as isize) = i;
        i += 1;
    }
    i = K;
    while i < L {
        value = *a.offset(i as isize);
        if value > *a.offset((K - 1) as isize) {
            j = K - 2;
            while j >= 0 && value > *a.offset(j as isize) {
                *a.offset((j + 1) as isize) = *a.offset(j as isize);
                *idx.offset((j + 1) as isize) = *idx.offset(j as isize);
                j -= 1;
            }
            *a.offset((j + 1) as isize) = value;
            *idx.offset((j + 1) as isize) = i;
        }
        i += 1;
    }
}
