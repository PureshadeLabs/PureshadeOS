//! lythd — PID 1 init process for OROS (Open Runtime Operating System).
//!
//! ## Boot sequence
//!
//! 1. Receive the 64-byte `BootInfo` message on cap handle 2.
//! 2. Create the service registry IPC endpoint.
//! 3. Load service manifests from two sources, deduped by name:
//!    a. `/etc/svc/*.svc`  — line-based key=value manifest files (legacy / plain ELFs).
//!    b. `/bin/*`          — scan every binary; OROX-prefixed ones are self-describing.
//! 4. Toposort by `dep=` fields; spawn services in toposorted order.
//! 5. Supervisor loop: handle registry requests, check service health.
//!
//! ## Manifest format (line-based key=value, .svc files)
//!
//! ```text
//! name=<service-name>
//! path=<elf-path>
//! restart=never|on-failure[:N]|always
//! cap=memory|rollback|ipc|registry
//! dep=<dep-name>
//! ```
//!
//! ## OROX format (self-describing binary prefix)
//!
//! An OROX binary embeds its manifest in a 264-byte prefix prepended to the ELF.
//! lythd strips the prefix before passing the ELF slice to `exec()`.
//! OROX manifests take precedence over same-name `.svc` manifests.
//!
//! ## Capability handles at entry
//!
//! | Handle | Kind     | Contents                                   |
//! |--------|----------|--------------------------------------------|
//! | 0      | Memory   | Root memory cap — all free physical frames |
//! | 1      | Rollback | `SYS_ROLLBACK` gate                        |
//! | 2      | Ipc      | Boot-info endpoint — one pre-queued msg    |

#![no_std]
#![no_main]

extern crate alloc;

use alloc::{string::String, vec::Vec};
use lythos_std::{
    orox,
    BootInfo,
    ipc::Endpoint,
    println, eprintln,
    sys_close, sys_ipc_create, sys_open, sys_read_fd, sys_readdir,
    sys_rollback, sys_stat, sys_task_exit,
    task::{yield_now, TaskId, TaskStatus},
};

// ── Capability handle constants ───────────────────────────────────────────────

const MEM_CAP:       u64 = 0;
const ROLLBACK_CAP:  u64 = 1;
const BOOT_INFO_CAP: u64 = 2;

// ── Manifest types ────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum RestartPolicy {
    Never,
    OnFailure(u8),
    Always,
}

#[derive(Clone, Copy)]
enum CapSpec {
    Memory,
    Rollback,
    Ipc,
    Registry,
}

struct Manifest {
    name:    String,
    path:    String,
    restart: RestartPolicy,
    caps:    Vec<CapSpec>,
    deps:    Vec<String>,
}

// ── Manifest parsing (.svc files) ─────────────────────────────────────────────

fn parse_restart(s: &str) -> Option<RestartPolicy> {
    match s {
        "never"      => Some(RestartPolicy::Never),
        "always"     => Some(RestartPolicy::Always),
        "on-failure" => Some(RestartPolicy::OnFailure(3)),
        _ => {
            let n_str = s.strip_prefix("on-failure:")?;
            let n: u8 = n_str.parse().ok()?;
            Some(RestartPolicy::OnFailure(n))
        }
    }
}

fn parse_cap(s: &str) -> Option<CapSpec> {
    let key = s.split_once(':').map_or(s, |(k, _)| k);
    match key {
        "memory"   => Some(CapSpec::Memory),
        "rollback" => Some(CapSpec::Rollback),
        "ipc"      => Some(CapSpec::Ipc),
        "registry" => Some(CapSpec::Registry),
        _          => None,
    }
}

fn parse_manifest(text: &str) -> Option<Manifest> {
    let mut name:    Option<String> = None;
    let mut path:    Option<String> = None;
    let mut restart: RestartPolicy  = RestartPolicy::OnFailure(3);
    let mut caps:    Vec<CapSpec>   = Vec::new();
    let mut deps:    Vec<String>    = Vec::new();

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        let (k, v) = match line.split_once('=') {
            Some(pair) => (pair.0.trim(), pair.1.trim()),
            None       => continue,
        };
        match k {
            "name" => name = Some(String::from(v)),
            "path" => path = Some(String::from(v)),
            "restart" => {
                match parse_restart(v) {
                    Some(p) => restart = p,
                    None    => {
                        eprintln!("[lythd] unknown restart policy: {}", v);
                        return None;
                    }
                }
            }
            "cap" => {
                match parse_cap(v) {
                    Some(c) => caps.push(c),
                    None    => {
                        eprintln!("[lythd] unknown cap spec: {}", v);
                        return None;
                    }
                }
            }
            "dep" => {
                if !v.is_empty() { deps.push(String::from(v)); }
            }
            _ => {}
        }
    }

    Some(Manifest { name: name?, path: path?, restart, caps, deps })
}

fn load_manifests() -> Vec<Manifest> {
    let entries = match sys_readdir("/etc/svc") {
        Some(e) => e,
        None    => {
            eprintln!("[lythd] /etc/svc not found — no .svc manifests");
            return Vec::new();
        }
    };

    let mut manifests = Vec::new();
    for entry in &entries {
        let fname = entry.name();
        if !fname.ends_with(".svc") { continue; }

        let mut fpath = String::from("/etc/svc/");
        fpath.push_str(fname);

        let stat = match sys_stat(&fpath) {
            Some(s) => s,
            None    => { eprintln!("[lythd] stat failed: {}", fpath); continue; }
        };
        if stat.size == 0 || stat.size > 8192 { continue; }

        let fd = match sys_open(&fpath) {
            Ok(fd)  => fd,
            Err(()) => { eprintln!("[lythd] open failed: {}", fpath); continue; }
        };

        let mut buf = alloc::vec![0u8; stat.size as usize];
        let mut off = 0usize;
        let mut chunk = [0u8; 512];
        loop {
            match sys_read_fd(fd, &mut chunk) {
                Ok(0) | Err(()) => break,
                Ok(n)           => {
                    let copy = n.min(buf.len() - off);
                    buf[off..off + copy].copy_from_slice(&chunk[..copy]);
                    off += copy;
                    if off >= buf.len() { break; }
                }
            }
        }
        sys_close(fd);

        let text = match core::str::from_utf8(&buf[..off]) {
            Ok(s)  => s,
            Err(_) => { eprintln!("[lythd] {} is not valid UTF-8", fpath); continue; }
        };

        match parse_manifest(text) {
            Some(m) => {
                println!("[lythd] loaded manifest: {} ({})", m.name, m.path);
                manifests.push(m);
            }
            None => eprintln!("[lythd] parse error in {}", fpath),
        }
    }
    manifests
}

// ── OROX manifest loading ─────────────────────────────────────────────────────

/// Convert an `OroxBody` + binary path into a lythd `Manifest`.
fn manifest_from_orox(body: &orox::OroxBody, path: &str) -> Option<Manifest> {
    let name_str = body.name_str();
    if name_str.is_empty() { return None; }

    let restart = match body.restart {
        orox::RESTART_NEVER      => RestartPolicy::Never,
        orox::RESTART_ON_FAILURE => RestartPolicy::OnFailure(
            if body.restart_max > 0 { body.restart_max } else { 3 }
        ),
        orox::RESTART_ALWAYS     => RestartPolicy::Always,
        _                        => RestartPolicy::OnFailure(3),
    };

    let count = (body.cap_count as usize).min(8);
    let mut caps = Vec::new();
    for i in 0..count {
        let cap = match body.caps[i] {
            orox::CAP_MEMORY   => CapSpec::Memory,
            orox::CAP_ROLLBACK => CapSpec::Rollback,
            orox::CAP_IPC      => CapSpec::Ipc,
            orox::CAP_REGISTRY => CapSpec::Registry,
            _                  => continue,
        };
        caps.push(cap);
    }

    let dep_count = (body.dep_count as usize).min(4);
    let mut deps = Vec::new();
    for i in 0..dep_count {
        let dep = body.dep_str(i);
        if !dep.is_empty() {
            deps.push(String::from(dep));
        }
    }

    Some(Manifest {
        name:    String::from(name_str),
        path:    String::from(path),
        restart,
        caps,
        deps,
    })
}

/// Read the first `OROX_PREFIX_SIZE` bytes of `path` and probe for an OROX header.
fn probe_orox(path: &str) -> Option<orox::OroxBody> {
    let fd = sys_open(path).ok()?;
    let mut buf = [0u8; orox::OROX_PREFIX_SIZE];
    let n = sys_read_fd(fd, &mut buf).unwrap_or(0);
    sys_close(fd);
    if n < orox::OROX_PREFIX_SIZE { return None; }
    orox::parse_orox(&buf)
}

/// Scan `/bin/` and return manifests for every OROX-prefixed binary found.
fn load_orox_manifests() -> Vec<Manifest> {
    let entries = match sys_readdir("/bin") {
        Some(e) => e,
        None    => {
            eprintln!("[lythd] /bin not found — no OROX binaries");
            return Vec::new();
        }
    };

    let mut manifests = Vec::new();
    for entry in &entries {
        let fname = entry.name();
        if fname.is_empty() { continue; }

        let mut path = String::from("/bin/");
        path.push_str(fname);

        if let Some(body) = probe_orox(&path) {
            match manifest_from_orox(&body, &path) {
                Some(m) => {
                    println!("[lythd] OROX: {} @ {} ({} cap(s), {} dep(s))",
                             m.name, path, m.caps.len(), m.deps.len());
                    manifests.push(m);
                }
                None => eprintln!("[lythd] OROX: empty name in {}", path),
            }
        }
    }
    manifests
}

// ── Toposort ──────────────────────────────────────────────────────────────────

fn toposort(mut remaining: Vec<Manifest>) -> Vec<Manifest> {
    let mut result: Vec<Manifest> = Vec::with_capacity(remaining.len());

    while !remaining.is_empty() {
        let pos = remaining.iter().position(|m| {
            m.deps.iter().all(|dep| result.iter().any(|r| r.name == *dep))
        });
        match pos {
            Some(i) => result.push(remaining.remove(i)),
            None    => { result.extend(remaining.drain(..)); break; }
        }
    }
    result
}

// ── File loader ───────────────────────────────────────────────────────────────

/// Load the raw bytes of `path` (may be OROX-prefixed or a bare ELF).
fn load_file(path: &str) -> Option<Vec<u8>> {
    let stat = sys_stat(path)?;
    let size = stat.size as usize;
    if size == 0 || size > 32 * 1024 * 1024 {
        eprintln!("[lythd] load_file: {} bad size {}", path, size);
        return None;
    }
    let fd = match sys_open(path) {
        Ok(fd)  => fd,
        Err(()) => { eprintln!("[lythd] load_file: cannot open {}", path); return None; }
    };
    let mut buf = alloc::vec![0u8; size];
    let mut off = 0usize;
    let mut chunk = [0u8; 4096];
    loop {
        match sys_read_fd(fd, &mut chunk) {
            Ok(0) | Err(()) => break,
            Ok(n)           => {
                let copy = n.min(buf.len() - off);
                buf[off..off + copy].copy_from_slice(&chunk[..copy]);
                off += copy;
                if off >= buf.len() { break; }
            }
        }
    }
    sys_close(fd);
    if off < size {
        eprintln!("[lythd] load_file: short read on {} ({}/{})", path, off, size);
        return None;
    }
    Some(buf)
}

/// Load `path`, strip any OROX prefix, and spawn as a userspace task.
fn spawn_from_disk(path: &str, caps: &[u64]) -> Option<TaskId> {
    let file = load_file(path)?;
    let elf  = orox::elf_slice(&file);
    lythos_std::task::spawn(elf, caps).ok()
}

// ── Cap provisioning ──────────────────────────────────────────────────────────

fn build_exec_caps(
    manifest:     &Manifest,
    mem_cap:      u64,
    rollback_cap: u64,
    registry_cap: u64,
) -> Option<Vec<u64>> {
    let mut caps: Vec<u64> = Vec::new();
    for spec in &manifest.caps {
        let handle = match spec {
            CapSpec::Memory   => mem_cap,
            CapSpec::Rollback => rollback_cap,
            CapSpec::Registry => registry_cap,
            CapSpec::Ipc => {
                match sys_ipc_create() {
                    Ok(h)  => h,
                    Err(e) => {
                        eprintln!("[lythd] ipc_create failed for {}: {:?}", manifest.name, e);
                        return None;
                    }
                }
            }
        };
        caps.push(handle);
    }
    Some(caps)
}

// ── Service registry protocol ─────────────────────────────────────────────────

const KIND_REGISTER: u8 = 0;
const KIND_LOOKUP:   u8 = 1;
const KIND_ACK:      u8 = 2;
const KIND_NACK:     u8 = 3;

struct Service {
    name:    [u8; 32],
    task_id: u64,
    cap:     u64,
}

impl Service {
    fn name_str(&self) -> &str {
        let n = self.name.iter().position(|&b| b == 0).unwrap_or(32);
        core::str::from_utf8(&self.name[..n]).unwrap_or("<invalid>")
    }
}

// ── Health monitoring ─────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ServiceState {
    Running,
    Restarting(u8),
    Failed,
}

struct ManagedSvc {
    name:      String,
    path:      String,
    restart:   RestartPolicy,
    task_id:   TaskId,
    state:     ServiceState,
    caps:      [u64; 8],
    cap_count: usize,
}

impl ManagedSvc {
    fn new(manifest: &Manifest, task_id: TaskId, exec_caps: &[u64]) -> Self {
        let mut caps_buf = [0u64; 8];
        let cap_count = exec_caps.len().min(8);
        caps_buf[..cap_count].copy_from_slice(&exec_caps[..cap_count]);
        ManagedSvc {
            name:      String::from(manifest.name.as_str()),
            path:      String::from(manifest.path.as_str()),
            restart:   manifest.restart,
            task_id,
            state:     ServiceState::Running,
            caps:      caps_buf,
            cap_count,
        }
    }

    fn check_and_restart(&mut self) {
        if self.state == ServiceState::Failed { return; }
        if lythos_std::task::task_status(self.task_id) != TaskStatus::Dead { return; }

        let attempts = match self.state {
            ServiceState::Running       => 0,
            ServiceState::Restarting(n) => n,
            ServiceState::Failed        => return,
        };

        match self.restart {
            RestartPolicy::Never => {
                eprintln!("[lythd] {} exited (restart=never)", self.name);
                self.state = ServiceState::Failed;
                return;
            }
            RestartPolicy::OnFailure(max) if attempts >= max => {
                eprintln!("[lythd] {} failed permanently after {} restart(s)", self.name, attempts);
                self.state = ServiceState::Failed;
                return;
            }
            RestartPolicy::Always | RestartPolicy::OnFailure(_) => {}
        }

        eprintln!("[lythd] {} died — restarting (attempt {})", self.name, attempts + 1);

        match spawn_from_disk(&self.path, &self.caps[..self.cap_count]) {
            Some(new_id) => {
                self.task_id = new_id;
                self.state   = ServiceState::Restarting(attempts + 1);
                println!("[lythd] {} restarted as task {}", self.name, new_id);
            }
            None => {
                eprintln!("[lythd] {} respawn failed (cannot load {})", self.name, self.path);
                self.state = ServiceState::Failed;
            }
        }
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // 1. Consume the boot-info message.
    let boot_ep = Endpoint::from_raw(BOOT_INFO_CAP);
    let frame   = boot_ep.recv_frame().expect("lythd: boot-info recv failed");
    let info    = BootInfo::from_bytes(&frame).expect("lythd: boot-info sig mismatch");

    let mem_mib = { info.mem_bytes   } / (1024 * 1024);
    let frames  = { info.free_frames };
    println!("[lythd] lythos init — {} MiB free ({} frames), cpu: {}",
             mem_mib, frames, info.vendor_str());

    // 2. Create the service registry endpoint.
    let registry = Endpoint::create().expect("lythd: registry endpoint alloc failed");
    println!("[lythd] service registry online (cap {})", registry.as_raw());

    // 3. Load manifests from both sources; OROX binaries win on name collision.
    let mut manifests = load_manifests();
    let orox_manifests = load_orox_manifests();
    for om in orox_manifests {
        if !manifests.iter().any(|m| m.name == om.name) {
            manifests.push(om);
        } else {
            println!("[lythd] OROX manifest for '{}' overrides .svc", om.name);
            if let Some(pos) = manifests.iter().position(|m| m.name == om.name) {
                manifests[pos] = om;
            }
        }
    }

    let manifests = toposort(manifests);
    println!("[lythd] {} service manifest(s) loaded", manifests.len());

    // 4. Spawn services in toposorted order.
    let mut managed: Vec<ManagedSvc> = Vec::new();

    for m in &manifests {
        let exec_caps = match build_exec_caps(&m, MEM_CAP, ROLLBACK_CAP, registry.as_raw()) {
            Some(c) => c,
            None    => {
                eprintln!("[lythd] skipping {} — cap provisioning failed", m.name);
                continue;
            }
        };

        match spawn_from_disk(&m.path, &exec_caps) {
            Some(task_id) => {
                println!("[lythd] spawned {} (task {})", m.name, task_id);
                managed.push(ManagedSvc::new(&m, task_id, &exec_caps));
            }
            None => eprintln!("[lythd] failed to spawn {}", m.name),
        }
    }

    // 5. Supervisor loop.
    println!("[lythd] entering supervisor loop ({} managed service(s))", managed.len());

    let mut services: Vec<Service> = Vec::new();

    loop {
        for svc in managed.iter_mut() {
            svc.check_and_restart();
        }

        let frame = match registry.recv_frame() {
            Ok(f)  => f,
            Err(e) => {
                eprintln!("[lythd] registry recv error: {:?}", e);
                yield_now();
                continue;
            }
        };

        match frame[0] {
            KIND_REGISTER => {
                let name_len = (frame[1] as usize).min(32);
                let mut name = [0u8; 32];
                name[..name_len].copy_from_slice(&frame[2..2 + name_len]);
                let task_id = u64::from_le_bytes(frame[34..42].try_into().unwrap_or([0; 8]));
                let cap     = u64::from_le_bytes(frame[42..50].try_into().unwrap_or([0; 8]));
                let svc = Service { name, task_id, cap };
                println!("[lythd] registered '{}' task={} cap={}", svc.name_str(), task_id, cap);
                services.push(svc);
            }

            KIND_LOOKUP => {
                let name_len = (frame[1] as usize).min(32);
                let query    = core::str::from_utf8(&frame[2..2 + name_len]).unwrap_or("");
                match services.iter().find(|s| s.name_str() == query) {
                    Some(svc) => {
                        let mut ack = [0u8; 64];
                        ack[0] = KIND_ACK;
                        ack[34..42].copy_from_slice(&svc.task_id.to_le_bytes());
                        ack[42..50].copy_from_slice(&svc.cap.to_le_bytes());
                        let _ = registry.send(&ack);
                    }
                    None => {
                        let mut nack = [0u8; 64];
                        nack[0] = KIND_NACK;
                        let _ = registry.send(&nack);
                    }
                }
            }

            kind => eprintln!("[lythd] unknown registry msg kind={}", kind),
        }
    }
}

// ── Panic handler ─────────────────────────────────────────────────────────────

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    lythos_std::sys_log("[lythd] PANIC");
    if let Some(msg) = info.message().as_str() {
        lythos_std::sys_log(": ");
        lythos_std::sys_log(msg);
    }
    if let Some(loc) = info.location() {
        lythos_std::sys_log(" at ");
        lythos_std::sys_log(loc.file());
        lythos_std::sys_log(":");
        let line = loc.line();
        let mut buf = [0u8; 10];
        let mut n = 0usize;
        let mut v = line;
        if v == 0 { buf[0] = b'0'; n = 1; } else {
            while v > 0 { buf[n] = b'0' + (v % 10) as u8; n += 1; v /= 10; }
            buf[..n].reverse();
        }
        if let Ok(s) = core::str::from_utf8(&buf[..n]) { lythos_std::sys_log(s); }
        lythos_std::sys_log("\n");
    } else {
        lythos_std::sys_log("\n");
    }
    let _ = sys_rollback();
    sys_task_exit()
}
