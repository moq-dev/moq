use crate::externs::memset;
use crate::silk::float::structs_FLP::silk_encoder_state_FLP;
use crate::silk::lin2log::silk_lin2log;
use crate::silk::VAD::silk_VAD_Init;

pub unsafe fn silk_init_encoder(psEnc: *mut silk_encoder_state_FLP, arch: i32) -> i32 {
    let mut ret: i32 = 0;
    memset(
        psEnc as *mut core::ffi::c_void,
        0,
        ::core::mem::size_of::<silk_encoder_state_FLP>() as u64,
    );
    (*psEnc).sCmn.arch = arch;
    (*psEnc).sCmn.variable_HP_smth1_Q15 =
        (((silk_lin2log(((60 * ((1) << 16)) as f64 + 0.5f64) as i32) - ((16) << 7)) as u32) << 8)
            as i32;
    (*psEnc).sCmn.variable_HP_smth2_Q15 = (*psEnc).sCmn.variable_HP_smth1_Q15;
    (*psEnc).sCmn.first_frame_after_reset = 1;
    ret += silk_VAD_Init(&mut (*psEnc).sCmn.sVAD);
    return ret;
}
