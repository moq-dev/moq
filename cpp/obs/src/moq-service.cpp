// SPDX-License-Identifier: GPL-2.0-or-later
#include "moq-service.h"
#include "moq-encoder-latency.h"
#include "moq-settings.h"
#include "logger.h"

#ifdef MOQ_FRONTEND_API
#include <obs-frontend-api.h>
#include <util/config-file.h>
#endif
#include <cstring>

// TODO: Define supported codecs.
const char *audio_codecs[] = {"aac", "opus", nullptr};
const char *video_codecs[] = {"h264", "hevc", "av1", nullptr};

MoQService::MoQService(obs_data_t *settings, obs_service_t *) : server(), path(), token()
{
	Update(settings);
}

void MoQService::Update(obs_data_t *settings)
{
	server = obs_data_get_string(settings, "server");
	path = obs_data_get_string(settings, "key");
	token = obs_data_get_string(settings, "bearer_token");
}

obs_properties_t *MoQService::Properties()
{
	obs_properties_t *ppts = obs_properties_create();

	// Adds properties to be modified by the UI.
	// obs_property_t *obs_properties_add_text(obs_properties_t *props, const char *name, const char *desc, enum obs_text_type type)
	obs_properties_add_text(ppts, "server", "URL", OBS_TEXT_DEFAULT);
	obs_properties_add_text(ppts, "key", "Path (optional)", OBS_TEXT_DEFAULT);

	MoQSettings::AddProperties(ppts);

	return ppts;
}

void MoQService::Defaults(obs_data_t *settings)
{
	MoQSettings::Defaults(settings);
}

// The stream encoder the profile configures. OBS never says which encoder the settings it
// hands a service are for; its stream outputs, the only callers that encode with them on a
// MoQ service, build them for this one.
static std::string ProfileStreamEncoderId()
{
#ifndef MOQ_FRONTEND_API
	return {};
#else
	config_t *config = obs_frontend_get_profile_config();
	if (!config)
		return {};
	const char *mode = config_get_string(config, "Output", "Mode");
	if (mode && strcmp(mode, "Advanced") == 0) {
		const char *id = config_get_string(config, "AdvOut", "Encoder");
		return id ? id : "";
	}
	return SimpleVideoEncoderId(config_get_string(config, "SimpleOutput", "StreamEncoder"));
#endif
}

void MoQService::ApplyEncoderSettings(obs_data_t *video_settings, obs_data_t *)
{
	if (!video_settings)
		return;
	obs_data_set_bool(video_settings, "repeat_headers", true);
	// A MoQ group starts at a keyframe, so a viewer joins only as fast as the
	// keyframe interval, and B-frames or lookahead hold every frame back.
	const std::string id = ProfileStreamEncoderId();
	LOG_INFO("Service encoder %s: %s", id.c_str(), ApplyEncoderLatency(id, video_settings));
}

const char *MoQService::GetConnectInfo(enum obs_service_connect_info type)
{
	switch (type) {
	case OBS_SERVICE_CONNECT_INFO_SERVER_URL:
		return server.c_str();
	case OBS_SERVICE_CONNECT_INFO_STREAM_KEY:
		return path.c_str();
	case OBS_SERVICE_CONNECT_INFO_BEARER_TOKEN:
		return token.empty() ? nullptr : token.c_str();
	default:
		return nullptr;
	}
}

bool MoQService::CanTryToConnect()
{
	return !server.empty();
}

void register_moq_service()
{
	struct obs_service_info info = {};

	info.id = "moq_service";
	info.get_name = [](void *) -> const char * {
		return "MoQ (Debug)";
	};
	info.create = [](obs_data_t *settings, obs_service_t *service) -> void * {
		return new MoQService(settings, service);
	};
	info.destroy = [](void *priv_data) {
		delete static_cast<MoQService *>(priv_data);
	};
	info.update = [](void *priv_data, obs_data_t *settings) {
		static_cast<MoQService *>(priv_data)->Update(settings);
	};
	info.get_properties = [](void *) -> obs_properties_t * {
		return MoQService::Properties();
	};
	info.get_defaults = [](obs_data_t *settings) {
		MoQService::Defaults(settings);
	};
	info.get_protocol = [](void *) -> const char * {
		return "MoQ";
	};
	info.get_url = [](void *priv_data) -> const char * {
		return static_cast<MoQService *>(priv_data)->server.c_str();
	};
	info.get_output_type = [](void *) -> const char * {
		return "moq_output";
	};
	info.apply_encoder_settings = [](void *, obs_data_t *video_settings, obs_data_t *audio_settings) {
		MoQService::ApplyEncoderSettings(video_settings, audio_settings);
	};
	info.get_supported_video_codecs = [](void *) -> const char ** {
		return video_codecs;
	};
	info.get_supported_audio_codecs = [](void *) -> const char ** {
		return audio_codecs;
	};
	info.can_try_to_connect = [](void *priv_data) -> bool {
		return static_cast<MoQService *>(priv_data)->CanTryToConnect();
	};
	info.get_connect_info = [](void *priv_data, uint32_t type) -> const char * {
		return static_cast<MoQService *>(priv_data)->GetConnectInfo((enum obs_service_connect_info)type);
	};
	obs_register_service(&info);
}
