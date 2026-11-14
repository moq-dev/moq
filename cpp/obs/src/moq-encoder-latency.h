// SPDX-License-Identifier: GPL-2.0-or-later
#pragma once

#include <obs-data.h>
#include <string>

// The longest group a viewer waits through to join, in seconds.
constexpr long long kMoQKeyintSec = 2;

// Tunes settings for low latency on every encoder OBS 32.2.2 bundles, since a
// service's apply_encoder_settings is never told which encoder they are for.
// Each encoder reads only its own keys, and the one shared key is noted below.
inline void ApplyEncoderLatency(obs_data_t *settings)
{
	// x264 applies x264opts after its tune, and a tune there overrides the
	// "tune" key NVENC also reads. Appending the latency constraints means saved
	// advanced options cannot silently restore lookahead or B-frames.
	std::string options = obs_data_get_string(settings, "x264opts");
	options += " tune=zerolatency bframes=0 rc-lookahead=0 sync-lookahead=0 sliced-threads=1 mbtree=0";
	obs_data_set_string(settings, "x264opts", options.c_str());

	// NVENC, and the FFmpeg NVENC fallback.
	obs_data_set_string(settings, "tune", "ull");
	obs_data_set_bool(settings, "lookahead", false);

	// x264, NVENC, Quick Sync (over its own "bframes"), AMF, and VA-API.
	obs_data_set_int(settings, "bf", 0);
	// VideoToolbox.
	obs_data_set_bool(settings, "bframes", false);

	// Quick Sync. Its legacy keys otherwise rewrite latency during encoder creation.
	obs_data_erase(settings, "async_depth");
	obs_data_erase(settings, "la_depth");
	obs_data_set_string(settings, "latency", "ultra-low");

	// Every encoder; 0 means its own automatic interval. A shorter one the user chose stays.
	const long long keyint = obs_data_get_int(settings, "keyint_sec");
	if (keyint <= 0 || keyint > kMoQKeyintSec)
		obs_data_set_int(settings, "keyint_sec", kMoQKeyintSec);
}

// Describes what ApplyEncoderLatency achieves on the encoder with this id.
inline const char *EncoderLatencySummary(const std::string &id, bool low)
{
	if (!low)
		return "Encoder settings unchanged";
	if (id == "obs_x264")
		return "Low latency: x264 zero-latency tune";
	if (id.rfind("com.apple.videotoolbox.videoencoder.", 0) == 0)
		return "Low latency: no B-frames (VideoToolbox may still buffer)";
	if (id.find("nvenc") != std::string::npos)
		return "Low latency: NVENC ultra-low tune, no B-frames or lookahead";
	if (id.rfind("obs_qsv11", 0) == 0)
		return "Low latency: Quick Sync ultra-low mode, no B-frames";
	if (id.size() > 4 && id.compare(id.size() - 4, 4, "_amf") == 0)
		return "Low latency: AMF, no B-frames";
	if (id.find("vaapi") != std::string::npos)
		return "Low latency: VA-API, no B-frames";
	return "Low latency preset unavailable: only a 2 s keyframe interval requested";
}
