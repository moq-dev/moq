/*
 * Copyright (c) 2017 Intel Corporation. All Rights Reserved.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a
 * copy of this software and associated documentation files (the
 * "Software"), to deal in the Software without restriction, including
 * without limitation the rights to use, copy, modify, merge, publish,
 * distribute, sub license, and/or sell copies of the Software, and to
 * permit persons to whom the Software is furnished to do so, subject to
 * the following conditions:
 *
 * The above copyright notice and this permission notice (including the
 * next paragraph) shall be included in all copies or substantial portions
 * of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
 * OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
 * MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NON-INFRINGEMENT.
 * IN NO EVENT SHALL PRECISION INSIGHT AND/OR ITS SUPPLIERS BE LIABLE FOR
 * ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT,
 * TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
 * SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
 */

#ifndef VA_INTERNAL_H
#define VA_INTERNAL_H

#ifdef __cplusplus
extern "C" {
#endif

#define CTX(dpy) (((VADisplayContextP)dpy)->pDriverContext)
#define CHECK_DISPLAY(dpy) if (!vaDisplayIsValid(dpy)) { return VA_STATUS_ERROR_INVALID_DISPLAY; }

DLL_HIDDEN
void va_errorMessage(VADisplay dpy, const char *msg, ...);

DLL_HIDDEN
void va_infoMessage(VADisplay dpy, const char *msg, ...);

DLL_HIDDEN
int  va_parseConfig(char *env, char *env_value);

VADisplayContextP va_newDisplayContext(void);

VADriverContextP va_newDriverContext(VADisplayContextP dctx);

#ifdef __cplusplus
}
#endif

#endif /* VA_INTERNAL_H */
