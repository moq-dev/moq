// Reads s16le mono PCM at RATE on stdin; prints SILK's speech_activity_Q8 per BLOCK ms block,
// after SILK's integer-ratio downsampler, as the JS port runs it.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "main.h"
#include "resampler_private.h"

int main(int argc, char **argv) {
  int rate = atoi(argv[1]);
  int block = atoi(argv[2]);
  int vadRate = rate == 48000 ? 16000 : rate == 24000 ? 12000 : rate == 32000 ? 16000 : rate;
  int ratio = rate / vadRate;
  silk_resampler_state_struct rs;
  // silk_resampler_init only takes Opus rates; 24k->12k picks the same 1:2 filter 32k->16k needs.
  if (ratio > 1) silk_resampler_init(&rs, rate == 32000 ? 24000 : rate, rate == 32000 ? 12000 : vadRate, 1);
  silk_encoder_state enc;
  memset(&enc, 0, sizeof(enc));
  enc.fs_kHz = vadRate / 1000;
  enc.frame_length = enc.fs_kHz * block;
  silk_VAD_Init(&enc.sVAD);
  int inLen = enc.frame_length * ratio;
  opus_int16 *in = malloc(inLen * 2), *out = malloc(enc.frame_length * 2);
  while (fread(in, 2, inLen, stdin) == (size_t)inLen) {
    if (ratio > 1) silk_resampler_private_down_FIR(&rs, out, in, inLen);
    else memcpy(out, in, inLen * 2);
    silk_VAD_GetSA_Q8_c(&enc, out);
    printf("%d\n", enc.speech_activity_Q8);
  }
  return 0;
}
