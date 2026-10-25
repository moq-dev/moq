/* The moq C package: the C API generated from moq-ffi, plus the shutdown it cannot generate. */
#ifndef MOQ_MOQ_H
#define MOQ_MOQ_H

#include <moq/ffi/moq.h>
#include <moq/version.h>

/* This package's product token. Every config is generated, so nothing sets it for you: put it in
 * moq_client_config.implementation and moq_server_config.implementation, after your own tokens. */
#define MOQ_IMPLEMENTATION "moq-c/" MOQ_C_VERSION

#ifdef __cplusplus
extern "C" {
#endif

/**
 * Stops the moq runtime workers, then the dispatcher, before the process or module goes away.
 * Pending calls may still call back while it runs, typically with MOQ_ERROR_CANCELLED; none
 * call back after it returns, so free their user_data then. Call it once, from a thread that
 * is not running a callback.
 */
void moq_shutdown(void);

#ifdef __cplusplus
}
#endif

#endif
