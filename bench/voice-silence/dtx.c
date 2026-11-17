// Encodes s16le PCM (CHANNELS interleaved) at RATE with libopus through the float API, as WebCodecs
// and the Rust publisher do (VoIP, voice signal, 32 kbps, DTX), in FRAME-sample frames, and prints
// per frame 1 when libopus sent no audio for it (a DTX packet, or a keep-alive inside DTX), else 0.
#include <opus.h>
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
  int rate = atoi(argv[1]), frame = atoi(argv[2]), complexity = atoi(argv[3]);
  int channels = argc > 4 ? atoi(argv[4]) : 1;
  int err;
  OpusEncoder *enc = opus_encoder_create(rate, channels, OPUS_APPLICATION_VOIP, &err);
  opus_encoder_ctl(enc, OPUS_SET_BITRATE(argc > 5 ? atoi(argv[5]) : 32000));
  opus_encoder_ctl(enc, OPUS_SET_SIGNAL(OPUS_SIGNAL_VOICE));
  opus_encoder_ctl(enc, OPUS_SET_COMPLEXITY(complexity));
  opus_encoder_ctl(enc, OPUS_SET_DTX(1));
  opus_int16 *in = malloc(frame * channels * 2);
  float *pcm = malloc(frame * channels * 4);
  unsigned char packet[4000];
  int shut = 0;
  while (fread(in, 2, frame * channels, stdin) == (size_t)(frame * channels)) {
    for (int i = 0; i < frame * channels; i++) pcm[i] = in[i] / 32768.f;
    int len = opus_encode_float(enc, pcm, frame, packet, sizeof(packet));
    opus_int32 dtx;
    opus_encoder_ctl(enc, OPUS_GET_IN_DTX(&dtx));
    // A multi-frame packet carries no audio when every frame in it is empty.
    const unsigned char *frames[48];
    opus_int16 sizes[48];
    int count = opus_packet_parse(packet, len, NULL, frames, sizes, NULL), empty = 1;
    for (int i = 0; i < count; i++) empty &= sizes[i] == 0;
    shut = len <= 2 || (count > 1 && empty) || (dtx && shut);
    printf("%d\n", shut);
  }
  return 0;
}
