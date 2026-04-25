/**
 * Input event capture and forwarding stub.
 *
 * Real implementation should:
 *   1. Open /dev/input/event* via libinput (or evdev directly)
 *   2. Upgrade an HTTP connection to ws://localhost:7701
 *   3. On every libinput event: serialize to JSON and send over the WS
 *
 * JSON format mirrors InputMsg in bridge/src/main.rs:
 *   {"type":"key_down","keycode":65,"modifiers":0}
 *   {"type":"mouse_move","x":400,"y":300}
 *   {"type":"mouse_button","button":1,"pressed":true,"x":400,"y":300}
 */

#include "input_fwd.h"
#include <stdio.h>
#include <string.h>

int input_fwd_init(input_fwd_t *in, const char *ws_url) {
    memset(in, 0, sizeof(*in));
    strncpy(in->ws_url, ws_url, sizeof(in->ws_url) - 1);
    in->ws_fd = -1;

    printf("[input_fwd] stub init — target %s\n", ws_url);
    /* TODO: open WebSocket connection to ws_url */
    return 0;
}

void input_fwd_pump(input_fwd_t *in) {
    (void)in;
    /* TODO: poll libinput fd; for each event: serialize + ws_send */
}

void input_fwd_destroy(input_fwd_t *in) {
    if (!in) return;
    /* TODO: close WebSocket, release libinput context */
    printf("[input_fwd] destroyed\n");
}
