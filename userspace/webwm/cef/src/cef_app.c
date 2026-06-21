/**
 * CEF application layer stub.
 *
 * Real implementation should:
 *   1. Call CefInitialize with the main args and settings:
 *        settings.windowless_rendering_enabled = 1  (off-screen to DRM fb)
 *        settings.no_sandbox = 1                    (no setuid sandbox on OROS)
 *   2. Add command-line switches:
 *        --enable-features=Vulkan,UseSkiaRenderer
 *        --use-webgpu                               (enable WebGPU)
 *        --use-gl=egl
 *        --disable-gpu-sandbox
 *   3. Create a CefBrowserHost with:
 *        window_info.SetAsWindowless(0)             (no native window)
 *        browser_settings.javascript = STATE_ENABLED
 *   4. Load cfg->url
 *   5. In OnPaint callback: blit the pixel buffer to the DRM framebuffer
 */

#include "cef_app.h"
#include <stdio.h>

#ifdef HAVE_CEF
#  include "include/cef_app.h"
#  include "include/cef_browser.h"
#endif

int cef_app_init(int argc, char *argv[], const cef_app_config_t *cfg) {
    (void)argc; (void)argv;
    printf("[cef_app] init — url=%s webgpu=%d fullscreen=%d\n",
           cfg->url, cfg->webgpu, cfg->fullscreen);

#ifdef HAVE_CEF
    /* TODO: real CefInitialize + browser creation */
    (void)cfg;
#else
    printf("[cef_app] stub (HAVE_CEF not defined) — no browser will open\n");
    printf("[cef_app] open %s in your browser for development\n", cfg->url);
#endif

    return 0;
}

void cef_app_do_message_loop_work(void) {
#ifdef HAVE_CEF
    /* CefDoMessageLoopWork(); */
#endif
    /* stub: yield to avoid busy-spin in main loop */
    struct timespec ts = { .tv_sec = 0, .tv_nsec = 16000000 }; /* ~60 fps */
    nanosleep(&ts, NULL);
}

void cef_app_shutdown(void) {
    printf("[cef_app] shutdown\n");
#ifdef HAVE_CEF
    /* CefShutdown(); */
#endif
}
