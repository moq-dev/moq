// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <obs-data.h>
#include <cctype>
#include <cstdlib>
#include <sstream>
#include <string>
#include <utility>
#include <vector>

inline std::string Upper(std::string text)
{
	for (char &c : text)
		c = static_cast<char>(std::toupper(static_cast<unsigned char>(c)));
	return text;
}

// The setting that holds the hard peak an encoder honors under `rate_control`, or null
// when it has none (CQP, CRF, ICQ, or an encoder whose peak is not modeled, like QSV and AMF).
// The peak is what reservations, the catalog, and dynamic bitrate price a rendition at.
inline const char *EncoderPeakKey(const std::string &id, std::string rate_control)
{
	rate_control = Upper(std::move(rate_control));
	// CBR holds the encoder at its bitrate, so that is also its peak.
	if (rate_control == "CBR")
		return "bitrate";
	// x264's VBR is constant quality (CRF) and its ABR an average, both under a VBV whose max
	// rate is the bitrate.
	if (id == "obs_x264")
		return rate_control == "VBR" || rate_control == "ABR" ? "bitrate" : nullptr;
	// NVENC's VBR averages at "bitrate" but never exceeds "max_bitrate"; CQVBR is
	// constant quality under that same max, and ignores "bitrate".
	if (id.rfind("obs_nvenc_", 0) == 0)
		return rate_control == "VBR" || rate_control == "CQVBR" ? "max_bitrate" : nullptr;
	return nullptr;
}

// An encoder's hard peak in kbps and the setting that holds it.
struct MoQPeak {
	const char *key = nullptr;
	long long kbps = 0;
	// The average NVENC's plain VBR holds beside its peak, which must stay at or below it,
	// or 0 when the encoder has none.
	long long average_kbps = 0;
};

// The peak these encoder settings configure, with a null key when they configure none.
inline MoQPeak EncoderPeak(const std::string &id, obs_data_t *settings)
{
	const std::string rate_control = obs_data_get_string(settings, "rate_control");
	const char *key = EncoderPeakKey(id, rate_control);
	if (!key)
		return {};
	const long long kbps = obs_data_get_int(settings, key);
	if (kbps <= 0)
		return {};
	// NVENC's plain VBR averages at "bitrate" beside its peak in "max_bitrate".
	const bool averaged = std::string(key) == "max_bitrate" && Upper(rate_control) == "VBR";
	return {key, kbps, averaged ? obs_data_get_int(settings, "bitrate") : 0};
}

// x264's own default constant quality level, which a lossless CRF of 0 gives way to.
constexpr long long kDefaultX264Crf = 23;

inline bool IsNvenc(const std::string &id)
{
	return id.rfind("obs_nvenc_", 0) == 0;
}

// The cap in kbps the settings name: NVENC's own peak when its VBR or constant quality
// sets one, otherwise the bitrate. 0 when the settings name none.
inline long long ConfiguredCapKbps(const std::string &id, obs_data_t *settings)
{
	if (IsNvenc(id)) {
		const std::string rate_control = Upper(obs_data_get_string(settings, "rate_control"));
		const long long peak = obs_data_get_int(settings, "max_bitrate");
		if ((rate_control == "VBR" || rate_control == "CQVBR") && peak > 0)
			return peak;
	}
	return obs_data_get_int(settings, "bitrate");
}

// The hard peak in kbps an encoder already running with `settings` holds, or 0 when it runs
// uncapped: a quality mode with no peak (CQP, CRF, ICQ), x264's lossless CRF of 0 (constant
// QP with no VBV), or x264 with a buffer of its own set to 0, which makes it ignore the VBV
// in every mode. The settings are read as they are, never rewritten.
inline long long RunningCapKbps(const std::string &id, obs_data_t *settings)
{
	const MoQPeak peak = EncoderPeak(id, settings);
	if (!peak.key)
		return 0;
	if (id == "obs_x264") {
		if (obs_data_get_bool(settings, "use_bufsize") && obs_data_get_int(settings, "buffer_size") <= 0)
			return 0;
		// Only VBR reads the CRF: ABR and CBR zero it.
		if (Upper(obs_data_get_string(settings, "rate_control")) == "VBR" &&
		    obs_data_get_int(settings, "crf") <= 0)
			return 0;
	}
	return peak.kbps;
}

inline bool IsVaapi(const std::string &id)
{
	return id.find("vaapi") != std::string::npos;
}

// The setting that holds an encoder's free-form options, which OBS applies after its own
// rate settings, or null when it has none.
inline const char *EncoderOptionsKey(const std::string &id)
{
	if (id == "obs_x264")
		return "x264opts";
	if (IsVaapi(id))
		return "ffmpeg_opts";
	return IsNvenc(id) ? "opts" : nullptr;
}

// The options the encoder's free-form options set a rate with, which replace the cap.
// For x264, `vbv-maxrate` and `vbv-bufsize` replace the VBV (0 disables it), `bitrate`
// replaces the rate mode, and `qp` or a `crf` of 0 (lossless) drops the VBV for constant QP.
// For NVENC, `maxBitRate`, `averageBitRate`, and `vbvBufferSize` replace the rate it was
// given. For VA-API, whose rate mode FFmpeg infers from the settings, `rc_mode` picks
// another and `qp` turns it into constant QP. The plugin never rewrites the text, so these
// can override the cap.
inline std::vector<std::string> RateOptions(const std::string &id, obs_data_t *settings)
{
	const char *key = EncoderOptionsKey(id);
	std::vector<std::string> found;
	if (!key)
		return found;
	const bool x264 = id == "obs_x264";
	const bool vaapi = IsVaapi(id);
	std::istringstream input(obs_data_get_string(settings, key));
	std::string option;
	while (input >> option) {
		std::string name = option.substr(0, option.find('='));
		for (char &c : name)
			if (c == '_')
				c = '-';
		const size_t assign = option.find('=');
		const bool lossless = name == "crf" && assign != std::string::npos &&
				      std::atof(option.c_str() + assign + 1) <= 0;
		bool rate;
		if (x264)
			rate = name == "vbv-maxrate" || name == "vbv-bufsize" || name == "bitrate" || name == "qp" ||
			       lossless;
		else if (vaapi)
			rate = name == "rc-mode" || name == "qp";
		else
			rate = name == "maxBitRate" || name == "averageBitRate" || name == "vbvBufferSize";
		if (rate)
			found.push_back(option);
	}
	return found;
}

// What EnforceCap changed.
struct MoQCap {
	// Whether `write` holds anything the encoder's settings did not already say.
	bool changed = false;
	// The user's rate settings it replaced, each described for a log line.
	std::vector<std::string> replaced;
	// The free-form options that can override the cap, which it leaves as the user wrote them.
	std::vector<std::string> overridden;
};

// Makes `id` hold `cap_kbps` as its hard peak, writing what differs from `read` into
// `write` (the same settings, to rewrite them in place). The cap is the plugin's: it
// replaces any rate the user set that would exceed it, and says which in `replaced`.
//  - x264 keeps CBR and otherwise runs VBR (CRF under a VBV whose max rate and buffer are
//    the bitrate). It writes only OBS's own settings: `x264opts` (and NVENC's `opts`) stay as
//    the user wrote them, and the options in them that set a rate are reported in `overridden`.
//  - NVENC keeps CBR and VBR, with `max_bitrate` at the cap and the average under it, and
//    otherwise runs CQVBR (constant quality under `max_bitrate`).
//  - Everything else runs CBR at the cap: its quality modes carry no hard peak (VA-API's
//    QVBR was measured past the cap on Mesa and Intel drivers), or need a Mac to check.
// The quality level is the encoder's own default. An unset rate mode (a fresh dock profile)
// takes the quality mode where there is one without being reported as replaced.
inline MoQCap EnforceCap(const std::string &id, obs_data_t *read, obs_data_t *write, long long cap_kbps)
{
	MoQCap result;
	const std::string before = obs_data_get_string(read, "rate_control");
	const std::string mode = Upper(before);
	const long long average = obs_data_get_int(read, "bitrate");
	const bool use_bufsize = obs_data_get_bool(read, "use_bufsize");

	auto set_int = [&](const char *key, long long value) {
		if (obs_data_get_int(read, key) == value)
			return;
		obs_data_set_int(write, key, value);
		result.changed = true;
	};
	auto set_mode = [&](const char *want) {
		if (mode == want)
			return;
		obs_data_set_string(write, "rate_control", want);
		result.changed = true;
		if (!before.empty())
			result.replaced.push_back("rate_control " + before + " -> " + want);
	};

	if (id == "obs_x264") {
		set_mode(mode == "CBR" ? "CBR" : "VBR");
		set_int("bitrate", cap_kbps);
		// A CRF of 0 is lossless, which x264 runs as constant QP with no VBV. CBR ignores the setting.
		if (mode != "CBR" && obs_data_get_int(read, "crf") <= 0) {
			obs_data_set_int(write, "crf", kDefaultX264Crf);
			result.changed = true;
			result.replaced.push_back("crf 0");
		}
		// A buffer of its own can hold a burst past the cap; unset, OBS sizes it to the bitrate.
		if (use_bufsize) {
			obs_data_set_bool(write, "use_bufsize", false);
			result.changed = true;
			result.replaced.push_back("use_bufsize");
		}
	} else if (IsNvenc(id)) {
		set_mode(mode == "CBR" || mode == "VBR" ? mode.c_str() : "CQVBR");
		// CBR's peak is its bitrate and constant quality ignores it; VBR averages under its peak.
		if (mode == "CBR") {
			set_int("bitrate", cap_kbps);
		} else {
			set_int("max_bitrate", cap_kbps);
			if (mode == "VBR" && average > cap_kbps) {
				set_int("bitrate", cap_kbps);
				result.replaced.push_back("bitrate " + std::to_string(average));
			}
		}
	} else {
		set_mode("CBR");
		set_int("bitrate", cap_kbps);
	}
	result.overridden = RateOptions(id, read);
	return result;
}

// Configures a fresh `settings` to run `id` at constant quality under a peak of `cap_kbps`,
// where the encoder honors one, and at CBR at that rate where it does not. Returns whether
// the quality mode took.
inline bool ApplyEncoderRate(const std::string &id, obs_data_t *settings, long long cap_kbps)
{
	obs_data_set_string(settings, "rate_control", "");
	EnforceCap(id, settings, settings, cap_kbps);
	return EncoderPeakKey(id, obs_data_get_string(settings, "rate_control")) != nullptr &&
	       Upper(obs_data_get_string(settings, "rate_control")) != "CBR";
}
