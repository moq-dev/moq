// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <string>
#include <vector>

// Recommended Quality-tab picks for a Profile change. Kept free of Qt / libobs so
// the unit tests can pin the mapping without spinning up the dock.

struct MoQEncoderCapability {
	std::string codec; // h264, hevc, av1, aac, opus, …
	bool hardware = false;
	bool video = true;
};

struct MoQQualityDefaults {
	bool high = true;          // Quality (8 Mbps) vs Performance (2.5 Mbps)
	std::string path;          // "hardware" or "software"
	std::string video_codec;   // "h264" / "hevc" / …
	std::string video_encoder; // always "auto" for a profile switch
	std::string audio_codec;   // "aac" / "opus"
};

inline MoQQualityDefaults RecommendQualityDefaults(const std::string &profile,
						   const std::vector<MoQEncoderCapability> &offers)
{
	bool haveHw = false;
	bool haveHwH264 = false;
	bool haveHwHevc = false;
	bool haveH264 = false;
	bool haveHevc = false;
	bool haveAac = false;
	bool haveOpus = false;

	for (const auto &o : offers) {
		if (o.video) {
			if (o.codec == "h264") {
				haveH264 = true;
				if (o.hardware)
					haveHwH264 = true;
			} else if (o.codec == "hevc") {
				haveHevc = true;
				if (o.hardware)
					haveHwHevc = true;
			}
			if (o.hardware)
				haveHw = true;
		} else if (o.codec == "aac") {
			haveAac = true;
		} else if (o.codec == "opus") {
			haveOpus = true;
		}
	}

	// Auto maps to Quality when hardware exists, Performance otherwise.
	const bool high = profile == "high" || (profile == "auto" && haveHw);
	const std::string path = haveHw ? "hardware" : "software";
	const bool preferHw = path != "software";

	std::string video_codec = "h264";
	if (high) {
		if (preferHw ? haveHwHevc : haveHevc)
			video_codec = "hevc";
		else if (preferHw ? haveHwH264 : haveH264)
			video_codec = "h264";
		else if (haveHevc)
			video_codec = "hevc";
	} else if (!haveH264 && haveHevc) {
		video_codec = "hevc";
	}

	const std::string audio_codec = high ? (haveOpus ? "opus" : "aac") : (haveAac ? "aac" : "opus");

	return MoQQualityDefaults{high, path, video_codec, "auto", audio_codec};
}
