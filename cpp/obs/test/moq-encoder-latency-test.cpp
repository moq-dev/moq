// SPDX-License-Identifier: GPL-2.0-or-later
#include "moq-encoder-latency.h"

#include <cassert>
#include <cstdio>
#include <map>
#include <set>
#include <sstream>
#include <variant>

struct obs_data {
	std::map<std::string, std::variant<std::string, long long, bool>> values;
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
void obs_data_set_bool(obs_data_t *data, const char *key, bool value)
{
	data->values[key] = value;
}
void obs_data_erase(obs_data_t *data, const char *key)
{
	data->values.erase(key);
}

template<typename T> static T get(const obs_data_t &settings, const char *key)
{
	return std::get<T>(settings.values.at(key));
}

// Tunes fresh settings for this encoder and returns the keys it wrote.
static std::set<std::string> keys(const char *id)
{
	obs_data_t settings;
	ApplyEncoderLatency(id, &settings, 30);
	std::set<std::string> written;
	for (const auto &entry : settings.values)
		written.insert(entry.first);
	return written;
}

int main()
{
	obs_data_t settings;
	obs_data_set_string(&settings, "x264opts", "tune=film bframes=4 rc-lookahead=40 sliced-threads=0 crf=20");
	ApplyEncoderLatency("obs_x264", &settings, 30);
	// x264 consumes these after applying its tune; the last value must win.
	std::map<std::string, std::string> options;
	std::istringstream input(obs_data_get_string(&settings, "x264opts"));
	for (std::string token; input >> token;) {
		auto split = token.find('=');
		options[token.substr(0, split)] = token.substr(split + 1);
	}
	assert(options["tune"] == "zerolatency");
	assert(options["bframes"] == "0" && options["rc-lookahead"] == "0");
	assert(options["sync-lookahead"] == "0" && options["sliced-threads"] == "1");
	assert(options["crf"] == "20");

	settings = {};
	obs_data_set_int(&settings, "async_depth", 4);
	obs_data_set_int(&settings, "la_depth", 15);
	ApplyEncoderLatency("obs_qsv11_v2", &settings, 30);
	assert(settings.values.count("async_depth") == 0 && settings.values.count("la_depth") == 0);
	assert(get<std::string>(settings, "latency") == "ultra-low");
	assert(get<long long>(settings, "bf") == 0);

	settings = {};
	ApplyEncoderLatency("obs_nvenc_h264_tex", &settings, 30);
	assert(get<std::string>(settings, "tune") == "ull");
	assert(get<long long>(settings, "bf") == 0 && get<bool>(settings, "lookahead") == false);

	settings = {};
	ApplyEncoderLatency("com.apple.videotoolbox.videoencoder.ave.avc", &settings, 30);
	assert(get<bool>(settings, "bframes") == false);

	// Each encoder gets only the keys it reads, so none sees another's values.
	using Keys = std::set<std::string>;
	assert((keys("obs_x264") == Keys{"tune", "x264opts", "keyint_sec"}));
	for (const char *id : {"obs_nvenc_hevc_tex", "jim_av1_nvenc", "ffmpeg_nvenc", "ffmpeg_hevc_nvenc"})
		assert((keys(id) == Keys{"tune", "bf", "lookahead", "keyint_sec"}));
	for (const char *id : {"obs_qsv11", "obs_qsv11_av1", "obs_qsv11_hevc_soft"})
		assert((keys(id) == Keys{"latency", "bf", "keyint_sec"}));
	for (const char *id : {"h264_texture_amf", "h265_fallback_amf", "av1_texture_amf"})
		assert((keys(id) == Keys{"bf", "pre_analysis", "keyint_sec"}));
	for (const char *id : {"ffmpeg_vaapi_tex", "hevc_ffmpeg_vaapi", "av1_ffmpeg_vaapi_tex"})
		assert((keys(id) == Keys{"bf", "keyint_sec"}));
	assert((keys("com.apple.videotoolbox.videoencoder.ave.hevc") == Keys{"bframes", "keyint_sec"}));

	// An unknown encoder, or none, keeps its settings, keyframe interval included.
	for (const char *id : {"third_party_encoder", "", "obs_x265"}) {
		settings = {};
		obs_data_set_int(&settings, "keyint_sec", 8);
		const std::string summary = ApplyEncoderLatency(id, &settings, 30);
		assert(settings.values.size() == 1 && get<long long>(settings, "keyint_sec") == 8);
		assert(summary.find("unavailable") != std::string::npos);
	}

	// An automatic or longer interval becomes 2 s; a shorter one the user chose stays.
	for (long long keyint : {0LL, 3LL, 10LL}) {
		settings = {};
		obs_data_set_int(&settings, "keyint_sec", keyint);
		ApplyEncoderLatency("ffmpeg_vaapi_tex", &settings, 30);
		assert(get<long long>(settings, "keyint_sec") == kMoQKeyintSec);
	}
	settings = {};
	obs_data_set_int(&settings, "keyint_sec", 1);
	ApplyEncoderLatency("ffmpeg_vaapi_tex", &settings, 30);
	assert(get<long long>(settings, "keyint_sec") == 1);

	// x264 reads its options after keyint_sec: a longer saved keyint is capped at 2 s of
	// frames, and a shorter one stays.
	const auto x264Keyint = [](const char *saved, double fps) {
		obs_data_t x264;
		obs_data_set_string(&x264, "x264opts", saved);
		ApplyEncoderLatency("obs_x264", &x264, fps);
		return X264OptionsKeyint(obs_data_get_string(&x264, "x264opts"));
	};
	assert(x264Keyint("keyint=300", 30) == 60);
	assert(x264Keyint("keyint_max=300", 60) == 120);
	assert(x264Keyint("keyint=infinite", 29.97) == 60);
	assert(x264Keyint("keyint=30", 30) == 30);
	assert(x264Keyint("", 30) == 0);
	// An unknown frame rate leaves the saved keyint to x264.
	assert(x264Keyint("keyint=300", 0) == 300);

	// Simple output names resolve to the ids OBS 32.2.2 creates.
	const std::map<std::string, std::string> simple = {
		{"x264", "obs_x264"},
		{"x264_lowcpu", "obs_x264"},
		{"qsv", "obs_qsv11_v2"},
		{"qsv_av1", "obs_qsv11_av1"},
		{"nvenc", "obs_nvenc_h264_tex"},
		{"nvenc_hevc", "obs_nvenc_hevc_tex"},
		{"nvenc_av1", "obs_nvenc_av1_tex"},
		{"amd", "h264_texture_amf"},
		{"amd_hevc", "h265_texture_amf"},
		{"amd_av1", "av1_texture_amf"},
		{"apple_h264", "com.apple.videotoolbox.videoencoder.ave.avc"},
		{"apple_hevc", "com.apple.videotoolbox.videoencoder.ave.hevc"},
	};
	for (const auto &[name, id] : simple)
		assert(SimpleVideoEncoderId(name.c_str()) == id);
	assert(std::string(SimpleVideoEncoderId(nullptr)) == "obs_x264");
	std::puts("encoder latency mapping and preserved settings: ok");
}
