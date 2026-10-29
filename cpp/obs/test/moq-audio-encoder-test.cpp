// SPDX-License-Identifier: GPL-2.0-or-later
//
// Drives the moq_opus encoder through the obs_encoder_info it registers, the way
// libobs calls it, against stubbed libobs and the real moq-ffi. Its packets are
// then published the way MoQOutput publishes an OBS encoder's packets and decoded
// by a real moq-audio subscriber, which checks both the audio and where each
// decoded sample is stamped.
//
// Run with `just obs test` (ThreadSanitizer) or `just obs ci` (plain). This is not
// part of the plugin build.
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <optional>
#include <string>
#include <variant>
#include <vector>

#include <obs-module.h>

#include "moq-audio-encoder.h"
#include "moq-test-relay.h"

// libobs keeps these opaque, so the test gives them the shape its stubs need.
struct obs_data {
	long long bitrate = 128;
	std::string preset = "balanced";
};

struct audio_output {
	size_t channels = 2;
};

struct obs_encoder {
	audio_output *audio = nullptr;
};

namespace {
obs_encoder_info g_info{};
int g_registered = 0;
} // namespace

extern "C" {

void blog(int, const char *, ...) {}

void obs_register_encoder_s(const struct obs_encoder_info *info, size_t size)
{
	if (size == sizeof(g_info))
		g_info = *info;
	g_registered++;
}

long long obs_data_get_int(obs_data_t *settings, const char *name)
{
	return strcmp(name, "bitrate") == 0 ? settings->bitrate : 0;
}

const char *obs_data_get_string(obs_data_t *settings, const char *name)
{
	return strcmp(name, "preset") == 0 ? settings->preset.c_str() : "";
}

void obs_data_set_default_int(obs_data_t *settings, const char *name, long long value)
{
	if (strcmp(name, "bitrate") == 0)
		settings->bitrate = value;
}

void obs_data_set_default_string(obs_data_t *settings, const char *name, const char *value)
{
	if (strcmp(name, "preset") == 0)
		settings->preset = value;
}

audio_t *obs_encoder_audio(const obs_encoder_t *encoder)
{
	return encoder->audio;
}

size_t audio_output_get_channels(const audio_t *audio)
{
	return audio->channels;
}

} // extern "C"

namespace {
int g_failures = 0;

#define CHECK(cond)                                                                 \
	do {                                                                        \
		if (!(cond)) {                                                      \
			fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); \
			g_failures++;                                               \
		}                                                                   \
	} while (0)

constexpr uint32_t RATE = 48000;
constexpr double TONE_HZ = 440.0;

// One encoder instance as libobs holds it: created against an audio mix, framed by
// the size and layout it asks for.
class Encoder {
public:
	explicit Encoder(obs_data settings = {}, size_t mix_channels = 2) : settings(std::move(settings))
	{
		audio.channels = mix_channels;
		owner.audio = &audio;
		data = g_info.create(&this->settings, &owner);
		if (!data)
			return;
		g_info.get_audio_info(data, &info);
		frame_size = g_info.get_frame_size(data);
	}
	~Encoder()
	{
		if (data)
			g_info.destroy(data);
	}
	Encoder(const Encoder &) = delete;
	Encoder &operator=(const Encoder &) = delete;

	size_t Channels() const { return get_audio_channels(info.speakers); }

	// Encodes one frame of interleaved float samples at `pts`, in samples.
	std::optional<encoder_packet> Encode(std::vector<float> &pcm, int64_t pts, uint32_t frames)
	{
		encoder_frame frame{};
		frame.data[0] = reinterpret_cast<uint8_t *>(pcm.data());
		frame.linesize[0] = static_cast<uint32_t>(pcm.size() * sizeof(float));
		frame.frames = frames;
		frame.pts = pts;
		encoder_packet packet{};
		bool received = false;
		if (!g_info.encode(data, &frame, &packet, &received) || !received)
			return std::nullopt;
		return packet;
	}

	std::vector<uint8_t> ExtraData()
	{
		uint8_t *extra = nullptr;
		size_t size = 0;
		if (!g_info.get_extra_data(data, &extra, &size))
			return {};
		return {extra, extra + size};
	}

	obs_data settings;
	audio_output audio;
	obs_encoder owner;
	void *data = nullptr;
	audio_convert_info info{};
	size_t frame_size = 0;
};

// Sample `index` of the test tone, the same on every channel.
float Tone(int64_t index, float amplitude = 0.5f)
{
	return amplitude * static_cast<float>(std::sin(2.0 * M_PI * TONE_HZ * static_cast<double>(index) / RATE));
}

std::vector<float> ToneFrame(int64_t start, size_t frames, size_t channels, float amplitude = 0.5f)
{
	std::vector<float> pcm;
	pcm.reserve(frames * channels);
	for (size_t i = 0; i < frames; i++)
		pcm.insert(pcm.end(), channels, Tone(start + static_cast<int64_t>(i), amplitude));
	return pcm;
}

// Deterministic white noise, which codes at whatever rate the encoder is given.
std::vector<float> NoiseFrame(uint32_t &state, size_t samples)
{
	std::vector<float> pcm(samples);
	for (auto &sample : pcm) {
		state = state * 1664525u + 1013904223u;
		sample = static_cast<float>(state >> 8) / static_cast<float>(1u << 24) - 0.5f;
	}
	return pcm;
}

// MoQOutput's conversion of an OBS packet's timestamp to the track's microseconds.
uint64_t PacketMicros(const encoder_packet &packet)
{
	const int64_t pts = packet.pts + packet.timebase_den / packet.timebase_num;
	return static_cast<uint64_t>(pts) * 1000000ULL * packet.timebase_num / packet.timebase_den;
}

// Publishes `count` tone packets from an encoder with `settings` the way MoQOutput
// does, then decodes them with a moq-audio subscriber and checks that each decoded
// sample is the tone sample its timestamp names.
void CheckSubscriberDecodes(const char *preset, size_t channels)
{
	obs_data settings;
	settings.preset = preset;
	Encoder encoder(settings, channels);
	CHECK(encoder.data != nullptr);
	if (!encoder.data)
		return;

	auto origin = moq::OriginProducer::init(moq::OriginConfig{});
	auto publication = TestOk(origin->create_broadcast("room"), "create_broadcast");
	auto broadcast = publication->broadcast();
	moq::AnnounceRoute route;
	route.epoch = moq::mint_epoch();
	TestOk(publication->announce(route), "announce");

	moq::AudioInit init{};
	init.format = moq::AudioFormat::kOpus;
	init.data = encoder.ExtraData();
	auto track = TestOk(moq::MediaTrackProducer::audio(broadcast, moq::MediaTarget::kNamed{}, init), "audio track");

	auto announced = TestOk(origin->consume()->announced_broadcast("room"), "announced_broadcast");
	auto consumer = TestOk(announced->available().get(), "available");
	auto catalogs = TestOk(moq::MediaCatalogConsumer::subscribe(consumer).get(), "catalog consumer");
	std::optional<moq::Catalog> catalog;
	while (!catalog || catalog->audio.empty()) {
		auto next = catalogs->next();
		if (next.wait_for(std::chrono::seconds(10)) != std::future_status::ready)
			break;
		catalog = TestOk(next.get(), "catalog next");
		if (!catalog)
			break;
	}
	CHECK(catalog && catalog->audio.size() == 1);
	if (!catalog || catalog->audio.empty())
		return;
	const auto &[name, rendition] = *catalog->audio.begin();
	CHECK(rendition.codec == "opus");
	CHECK(rendition.channel_count == channels);

	moq::AudioDecoderOutput output{moq::AudioSampleFormat::kF32};
	output.channels = static_cast<uint32_t>(channels);
	// The packets are written faster than real time, so let none be skipped as stale.
	output.max_delay = std::chrono::seconds(10);
	auto audio = TestOk(moq::AudioConsumer::subscribe(consumer, name, rendition, output).get(), "audio consumer");

	const int count = 50;
	const int64_t start = 3 * RATE;
	// Where input sample `index` lands on the track: MoQOutput's one second offset on top.
	auto micros = [&](int64_t index) {
		return static_cast<uint64_t>((start + RATE + index) * 1000000LL / RATE);
	};
	for (int i = 0; i < count; i++) {
		const int64_t pts = start + i * static_cast<int64_t>(encoder.frame_size);
		auto pcm = ToneFrame(pts - start, encoder.frame_size, channels);
		auto packet = encoder.Encode(pcm, pts, static_cast<uint32_t>(encoder.frame_size));
		CHECK(packet.has_value());
		if (!packet)
			return;
		CHECK(packet->pts == pts && packet->dts == pts);
		const uint64_t us = PacketMicros(*packet);
		TestOk(track->write_frame({std::vector<uint8_t>(packet->data, packet->data + packet->size), us}),
		       "write_frame");
		TestOk(track->cut(), "cut");
	}
	TestOk(track->finish(), "finish");

	// Every decoded sample, compared against the input sample its frame's timestamp
	// names. A decoder trimming the wrong pre-skip, or a packet stamped
	// with anything but its first input sample, shifts the phase and fails this.
	size_t decoded = 0;
	double error = 0, energy = 0;
	std::optional<uint64_t> first_decoded_us;
	for (;;) {
		auto next = audio->next();
		if (next.wait_for(std::chrono::seconds(10)) != std::future_status::ready) {
			CHECK(!"decoded audio stalled");
			break;
		}
		auto read = TestOk(next.get(), "audio next");
		if (!read)
			break;
		// Raw packets carry no encoder end, and nothing was lost.
		const auto *variant = std::get_if<moq::AudioRead::kFrame>(&read->get_variant());
		CHECK(variant != nullptr);
		if (!variant)
			break;
		const auto *frame = &variant->frame;
		if (!first_decoded_us)
			first_decoded_us = frame->timestamp_us;
		const auto *samples = reinterpret_cast<const float *>(frame->data.data());
		const size_t frames = frame->data.size() / sizeof(float) / channels;
		const int64_t offset = static_cast<int64_t>(frame->timestamp_us * RATE / 1000000ULL) - (start + RATE);
		for (size_t i = 0; i < frames; i++) {
			const double expected = Tone(offset + static_cast<int64_t>(i));
			for (size_t c = 0; c < channels; c++) {
				const double got = samples[i * channels + c];
				error += (got - expected) * (got - expected);
				energy += expected * expected;
			}
		}
		decoded += frames;
	}

	// OBS never drains an audio encoder, so the decoder trims the 312-sample pre-skip
	// and nothing pads the tail: every input sample but that lookahead comes back.
	CHECK(first_decoded_us == micros(0));
	CHECK(decoded == count * encoder.frame_size - 312);
	CHECK(energy > 0 && error / energy < 0.05);
	printf("  %s, %zu channels: %zu samples, error %.4f\n", preset, channels, decoded,
	       energy > 0 ? error / energy : -1.0);
}
} // namespace

int main()
{
	register_moq_audio_encoder();
	CHECK(g_registered == 1);
	CHECK(strcmp(g_info.id, "moq_opus") == 0);
	CHECK(strcmp(g_info.codec, "opus") == 0);
	CHECK(g_info.type == OBS_ENCODER_AUDIO);
	// Hidden from OBS's encoder lists until Use MoQ encoders offers it.
	CHECK(g_info.caps & OBS_ENCODER_CAP_INTERNAL);
	printf("registers an internal Opus encoder: ok\n");

	// OBS converts its mix to what the encoder asks for: 48 kHz interleaved float,
	// mono kept mono and anything wider downmixed to stereo.
	for (auto [mix, speakers] :
	     {std::pair<size_t, speaker_layout>{1, SPEAKERS_MONO}, {2, SPEAKERS_STEREO}, {6, SPEAKERS_STEREO}}) {
		Encoder encoder({}, mix);
		CHECK(encoder.data != nullptr);
		CHECK(encoder.info.format == AUDIO_FORMAT_FLOAT);
		CHECK(encoder.info.samples_per_sec == RATE);
		CHECK(encoder.info.speakers == speakers);
		auto head = encoder.ExtraData();
		CHECK(head.size() >= 19 && memcmp(head.data(), "OpusHead", 8) == 0);
		CHECK(head.size() >= 19 && head[9] == encoder.Channels());
	}
	printf("asks OBS for a layout Opus codes: ok\n");

	// 20 ms by default and for quality, 10 ms for low latency. A preset changed on a
	// live encoder waits for the next one, since OBS frames its input by this size.
	for (auto [preset, frames] :
	     {std::pair<const char *, size_t>{"balanced", 960}, {"quality", 960}, {"low_latency", 480}}) {
		obs_data settings;
		settings.preset = preset;
		Encoder encoder(settings);
		CHECK(encoder.frame_size == frames);
	}
	{
		Encoder encoder;
		obs_data settings;
		settings.preset = "low_latency";
		CHECK(g_info.update(encoder.data, &settings));
		CHECK(g_info.get_frame_size(encoder.data) == 960);
		Encoder next(settings);
		CHECK(next.frame_size == 480);
	}
	printf("packetizes by preset, changing size only at a new encoder: ok\n");

	{
		obs_data settings;
		settings.preset = "fastest";
		Encoder encoder(settings);
		CHECK(encoder.data == nullptr);
	}
	printf("refuses an unknown preset: ok\n");

	// OBS always hands over a whole frame; a short one means the layouts disagree.
	{
		Encoder encoder;
		auto pcm = ToneFrame(0, encoder.frame_size / 2, 2);
		CHECK(!encoder.Encode(pcm, 0, static_cast<uint32_t>(encoder.frame_size / 2)));
	}
	printf("refuses a partial frame: ok\n");

	// Silence codes to almost nothing, and samples far past full scale still encode.
	{
		Encoder encoder;
		std::vector<float> silence(encoder.frame_size * 2, 0.0f);
		for (int i = 0; i < 5; i++) {
			auto packet = encoder.Encode(silence, i * 960, 960);
			CHECK(packet && packet->size > 0 && packet->size < 16);
		}
		for (int i = 5; i < 10; i++) {
			auto pcm = ToneFrame(i * 960, 960, 2, 4.0f);
			auto packet = encoder.Encode(pcm, i * 960, 960);
			CHECK(packet && packet->size > 0);
			CHECK(packet && packet->timebase_num == 1 && packet->timebase_den == RATE);
			CHECK(packet && packet->type == OBS_ENCODER_AUDIO);
		}
	}
	printf("encodes silence and saturation: ok\n");

	// The bitrate retunes the live encoder.
	{
		Encoder encoder;
		uint32_t state = 1;
		auto average = [&](int64_t from) {
			size_t bytes = 0;
			for (int i = 0; i < 50; i++) {
				auto pcm = NoiseFrame(state, 960 * 2);
				auto packet = encoder.Encode(pcm, (from + i) * 960, 960);
				CHECK(packet.has_value());
				bytes += packet ? packet->size : 0;
			}
			return bytes / 50;
		};
		const size_t high = average(0);
		obs_data settings;
		settings.bitrate = 16;
		CHECK(g_info.update(encoder.data, &settings));
		const size_t low = average(50);
		// 128 kbps is 320 bytes per 20 ms packet, 16 kbps is 40.
		CHECK(high > 250);
		CHECK(low < 60);
		settings.bitrate = 1;
		CHECK(!g_info.update(encoder.data, &settings));
	}
	printf("retunes the bitrate live: ok\n");

	for (auto preset : {"balanced", "low_latency"}) {
		for (size_t channels : {1, 2})
			CheckSubscriberDecodes(preset, channels);
	}
	printf("a subscriber decodes the tone where its timestamps say: ok\n");

	if (g_failures) {
		fprintf(stderr, "%d check(s) failed\n", g_failures);
		return 1;
	}
	return 0;
}
