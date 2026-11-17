// Encodes s16le mono 48 kHz PCM on stdin with libopus as the voice kind configures WebCodecs
// (VoIP, voice signal, 32 kbps, 20 ms), with DTX on, and prints per frame: the SILK VAD flag
// (-1 for a CELT-only packet), then 1 when libopus sent no audio (a DTX packet of 2 bytes or less).
#include <opus.h>
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
  int complexity = atoi(argv[1]);
  int err;
  OpusEncoder *enc = opus_encoder_create(48000, 1, OPUS_APPLICATION_VOIP, &err);
  opus_encoder_ctl(enc, OPUS_SET_BITRATE(32000));
  opus_encoder_ctl(enc, OPUS_SET_SIGNAL(OPUS_SIGNAL_VOICE));
  opus_encoder_ctl(enc, OPUS_SET_COMPLEXITY(complexity));
  opus_encoder_ctl(enc, OPUS_SET_DTX(1));
  opus_int16 pcm[960];
  unsigned char packet[4000];
  while (fread(pcm, 2, 960, stdin) == 960) {
    int len = opus_encode(enc, pcm, 960, packet, sizeof(packet));
    int config = packet[0] >> 3;
    int vad = config < 16 && len > 1 ? packet[1] >> 7 : -1;
    printf("%d %d\n", vad, len <= 2);
  }
  return 0;
}
