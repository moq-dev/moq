use crate::silk::define::CODE_INDEPENDENTLY;
use crate::silk::float::structs_FLP::{silk_encoder_control_FLP, silk_encoder_state_FLP};
use crate::silk::tables_other::silk_LTPScales_table_Q14;

pub unsafe fn silk_LTP_scale_ctrl_FLP(
    psEnc: *mut silk_encoder_state_FLP,
    psEncCtrl: *mut silk_encoder_control_FLP,
    condCoding: i32,
) {
    let mut round_loss: i32 = 0;
    if condCoding == CODE_INDEPENDENTLY {
        round_loss = (*psEnc).sCmn.PacketLoss_perc + (*psEnc).sCmn.nFramesPerPacket;
        (*psEnc).sCmn.indices.LTP_scaleIndex = (if 0.0f32 > 2.0f32 {
            if round_loss as f32 * (*psEncCtrl).LTPredCodGain * 0.1f32 > 0.0f32 {
                0.0f32
            } else if round_loss as f32 * (*psEncCtrl).LTPredCodGain * 0.1f32 < 2.0f32 {
                2.0f32
            } else {
                round_loss as f32 * (*psEncCtrl).LTPredCodGain * 0.1f32
            }
        } else if round_loss as f32 * (*psEncCtrl).LTPredCodGain * 0.1f32 > 2.0f32 {
            2.0f32
        } else if round_loss as f32 * (*psEncCtrl).LTPredCodGain * 0.1f32 < 0.0f32 {
            0.0f32
        } else {
            round_loss as f32 * (*psEncCtrl).LTPredCodGain * 0.1f32
        }) as i8;
    } else {
        (*psEnc).sCmn.indices.LTP_scaleIndex = 0;
    }
    (*psEncCtrl).LTP_scale =
        silk_LTPScales_table_Q14[(*psEnc).sCmn.indices.LTP_scaleIndex as usize] as f32 / 16384.0f32;
}
