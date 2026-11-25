use crate::silk::pitch_est_tables::{
    silk_CB_lags_stage2, silk_CB_lags_stage2_10_ms, silk_CB_lags_stage3, silk_CB_lags_stage3_10_ms,
    PE_MAX_NB_SUBFR, PE_NB_CBKS_STAGE2_10MS, PE_NB_CBKS_STAGE2_EXT, PE_NB_CBKS_STAGE3_10MS,
    PE_NB_CBKS_STAGE3_MAX,
};

pub unsafe fn silk_decode_pitch(
    lagIndex: i16,
    contourIndex: i8,
    pitch_lags: *mut i32,
    Fs_kHz: i32,
    nb_subfr: i32,
) {
    let mut lag: i32 = 0;
    let mut k: i32 = 0;
    let mut min_lag: i32 = 0;
    let mut max_lag: i32 = 0;
    let mut cbk_size: i32 = 0;
    let mut Lag_CB_ptr: *const i8 = 0 as *const i8;
    if Fs_kHz == 8 {
        if nb_subfr == PE_MAX_NB_SUBFR {
            Lag_CB_ptr = &*(*silk_CB_lags_stage2.as_ptr().offset(0 as isize))
                .as_ptr()
                .offset(0 as isize) as *const i8;
            cbk_size = PE_NB_CBKS_STAGE2_EXT;
        } else {
            assert!(nb_subfr == 4 >> 1);
            Lag_CB_ptr = &*(*silk_CB_lags_stage2_10_ms.as_ptr().offset(0 as isize))
                .as_ptr()
                .offset(0 as isize) as *const i8;
            cbk_size = PE_NB_CBKS_STAGE2_10MS;
        }
    } else if nb_subfr == PE_MAX_NB_SUBFR {
        Lag_CB_ptr = &*(*silk_CB_lags_stage3.as_ptr().offset(0 as isize))
            .as_ptr()
            .offset(0 as isize) as *const i8;
        cbk_size = PE_NB_CBKS_STAGE3_MAX;
    } else {
        assert!(nb_subfr == 4 >> 1);
        Lag_CB_ptr = &*(*silk_CB_lags_stage3_10_ms.as_ptr().offset(0 as isize))
            .as_ptr()
            .offset(0 as isize) as *const i8;
        cbk_size = PE_NB_CBKS_STAGE3_10MS;
    }
    min_lag = 2 * Fs_kHz as i16 as i32;
    max_lag = 18 * Fs_kHz as i16 as i32;
    lag = min_lag + lagIndex as i32;
    k = 0;
    while k < nb_subfr {
        *pitch_lags.offset(k as isize) =
            lag + *Lag_CB_ptr.offset((k * cbk_size + contourIndex as i32) as isize) as i32;
        *pitch_lags.offset(k as isize) = if min_lag > max_lag {
            if *pitch_lags.offset(k as isize) > min_lag {
                min_lag
            } else if *pitch_lags.offset(k as isize) < max_lag {
                max_lag
            } else {
                *pitch_lags.offset(k as isize)
            }
        } else if *pitch_lags.offset(k as isize) > max_lag {
            max_lag
        } else if *pitch_lags.offset(k as isize) < min_lag {
            min_lag
        } else {
            *pitch_lags.offset(k as isize)
        };
        k += 1;
    }
}
