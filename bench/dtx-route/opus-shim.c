#include <stdlib.h>
#include "opus.h"

OpusEncoder *enc_create(int rate, int channels, int application, int bitrate, int dtx, int complexity, int signal) {
	int err = 0;
	OpusEncoder *enc = opus_encoder_create(rate, channels, application, &err);
	if (err != OPUS_OK) return 0;
	opus_encoder_ctl(enc, OPUS_SET_BITRATE(bitrate));
	opus_encoder_ctl(enc, OPUS_SET_DTX(dtx));
	if (complexity >= 0) opus_encoder_ctl(enc, OPUS_SET_COMPLEXITY(complexity));
	if (signal) opus_encoder_ctl(enc, OPUS_SET_SIGNAL(signal));
	return enc;
}
int enc_encode(OpusEncoder *enc, const float *pcm, int frame, unsigned char *out, int max) {
	return opus_encode_float(enc, pcm, frame, out, max);
}
int enc_in_dtx(OpusEncoder *enc) {
	opus_int32 v = 0;
	opus_encoder_ctl(enc, OPUS_GET_IN_DTX(&v));
	return v;
}
int enc_lookahead(OpusEncoder *enc) {
	opus_int32 v = 0;
	opus_encoder_ctl(enc, OPUS_GET_LOOKAHEAD(&v));
	return v;
}
void enc_destroy(OpusEncoder *enc) { opus_encoder_destroy(enc); }
#ifdef WITH_DECODER
OpusDecoder *dec_create(int rate, int channels) {
	int err = 0;
	OpusDecoder *dec = opus_decoder_create(rate, channels, &err);
	return err == OPUS_OK ? dec : 0;
}
int dec_decode(OpusDecoder *dec, const unsigned char *data, int len, float *pcm, int frame) {
	return opus_decode_float(dec, len ? data : 0, len, pcm, frame, 0);
}
#endif
void *mem_alloc(int n) { return malloc(n); }
void mem_free(void *p) { free(p); }
