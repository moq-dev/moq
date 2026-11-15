// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <obs-data.h>
#include <cstring>
#include <string>

// The longest group a viewer waits through to join, in seconds.
constexpr long long kMoQKeyintSec = 2;

// The encoder id behind a Simple output encoder name, mirroring OBS 32.2.2's own table.
// NVENC names map to the new encoders; OBS's FFmpeg fallback reads the same latency keys.
inline const char *SimpleVideoEncoderId(const char *name)
{
	if (!name)
		return "obs_x264";
	if (strcmp(name, "qsv") == 0)
		return "obs_qsv11_v2";
	if (strcmp(name, "qsv_av1") == 0)
		return "obs_qsv11_av1";
	if (strcmp(name, "amd") == 0)
		return "h264_texture_amf";
	if (strcmp(name, "amd_hevc") == 0)
		return "h265_texture_amf";
	if (strcmp(name, "amd_av1") == 0)
		return "av1_texture_amf";
	if (strcmp(name, "nvenc") == 0)
		return "obs_nvenc_h264_tex";
	if (strcmp(name, "nvenc_hevc") == 0)
		return "obs_nvenc_hevc_tex";
	if (strcmp(name, "nvenc_av1") == 0)
		return "obs_nvenc_av1_tex";
	if (strcmp(name, "apple_h264") == 0)
		return "com.apple.videotoolbox.videoencoder.ave.avc";
	if (strcmp(name, "apple_hevc") == 0)
		return "com.apple.videotoolbox.videoencoder.ave.hevc";
	return "obs_x264";
}

// Tunes the settings of the encoder with this id for low latency: no B-frames or
// lookahead, and a keyframe at least every 2 s, keeping a shorter interval the user
// chose. Leaves an encoder it does not know untouched, and says what it did.
inline const char *ApplyEncoderLatency(const std::string &id, obs_data_t *settings)
{
	const char *summary;
	if (id == "obs_x264") {
		// OBS applies x264opts after its tune. Append the latency constraints so
		// saved advanced options cannot silently restore lookahead or B-frames.
		std::string options = obs_data_get_string(settings, "x264opts");
		options += " tune=zerolatency bframes=0 rc-lookahead=0 sync-lookahead=0 sliced-threads=1 mbtree=0";
		obs_data_set_string(settings, "tune", "zerolatency");
		obs_data_set_string(settings, "x264opts", options.c_str());
		summary = "Low latency: x264 zero-latency tune";
	} else if (id.find("nvenc") != std::string::npos) {
		// The NVENC encoders, their legacy ids, and OBS's FFmpeg fallback.
		obs_data_set_string(settings, "tune", "ull");
		obs_data_set_int(settings, "bf", 0);
		obs_data_set_bool(settings, "lookahead", false);
		summary = "Low latency: NVENC ultra-low tune, no B-frames or lookahead";
	} else if (id.rfind("obs_qsv11", 0) == 0) {
		// Legacy settings otherwise rewrite latency during encoder creation.
		obs_data_erase(settings, "async_depth");
		obs_data_erase(settings, "la_depth");
		obs_data_set_string(settings, "latency", "ultra-low");
		// Quick Sync reads "bf" over its own "bframes" when set.
		obs_data_set_int(settings, "bf", 0);
		summary = "Low latency: Quick Sync ultra-low mode, no B-frames";
	} else if (id.size() > 4 && id.compare(id.size() - 4, 4, "_amf") == 0) {
		obs_data_set_int(settings, "bf", 0);
		obs_data_set_bool(settings, "pre_analysis", false);
		summary = "Low latency: AMF, no B-frames or pre-analysis";
	} else if (id.find("vaapi") != std::string::npos) {
		obs_data_set_int(settings, "bf", 0);
		summary = "Low latency: VA-API, no B-frames";
	} else if (id.rfind("com.apple.videotoolbox.videoencoder.", 0) == 0) {
		obs_data_set_bool(settings, "bframes", false);
		summary = "Low latency: no B-frames (VideoToolbox may still buffer)";
	} else {
		return "Low latency preset unavailable: encoder settings unchanged";
	}

	// Every encoder above reads it; 0 means its own automatic interval.
	const long long keyint = obs_data_get_int(settings, "keyint_sec");
	if (keyint <= 0 || keyint > kMoQKeyintSec)
		obs_data_set_int(settings, "keyint_sec", kMoQKeyintSec);
	return summary;
}
