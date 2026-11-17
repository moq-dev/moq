// Reads s16le PCM (CHANNELS interleaved) at RATE on stdin in FRAME-sample codec frames and prints,
// per frame, libopus's analysis activity probability as opus_encode would see it (run_analysis
// on the frame, then tonality_get_info), or "invalid" when no analysis applies yet.
#include <stdio.h>
#include <stdlib.h>
#include "analysis.h"
#include "modes.h"
#include "mathops.h"

int main(int argc, char **argv) {
  int rate = atoi(argv[1]), frame = atoi(argv[2]), channels = argc > 3 ? atoi(argv[3]) : 1;
  CELTMode *mode = opus_custom_mode_create(48000, 960, NULL);
  static TonalityAnalysisState st;
  tonality_analysis_init(&st, rate);
  opus_int16 *pcm = malloc(frame * channels * 2);
  while (fread(pcm, 2, frame * channels, stdin) == (size_t)(frame * channels)) {
    AnalysisInfo info;
    run_analysis(&st, mode, pcm, frame, frame, 0, -2, channels, rate, 16, downmix_int, &info);
    if (info.valid) printf("%.9g\n", info.activity_probability);
    else printf("invalid\n");
  }
  return 0;
}

// opus_encoder.c's, which the harness does not link.
void downmix_int(const void *_x, opus_val32 *y, int subframe, int offset, int c1, int c2, int C) {
  const opus_int16 *x = _x;
  for (int j = 0; j < subframe; j++) y[j] = INT16TOSIG(x[(j + offset) * C + c1]);
  if (c2 == -2)
    for (int c = 1; c < C; c++)
      for (int j = 0; j < subframe; j++) y[j] += INT16TOSIG(x[(j + offset) * C + c]);
}

int is_digital_silence(const opus_res *pcm, int frame_size, int channels, int lsb_depth) {
  return celt_maxabs_res(pcm, frame_size * channels) <= (opus_val16)1 / (1 << lsb_depth);
}
