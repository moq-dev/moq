// SPDX-License-Identifier: GPL-2.0-or-later
//
// Pins the Quality-tab Profile → path / codec / encoder / audio mapping so a
// Profile change keeps applying the recommended picks.
#include <cstdio>
#include <string>
#include <vector>

#include "moq-quality-defaults.h"

namespace {
int g_failures = 0;
}

#define CHECK(cond)                                                                 \
	do {                                                                        \
		if (!(cond)) {                                                      \
			fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); \
			g_failures++;                                               \
		}                                                                   \
	} while (0)

static std::vector<MoQEncoderCapability> AppleLikeOffers()
{
	return {
		{"h264", true, true},  {"h264", false, true}, {"hevc", true, true},
		{"hevc", false, true}, {"aac", false, false}, {"opus", false, false},
	};
}

static std::vector<MoQEncoderCapability> SoftwareOnlyOffers()
{
	return {
		{"h264", false, true},
		{"aac", false, false},
		{"opus", false, false},
	};
}

int main()
{
	const auto apple = AppleLikeOffers();

	{
		const auto d = RecommendQualityDefaults("high", apple);
		CHECK(d.high);
		CHECK(d.path == "hardware");
		CHECK(d.video_codec == "hevc");
		CHECK(d.video_encoder == "auto");
		CHECK(d.audio_codec == "opus");
	}
	printf("Quality on Apple-like hardware: ok\n");

	{
		const auto d = RecommendQualityDefaults("low", apple);
		CHECK(!d.high);
		CHECK(d.path == "hardware");
		CHECK(d.video_codec == "h264");
		CHECK(d.video_encoder == "auto");
		CHECK(d.audio_codec == "aac");
	}
	printf("Performance on Apple-like hardware: ok\n");

	{
		const auto d = RecommendQualityDefaults("auto", apple);
		CHECK(d.high);
		CHECK(d.path == "hardware");
		CHECK(d.video_codec == "hevc");
		CHECK(d.audio_codec == "opus");
	}
	printf("Auto with hardware → Quality: ok\n");

	{
		const auto soft = SoftwareOnlyOffers();
		const auto d = RecommendQualityDefaults("auto", soft);
		CHECK(!d.high);
		CHECK(d.path == "software");
		CHECK(d.video_codec == "h264");
		CHECK(d.audio_codec == "aac");
	}
	printf("Auto without hardware → Performance: ok\n");

	{
		// Switching Quality → Performance must not keep HEVC / Opus.
		const auto quality = RecommendQualityDefaults("high", apple);
		const auto performance = RecommendQualityDefaults("low", apple);
		CHECK(quality.video_codec == "hevc");
		CHECK(quality.audio_codec == "opus");
		CHECK(performance.video_codec == "h264");
		CHECK(performance.audio_codec == "aac");
		CHECK(quality.video_codec != performance.video_codec);
		CHECK(quality.audio_codec != performance.audio_codec);
	}
	printf("Profile switch changes codec and audio: ok\n");

	if (g_failures) {
		fprintf(stderr, "%d failure(s)\n", g_failures);
		return 1;
	}
	printf("\nall passed\n");
	return 0;
}
