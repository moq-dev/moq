/* The moq C package: the C API generated from moq-ffi, plus the shutdown it cannot generate. */
#ifndef MOQ_MOQ_H
#define MOQ_MOQ_H

#include <moq/ffi/moq.h>

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
