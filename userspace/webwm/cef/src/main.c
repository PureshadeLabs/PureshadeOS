/**
 * webwm-cef — CEF embedding layer for the OROS webWM compositor.
 *
 * Responsibilities:
 *   1. Open a DRM/KMS fullscreen surface (no display server required)
 *   2. Embed CEF and load http://localhost:7703 (the WM frontend dev server)
 *   3. Enable WebGPU / WebGL via CEF command-line switches
 *   4. Capture raw input events (libinput) and forward them to the input
 *      WebSocket channel at ws://localhost:7701
 *
 * Build with CMake — see CMakeLists.txt.
 * When CEF_ROOT is not set the binary stubs all CEF calls so the
 * DRM/input scaffolding can be tested independently.
 */

#include <stdio.h>
#include <stdlib.h>
#include <signal.h>
#include <string.h>

#include "drm_output.h"
#include "input_fwd.h"
#include "cef_app.h"

static volatile int g_running = 1;

static void handle_sigint(int sig) {
    (void)sig;
    printf("[cef] SIGINT — shutting down\n");
    g_running = 0;
}

int main(int argc, char *argv[]) {
    signal(SIGINT,  handle_sigint);
    signal(SIGTERM, handle_sigint);

    printf("[cef] webwm-cef starting\n");
    printf("[cef] frontend url : %s\n", WM_FRONTEND_URL);
    printf("[cef] input ws url : %s\n", INPUT_WS_URL);

    /* --- DRM/KMS output -------------------------------------------------- */
    drm_output_t drm = {0};
    if (drm_output_init(&drm) != 0) {
        fprintf(stderr, "[cef] DRM init failed — running headless stub\n");
    }

    /* --- Input forwarding ------------------------------------------------- */
    input_fwd_t input = {0};
    if (input_fwd_init(&input, INPUT_WS_URL) != 0) {
        fprintf(stderr, "[cef] input fwd init failed — input disabled\n");
    }

    /* --- CEF app ---------------------------------------------------------- */
    cef_app_config_t cef_cfg = {
        .url      = WM_FRONTEND_URL,
        .drm      = &drm,
        .input    = &input,
        .webgpu   = 1,
        .fullscreen = 1,
    };
    if (cef_app_init(argc, argv, &cef_cfg) != 0) {
        fprintf(stderr, "[cef] CEF init failed\n");
        goto cleanup;
    }

    printf("[cef] running — press Ctrl-C to quit\n");

    while (g_running) {
        cef_app_do_message_loop_work();
        input_fwd_pump(&input);
    }

cleanup:
    printf("[cef] cleanup\n");
    cef_app_shutdown();
    input_fwd_destroy(&input);
    drm_output_destroy(&drm);
    return 0;
}
