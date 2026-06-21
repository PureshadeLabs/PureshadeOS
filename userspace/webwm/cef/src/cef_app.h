#pragma once
#include "drm_output.h"
#include "input_fwd.h"

typedef struct {
    const char  *url;
    drm_output_t *drm;
    input_fwd_t  *input;
    int           webgpu;
    int           fullscreen;
} cef_app_config_t;

int  cef_app_init(int argc, char *argv[], const cef_app_config_t *cfg);
void cef_app_do_message_loop_work(void);
void cef_app_shutdown(void);
