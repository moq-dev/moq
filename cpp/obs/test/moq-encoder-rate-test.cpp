// SPDX-License-Identifier: GPL-2.0-or-later
#include "moq-encoder-rate.h"

#include <cassert>
#include <cstdio>
#include <map>
#include <variant>
#include <vector>

struct obs_data {
	std::map<std::string, std::variant<std::string, long long>> values;
};

bool obs_data_get_bool(obs_data_t *data, const char *key)
{
	auto entry = data->values.find(key);
	return entry != data->values.end() && std::get<long long>(entry->second) != 0;
}
void obs_data_set_bool(obs_data_t *data, const char *key, bool value)
{
	data->values[key] = (long long)value;
}

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

	// A running encoder's cap is what its settings hold: its peak, or none for a quality mode
	// and for x264's lossless CRF.
	struct Running {
		const char *id;
		const char *mode;
		long long crf;
		long long expect;
	};
	for (const Running &c :
	     {Running{"obs_x264", "CBR", 0, 6000}, Running{"obs_x264", "VBR", 23, 6000},
	      Running{"obs_x264", "VBR", 0, 0}, Running{"obs_x264", "CRF", 23, 0}, Running{"obs_x264", "CQP", 23, 0},
	      Running{"obs_nvenc_h264_tex", "CQP", 0, 0}, Running{"obs_nvenc_h264_tex", "CQVBR", 0, 7000},
	      Running{"obs_qsv11_v2", "ICQ", 0, 0}}) {
		obs_data_t running;
		obs_data_set_string(&running, "rate_control", c.mode);
		obs_data_set_int(&running, "bitrate", 6000);
		obs_data_set_int(&running, "max_bitrate", 7000);
		obs_data_set_int(&running, "crf", c.crf);
		assert(RunningCapKbps(c.id, &running) == c.expect);
	}

	// x264 runs the user's CBR or VBR at the cap, and anything else as VBR. A buffer of
	// its own is replaced. x264opts is never rewritten: the options in it that set a rate
	// are reported as overriding the cap, and the rest are not.
	struct Case {
		const char *before;
		const char *after;
	};
	for (const Case &c : {Case{"CBR", "CBR"}, Case{"cbr", "cbr"}, Case{"VBR", "VBR"}, Case{"CRF", "VBR"},
			      Case{"ABR", "VBR"}, Case{"CQP", "VBR"}, Case{"", "VBR"}}) {
		obs_data_t user, patch;
		obs_data_set_string(&user, "rate_control", c.before);
		obs_data_set_int(&user, "bitrate", 6000);
		obs_data_set_int(&user, "buffer_size", 20000);
		obs_data_set_int(&user, "crf", 23);
		obs_data_set_bool(&user, "use_bufsize", true);
		obs_data_set_string(&user, "x264opts",
				    "tune=film vbv-bufsize=0 vbv_maxrate=0 qp=20 bitrate=1 crf=20 crf=0");
		assert(ConfiguredCapKbps("obs_x264", &user) == 6000);
		const MoQCap cap = EnforceCap("obs_x264", &user, &patch, 6000);
		assert(cap.changed);
		const std::string after =
			patch.values.count("rate_control") ? obs_data_get_string(&patch, "rate_control") : c.before;
		assert(after == c.after);
		// The bitrate is already the cap; the buffer and the options are not.
		assert(!patch.values.count("bitrate"));
		assert(!obs_data_get_bool(&patch, "use_bufsize"));
		assert(!patch.values.count("x264opts"));
		assert((cap.overridden ==
			std::vector<std::string>{"vbv-bufsize=0", "vbv_maxrate=0", "qp=20", "bitrate=1", "crf=0"}));
		// The buffer, plus the mode when it changed.
		assert(cap.replaced.size() == 1 + (std::string(c.before) != c.after && *c.before ? 1 : 0));
	}
	// A lossless CRF setting is a constant QP with no VBV.
	obs_data_t lossless, lossless_patch;
	obs_data_set_string(&lossless, "rate_control", "CRF");
	obs_data_set_int(&lossless, "bitrate", 6000);
	obs_data_set_int(&lossless, "crf", 0);
	const MoQCap crf0 = EnforceCap("obs_x264", &lossless, &lossless_patch, 6000);
	assert(crf0.changed && obs_data_get_int(&lossless_patch, "crf") == 23);
	// CBR ignores the setting, and only a zero option is lossless; x264opts is left alone.
	obs_data_t cbr_crf, cbr_crf_patch;
	obs_data_set_string(&cbr_crf, "rate_control", "CBR");
	obs_data_set_int(&cbr_crf, "bitrate", 6000);
	obs_data_set_int(&cbr_crf, "crf", 0);
	obs_data_set_string(&cbr_crf, "x264opts", "crf=0.0 crf=18");
	EnforceCap("obs_x264", &cbr_crf, &cbr_crf_patch, 6000);
	assert(!cbr_crf_patch.values.count("crf"));
	assert(!cbr_crf_patch.values.count("x264opts"));
	assert(X264RateOptions("crf=0.0 crf=18") == std::vector<std::string>{"crf=0.0"});

	// A compliant encoder is not touched.
	obs_data_t clean, nothing;
	obs_data_set_string(&clean, "rate_control", "VBR");
	obs_data_set_int(&clean, "crf", 23);
	obs_data_set_int(&clean, "bitrate", 6000);
	obs_data_set_string(&clean, "x264opts", "tune=film crf=20");
	const MoQCap untouched = EnforceCap("obs_x264", &clean, &nothing, 6000);
	assert(!untouched.changed && untouched.replaced.empty() && untouched.overridden.empty() &&
	       nothing.values.empty());

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

	// NVENC keeps CBR and VBR, and runs anything else as constant quality, under max_bitrate.
	// Its own peak is the cap when it names one, and VBR's average stays under it.
	obs_data_t vbr, vbr_patch;
	obs_data_set_string(&vbr, "rate_control", "VBR");
	obs_data_set_int(&vbr, "bitrate", 9000);
	obs_data_set_int(&vbr, "max_bitrate", 6000);
	assert(ConfiguredCapKbps("obs_nvenc_h264_tex", &vbr) == 6000);
	const MoQCap lowered = EnforceCap("obs_nvenc_h264_tex", &vbr, &vbr_patch, 6000);
	assert(lowered.changed && lowered.replaced.size() == 1);
	assert(obs_data_get_int(&vbr_patch, "bitrate") == 6000 && !vbr_patch.values.count("max_bitrate"));
	assert(!vbr_patch.values.count("rate_control"));
	// An unset peak is the bitrate.
	obs_data_set_int(&vbr, "max_bitrate", 0);
	assert(ConfiguredCapKbps("obs_nvenc_h264_tex", &vbr) == 9000);
	obs_data_t vbr_unset_patch;
	EnforceCap("obs_nvenc_h264_tex", &vbr, &vbr_unset_patch, 9000);
	assert(obs_data_get_int(&vbr_unset_patch, "max_bitrate") == 9000 && !vbr_unset_patch.values.count("bitrate"));
	obs_data_t cbr, cbr_patch;
	obs_data_set_string(&cbr, "rate_control", "CBR");
	obs_data_set_int(&cbr, "bitrate", 5000);
	obs_data_set_int(&cbr, "max_bitrate", 20000);
	assert(ConfiguredCapKbps("obs_nvenc_h264_tex", &cbr) == 5000);
	assert(!EnforceCap("obs_nvenc_h264_tex", &cbr, &cbr_patch, 5000).changed);
	for (const char *mode : {"CQP", "lossless", ""}) {
		obs_data_t user, patch;
		obs_data_set_string(&user, "rate_control", mode);
		obs_data_set_int(&user, "bitrate", 5000);
		const MoQCap cap = EnforceCap("obs_nvenc_h264_tex", &user, &patch, 5000);
		assert(cap.changed && std::string(obs_data_get_string(&patch, "rate_control")) == "CQVBR");
		assert(obs_data_get_int(&patch, "max_bitrate") == 5000);
	}

	// An encoder with no quality mode that holds a peak runs CBR at the cap, whatever the user chose.
	obs_data_t qsv, qsv_patch;
	obs_data_set_string(&qsv, "rate_control", "ICQ");
	obs_data_set_int(&qsv, "bitrate", 3000);
	const MoQCap cbr_cap = EnforceCap("obs_qsv11_v2", &qsv, &qsv_patch, 3000);
	assert(cbr_cap.replaced.size() == 1 && std::string(obs_data_get_string(&qsv_patch, "rate_control")) == "CBR");
	assert(ConfiguredCapKbps("obs_qsv11_v2", &qsv) == 3000);

	// A settings with no bitrate names no cap.
	obs_data_t bare;
	assert(ConfiguredCapKbps("obs_x264", &bare) == 0);

	// What the settings hold decides the peak, whatever the encoder.
	assert(std::string(EncoderPeakKey("obs_x264", "cbr")) == "bitrate");
	assert(std::string(EncoderPeakKey("obs_nvenc_h264_tex", "vbr")) == "max_bitrate");
	assert(std::string(EncoderPeakKey("obs_nvenc_h264_tex", "CBR")) == "bitrate");
	for (const char *mode : {"CRF", "ABR", "CQP", "ICQ", "QVBR", "lossless", ""}) {
		assert(!EncoderPeakKey("obs_x264", mode) && !EncoderPeakKey("obs_nvenc_h264_tex", mode));
	}
	assert(!EncoderPeakKey("obs_qsv11_v2", "VBR"));

	// NVENC's plain VBR averages beside its peak; CQVBR and CBR have no average to keep.
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
