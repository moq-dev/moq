#pragma once
#include <string>
#include <obs-module.h>

struct MoQService {
    // TODO: Define needed params to connect to a relay
    std::string server;
    std::string path;

    MoQService(obs_data_t *settings, obs_service_t *service);

    void Update(obs_data_t *settings);
    static obs_properties_t *Properties();
    static void ApplyEncoderSettings(obs_data_t *video_settings, obs_data_t *audio_settings);
    bool CanTryToConnect();
    const char *GetConnectInfo(enum obs_service_connect_info type);
};

void register_moq_service();
