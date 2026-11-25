#![forbid(unsafe_code)]

use num_traits::Zero;
pub type kiss_fft_cpx = num_complex::Complex32;
pub type kiss_twiddle_cpx = num_complex::Complex32;

#[derive(Copy, Clone)]
pub struct kiss_fft_state<'a> {
    pub nfft: usize,
    pub scale: f32,
    pub shift: i32,
    pub factors: [(i32, i32); 8],
    pub bitrev: &'a [i16],
    pub twiddles: &'a [kiss_twiddle_cpx; 480],
}

fn kf_bfly2(Fout: &mut [kiss_fft_cpx], m: i32, N: i32) {
    let tw: f32 = std::f32::consts::FRAC_1_SQRT_2;
    /* We know that m==4 here because the radix-2 is just after a radix-4 */
    assert_eq!(m, 4);
    assert_eq!(Fout.len(), N as usize * 8);
    for chunk in Fout.chunks_exact_mut(8) {
        let (Fout, Fout2) = chunk.split_at_mut(4);

        let t = Fout2[0];
        Fout2[0] = Fout[0] - t;
        Fout[0] += t;

        let t = kiss_fft_cpx::new(
            (Fout2[1].re + Fout2[1].im) * tw,
            (Fout2[1].im - Fout2[1].re) * tw,
        );
        Fout2[1] = Fout[1] - t;
        Fout[1] += t;

        let t = kiss_fft_cpx::new(Fout2[2].im, -Fout2[2].re);
        Fout2[2] = Fout[2] - t;
        Fout[2] += t;

        let t = kiss_fft_cpx::new(
            (Fout2[3].im - Fout2[3].re) * tw,
            -(Fout2[3].im + Fout2[3].re) * tw,
        );
        Fout2[3] = Fout[3] - t;
        Fout[3] += t;
    }
}
fn kf_bfly4(
    Fout: &mut [kiss_fft_cpx],
    fstride: usize,
    st: &kiss_fft_state,
    m: i32,
    N: i32,
    mm: i32,
) {
    if m == 1 {
        /* Degenerate case where all the twiddles are 1. */
        assert_eq!(Fout.len(), N as usize * 4);
        for chunk in Fout.chunks_exact_mut(4) {
            let scratch0 = chunk[0] - chunk[2];
            chunk[0] += chunk[2];
            let scratch1 = chunk[1] + chunk[3];
            chunk[2] = chunk[0] - scratch1;
            chunk[0] += scratch1;
            let scratch1 = chunk[1] - chunk[3];

            chunk[1].re = scratch0.re + scratch1.im;
            chunk[1].im = scratch0.im - scratch1.re;
            chunk[3].re = scratch0.re - scratch1.im;
            chunk[3].im = scratch0.im + scratch1.re;
        }
    } else {
        let mut scratch: [kiss_fft_cpx; 6] = [kiss_fft_cpx::zero(); 6];
        let m = m as usize;
        let m2 = 2 * m;
        let m3 = 3 * m;

        for i in 0..N {
            let mut chunk = &mut Fout[(i * mm) as usize..][..4 * m];
            let mut tw1 = st.twiddles.as_slice();
            let mut tw2 = tw1;
            let mut tw3 = tw2;
            /* m is guaranteed to be a multiple of 4. */
            for _ in 0..m {
                scratch[0] = chunk[m] * tw1[0];
                scratch[1] = chunk[m2] * tw2[0];
                scratch[2] = chunk[m3] * tw3[0];

                scratch[5] = chunk[0] - scratch[1];
                chunk[0] += scratch[1];
                scratch[3] = scratch[0] + scratch[2];
                scratch[4] = scratch[0] - scratch[2];
                chunk[m2] = chunk[0] - scratch[3];
                tw1 = &tw1[fstride..];
                tw2 = &tw2[fstride * 2..];
                tw3 = &tw3[fstride * 3..];
                chunk[0] += scratch[3];

                chunk[m].re = scratch[5].re + scratch[4].im;
                chunk[m].im = scratch[5].im - scratch[4].re;
                chunk[m3].re = scratch[5].re - scratch[4].im;
                chunk[m3].im = scratch[5].im + scratch[4].re;
                chunk = &mut chunk[1..];
            }
        }
    };
}
fn kf_bfly3(
    Fout: &mut [kiss_fft_cpx],
    fstride: usize,
    st: &kiss_fft_state,
    m: i32,
    N: i32,
    mm: i32,
) {
    let m = m as usize;
    let m2 = 2 * m;
    let mut scratch: [kiss_fft_cpx; 5] = [kiss_fft_cpx::zero(); 5];
    let epi3 = st.twiddles[fstride * m];
    for i in 0..N {
        let mut chunk = &mut Fout[(i * mm) as usize..][..3 * m];
        let mut tw2 = st.twiddles.as_slice();
        let mut tw1 = tw2;
        /* For non-custom modes, m is guaranteed to be a multiple of 4. */
        for _ in 0..m {
            scratch[1] = chunk[m] * tw1[0];
            scratch[2] = chunk[m2] * tw2[0];

            scratch[3] = scratch[1] + scratch[2];
            scratch[0] = scratch[1] - scratch[2];
            tw1 = &tw1[fstride..];
            tw2 = &tw2[fstride * 2..];

            chunk[m] = chunk[0] - scratch[3] * 0.5f32;

            scratch[0] *= epi3.im;

            chunk[0] += scratch[3];

            chunk[m2].re = chunk[m].re + scratch[0].im;
            chunk[m2].im = chunk[m].im - scratch[0].re;

            chunk[m].re -= scratch[0].im;
            chunk[m].im += scratch[0].re;

            chunk = &mut chunk[1..];
        }
    }
}
fn kf_bfly5(
    Fout: &mut [kiss_fft_cpx],
    fstride: usize,
    st: &kiss_fft_state,
    m: i32,
    N: i32,
    mm: i32,
) {
    let mut scratch: [kiss_fft_cpx; 13] = [kiss_fft_cpx::zero(); 13];
    let ya = st.twiddles[fstride * m as usize];
    let yb = st.twiddles[fstride * m as usize * 2];
    let tw = st.twiddles.as_slice();
    let m = m as usize;
    for i in 0..N {
        let chunk = &mut Fout[(i * mm) as usize..][..5 * m];
        let (mut chunk0, chunk) = chunk.split_at_mut(m);
        let (mut chunk1, chunk) = chunk.split_at_mut(m);
        let (mut chunk2, chunk) = chunk.split_at_mut(m);
        let (mut chunk3, mut chunk4) = chunk.split_at_mut(m);

        /* For non-custom modes, m is guaranteed to be a multiple of 4. */
        for u in 0..m {
            scratch[0] = chunk0[0];

            scratch[1] = chunk1[0] * tw[1 * u * fstride];
            scratch[2] = chunk2[0] * tw[2 * u * fstride];
            scratch[3] = chunk3[0] * tw[3 * u * fstride];
            scratch[4] = chunk4[0] * tw[4 * u * fstride];

            scratch[7] = scratch[1] + scratch[4];
            scratch[10] = scratch[1] - scratch[4];
            scratch[8] = scratch[2] + scratch[3];
            scratch[9] = scratch[2] - scratch[3];

            chunk0[0] += scratch[7] + scratch[8];

            scratch[5].re = scratch[0].re + (scratch[7].re * ya.re + scratch[8].re * yb.re);
            scratch[5].im = scratch[0].im + (scratch[7].im * ya.re + scratch[8].im * yb.re);

            scratch[6].re = scratch[10].im * ya.im + scratch[9].im * yb.im;
            scratch[6].im = -(scratch[10].re * ya.im + scratch[9].re * yb.im);

            chunk1[0] = scratch[5] - scratch[6];
            chunk4[0] = scratch[5] + scratch[6];

            scratch[11].re = scratch[0].re + (scratch[7].re * yb.re + scratch[8].re * ya.re);
            scratch[11].im = scratch[0].im + (scratch[7].im * yb.re + scratch[8].im * ya.re);
            scratch[12].re = scratch[9].im * ya.im - scratch[10].im * yb.im;
            scratch[12].im = scratch[10].re * yb.im - scratch[9].re * ya.im;

            chunk2[0] = scratch[11] + scratch[12];
            chunk3[0] = scratch[11] - scratch[12];

            chunk0 = &mut chunk0[1..];
            chunk1 = &mut chunk1[1..];
            chunk2 = &mut chunk2[1..];
            chunk3 = &mut chunk3[1..];
            chunk4 = &mut chunk4[1..];
        }
    }
}

pub fn opus_fft_impl(st: &kiss_fft_state, fout: &mut [kiss_fft_cpx]) {
    let shift = st.shift.max(0);

    let mut fstride: [i32; 8] = [0; 8];
    fstride[0] = 1;

    let mut L = 0_usize;
    loop {
        let (p, m) = st.factors[L];
        fstride[L + 1] = fstride[L] * p;
        L += 1;
        if m == 1 {
            break;
        }
    }

    let mut m = st.factors[L - 1].1;
    for i in (0..L).rev() {
        let m2 = if i > 0 { st.factors[i - 1].1 } else { 1 };
        match st.factors[i].0 {
            2 => kf_bfly2(fout, m, fstride[i]),
            4 => kf_bfly4(fout, (fstride[i] << shift) as usize, st, m, fstride[i], m2),
            3 => kf_bfly3(fout, (fstride[i] << shift) as usize, st, m, fstride[i], m2),
            5 => kf_bfly5(fout, (fstride[i] << shift) as usize, st, m, fstride[i], m2),
            _ => {}
        }
        m = m2;
    }
}

pub fn opus_fft_c(st: &kiss_fft_state, fin: &[kiss_fft_cpx], fout: &mut [kiss_fft_cpx]) {
    let mut scale: f32 = 0.;
    scale = st.scale;
    assert_eq!(fin.len(), st.nfft);
    assert_eq!(fout.len(), st.nfft);
    for (&x, &i) in fin.iter().zip(st.bitrev) {
        fout[i as usize] = scale * x;
    }
    opus_fft_impl(st, fout);
}
