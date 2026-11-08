/*
 * Copyright (c) 2009 Intel Corporation. All Rights Reserved.
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

#ifndef VA_TRACE_H
#define VA_TRACE_H

#ifdef __cplusplus
extern "C" {
#endif

extern int va_trace_flag;

#define VA_TRACE_FLAG_LOG             0x1
#define VA_TRACE_FLAG_BUFDATA         0x2
#define VA_TRACE_FLAG_CODEDBUF        0x4
#define VA_TRACE_FLAG_SURFACE_DECODE  0x8
#define VA_TRACE_FLAG_SURFACE_ENCODE  0x10
#define VA_TRACE_FLAG_SURFACE_JPEG    0x20
#define VA_TRACE_FLAG_SURFACE_VPPOUT  0x80

#define VA_TRACE_FLAG_SURFACE         (VA_TRACE_FLAG_SURFACE_DECODE | \
                                       VA_TRACE_FLAG_SURFACE_ENCODE | \
                                       VA_TRACE_FLAG_SURFACE_JPEG)
#define VA_TRACE_FLAG_FTRACE          0x40
#define VA_TRACE_FLAG_FTRACE_BUFDATA  (VA_TRACE_FLAG_FTRACE | \
                                       VA_TRACE_FLAG_BUFDATA)


#define VA_TRACE_LOG(trace_func,...)            \
    if (va_trace_flag & VA_TRACE_FLAG_LOG) {    \
        trace_func(__VA_ARGS__);                \
    }
#define VA_TRACE_ALL(trace_func,...)            \
    if (va_trace_flag) {                        \
        trace_func(__VA_ARGS__);                \
    }
#define VA_TRACE_RET(dpy,ret)                   \
    if (va_trace_flag){                         \
        va_TraceStatus(dpy, __func__, ret);     \
    }

/** \brief event id definition
 * identifier of trace event, coresponding the task value in the manifest, located in libva-util/tracetool
 * the trace tool will translate this id to event name, also the trace data carried along.
 * Note: make sure event id definition backward compatible */
enum {
    INVALIDE_ID = 0,
    CREATE_CONFIG = 1,
    DESTROY_CONFIG,
    CREATE_CONTEXT,
    DESTROY_CONTEXT,
    CREATE_BUFFER,
    DESTROY_BUFFER,
    CREATE_SURFACE,
    DESTROY_SURFACE,
    BEGIN_PICTURE,
    RENDER_PICTURE,
    END_PICTURE,
    BUFFER_DATA,
    SYNC_SURFACE,
    SYNC_SURFACE2,
    QUERY_SURFACE_ATTR,
};

/** \brief event opcode definition
 * identifier of trace event operation, coresponding the opcode value in the manifest.
 * 4 predefined opcode */
#define TRACE_INFO  0
#define TRACE_BEGIN 1
#define TRACE_END   2
#define TRACE_DATA  3

/** \brief event data structure
 * structure list to pass event data without copy, each entry contain the event data address and size.
 * va_TraceEvent will write these raw data into ftrace entry */
typedef struct va_event_data {
    void *buf;
    unsigned int size;
} VAEventData;


/** \brief VA_TRACE
 * trace interface to send out trace event with empty event data. */
#define VA_TRACE(dpy,id,op) do {                        \
        if (va_trace_flag & VA_TRACE_FLAG_FTRACE) {     \
            va_TraceEvent(dpy, id, op, 0, NULL);        \
        }                                               \
    } while (0)
/** \brief VA_TRACE_V
 * trace interface to send out trace event with 1 data element from variable. the variable data type could be 8/16/32/64 bitsize */
#define VA_TRACE_V(dpy,id,op,v) do {                    \
        if (va_trace_flag & VA_TRACE_FLAG_FTRACE) {     \
            VAEventData desc[1] = {{&v, sizeof(v)}};    \
            va_TraceEvent(dpy, id, op, 1, desc);        \
        }                                               \
    } while (0)
/** \brief VA_TRACE_PV
 * trace interface to send out trace event with 2 data element, from pointer and variable. their data size could be 8/16/32/64 bitsize */
#define VA_TRACE_PV(dpy,id,op,p,v) do {                 \
        if (va_trace_flag & VA_TRACE_FLAG_FTRACE) {     \
            VAEventData desc[2] = {{p, sizeof(*p)},     \
                                   {&v, sizeof(v)}};    \
            va_TraceEvent(dpy, id, op, 2, desc);        \
        }                                               \
    } while (0)
/** \brief VA_TRACE_VV
 * trace interface to send out trace event with 2 data element, both from variable. their data size could be 8/16/32/64 bitsize */
#define VA_TRACE_VV(dpy,id,op,v1,v2) do {               \
        if (va_trace_flag & VA_TRACE_FLAG_FTRACE) {     \
            VAEventData desc[2] = {{&v1, sizeof(v1)},   \
                                   {&v2, sizeof(v2)}};  \
            va_TraceEvent(dpy, id, op, 2, desc);        \
        }                                               \
    } while (0)
/** \brief VA_TRACE_VVVV
 * trace interface to send out trace event with 4 data element, all from variable. their data size could be 8/16/32/64 bitsize */
#define VA_TRACE_VVVV(dpy,id,op,v1,v2,v3,v4) do {       \
        if (va_trace_flag & VA_TRACE_FLAG_FTRACE) {     \
            VAEventData desc[4] = { {&v1, sizeof(v1)},  \
                                    {&v2, sizeof(v2)},  \
                                    {&v3, sizeof(v3)},  \
                                    {&v4, sizeof(v4)}}; \
            va_TraceEvent(dpy, id, op, 4, desc);        \
        }                                               \
    } while (0)
/** \brief VA_TRACE_VA
 * trace interface to send out trace event with a dynamic length array data element, array length from variable.
 * high 16bits of array length is used to set bitssize of array element. */
#define VA_TRACE_VA(dpy,id,op,n,a) do {                  \
        if (va_trace_flag & VA_TRACE_FLAG_FTRACE) {      \
            int num = n | sizeof(*a) << 16;              \
            VAEventData desc[2] = {{&num, sizeof(num)},  \
                                   {a, n * sizeof(*a)}}; \
            va_TraceEvent(dpy, id, op, 2, desc);         \
        }                                                \
    } while (0)
/** \brief VA_TRACE_PA
 * trace interface to send out trace event with a dynamic length array data element, array length from pointer. need check null before set.
 * high 16bits of array length is used to set bitssize of array element. */
#define VA_TRACE_PA(dpy,id,op,pn,a) do {                 \
        if (va_trace_flag & VA_TRACE_FLAG_FTRACE) {      \
            int num = sizeof(*a) << 16;                  \
            VAEventData desc[2] = {{&num, sizeof(num)},  \
                                   {a, 0}};              \
            if (pn) {                                    \
                num |= *pn;                              \
                desc[1].size = (*pn) * sizeof(*a);       \
            }                                            \
            va_TraceEvent(dpy, id, op, 2, desc);         \
        }                                                \
    } while (0)
/** \brief VA_TRACE_VVA
 * trace interface to send out trace event with 1 data element and a dynamic length array data element, array length from variable.
 * high 16bits of array length is used to set bitssize of array element. */
#define VA_TRACE_VVA(dpy,id,op,v,n,a) do {               \
        if (va_trace_flag & VA_TRACE_FLAG_FTRACE) {      \
            int num = n | (sizeof(*a) << 16);            \
            VAEventData desc[3] = {{&v, sizeof(v)},      \
                                   {&num, sizeof(num)},  \
                                   {a, n*sizeof(*a)}};   \
            va_TraceEvent(dpy, id, op, 3, desc);         \
        }                                                \
    } while (0)
/** \brief VA_TRACE_VVVA
 * trace interface to send out trace event with 2 data element and a dynamic length array data element, array length from variable.
 * high 16bits of array length is used to set bitssize of array element. */
#define VA_TRACE_VVVA(dpy,id,op,v1,v2,n,a) do {          \
        if (va_trace_flag & VA_TRACE_FLAG_FTRACE) {      \
            int num = n | (sizeof(*a) << 16);            \
            VAEventData desc[4] = {{&v1, sizeof(v1)},    \
                                   {&v2, sizeof(v2)},    \
                                   {&num, sizeof(num)},  \
                                   {a, n*sizeof(*a)}};   \
            va_TraceEvent(dpy, id, op, 4, desc);         \
        }                                                \
    } while (0)
/** \brief VA_TRACE_VVVVA
 * trace interface to send out trace event with 3 data element and a dynamic length array data element, array length from variable.
 * high 16bits of array length is used to set bitssize of array element. */
#define VA_TRACE_VVVVA(dpy,id,op,v1,v2,v3,n,a) do {      \
        if (va_trace_flag & VA_TRACE_FLAG_FTRACE) {      \
            int num = n | (sizeof(*a) << 16);            \
            VAEventData desc[5] = {{&v1, sizeof(v1)},    \
                                   {&v2, sizeof(v2)},    \
                                   {&v3, sizeof(v3)},    \
                                   {&num, sizeof(num)},  \
                                   {a, n*sizeof(*a)}};   \
            va_TraceEvent(dpy, id, op, 5, desc);         \
        }                                                \
    } while (0)
/** \brief VA_TRACE_VVVVVA
 * trace interface to send out trace event with 4 data elsement and a dynamic length array data element, array length from variable.
 * high 16bits of array length is used to set bitssize of array element. */
#define VA_TRACE_VVVVVA(dpy,id,op,v1,v2,v3,v4,n,a) do {  \
        if (va_trace_flag & VA_TRACE_FLAG_FTRACE) {      \
            int num = n | (sizeof(*a) << 16);            \
            VAEventData desc[6] = {{&v1, sizeof(v1)},    \
                                   {&v2, sizeof(v2)},    \
                                   {&v3, sizeof(v3)},    \
                                   {&v4, sizeof(v4)},    \
                                   {&num, sizeof(num)},  \
                                   {a, n*sizeof(*a)}};   \
            va_TraceEvent(dpy, id, op, 6, desc);         \
        }                                                \
    } while (0)

/* add macro interface to support new data type combination */


/** \brief VA_TRACE_BUFFERS
 * trace interface to dump all data in va buffer ids into trace.
 * libva-utils will parse buffer into fields according to buffer type */
#define VA_TRACE_BUFFERS(dpy,ctx,num,buffers) do {                                            \
        if ((va_trace_flag & VA_TRACE_FLAG_FTRACE_BUFDATA) == VA_TRACE_FLAG_FTRACE_BUFDATA) { \
            va_TraceEventBuffers(dpy, ctx, num, buffers);                                     \
        }                                                                                     \
    } while (0)

DLL_HIDDEN
void va_TraceInit(VADisplay dpy);
DLL_HIDDEN
void va_TraceEnd(VADisplay dpy);

DLL_HIDDEN
void va_TraceInitialize(
    VADisplay dpy,
    int *major_version,  /* out */
    int *minor_version   /* out */
);

DLL_HIDDEN
void va_TraceTerminate(
    VADisplay dpy
);

DLL_HIDDEN
void va_TraceCreateConfig(
    VADisplay dpy,
    VAProfile profile,
    VAEntrypoint entrypoint,
    VAConfigAttrib *attrib_list,
    int num_attribs,
    VAConfigID *config_id /* out */
);

DLL_HIDDEN
void va_TraceDestroyConfig(
    VADisplay dpy,
    VAConfigID config_id
);

DLL_HIDDEN
void va_TraceCreateSurfaces(
    VADisplay dpy,
    int width,
    int height,
    int format,
    int num_surfaces,
    VASurfaceID *surfaces,  /* out */
    VASurfaceAttrib    *attrib_list,
    unsigned int        num_attribs
);

DLL_HIDDEN
void va_TraceDestroySurfaces(
    VADisplay dpy,
    VASurfaceID *surface_list,
    int num_surfaces
);

DLL_HIDDEN
void va_TraceCreateContext(
    VADisplay dpy,
    VAConfigID config_id,
    int picture_width,
    int picture_height,
    int flag,
    VASurfaceID *render_targets,
    int num_render_targets,
    VAContextID *context        /* out */
);

DLL_HIDDEN
void va_TraceDestroyContext(
    VADisplay dpy,
    VAContextID context
);

DLL_HIDDEN
void va_TraceCreateMFContext(
    VADisplay dpy,
    VAContextID *mf_context /* out */
);

DLL_HIDDEN
void va_TraceMFAddContext(
    VADisplay dpy,
    VAMFContextID mf_context,
    VAContextID context
);

DLL_HIDDEN
void va_TraceMFReleaseContext(
    VADisplay dpy,
    VAMFContextID mf_context,
    VAContextID context
);

DLL_HIDDEN
void va_TraceMFSubmit(
    VADisplay dpy,
    VAMFContextID mf_context,
    VAContextID *contexts,
    int num_contexts
);

DLL_HIDDEN
void va_TraceCreateBuffer(
    VADisplay dpy,
    VAContextID context,    /* in */
    VABufferType type,      /* in */
    unsigned int size,      /* in */
    unsigned int num_elements,  /* in */
    void *data,         /* in */
    VABufferID *buf_id      /* out */
);

DLL_HIDDEN
void va_TraceDestroyBuffer(
    VADisplay dpy,
    VABufferID buf_id    /* in */
);

DLL_HIDDEN
void va_TraceMapBuffer(
    VADisplay dpy,
    VABufferID buf_id,  /* in */
    void **pbuf,     /* out */
    uint32_t flags  /* in */
);


DLL_HIDDEN
void va_TraceBeginPicture(
    VADisplay dpy,
    VAContextID context,
    VASurfaceID render_target
);

DLL_HIDDEN
void va_TraceRenderPicture(
    VADisplay dpy,
    VAContextID context,
    VABufferID *buffers,
    int num_buffers
);

DLL_HIDDEN
void va_TraceEndPicture(
    VADisplay dpy,
    VAContextID context,
    int endpic_done
);

DLL_HIDDEN
void va_TraceEndPictureExt(
    VADisplay dpy,
    VAContextID context,
    int endpic_done
);

DLL_HIDDEN
void va_TraceSyncSurface(
    VADisplay dpy,
    VASurfaceID render_target
);

DLL_HIDDEN
void va_TraceSyncSurface2(
    VADisplay dpy,
    VASurfaceID surface,
    uint64_t timeout_ns
);

DLL_HIDDEN
void va_TraceQuerySurfaceAttributes(
    VADisplay           dpy,
    VAConfigID          config,
    VASurfaceAttrib    *attrib_list,
    unsigned int       *num_attribs
);

DLL_HIDDEN
void va_TraceQuerySurfaceStatus(
    VADisplay dpy,
    VASurfaceID render_target,
    VASurfaceStatus *status /* out */
);

DLL_HIDDEN
void va_TraceQuerySurfaceError(
    VADisplay dpy,
    VASurfaceID surface,
    VAStatus error_status,
    void **error_info /*out*/
);

DLL_HIDDEN
void va_TraceSyncBuffer(
    VADisplay dpy,
    VABufferID buf_id,
    uint64_t timeout_ns
);

DLL_HIDDEN
void va_TraceMaxNumDisplayAttributes(
    VADisplay dpy,
    int number
);

DLL_HIDDEN
void va_TraceQueryDisplayAttributes(
    VADisplay dpy,
    VADisplayAttribute *attr_list,  /* out */
    int *num_attributes         /* out */
);

DLL_HIDDEN
void va_TraceGetDisplayAttributes(
    VADisplay dpy,
    VADisplayAttribute *attr_list,
    int num_attributes
);

DLL_HIDDEN
void va_TraceSetDisplayAttributes(
    VADisplay dpy,
    VADisplayAttribute *attr_list,
    int num_attributes
);

/* extern function called by display side */
void va_TracePutSurface(
    VADisplay dpy,
    VASurfaceID surface,
    void *draw, /* the target Drawable */
    short srcx,
    short srcy,
    unsigned short srcw,
    unsigned short srch,
    short destx,
    short desty,
    unsigned short destw,
    unsigned short desth,
    VARectangle *cliprects, /* client supplied clip list */
    unsigned int number_cliprects, /* number of clip rects in the clip list */
    unsigned int flags /* de-interlacing flags */
);

void va_TraceStatus(VADisplay dpy, const char * funcName, VAStatus status);

/** \brief va_TraceEvent
 * common trace interface to send out trace event with scatterd event data. */
DLL_HIDDEN
void va_TraceEvent(
    VADisplay dpy,
    unsigned short id,
    unsigned short opcode,
    unsigned int num,
    VAEventData *desc);

/** \brief va_TraceEventBuffers
 * trace buffer interface to send out data in buffers. */
DLL_HIDDEN
void va_TraceEventBuffers(
    VADisplay dpy,
    VAContextID context,
    int num_buffers,
    VABufferID *buffers);

/** \brief va_TraceExportSurfaceHandle
 * trace exported surface handle. */
DLL_HIDDEN
void va_TraceExportSurfaceHandle(
    VADisplay        dpy,
    VASurfaceID      surfaceId,
    uint32_t         memType,
    uint32_t         flags,
    void             *descriptor);

/** \brief va_TraceDeriveImage
 * trace derive image */
DLL_HIDDEN
void va_TraceDeriveImage(
    VADisplay dpy,
    VASurfaceID surface,
    VAImage *image);

#ifdef __cplusplus
}
#endif


#endif /* VA_TRACE_H */
