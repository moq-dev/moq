#pragma once
#include <obs-module.h>

#include <chrono>
#include <map>
#include <string>
#include "logger.h"

class MoQOutput
{
      public:
    MoQOutput(obs_data_t *settings, obs_output_t *output);
    ~MoQOutput();

    bool Start();
    void Stop(bool signal = true);
    void Data(struct encoder_packet *packet);

    inline size_t GetTotalBytes()
    {
        return total_bytes_sent;
    }

    inline int GetConnectTime()
    {
        return connect_time_ms;
    }

      private:
    void VideoInit(obs_encoder_t *encoder);
    void VideoData(struct encoder_packet *packet);
    void AudioInit(obs_encoder_t *encoder);
    void AudioData(struct encoder_packet *packet);

    obs_output_t *output;

    std::string server_url;
    std::string path;

    size_t total_bytes_sent;
    int connect_time_ms;
    std::chrono::steady_clock::time_point connect_start;

    int origin;
    int session;
    int broadcast;
    std::map<obs_encoder_t *, int> video_tracks;
    std::map<obs_encoder_t *, int> audio_tracks;
};

void register_moq_output();
