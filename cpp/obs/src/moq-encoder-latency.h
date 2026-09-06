// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <obs-data.h>
#include <string>

// Settings consumed by the OBS 32.2.2 encoder plugins, applied to a private copy.
inline const char *ApplyEncoderLatency(const std::string &id, obs_data_t *settings, bool low)
{
	if (!low)
		return "Encoder settings unchanged";
	if (id == "obs_x264") {
		obs_data_set_string(settings, "tune", "zerolatency");
		// OBS applies x264opts after its tune. Append the latency constraints so
		// saved advanced options cannot silently restore lookahead or B-frames.
		std::string options = obs_data_get_string(settings, "x264opts");
		options += " tune=zerolatency bframes=0 rc-lookahead=0 sync-lookahead=0 sliced-threads=1 mbtree=0";
		obs_data_set_string(settings, "x264opts", options.c_str());
		return "Low latency: x264 zero-latency tune";
	}
	if (id.rfind("com.apple.videotoolbox.videoencoder.", 0) == 0) {
		obs_data_set_bool(settings, "bframes", false);
		return "Low latency: no B-frames (VideoToolbox may still buffer)";
	}
	if (id.rfind("obs_nvenc_", 0) == 0) {
		obs_data_set_string(settings, "tune", "ull");
		obs_data_set_int(settings, "bf", 0);
		obs_data_set_bool(settings, "lookahead", false);
		return "Low latency: NVENC ultra-low tune, no B-frames or lookahead";
	}
	if (id.rfind("obs_qsv11", 0) == 0) {
		// Legacy settings otherwise rewrite latency during encoder creation.
		obs_data_erase(settings, "async_depth");
		obs_data_erase(settings, "la_depth");
		obs_data_set_string(settings, "latency", "ultra-low");
		obs_data_set_int(settings, "bframes", 0);
		return "Low latency: Quick Sync ultra-low mode, no B-frames";
	}
	return "Low latency preset unavailable: encoder settings unchanged";
}
