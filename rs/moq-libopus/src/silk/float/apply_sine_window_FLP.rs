pub unsafe fn silk_apply_sine_window_FLP(
    px_win: *mut f32,
    px: *const f32,
    win_type: i32,
    length: i32,
) {
    let mut k: i32 = 0;
    let mut freq: f32 = 0.;
    let mut c: f32 = 0.;
    let mut S0: f32 = 0.;
    let mut S1: f32 = 0.;
    assert!(win_type == 1 || win_type == 2);
    assert!(length & 3 == 0);
    freq = std::f32::consts::PI / (length + 1) as f32;
    c = 2.0f32 - freq * freq;
    if win_type < 2 {
        S0 = 0.0f32;
        S1 = freq;
    } else {
        S0 = 1.0f32;
        S1 = 0.5f32 * c;
    }
    k = 0;
    while k < length {
        *px_win.offset((k + 0) as isize) = *px.offset((k + 0) as isize) * 0.5f32 * (S0 + S1);
        *px_win.offset((k + 1) as isize) = *px.offset((k + 1) as isize) * S1;
        S0 = c * S1 - S0;
        *px_win.offset((k + 2) as isize) = *px.offset((k + 2) as isize) * 0.5f32 * (S1 + S0);
        *px_win.offset((k + 3) as isize) = *px.offset((k + 3) as isize) * S0;
        S1 = c * S0 - S1;
        k += 4;
    }
}
