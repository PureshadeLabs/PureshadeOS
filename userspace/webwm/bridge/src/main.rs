//! webwm-bridge — WebSocket bridge daemon for the OROS webWM compositor.
//!
//! Ports:
//!   7700  render   — pixel buffer / frame delivery to the WM frontend
//!   7701  input    — raw input events forwarded from CEF / kernel
//!   7702  control  — app lifecycle commands (spawn, kill, list)

use std::net::SocketAddr;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::net::{TcpListener, TcpStream};
use tokio::signal;
use tokio::sync::broadcast;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

// ---------------------------------------------------------------------------
// Message types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RenderMsg {
    /// Placeholder: signal the frontend that a new frame is ready.
    FrameReady { app_id: u64, width: u32, height: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputMsg {
    KeyDown { keycode: u32, modifiers: u32 },
    KeyUp   { keycode: u32, modifiers: u32 },
    MouseMove { x: i32, y: i32 },
    MouseButton { button: u8, pressed: bool, x: i32, y: i32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMsg {
    SpawnApp  { elf_path: String },
    KillApp   { app_id: u64 },
    ListApps,
    AppList   { apps: Vec<AppInfo> },
    Ack       { ok: bool, detail: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppInfo {
    pub id:    u64,
    pub name:  String,
    pub state: String,
}

// ---------------------------------------------------------------------------
// App registry (stub)
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct AppRegistry {
    apps: Vec<AppInfo>,
    next_id: u64,
}

impl AppRegistry {
    fn spawn(&mut self, elf_path: &str) -> AppInfo {
        let id = self.next_id;
        self.next_id += 1;
        let info = AppInfo {
            id,
            name: elf_path.rsplit('/').next().unwrap_or(elf_path).to_owned(),
            state: "running".into(),
        };
        self.apps.push(info.clone());
        info!("[control] stub-spawn app id={id} elf={elf_path}");
        // TODO: call lythmsg IPC to exec the ELF on OROS
        info
    }

    fn kill(&mut self, app_id: u64) -> bool {
        if let Some(pos) = self.apps.iter().position(|a| a.id == app_id) {
            info!("[control] stub-kill app id={app_id}");
            self.apps.remove(pos);
            // TODO: call lythmsg IPC to SYS_TASK_EXIT the task
            true
        } else {
            warn!("[control] kill unknown app id={app_id}");
            false
        }
    }

    fn list(&self) -> Vec<AppInfo> {
        self.apps.clone()
    }
}

// ---------------------------------------------------------------------------
// Shared memory buffer stubs
// ---------------------------------------------------------------------------

mod shmbuf {
    use tracing::info;

    pub fn alloc(_app_id: u64, width: u32, height: u32) -> u64 {
        // TODO: allocate a real shared memory region (memfd / SYS_MMAP)
        info!("[shmbuf] stub-alloc {}x{}", width, height);
        0xDEAD_BEEF_0000_0000
    }

    pub fn free(handle: u64) {
        info!("[shmbuf] stub-free handle={handle:#x}");
        // TODO: release the shared region
    }
}

// ---------------------------------------------------------------------------
// lythmsg IPC stub
// ---------------------------------------------------------------------------

mod lythmsg {
    use tracing::info;

    pub fn connect() {
        // TODO: open SYS_IPC_RECV loop on the lythmsg endpoint cap
        info!("[lythmsg] IPC stub connected (no-op)");
    }
}

// ---------------------------------------------------------------------------
// Per-channel WebSocket acceptor
// ---------------------------------------------------------------------------

async fn run_render_server(
    addr: SocketAddr,
    tx: broadcast::Sender<String>,
) {
    let listener = TcpListener::bind(addr).await
        .unwrap_or_else(|e| panic!("render bind {addr}: {e}"));
    info!("[render] listening on ws://{addr}");

    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                info!("[render] client connected {peer}");
                let tx = tx.clone();
                tokio::spawn(handle_render(stream, peer, tx));
            }
            Err(e) => error!("[render] accept error: {e}"),
        }
    }
}

async fn handle_render(stream: TcpStream, peer: SocketAddr, tx: broadcast::Sender<String>) {
    let ws = match accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => { error!("[render] handshake {peer}: {e}"); return; }
    };
    let (mut sink, mut src) = ws.split();
    let mut rx = tx.subscribe();

    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Ok(json) => { let _ = sink.send(Message::Text(json.into())).await; }
                    Err(_) => break,
                }
            }
            incoming = src.next() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None => {
                        info!("[render] client disconnected {peer}");
                        break;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => { error!("[render] recv {peer}: {e}"); break; }
                }
            }
        }
    }
}

async fn run_input_server(addr: SocketAddr, tx: broadcast::Sender<String>) {
    let listener = TcpListener::bind(addr).await
        .unwrap_or_else(|e| panic!("input bind {addr}: {e}"));
    info!("[input] listening on ws://{addr}");

    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                info!("[input] client connected {peer}");
                let tx = tx.clone();
                tokio::spawn(handle_input(stream, peer, tx));
            }
            Err(e) => error!("[input] accept error: {e}"),
        }
    }
}

async fn handle_input(stream: TcpStream, peer: SocketAddr, tx: broadcast::Sender<String>) {
    let ws = match accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => { error!("[input] handshake {peer}: {e}"); return; }
    };
    let (mut sink, mut src) = ws.split();
    let mut rx = tx.subscribe();

    loop {
        tokio::select! {
            msg = rx.recv() => {
                match msg {
                    Ok(json) => { let _ = sink.send(Message::Text(json.into())).await; }
                    Err(_) => break,
                }
            }
            incoming = src.next() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None => {
                        info!("[input] client disconnected {peer}");
                        break;
                    }
                    Some(Ok(Message::Text(t))) => {
                        // Forward raw input JSON from CEF to all subscribers
                        info!("[input] forwarding: {t}");
                        let _ = tx.send(t.to_string());
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => { error!("[input] recv {peer}: {e}"); break; }
                }
            }
        }
    }
}

async fn run_control_server(
    addr: SocketAddr,
    registry: Arc<tokio::sync::Mutex<AppRegistry>>,
) {
    let listener = TcpListener::bind(addr).await
        .unwrap_or_else(|e| panic!("control bind {addr}: {e}"));
    info!("[control] listening on ws://{addr}");

    loop {
        match listener.accept().await {
            Ok((stream, peer)) => {
                info!("[control] client connected {peer}");
                let registry = registry.clone();
                tokio::spawn(handle_control(stream, peer, registry));
            }
            Err(e) => error!("[control] accept error: {e}"),
        }
    }
}

async fn handle_control(
    stream: TcpStream,
    peer: SocketAddr,
    registry: Arc<tokio::sync::Mutex<AppRegistry>>,
) {
    let ws = match accept_async(stream).await {
        Ok(ws) => ws,
        Err(e) => { error!("[control] handshake {peer}: {e}"); return; }
    };
    let (mut sink, mut src) = ws.split();

    while let Some(msg) = src.next().await {
        let text = match msg {
            Ok(Message::Text(t)) => t,
            Ok(Message::Close(_)) | Err(_) => break,
            _ => continue,
        };

        let cmd: ControlMsg = match serde_json::from_str(&text) {
            Ok(c) => c,
            Err(e) => {
                warn!("[control] bad msg from {peer}: {e}");
                let ack = ControlMsg::Ack { ok: false, detail: format!("parse error: {e}") };
                let _ = sink.send(Message::Text(serde_json::to_string(&ack).unwrap().into())).await;
                continue;
            }
        };

        info!("[control] cmd from {peer}: {cmd:?}");
        let reply = match cmd {
            ControlMsg::SpawnApp { elf_path } => {
                let mut reg = registry.lock().await;
                let info = reg.spawn(&elf_path);
                let _buf = shmbuf::alloc(info.id, 1280, 720);
                ControlMsg::Ack { ok: true, detail: format!("spawned id={}", info.id) }
            }
            ControlMsg::KillApp { app_id } => {
                let mut reg = registry.lock().await;
                let ok = reg.kill(app_id);
                if ok { shmbuf::free(app_id); }
                ControlMsg::Ack { ok, detail: if ok { "killed".into() } else { "not found".into() } }
            }
            ControlMsg::ListApps => {
                let reg = registry.lock().await;
                ControlMsg::AppList { apps: reg.list() }
            }
            other => {
                warn!("[control] unexpected cmd: {other:?}");
                ControlMsg::Ack { ok: false, detail: "unexpected command direction".into() }
            }
        };

        let json = serde_json::to_string(&reply).unwrap();
        let _ = sink.send(Message::Text(json.into())).await;
    }

    info!("[control] client disconnected {peer}");
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "webwm_bridge=debug,info".parse().unwrap()),
        )
        .init();

    info!("[bridge] webwm-bridge starting");

    lythmsg::connect();

    let registry = Arc::new(tokio::sync::Mutex::new(AppRegistry::default()));

    // Broadcast channels — render and input broadcast to all connected clients.
    let (render_tx, _) = broadcast::channel::<String>(64);
    let (input_tx, _)  = broadcast::channel::<String>(64);

    tokio::spawn(run_render_server("127.0.0.1:7700".parse().unwrap(), render_tx));
    tokio::spawn(run_input_server( "127.0.0.1:7701".parse().unwrap(), input_tx));
    tokio::spawn(run_control_server("127.0.0.1:7702".parse().unwrap(), registry));

    info!("[bridge] all channels up — render:7700 input:7701 control:7702");

    signal::ctrl_c().await.expect("failed to listen for ctrl-c");
    info!("[bridge] SIGINT — shutting down");
}
