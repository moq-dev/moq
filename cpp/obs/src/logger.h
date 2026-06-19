#include <iostream>

// Logging macros - use MOQ_ prefix to avoid conflicts with OBS log level constants
#define MOQ_LOG(level, format, ...) blog(level, "[obs-moq] " format, ##__VA_ARGS__)
#define LOG_DEBUG(format, ...) MOQ_LOG(400, format, ##__VA_ARGS__)
#define LOG_INFO(format, ...) MOQ_LOG(300, format, ##__VA_ARGS__)
#define LOG_WARNING(format, ...) MOQ_LOG(200, format, ##__VA_ARGS__)
#define LOG_ERROR(format, ...) MOQ_LOG(100, format, ##__VA_ARGS__)
