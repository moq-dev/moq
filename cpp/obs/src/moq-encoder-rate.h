// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <obs-data.h>
#include <cctype>
#include <sstream>
#include <string>
#include <utility>

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

// Whether x264's advanced options set a rate of their own. OBS applies `x264opts` after its
// rate parameters, so a `vbv-maxrate` replaces the peak, `bitrate` replaces the rate mode,
// and `qp` drops the VBV, and the configured bitrate no longer bounds the encoder.
inline bool X264OptionsOverrideRate(const std::string &options)
{
	std::istringstream input(options);
	std::string option;
	while (input >> option) {
		std::string name = option.substr(0, option.find('='));
		for (char &c : name)
			if (c == '_')
				c = '-';
		if (name == "vbv-maxrate" || name == "bitrate" || name == "qp")
			return true;
	}
	return false;
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
	// A peak the user's own x264 options replace is not one we can price.
	if (id == "obs_x264" && X264OptionsOverrideRate(obs_data_get_string(settings, "x264opts")))
		return {};
	const long long kbps = obs_data_get_int(settings, key);
	if (kbps <= 0)
		return {};
	// NVENC's plain VBR averages at "bitrate" beside its peak in "max_bitrate".
	const bool averaged = std::string(key) == "max_bitrate" && Upper(rate_control) == "VBR";
	return {key, kbps, averaged ? obs_data_get_int(settings, "bitrate") : 0};
}

// Configures `settings` to run `id` at constant quality under a peak of `cap_kbps`, where
// the encoder honors one, and at CBR at that rate where it does not, so it never
// exceeds the rate a viewer priced it at. Returns whether the quality mode took.
// The quality level is the encoder's own default.
inline bool ApplyEncoderRate(const std::string &id, obs_data_t *settings, long long cap_kbps)
{
	obs_data_set_int(settings, "bitrate", cap_kbps);
	if (id == "obs_x264") {
		obs_data_set_string(settings, "rate_control", "VBR");
		return true;
	}
	if (id.rfind("obs_nvenc_", 0) == 0) {
		obs_data_set_string(settings, "rate_control", "CQVBR");
		obs_data_set_int(settings, "max_bitrate", cap_kbps);
		return true;
	}
	// Quick Sync's ICQ and AMF's QVBR carry no peak, VA-API's QVBR depends on the driver,
	// and VideoToolbox's quality mode needs a Mac to check.
	obs_data_set_string(settings, "rate_control", "CBR");
	return false;
}
