//! orox-pack — prepend an OROX manifest header to an ELF64 binary.
//!
//! Usage:
//!   orox-pack [OPTIONS] <input> <output>
//!
//! Options:
//!   --name <name>       service name (required unless --svc is given)
//!   --restart <policy>  never | always | on-failure | on-failure:N  [default: on-failure:3]
//!   --cap <kind>        memory | rollback | ipc | registry  (repeatable)
//!   --dep <name>        dependency service name  (repeatable, max 4)
//!   --svc <file>        parse name/restart/cap/dep from a .svc manifest file
//!   --strip             strip an existing OROX prefix from the input before repacking
//!
//! The output file is the input ELF with a 264-byte OROX prefix prepended.
//! lythd reads the prefix and passes only the ELF slice to the kernel exec() syscall.

// ── OROX constants (mirrors lythos-std/src/orox.rs) ──────────────────────────

const OROX_MAGIC:       [u8; 4] = *b"OROX";
const OROX_VERSION:     u8      = 1;
const OROX_PREFIX_SIZE: usize   = 264;

const RESTART_NEVER:      u8 = 0;
const RESTART_ON_FAILURE: u8 = 1;
const RESTART_ALWAYS:     u8 = 2;

const CAP_MEMORY:   u8 = 0;
const CAP_ROLLBACK: u8 = 1;
const CAP_IPC:      u8 = 2;
const CAP_REGISTRY: u8 = 3;

// ── Body builder ─────────────────────────────────────────────────────────────

struct Config {
    name:        String,
    restart:     u8,
    restart_max: u8,
    caps:        Vec<u8>,
    deps:        Vec<String>,
}

fn build_prefix(cfg: &Config) -> [u8; OROX_PREFIX_SIZE] {
    let mut prefix = [0u8; OROX_PREFIX_SIZE];

    prefix[0..4].copy_from_slice(&OROX_MAGIC);
    prefix[4] = OROX_VERSION;
    // [5..8] = 0 (pad)

    let b = &mut prefix[8..264];
    b[0] = cfg.restart;
    b[1] = cfg.restart_max;
    b[2] = cfg.caps.len().min(8) as u8;
    b[3] = cfg.deps.len().min(4) as u8;

    for (i, &cap) in cfg.caps.iter().take(8).enumerate() {
        b[4 + i] = cap;
    }

    let name_bytes = cfg.name.as_bytes();
    let name_len   = name_bytes.len().min(31);
    b[12..12 + name_len].copy_from_slice(&name_bytes[..name_len]);
    // null terminator already zeroed

    for (i, dep) in cfg.deps.iter().take(4).enumerate() {
        let db  = dep.as_bytes();
        let dln = db.len().min(31);
        let off = 44 + i * 32;
        b[off..off + dln].copy_from_slice(&db[..dln]);
    }

    prefix
}

// ── .svc manifest parser ─────────────────────────────────────────────────────

fn parse_svc(text: &str, cfg: &mut Config) {
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        let (k, v) = match line.split_once('=') {
            Some(p) => (p.0.trim(), p.1.trim()),
            None    => continue,
        };
        match k {
            "name" => cfg.name = v.to_string(),
            "restart" => match parse_restart(v) {
                Some((r, max)) => { cfg.restart = r; cfg.restart_max = max; }
                None           => eprintln!("orox-pack: unknown restart policy '{}'", v),
            },
            "cap" => match parse_cap(v) {
                Some(c) => cfg.caps.push(c),
                None    => eprintln!("orox-pack: unknown cap kind '{}'", v),
            },
            "dep" => { if !v.is_empty() { cfg.deps.push(v.to_string()); } }
            _     => {}
        }
    }
}

fn parse_restart(s: &str) -> Option<(u8, u8)> {
    match s {
        "never"      => Some((RESTART_NEVER, 0)),
        "always"     => Some((RESTART_ALWAYS, 0)),
        "on-failure" => Some((RESTART_ON_FAILURE, 3)),
        _ => {
            let n_str = s.strip_prefix("on-failure:")?;
            let n: u8 = n_str.parse().ok()?;
            Some((RESTART_ON_FAILURE, n))
        }
    }
}

fn parse_cap(s: &str) -> Option<u8> {
    let key = s.split_once(':').map_or(s, |(k, _)| k);
    match key {
        "memory"   => Some(CAP_MEMORY),
        "rollback" => Some(CAP_ROLLBACK),
        "ipc"      => Some(CAP_IPC),
        "registry" => Some(CAP_REGISTRY),
        _          => None,
    }
}

// ── CLI ───────────────────────────────────────────────────────────────────────

fn usage() -> ! {
    eprintln!("\
Usage: orox-pack [OPTIONS] <input> <output>

Options:
  --name <name>       service name (required unless --svc is given)
  --restart <policy>  never|always|on-failure|on-failure:N  [default: on-failure:3]
  --cap <kind>        memory|rollback|ipc|registry  (repeatable, max 8)
  --dep <name>        dependency service name  (repeatable, max 4)
  --svc <file>        load defaults from a .svc manifest (CLI flags override)
  --strip             strip existing OROX prefix from input before repacking");
    std::process::exit(1);
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut cfg = Config {
        name:        String::new(),
        restart:     RESTART_ON_FAILURE,
        restart_max: 3,
        caps:        Vec::new(),
        deps:        Vec::new(),
    };
    let mut input_path:  Option<String> = None;
    let mut output_path: Option<String> = None;
    let mut svc_path:    Option<String> = None;
    let mut strip = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--name" => {
                i += 1;
                cfg.name = args.get(i).cloned().unwrap_or_default();
            }
            "--restart" => {
                i += 1;
                let v = args.get(i).map(|s| s.as_str()).unwrap_or("");
                match parse_restart(v) {
                    Some((r, max)) => { cfg.restart = r; cfg.restart_max = max; }
                    None           => { eprintln!("orox-pack: unknown restart '{}'", v); usage(); }
                }
            }
            "--cap" => {
                i += 1;
                let v = args.get(i).map(|s| s.as_str()).unwrap_or("");
                match parse_cap(v) {
                    Some(c) => cfg.caps.push(c),
                    None    => { eprintln!("orox-pack: unknown cap '{}'", v); usage(); }
                }
            }
            "--dep" => {
                i += 1;
                if let Some(dep) = args.get(i) {
                    cfg.deps.push(dep.clone());
                }
            }
            "--svc" => {
                i += 1;
                svc_path = args.get(i).cloned();
            }
            "--strip" => strip = true,
            arg if arg.starts_with("--") => {
                eprintln!("orox-pack: unknown option '{}'", arg);
                usage();
            }
            _ => {
                if input_path.is_none() {
                    input_path = Some(args[i].clone());
                } else if output_path.is_none() {
                    output_path = Some(args[i].clone());
                } else {
                    eprintln!("orox-pack: unexpected argument '{}'", args[i]);
                    usage();
                }
            }
        }
        i += 1;
    }

    let input  = input_path.unwrap_or_else(|| usage());
    let output = output_path.unwrap_or_else(|| usage());

    // Load .svc defaults first, then CLI flags override.
    if let Some(svc) = svc_path {
        let text = std::fs::read_to_string(&svc)
            .unwrap_or_else(|e| { eprintln!("orox-pack: cannot read {}: {}", svc, e); usage(); });
        // Temporarily parse into a scratch config to fill only unset fields.
        let mut scratch = Config {
            name:        String::new(),
            restart:     RESTART_ON_FAILURE,
            restart_max: 3,
            caps:        Vec::new(),
            deps:        Vec::new(),
        };
        parse_svc(&text, &mut scratch);
        if cfg.name.is_empty()  { cfg.name = scratch.name; }
        if cfg.caps.is_empty()  { cfg.caps = scratch.caps; }
        if cfg.deps.is_empty()  { cfg.deps = scratch.deps; }
    }

    if cfg.name.is_empty() {
        eprintln!("orox-pack: service name is required (--name or --svc)");
        usage();
    }
    if cfg.name.len() > 31 {
        eprintln!("orox-pack: name '{}' exceeds 31-byte limit", cfg.name);
        std::process::exit(1);
    }
    if cfg.caps.len() > 8 {
        eprintln!("orox-pack: at most 8 caps allowed ({} given)", cfg.caps.len());
        std::process::exit(1);
    }
    if cfg.deps.len() > 4 {
        eprintln!("orox-pack: at most 4 deps allowed ({} given)", cfg.deps.len());
        std::process::exit(1);
    }

    // Read input ELF.
    let mut elf = std::fs::read(&input)
        .unwrap_or_else(|e| { eprintln!("orox-pack: cannot read {}: {}", input, e); std::process::exit(1); });

    // Strip existing OROX prefix if requested.
    if strip && elf.len() >= OROX_PREFIX_SIZE && &elf[0..4] == b"OROX" {
        elf = elf[OROX_PREFIX_SIZE..].to_vec();
        eprintln!("orox-pack: stripped existing OROX prefix");
    }

    // Validate that what remains looks like an ELF.
    if elf.len() < 4 || &elf[0..4] != b"\x7fELF" {
        eprintln!("orox-pack: input does not appear to be an ELF64 binary");
        std::process::exit(1);
    }

    // Build and prepend the OROX prefix.
    let prefix = build_prefix(&cfg);
    let mut out = Vec::with_capacity(OROX_PREFIX_SIZE + elf.len());
    out.extend_from_slice(&prefix);
    out.extend_from_slice(&elf);

    std::fs::write(&output, &out)
        .unwrap_or_else(|e| { eprintln!("orox-pack: cannot write {}: {}", output, e); std::process::exit(1); });

    println!("orox-pack: wrote {} ({} + {} bytes) — service '{}'",
             output, OROX_PREFIX_SIZE, elf.len(), cfg.name);
}
