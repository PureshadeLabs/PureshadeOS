/**
 * DRM/KMS output stub.
 *
 * Real implementation should:
 *   1. Open /dev/dri/card0 (or enumerate via udev)
 *   2. Find a connected connector → encoder → CRTC chain
 *   3. Create a GBM device + EGL display on it
 *   4. Create a fullscreen EGL surface
 *   5. Hand the EGL context to CEF's off-screen renderer or
 *      use CEF's --use-gl=egl + DRM PRIME for zero-copy compositing
 */

#include "drm_output.h"
#include <stdio.h>
#include <string.h>

#ifdef HAVE_DRM
#  include <xf86drm.h>
#  include <xf86drmMode.h>
#  include <gbm.h>
#  include <EGL/egl.h>
#endif

int drm_output_init(drm_output_t *out) {
    memset(out, 0, sizeof(*out));
    out->fd = -1;

#ifdef HAVE_DRM
    out->fd = open("/dev/dri/card0", O_RDWR | O_CLOEXEC);
    if (out->fd < 0) {
        fprintf(stderr, "[drm] open /dev/dri/card0 failed: %m\n");
        return -1;
    }
    /* TODO: mode-set, GBM device, EGL display/surface */
    printf("[drm] card0 opened fd=%d (mode-set stub)\n", out->fd);
    out->width  = 1920;
    out->height = 1080;
    return 0;
#else
    printf("[drm] stub — no libdrm at compile time (width=1920 height=1080)\n");
    out->width  = 1920;
    out->height = 1080;
    return 0;  /* stub succeeds */
#endif
}

void drm_output_swap(drm_output_t *out) {
    (void)out;
    /* TODO: eglSwapBuffers + drmModePageFlip */
}

void drm_output_destroy(drm_output_t *out) {
    if (!out) return;
#ifdef HAVE_DRM
    if (out->fd >= 0) { close(out->fd); out->fd = -1; }
#endif
    printf("[drm] destroyed\n");
}
