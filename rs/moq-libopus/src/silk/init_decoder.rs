pub mod cpu_support_h {
    #[inline]
    pub unsafe fn opus_select_arch() -> i32 {
        return 0;
    }
}

use self::cpu_support_h::opus_select_arch;
use crate::externs::memset;
use crate::silk::structs::silk_decoder_state;
use crate::silk::CNG::silk_CNG_Reset;
use crate::silk::PLC::silk_PLC_Reset;

pub unsafe fn silk_init_decoder(psDec: *mut silk_decoder_state) -> i32 {
    memset(
        psDec as *mut core::ffi::c_void,
        0,
        ::core::mem::size_of::<silk_decoder_state>() as u64,
    );
    (*psDec).first_frame_after_reset = 1;
    (*psDec).prev_gain_Q16 = 65536;
    (*psDec).arch = opus_select_arch();
    silk_CNG_Reset(psDec);
    silk_PLC_Reset(psDec);
    return 0;
}
