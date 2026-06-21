#pragma once

typedef struct {
    int         ws_fd;      /* WebSocket fd — stub: -1 */
    char        ws_url[256];
} input_fwd_t;

int  input_fwd_init(input_fwd_t *in, const char *ws_url);
void input_fwd_pump(input_fwd_t *in);
void input_fwd_destroy(input_fwd_t *in);
