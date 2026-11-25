// Interpolate two vectors
pub fn silk_interpolate(
    xi: &mut [i16],
    x0: &[i16],
    x1: &[i16],
    // interp. factor, weight on 2nd vector
    ifact_Q2: i32,
) {
    assert_eq!(xi.len(), x0.len());
    assert_eq!(xi.len(), x1.len());

    assert!(ifact_Q2 >= 0);
    assert!(ifact_Q2 <= 4);

    for (xi, (x0, x1)) in xi.iter_mut().zip(x0.iter().zip(x1)) {
        *xi = (*x0 as i32
            + (((*x1 as i32 - *x0 as i32) as i16 as i32 * ifact_Q2 as i16 as i32) >> 2))
            as i16
    }
}
