// SPDX-License-Identifier: GPL-2.0-or-later
#include "moq-encoder-rate.h"

#include <cassert>
#include <cstdio>
#include <map>
#include <variant>

struct obs_data {
	std::map<std::string, std::variant<std::string, long long>> values;
};

const char *obs_data_get_string(obs_data_t *data, const char *key)
{
	auto entry = data->values.find(key);
	return entry == data->values.end() ? "" : std::get<std::string>(entry->second).c_str();
}
long long obs_data_get_int(obs_data_t *data, const char *key)
{
	auto entry = data->values.find(key);
	return entry == data->values.end() ? 0 : std::get<long long>(entry->second);
}
void obs_data_set_string(obs_data_t *data, const char *key, const char *value)
{
	data->values[key] = std::string(value);
}
void obs_data_set_int(obs_data_t *data, const char *key, long long value)
{
	data->values[key] = value;
}

static std::string rate_control(obs_data_t &settings)
{
	return obs_data_get_string(&settings, "rate_control");
}

int main()
{
	// x264 and NVENC take constant quality under the cap.
	obs_data_t x264;
	assert(ApplyEncoderRate("obs_x264", &x264, 8000));
	assert(rate_control(x264) == "VBR" && obs_data_get_int(&x264, "bitrate") == 8000);
	assert(EncoderPeak("obs_x264", &x264).kbps == 8000);

	for (const char *id : {"obs_nvenc_h264_tex", "obs_nvenc_hevc_soft", "obs_nvenc_av1_tex"}) {
		obs_data_t nvenc;
		assert(ApplyEncoderRate(id, &nvenc, 2500));
		assert(rate_control(nvenc) == "CQVBR" && obs_data_get_int(&nvenc, "max_bitrate") == 2500);
		// The peak is max_bitrate: CQVBR ignores the bitrate.
		const MoQPeak peak = EncoderPeak(id, &nvenc);
		assert(std::string(peak.key) == "max_bitrate" && peak.kbps == 2500);
	}

	// An encoder whose quality mode carries no hard peak stays at CBR, which is its peak.
	for (const char *id : {"obs_qsv11_v2", "obs_qsv11_av1", "h264_texture_amf", "ffmpeg_vaapi_tex",
			       "com.apple.videotoolbox.videoencoder.ave.avc", "ffmpeg_nvenc", "unknown"}) {
		obs_data_t other;
		assert(!ApplyEncoderRate(id, &other, 8000));
		assert(rate_control(other) == "CBR" && obs_data_get_int(&other, "bitrate") == 8000);
		const MoQPeak peak = EncoderPeak(id, &other);
		assert(std::string(peak.key) == "bitrate" && peak.kbps == 8000);
	}

	// What a user configured decides the peak, whatever the encoder.
	assert(std::string(EncoderPeakKey("obs_x264", "cbr")) == "bitrate");
	assert(std::string(EncoderPeakKey("obs_nvenc_h264_tex", "vbr")) == "max_bitrate");
	assert(std::string(EncoderPeakKey("obs_nvenc_h264_tex", "CBR")) == "bitrate");
	for (const char *mode : {"CRF", "ABR", "CQP", "ICQ", "QVBR", "lossless", ""}) {
		assert(!EncoderPeakKey("obs_x264", mode) && !EncoderPeakKey("obs_nvenc_h264_tex", mode));
	}
	assert(!EncoderPeakKey("obs_qsv11_v2", "VBR"));

	// NVENC's plain VBR averages beside its peak; CQVBR and CBR have no average to keep.
	obs_data_t vbr;
	obs_data_set_string(&vbr, "rate_control", "VBR");
	obs_data_set_int(&vbr, "bitrate", 6000);
	obs_data_set_int(&vbr, "max_bitrate", 8000);
	const MoQPeak averaged = EncoderPeak("obs_nvenc_h264_tex", &vbr);
	assert(averaged.kbps == 8000 && averaged.average_kbps == 6000);
	assert(EncoderPeak("obs_x264", &x264).average_kbps == 0);
	obs_data_t cq;
	assert(ApplyEncoderRate("obs_nvenc_h264_tex", &cq, 2500));
	assert(EncoderPeak("obs_nvenc_h264_tex", &cq).average_kbps == 0);

	// A peak of 0 is none: NVENC reads it as the driver's own limit.
	obs_data_t unbounded;
	obs_data_set_string(&unbounded, "rate_control", "CQVBR");
	obs_data_set_int(&unbounded, "max_bitrate", 0);
	assert(!EncoderPeak("obs_nvenc_h264_tex", &unbounded).key);

	printf("encoder rate: ok\n");
	return 0;
}
