// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <obs-data.h>
#include <cctype>
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
// when it has none (CQP, CRF, ICQ, or a mode that only averages). The peak is what
// reservations, the catalog, and dynamic bitrate price a rendition at.
inline const char *EncoderPeakKey(const std::string &id, std::string rate_control)
{
	rate_control = Upper(std::move(rate_control));
	// CBR holds the encoder at its bitrate, so that is also its peak.
	if (rate_control == "CBR")
		return "bitrate";
	// x264's VBR is constant quality (CRF) under a VBV whose max rate is the bitrate.
	if (id == "obs_x264")
		return rate_control == "VBR" ? "bitrate" : nullptr;
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

// The options in `x264opts` that set a rate of their own: OBS applies them after its rate
// parameters, so `vbv-maxrate` and `vbv-bufsize` replace the VBV (0 disables it), `bitrate`
// replaces the rate mode, and `qp` drops the VBV for constant QP.
inline std::string StripX264RateOptions(const std::string &options, std::vector<std::string> *removed)
{
	std::istringstream input(options);
	std::string kept;
	std::string option;
	while (input >> option) {
		std::string name = option.substr(0, option.find('='));
		for (char &c : name)
			if (c == '_')
				c = '-';
		if (name == "vbv-maxrate" || name == "vbv-bufsize" || name == "bitrate" || name == "qp") {
			removed->push_back(option);
			continue;
		}
		kept += (kept.empty() ? "" : " ") + option;
	}
	return kept;
}

// What EnforceCap changed.
struct MoQCap {
	// Whether `write` holds anything the encoder's settings did not already say.
	bool changed = false;
	// The user's rate settings it replaced, each described for a log line.
	std::vector<std::string> replaced;
};

// Makes `id` hold `cap_kbps` as its hard peak, writing what differs from `read` into
// `write` (the same settings, to rewrite them in place). The cap is the plugin's: it
// replaces any rate the user set that would exceed it, and says which in `replaced`.
//  - x264 keeps CBR and otherwise runs VBR (CRF under a VBV whose max rate and buffer are
//    the bitrate), with `x264opts` stripped of the options that set a rate of their own.
//  - NVENC keeps CBR and VBR, with `max_bitrate` at the cap and the average under it, and
//    otherwise runs CQVBR (constant quality under `max_bitrate`).
//  - Everything else runs CBR at the cap: its quality modes carry no hard peak, or depend
//    on a driver or a Mac to check.
// The quality level is the encoder's own default. An unset rate mode (a fresh dock profile)
// takes the quality mode where there is one without being reported as replaced.
inline MoQCap EnforceCap(const std::string &id, obs_data_t *read, obs_data_t *write, long long cap_kbps)
{
	MoQCap result;
	const std::string before = obs_data_get_string(read, "rate_control");
	const std::string mode = Upper(before);
	const long long average = obs_data_get_int(read, "bitrate");
	const std::string x264opts = obs_data_get_string(read, "x264opts");
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
		// A buffer of its own can hold a burst past the cap; unset, OBS sizes it to the bitrate.
		if (use_bufsize) {
			obs_data_set_bool(write, "use_bufsize", false);
			result.changed = true;
			result.replaced.push_back("use_bufsize");
		}
		std::vector<std::string> removed;
		const std::string kept = StripX264RateOptions(x264opts, &removed);
		if (!removed.empty()) {
			obs_data_set_string(write, "x264opts", kept.c_str());
			result.changed = true;
			for (const std::string &option : removed)
				result.replaced.push_back("x264opts " + option);
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
