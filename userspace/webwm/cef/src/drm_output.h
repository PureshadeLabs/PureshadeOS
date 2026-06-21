#pragma once
#include <stdint.h>

typedef struct {
    int   fd;           /* DRM device fd */
    int   width;
    int   height;
    void *gbm_device;   /* gbm_device* — opaque stub */
    void *egl_display;  /* EGLDisplay — opaque stub */
    void *egl_surface;  /* EGLSurface — opaque stub */
} drm_output_t;

int  drm_output_init(drm_output_t *out);
void drm_output_swap(drm_output_t *out);
void drm_output_destroy(drm_output_t *out);
