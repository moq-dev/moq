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

int main()
{
	obs_data_t settings;
	obs_data_set_string(&settings, "x264opts", "tune=film bframes=4 rc-lookahead=40 sliced-threads=0 crf=20");
	const auto original = settings.values;
	ApplyEncoderLatency("obs_x264", &settings, false);
	assert(settings.values == original);
	ApplyEncoderLatency("obs_x264", &settings, true);
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
	ApplyEncoderLatency("com.apple.videotoolbox.videoencoder.ave.avc", &settings, true);
	assert(std::get<bool>(settings.values.at("bframes")) == false);

	settings = {};
	ApplyEncoderLatency("obs_nvenc_h264_tex", &settings, true);
	assert(std::get<std::string>(settings.values.at("tune")) == "ull");
	assert(std::get<long long>(settings.values.at("bf")) == 0);
	assert(std::get<bool>(settings.values.at("lookahead")) == false);

	settings = {};
	obs_data_set_int(&settings, "async_depth", 4);
	obs_data_set_int(&settings, "la_depth", 15);
	ApplyEncoderLatency("obs_qsv11_v2", &settings, true);
	assert(settings.values.count("async_depth") == 0 && settings.values.count("la_depth") == 0);
	assert(std::get<std::string>(settings.values.at("latency")) == "ultra-low");
	assert(std::get<long long>(settings.values.at("bframes")) == 0);

	settings = {};
	const std::string unsupported = ApplyEncoderLatency("third_party_encoder", &settings, true);
	assert(settings.values.empty() && unsupported.find("unavailable") != std::string::npos);
	std::puts("encoder latency overrides and preserved settings: ok");
}
