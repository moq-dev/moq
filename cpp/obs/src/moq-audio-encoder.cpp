// SPDX-License-Identifier: GPL-2.0-or-later
#include "moq-audio-encoder.h"

#include <obs-module.h>

#include <moq/moq.hpp>

#include <cstring>
#include <memory>
#include <optional>
#include <vector>

#include "logger.h"

namespace {

// Opus codes natively at 48 kHz, so OBS resamples its mix to it.
constexpr uint32_t SAMPLE_RATE = 48000;

// The preset picks the Opus packet duration in microseconds: low latency halves
// the packetization delay at twice the packet rate. Quality matches balanced,
// since libopus already runs at full complexity and longer packets only add delay.
std::optional<uint32_t> ParsePreset(const char *name)
{
	if (strcmp(name, "low_latency") == 0)
		return 10000;
	if (strcmp(name, "balanced") == 0 || strcmp(name, "quality") == 0)
		return 20000;
	return std::nullopt;
}

// OBS settings carry kbps, as its other audio encoders do; 0 lets the codec pick.
std::optional<uint32_t> Bitrate(obs_data_t *settings)
{
	const long long kbps = obs_data_get_int(settings, "bitrate");
	if (kbps <= 0)
		return std::nullopt;
	return static_cast<uint32_t>(kbps * 1000);
}

struct MoQAudioEncoder {
	std::shared_ptr<moq::AudioEncoder> codec;
	uint32_t channels;
	size_t frame_size;
	// The OpusHead, which the output publishes as the track's init.
	std::vector<uint8_t> header;
	std::vector<float> pcm;
	// OBS reads the packet until the next encode call.
	std::vector<uint8_t> packet;
};

void *Create(obs_data_t *settings, obs_encoder_t *encoder)
{
	const char *preset_name = obs_data_get_string(settings, "preset");
	const auto frame_duration_us = ParsePreset(preset_name);
	if (!frame_duration_us) {
		LOG_ERROR("Unknown MoQ audio preset: %s", preset_name);
		return nullptr;
	}

	// moq-audio's Opus codes mono or stereo and remaps nothing, so OBS downmixes
	// anything wider to stereo before it reaches the encoder.
	audio_t *audio = obs_encoder_audio(encoder);
	const uint32_t channels = audio && audio_output_get_channels(audio) == 1 ? 1 : 2;

	moq::AudioEncoderOutput output{moq::AudioCodec::opus()};
	output.sample_rate = SAMPLE_RATE;
	output.channels = channels;
	output.bitrate = Bitrate(settings);
	output.frame_duration_us = *frame_duration_us;

	auto codec = moq::AudioEncoder::init(output);
	if (!codec) {
		LOG_ERROR("Failed to create MoQ Opus encoder: %s", codec.error().to_string().c_str());
		return nullptr;
	}
	auto frame_size = (*codec)->frame_size();
	auto init = (*codec)->track_init();
	if (!frame_size || !init) {
		LOG_ERROR("Failed to configure MoQ Opus encoder");
		return nullptr;
	}

	auto enc =
		new MoQAudioEncoder{*codec, channels, static_cast<size_t>(*frame_size), std::move(init->data), {}, {}};
	LOG_INFO("MoQ Opus encoder: %u channels, %zu samples per packet", channels, enc->frame_size);
	return enc;
}

void Destroy(void *data)
{
	// OBS has no drain call for audio encoders, so the codec's lookahead tail is
	// never asked for: the same cut every OBS audio encoder makes at stop.
	delete static_cast<MoQAudioEncoder *>(data);
}

bool Encode(void *data, struct encoder_frame *frame, struct encoder_packet *packet, bool *received_packet)
{
	auto enc = static_cast<MoQAudioEncoder *>(data);

	// OBS hands over exactly get_frame_size() samples per call, so anything else
	// means the two disagree on the layout.
	if (frame->frames != enc->frame_size) {
		LOG_ERROR("MoQ Opus encoder got %u samples, expected %zu", frame->frames, enc->frame_size);
		return false;
	}

	const auto *samples = reinterpret_cast<const float *>(frame->data[0]);
	enc->pcm.assign(samples, samples + enc->frame_size * enc->channels);

	auto encoded = enc->codec->encode(enc->pcm);
	if (!encoded) {
		LOG_ERROR("MoQ Opus encode failed: %s", encoded.error().to_string().c_str());
		return false;
	}
	enc->packet = std::move(encoded->data);

	// One packet per frame, stamped with its first input sample: the OpusHead's
	// pre-skip already tells the decoder to trim the codec's lookahead.
	packet->data = enc->packet.data();
	packet->size = enc->packet.size();
	packet->type = OBS_ENCODER_AUDIO;
	packet->pts = frame->pts;
	packet->dts = frame->pts;
	packet->timebase_num = 1;
	packet->timebase_den = SAMPLE_RATE;
	packet->keyframe = true;
	*received_packet = true;
	return true;
}

size_t GetFrameSize(void *data)
{
	return static_cast<MoQAudioEncoder *>(data)->frame_size;
}

void GetDefaults(obs_data_t *settings)
{
	obs_data_set_default_int(settings, "bitrate", 128);
	obs_data_set_default_string(settings, "preset", "balanced");
}

// Only the bitrate retunes live. The preset fixes the packet size OBS frames the
// input by, so a new one applies from the next start.
bool Update(void *data, obs_data_t *settings)
{
	auto enc = static_cast<MoQAudioEncoder *>(data);
	const auto bitrate = Bitrate(settings);
	if (!bitrate)
		return true;
	if (auto result = enc->codec->set_bitrate(*bitrate); !result) {
		LOG_ERROR("Failed to retune MoQ Opus bitrate: %s", result.error().to_string().c_str());
		return false;
	}
	return true;
}

bool GetExtraData(void *data, uint8_t **extra_data, size_t *size)
{
	auto enc = static_cast<MoQAudioEncoder *>(data);
	*extra_data = enc->header.data();
	*size = enc->header.size();
	return true;
}

void GetAudioInfo(void *data, struct audio_convert_info *info)
{
	auto enc = static_cast<MoQAudioEncoder *>(data);
	info->format = AUDIO_FORMAT_FLOAT;
	info->samples_per_sec = SAMPLE_RATE;
	info->speakers = enc->channels == 1 ? SPEAKERS_MONO : SPEAKERS_STEREO;
}

} // namespace

void register_moq_audio_encoder()
{
	struct obs_encoder_info info = {};
	info.id = "moq_opus";
	info.type = OBS_ENCODER_AUDIO;
	info.codec = "opus";
	// Internal until Use MoQ encoders can pick it together with the video adapter.
	info.caps = OBS_ENCODER_CAP_INTERNAL;
	info.get_name = [](void *) -> const char * {
		return "MoQ Opus";
	};
	info.create = Create;
	info.destroy = Destroy;
	info.encode = Encode;
	info.get_frame_size = GetFrameSize;
	info.get_defaults = GetDefaults;
	info.update = Update;
	info.get_extra_data = GetExtraData;
	info.get_audio_info = GetAudioInfo;

	obs_register_encoder(&info);
}
