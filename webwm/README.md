# webwm — OROS Wayland-free Window Manager

Three-component web-based compositor for OROS running on raw DRM/KMS.

```
webwm/
├── bridge/    Rust WebSocket bridge daemon
├── cef/       CEF embedding layer (DRM/KMS fullscreen)
└── frontend/  Svelte WM shell (dev at http://localhost:7703)
```

---

## bridge

Rust async daemon. Opens three WebSocket servers:

| Port | Channel | Purpose |
|------|---------|---------|
| 7700 | render  | Frame delivery (pixel buffers) to frontend |
| 7701 | input   | Raw input events from CEF → frontend |
| 7702 | control | App lifecycle: spawn / kill / list |

```bash
cd bridge
cargo run
```

Env var `RUST_LOG=debug` for verbose output.

---

## cef

CEF embedding layer. Opens DRM/KMS fullscreen surface, loads
`http://localhost:7703`, enables WebGPU. Forwards input to ws://localhost:7701.

Requires libdrm, GBM, EGL, and a CEF binary distribution.
Set `CEF_ROOT` to the path of your CEF binary.

```bash
cd cef
mkdir build && cd build
cmake .. -DCEF_ROOT=/path/to/cef_binary
make
./webwm-cef
```

Without `CEF_ROOT` the binary builds as a headless stub (useful for testing
bridge + frontend independently).

---

## frontend

Svelte + Vite shell. Runnable standalone in any browser — no CEF needed for
development.

```bash
cd frontend
npm install
npm run dev        # starts at http://localhost:7703
```

### Features

- Connects to all three bridge channels on startup; auto-reconnects
- Dwindle tiling layout (2-window hardcoded split placeholder)
- M3 titlebars and bottom taskbar
- Dynamic color: extracts dominant color from wallpaper → M3 tonal palette
- All visual tokens (color, font, shape, motion) as CSS custom properties

### Ricing

Override `--font-family` in `src/theme.css` to swap the system font globally.
All M3 color tokens are CSS custom properties — override any subset in a local
stylesheet loaded after `theme.css`.
