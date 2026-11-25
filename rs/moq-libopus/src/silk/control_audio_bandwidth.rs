use crate::externs::memset;
use crate::silk::define::TRANSITION_FRAMES;
use crate::silk::enc_API::silk_EncControlStruct;
use crate::silk::structs::silk_encoder_state;
pub unsafe fn silk_control_audio_bandwidth(
    psEncC: *mut silk_encoder_state,
    encControl: *mut silk_EncControlStruct,
) -> i32 {
    let mut fs_kHz: i32 = 0;
    let mut orig_kHz: i32 = 0;
    let mut fs_Hz: i32 = 0;
    orig_kHz = (*psEncC).fs_kHz;
    if orig_kHz == 0 {
        orig_kHz = (*psEncC).sLP.saved_fs_kHz;
    }
    fs_kHz = orig_kHz;
    fs_Hz = fs_kHz as i16 as i32 * 1000;
    if fs_Hz == 0 {
        fs_Hz = if (*psEncC).desiredInternal_fs_Hz < (*psEncC).API_fs_Hz {
            (*psEncC).desiredInternal_fs_Hz
        } else {
            (*psEncC).API_fs_Hz
        };
        fs_kHz = fs_Hz / 1000;
    } else if fs_Hz > (*psEncC).API_fs_Hz
        || fs_Hz > (*psEncC).maxInternal_fs_Hz
        || fs_Hz < (*psEncC).minInternal_fs_Hz
    {
        fs_Hz = (*psEncC).API_fs_Hz;
        fs_Hz = if fs_Hz < (*psEncC).maxInternal_fs_Hz {
            fs_Hz
        } else {
            (*psEncC).maxInternal_fs_Hz
        };
        fs_Hz = if fs_Hz > (*psEncC).minInternal_fs_Hz {
            fs_Hz
        } else {
            (*psEncC).minInternal_fs_Hz
        };
        fs_kHz = fs_Hz / 1000;
    } else {
        if (*psEncC).sLP.transition_frame_no >= TRANSITION_FRAMES {
            (*psEncC).sLP.mode = 0;
        }
        if (*psEncC).allow_bandwidth_switch != 0 || (*encControl).opusCanSwitch != 0 {
            if orig_kHz as i16 as i32 * 1000 > (*psEncC).desiredInternal_fs_Hz {
                if (*psEncC).sLP.mode == 0 {
                    (*psEncC).sLP.transition_frame_no = TRANSITION_FRAMES;
                    memset(
                        ((*psEncC).sLP.In_LP_State).as_mut_ptr() as *mut core::ffi::c_void,
                        0,
                        ::core::mem::size_of::<[i32; 2]>() as u64,
                    );
                }
                if (*encControl).opusCanSwitch != 0 {
                    (*psEncC).sLP.mode = 0;
                    fs_kHz = if orig_kHz == 16 { 12 } else { 8 };
                } else if (*psEncC).sLP.transition_frame_no <= 0 {
                    (*encControl).switchReady = 1;
                    (*encControl).maxBits -=
                        (*encControl).maxBits * 5 / ((*encControl).payloadSize_ms + 5);
                } else {
                    (*psEncC).sLP.mode = -(2);
                }
            } else if (orig_kHz as i16 as i32 * 1000) < (*psEncC).desiredInternal_fs_Hz {
                if (*encControl).opusCanSwitch != 0 {
                    fs_kHz = if orig_kHz == 8 { 12 } else { 16 };
                    (*psEncC).sLP.transition_frame_no = 0;
                    memset(
                        ((*psEncC).sLP.In_LP_State).as_mut_ptr() as *mut core::ffi::c_void,
                        0,
                        ::core::mem::size_of::<[i32; 2]>() as u64,
                    );
                    (*psEncC).sLP.mode = 1;
                } else if (*psEncC).sLP.mode == 0 {
                    (*encControl).switchReady = 1;
                    (*encControl).maxBits -=
                        (*encControl).maxBits * 5 / ((*encControl).payloadSize_ms + 5);
                } else {
                    (*psEncC).sLP.mode = 1;
                }
            } else if (*psEncC).sLP.mode < 0 {
                (*psEncC).sLP.mode = 1;
            }
        }
    }
    return fs_kHz;
}
