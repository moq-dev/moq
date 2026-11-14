// SPDX-License-Identifier: GPL-2.0-or-later
#include "moq-encoder-latency.h"

#include <cassert>
#include <cstdio>
#include <map>
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

int main()
{
	// One call carries every bundled encoder's keys, since a service is never
	// told which encoder its settings are for.
	obs_data_t settings;
	obs_data_set_string(&settings, "x264opts", "tune=film bframes=4 rc-lookahead=40 sliced-threads=0 crf=20");
	obs_data_set_int(&settings, "async_depth", 4);
	obs_data_set_int(&settings, "la_depth", 15);
	ApplyEncoderLatency(&settings);

	// x264 consumes these after applying its tune; the last value must win, and
	// a tune here overrides the "tune" key NVENC reads.
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

	// NVENC.
	assert(get<std::string>(settings, "tune") == "ull");
	assert(get<bool>(settings, "lookahead") == false);
	// x264, NVENC, Quick Sync, AMF, and VA-API read "bf"; VideoToolbox reads "bframes".
	assert(get<long long>(settings, "bf") == 0);
	assert(get<bool>(settings, "bframes") == false);
	// Quick Sync.
	assert(settings.values.count("async_depth") == 0 && settings.values.count("la_depth") == 0);
	assert(get<std::string>(settings, "latency") == "ultra-low");
	assert(get<long long>(settings, "keyint_sec") == kMoQKeyintSec);

	// An automatic or longer interval becomes 2 s; a shorter one the user chose stays.
	for (long long keyint : {0LL, 3LL, 10LL}) {
		settings = {};
		obs_data_set_int(&settings, "keyint_sec", keyint);
		ApplyEncoderLatency(&settings);
		assert(get<long long>(settings, "keyint_sec") == kMoQKeyintSec);
	}
	settings = {};
	obs_data_set_int(&settings, "keyint_sec", 1);
	ApplyEncoderLatency(&settings);
	assert(get<long long>(settings, "keyint_sec") == 1);

	assert(std::string(EncoderLatencySummary("obs_x264", false)) == "Encoder settings unchanged");
	assert(std::string(EncoderLatencySummary("obs_nvenc_h264_tex", true)).find("NVENC") != std::string::npos);
	assert(std::string(EncoderLatencySummary("ffmpeg_hevc_nvenc", true)).find("NVENC") != std::string::npos);
	assert(std::string(EncoderLatencySummary("h265_fallback_amf", true)).find("AMF") != std::string::npos);
	assert(std::string(EncoderLatencySummary("third_party_encoder", true)).find("unavailable") !=
	       std::string::npos);
	std::puts("encoder latency overrides and preserved settings: ok");
}
