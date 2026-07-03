// SPDX-License-Identifier: GPL-2.0-or-later
#include "moq-service.h"

// TODO: Define supported codecs.
const char *audio_codecs[] = {"aac", "opus", nullptr};
const char *video_codecs[] = {"h264", "hevc", "av1", nullptr};

MoQService::MoQService(obs_data_t *settings, obs_service_t *) : server(), path()
{
	Update(settings);
}

void MoQService::Update(obs_data_t *settings)
{
	server = obs_data_get_string(settings, "server");
	path = obs_data_get_string(settings, "key");
}

obs_properties_t *MoQService::Properties()
{
	obs_properties_t *ppts = obs_properties_create();

	// Adds properties to be modified by the UI.
	// obs_property_t *obs_properties_add_text(obs_properties_t *props, const char *name, const char *desc, enum obs_text_type type)
	obs_properties_add_text(ppts, "server", "URL", OBS_TEXT_DEFAULT);
	obs_properties_add_text(ppts, "key", "Path (optional)", OBS_TEXT_DEFAULT);

	return ppts;
}

void MoQService::ApplyEncoderSettings(obs_data_t *video_settings, obs_data_t *audio_settings)
{
	/*
     This function is called to apply custom encoder settings specific to this service.
     For example, if a service requires a specific keyframe interval, or has a bitrate limit,
     the settings for the video and audio encoders can be optionally modified
     if the front-end optionally calls.
     */

	// Example:
	if (video_settings) {
		obs_data_set_int(video_settings, "bf", 0);
		obs_data_set_bool(video_settings, "repeat_headers", true);
	}

	if (audio_settings) {
		obs_data_set_int(audio_settings, "bf", 0);
	}
}

const char *MoQService::GetConnectInfo(enum obs_service_connect_info type)
{
	switch (type) {
	case OBS_SERVICE_CONNECT_INFO_SERVER_URL:
		return server.c_str();
	case OBS_SERVICE_CONNECT_INFO_STREAM_KEY:
		return path.c_str();
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
