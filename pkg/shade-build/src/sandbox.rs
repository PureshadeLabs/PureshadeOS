//! SeatbeltSandbox — the **host** macOS-Seatbelt [`BuildSandbox`] impl
//! (docs/shade/build-sandbox.md; contract: docs/shade-pkg/06-build.md §3.1,
//! sandbox profile `1`).
//!
//! This is a host vehicle, not the native OROS sandbox. It carries no Lythos
//! enforcement: it enforces isolation with macOS Seatbelt (`sandbox-exec` +
//! an SBPL profile) and its [`SeatbeltSandbox::new`] is **fail-closed to
//! macOS**. The real `SYS_MOUNT` + capability-enforced OROS sandbox that will
//! consume the same [`SandboxPlan`] is **unwritten** — it does not exist in the
//! tree, and this impl deliberately does not stand in for it.
//!
//! Two layers, deliberately separated:
//!
//! - [`SandboxPlan`] is the **pure model** (kept and named for the native impl):
//!   given a [`SandboxSpec`], it computes the mount list (the builder's
//!   filesystem view, in [`SYS_MOUNT`] terms — declared input store paths
//!   read-only under `/shade/store`, one writable build dir), the minimal
//!   capability grant set ([`CapKind`] + rights bits from `lythos-abi`), and
//!   the fixed deterministic environment. Its `check_read` / `check_write` /
//!   `check_network` answer with **ABI errnos** exactly as the Lythos syscall
//!   boundary would. This layer is what the native OROS builder task will be
//!   constructed from; it is fully unit-tested.
//! - The **host vehicle** enforces the plan while builds still run on the dev
//!   host (host-assisted mode, 06 intro): on macOS the plan compiles to a
//!   Seatbelt (SBPL) profile and every phase runs under
//!   `/usr/bin/sandbox-exec` with a **cleared** environment. Reads outside the
//!   plan's mounts, writes outside the build dir, and all network access fail
//!   inside the builder with EPERM — the host spelling of the plan's ENOPERM.
//!
//! Construction is **fail-closed**: [`SeatbeltSandbox::new`] errors if the host
//! has no enforcement facility. There is no silent fallback to permissive
//! behavior — that impl already exists ([`PermissiveSandbox`]) and is honestly
//! named.
//!
//! The executor is untouched: this is one impl of the [`BuildSandbox`] seam.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use lythos_abi::cap::{CapKind, RIGHT_READ, RIGHT_WRITE};
use lythos_abi::errno;

use crate::executor::{BuildEnv, BuildLogSink, BuildSandbox, SandboxError, SandboxSpec};

/// Fixed build identity (06 §3.1 "fixed build uid/gid"). Model values; the
/// host vehicle cannot change uid unprivileged and documents that gap
/// (docs/shade/build-sandbox.md §escape-hatches).
pub const BUILD_UID: u32 = 30001;
pub const BUILD_GID: u32 = 30001;
/// Fixed umask, applied by the phase wrapper before the recipe command runs.
pub const BUILD_UMASK: &str = "0022";
/// `$HOME` inside the sandbox: a nonexistent path, so anything that reads it
/// fails loudly (06 §4).
pub const SANDBOX_HOME: &str = "/homeless";

/// One entry in the builder's filesystem view, in mount terms: `source` is
/// made visible at `target` in the builder's namespace with `rights`
/// (`RIGHT_READ` = read-only store input; `RIGHT_READ|RIGHT_WRITE` = the
/// build dir). On OROS this list becomes per-builder `SYS_MOUNT` calls; on
/// the host it becomes Seatbelt path rules over `source`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountPlan {
    /// Path in the builder's namespace (`/shade/store/<name>` or
    /// `/shade/build/<drv>`).
    pub target: String,
    /// The real (canonicalized) path being exposed.
    pub source: PathBuf,
    /// `RIGHT_READ` or `RIGHT_READ | RIGHT_WRITE` — no other combination is
    /// ever planned.
    pub rights: u8,
}

/// One capability the builder task is granted. The whole set is minimal by
/// construction (06 §3.2): nothing is grantable that the plan did not emit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapGrant {
    pub kind: CapKind,
    /// `CapRights` bits (`lythos_abi::cap::RIGHT_*`).
    pub rights: u8,
    /// What the grant is for — fixed vocabulary, stable for tests and docs.
    pub role: &'static str,
}

/// The computed isolation plan for one build: filesystem view, capability
/// set, deterministic environment. Pure over the spec — same spec ⇒ same
/// plan, byte for byte (mount order, env order, profile text).
#[derive(Debug, Clone)]
pub struct SandboxPlan {
    pub store_name: String,
    /// Canonicalized scratch dir (the one writable mount's source).
    pub scratch: PathBuf,
    /// Canonicalized staging dir (`$out`), inside the scratch.
    pub staging: PathBuf,
    /// Read-only input mounts in `dep.*` order, then the writable build-dir
    /// mount, last. Order is part of the plan's determinism contract.
    pub mounts: Vec<MountPlan>,
    /// The builder task's complete capability set.
    pub caps: Vec<CapGrant>,
    /// The complete environment — the builder sees these and nothing else.
    /// Fixed-table values win over recipe `env` collisions (03 §5.3).
    pub env: Vec<(String, String)>,
}

/// Reject a path the plan cannot safely embed in an SBPL string literal.
/// Store names are digest+name+version and test temp dirs are ASCII, so this
/// only ever fires on hostile or accidental exotic paths.
fn sbpl_safe(p: &Path) -> io::Result<&str> {
    let s = p.to_str().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "non-UTF-8 path in sandbox plan")
    })?;
    if s.contains('"') || s.contains('\\') || s.chars().any(|c| c.is_control()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path not representable in a sandbox profile: {s:?}"),
        ));
    }
    Ok(s)
}

impl SandboxPlan {
    /// Compute the plan for one build. Fails if a declared input is not
    /// realized (the postorder guarantees it should be) or a path cannot be
    /// canonicalized/embedded.
    pub fn from_spec(spec: &SandboxSpec) -> io::Result<SandboxPlan> {
        let scratch = fs::canonicalize(spec.scratch)?;
        let staging = fs::canonicalize(spec.staging)?;

        // Filesystem view: each declared input read-only under the sandbox
        // /shade/store, in dep order (stable), then the one writable mount.
        let mut mounts = Vec::with_capacity(spec.inputs.len() + 1);
        let mut caps = Vec::with_capacity(spec.inputs.len() + 2);
        for input in spec.inputs {
            let source = fs::canonicalize(input).map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!("declared input {input} is not realized: {e}"),
                )
            })?;
            let name = source
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "input path has no store name")
                })?
                .to_string();
            mounts.push(MountPlan {
                target: format!("/shade/store/{name}"),
                source,
                rights: RIGHT_READ,
            });
            caps.push(CapGrant {
                kind: CapKind::Filesystem,
                rights: RIGHT_READ,
                role: "input store path (read-only mount)",
            });
        }
        mounts.push(MountPlan {
            target: format!("/shade/build/{}", spec.store_name),
            source: scratch.clone(),
            rights: RIGHT_READ | RIGHT_WRITE,
        });
        caps.push(CapGrant {
            kind: CapKind::Filesystem,
            rights: RIGHT_READ | RIGHT_WRITE,
            role: "build dir (scratch + $out staging + tmp)",
        });
        // The one permitted endpoint: the supervisor-granted log/control
        // channel (06 §3.1). On the host it is the inherited log fd.
        caps.push(CapGrant {
            kind: CapKind::Ipc,
            rights: RIGHT_WRITE,
            role: "supervisor log endpoint",
        });
        // Nothing else. No Device, no Rollback, no Memory grant beyond the
        // task default, no GRANT/REVOKE bits anywhere — the builder cannot
        // delegate or tear down what it was given.

        // Deterministic environment (06 §4), complete. Recipe env first so
        // the fixed table wins on collision (03 §5.3: recipes may not
        // override it; `Command::envs` keeps the later duplicate).
        let staging_str = spec.staging.to_string();
        let mut env: Vec<(String, String)> = spec.env.to_vec();
        // PATH: input bin/ dirs in dep order, then the host tool tail — the
        // host shell + coreutils are trusted bringup infrastructure, mirrored
        // by the profile's read allowances (documented escape hatch; the
        // native OROS plan drops the tail).
        let mut path_entries: Vec<String> = spec
            .inputs
            .iter()
            .map(|d| Path::new(d).join("bin").to_string_lossy().into_owned())
            .collect();
        path_entries.push("/usr/bin".into());
        path_entries.push("/bin".into());
        env.extend([
            ("PATH".to_string(), path_entries.join(":")),
            ("HOME".to_string(), SANDBOX_HOME.to_string()),
            ("OUT".to_string(), staging_str.clone()),
            ("out".to_string(), staging_str),
            ("TARGET".to_string(), spec.system.to_string()),
            ("TMPDIR".to_string(), Path::new(spec.scratch).join("tmp").to_string_lossy().into_owned()),
            ("SOURCE_DATE_EPOCH".to_string(), "0".to_string()),
            ("TZ".to_string(), "UTC".to_string()),
            ("LANG".to_string(), "C.UTF-8".to_string()),
            ("LC_ALL".to_string(), "C.UTF-8".to_string()),
            ("JOBS".to_string(), spec.jobs.to_string()),
        ]);

        Ok(SandboxPlan {
            store_name: spec.store_name.to_string(),
            scratch,
            staging,
            mounts,
            caps,
            env,
        })
    }

    /// Would the Lythos boundary allow the builder to read `path`? Answers in
    /// ABI errno terms over the **plan's** mounts (host runtime allowances
    /// like `/bin/sh` are vehicle concerns, not part of the contract).
    pub fn check_read(&self, path: &Path) -> Result<(), u64> {
        for m in &self.mounts {
            if path.starts_with(&m.source) && m.rights & RIGHT_READ != 0 {
                return Ok(());
            }
        }
        // No mount covers it ⇒ no capability whose rights could even be
        // checked ⇒ rights insufficient.
        Err(errno::ENOPERM)
    }

    /// Would the Lythos boundary allow the builder to write `path`? Writes to
    /// a read-only input mount are `EROFS` (the store's sealed-entry errno);
    /// writes outside every mount are `ENOPERM`.
    pub fn check_write(&self, path: &Path) -> Result<(), u64> {
        for m in &self.mounts {
            if path.starts_with(&m.source) {
                return if m.rights & RIGHT_WRITE != 0 { Ok(()) } else { Err(errno::EROFS) };
            }
        }
        Err(errno::ENOPERM)
    }

    /// Network access: the builder holds no capability that reaches a network
    /// device or the net daemons (no `Device`, no `Ipc` beyond the log
    /// endpoint), so any attempt fails rights-insufficient.
    pub fn check_network(&self) -> Result<(), u64> {
        Err(errno::ENOPERM)
    }

    /// Compile the plan to a Seatbelt (SBPL) profile — deterministic text:
    /// deny-default, reads = plan mounts + the fixed host shell runtime,
    /// writes = the build dir only, no network operation allowed.
    pub fn seatbelt_profile(&self) -> io::Result<String> {
        let mut p = String::new();
        p.push_str("(version 1)\n(deny default)\n");
        // Process machinery: builders fork/exec compilers (06 §3.1).
        p.push_str("(allow process-fork)\n(allow process-exec)\n");
        p.push_str("(allow signal (target same-sandbox))\n");
        p.push_str("(allow sysctl-read)\n");
        // Broad stat/existence visibility (metadata only — contents stay
        // gated). Needed by dyld and getcwd; a documented information leak.
        p.push_str("(allow file-read-metadata)\n");
        p.push_str("(allow file-read*\n  (literal \"/\")\n");
        // Host shell runtime — the escape-hatch read set, fixed and minimal:
        // dyld + libSystem + /bin/sh (via /private/var/select) + coreutils.
        for sys in [
            "/System",
            "/usr/lib",
            "/usr/share",
            "/usr/bin",
            "/bin",
            "/private/var/select",
            "/private/etc",
            "/Library/Preferences",
            "/dev",
        ] {
            p.push_str(&format!("  (subpath \"{sys}\")\n"));
        }
        for m in &self.mounts {
            if m.rights & RIGHT_READ != 0 {
                p.push_str(&format!("  (subpath \"{}\")\n", sbpl_safe(&m.source)?));
            }
        }
        p.push_str(")\n(allow file-write*\n");
        for m in &self.mounts {
            if m.rights & RIGHT_WRITE != 0 {
                p.push_str(&format!("  (subpath \"{}\")\n", sbpl_safe(&m.source)?));
            }
        }
        p.push_str("  (literal \"/dev/null\")\n  (literal \"/dev/tty\")\n)\n");
        p.push_str("(allow file-ioctl (literal \"/dev/null\") (literal \"/dev/tty\"))\n");
        p.push_str("(deny network*)\n");
        Ok(p)
    }
}

/// The host macOS-Seatbelt sandbox. See the module docs for the model/vehicle
/// split — the native OROS sandbox that consumes the same [`SandboxPlan`] is a
/// separate, as-yet-unwritten impl.
pub struct SeatbeltSandbox {
    /// Plans by scratch dir (`BuildEnv::cwd`), stashed at `prepare` for
    /// `spawn`/`collect_outputs` — the seam's `BuildEnv` stays unchanged.
    plans: Mutex<BTreeMap<String, SandboxPlan>>,
}

impl SeatbeltSandbox {
    /// Fail-closed constructor: errors unless the host enforcement facility
    /// exists (macOS `sandbox-exec`). No silent permissive fallback.
    pub fn new() -> io::Result<SeatbeltSandbox> {
        if !cfg!(target_os = "macos") {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "SeatbeltSandbox host vehicle requires macOS sandbox-exec; \
                 no enforcement facility on this host (see docs/shade/build-sandbox.md)",
            ));
        }
        if !Path::new("/usr/bin/sandbox-exec").exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "/usr/bin/sandbox-exec not found; cannot enforce the build sandbox",
            ));
        }
        Ok(SeatbeltSandbox { plans: Mutex::new(BTreeMap::new()) })
    }

    fn plan_for(&self, cwd: &str) -> io::Result<SandboxPlan> {
        self.plans.lock().unwrap().get(cwd).cloned().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "no sandbox plan prepared for this build")
        })
    }
}

impl BuildSandbox for SeatbeltSandbox {
    fn prepare(&self, spec: &SandboxSpec) -> Result<BuildEnv, SandboxError> {
        let plan = SandboxPlan::from_spec(spec)?;
        let env = BuildEnv {
            cwd: spec.scratch.to_string(),
            staging: spec.staging.to_string(),
            vars: plan.env.clone(),
        };
        self.plans.lock().unwrap().insert(env.cwd.clone(), plan);
        Ok(env)
    }

    fn spawn(
        &self,
        env: &BuildEnv,
        command: &str,
        log: &mut dyn BuildLogSink,
    ) -> Result<i32, SandboxError> {
        let plan = self.plan_for(&env.cwd)?;
        let profile = plan.seatbelt_profile()?;
        // The wrapper pins the umask before the recipe command; identity and
        // cwd are pinned by the spawn itself.
        let wrapped = format!("umask {BUILD_UMASK}; {command}");
        let mut cmd = Command::new("/usr/bin/sandbox-exec");
        cmd.arg("-p")
            .arg(profile)
            .arg("/bin/sh")
            .arg("-c")
            .arg(wrapped)
            .current_dir(&env.cwd)
            // The environment is the plan's table and nothing else — no host
            // inheritance of any kind.
            .env_clear()
            .envs(env.vars.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        crate::executor::spawn_capturing(cmd, log)
    }

    fn collect_outputs(
        &self,
        env: &BuildEnv,
        declared: &[String],
    ) -> Result<Vec<String>, SandboxError> {
        // Every declared output must exist…
        let mut out = Vec::with_capacity(declared.len());
        for rel in declared {
            let p = Path::new(&env.staging).join(rel);
            if !p.exists() {
                return Err(SandboxError::new(format!(
                    "declared output `{rel}` was not produced by the build"
                )));
            }
            out.push(p.to_string_lossy().into_owned());
        }
        // …and only declared top-level trees may exist under $out (06 §5
        // step 1): output confinement's staging half — the profile already
        // confined writes to the build dir; this rejects smuggling extra
        // trees into the store alongside the declared ones.
        let declared_tops: std::collections::BTreeSet<&str> = declared
            .iter()
            .filter_map(|d| d.split('/').next())
            .collect();
        for entry in fs::read_dir(&env.staging)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !declared_tops.contains(name.as_ref()) {
                return Err(SandboxError::new(format!(
                    "undeclared top-level entry `{name}` in $out (06 §5 step 1)"
                )));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod plan_tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("lythos-sbx-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    /// A spec over real dirs (canonicalization needs them to exist).
    fn mk_spec(root: &Path, inputs: &[PathBuf]) -> (PathBuf, PathBuf) {
        let scratch = root.join("build/abc-demo-1.0");
        let staging = scratch.join("out");
        fs::create_dir_all(scratch.join("tmp")).unwrap();
        fs::create_dir_all(&staging).unwrap();
        for i in inputs {
            fs::create_dir_all(i.join("bin")).unwrap();
        }
        (scratch, staging)
    }

    fn plan_with(root: &Path, inputs: Vec<PathBuf>, env: Vec<(String, String)>) -> SandboxPlan {
        let (scratch, staging) = mk_spec(root, &inputs);
        // The seam is `&str`-pathed; convert at the boundary (the dirs above
        // still exist on the host, which `from_spec`'s canonicalize needs).
        let scratch = scratch.to_str().unwrap().to_string();
        let staging = staging.to_str().unwrap().to_string();
        let inputs: Vec<String> = inputs.iter().map(|p| p.to_str().unwrap().to_string()).collect();
        let spec = SandboxSpec {
            store_name: "abc-demo-1.0",
            scratch: &scratch,
            staging: &staging,
            system: "x86_64-oros",
            env: &env,
            inputs: &inputs,
            jobs: 4,
        };
        SandboxPlan::from_spec(&spec).unwrap()
    }

    #[test]
    fn cap_set_is_minimal_and_named() {
        let root = tmp("caps");
        let dep = root.join("store/dd-dep-1.0");
        let plan = plan_with(&root, vec![dep], vec![]);

        // Exactly: one Filesystem READ per input, one Filesystem READ|WRITE
        // for the build dir, one Ipc WRITE log endpoint. Nothing else.
        assert_eq!(plan.caps.len(), 3);
        assert_eq!(plan.caps[0].kind, CapKind::Filesystem);
        assert_eq!(plan.caps[0].rights, RIGHT_READ);
        assert_eq!(plan.caps[1].kind, CapKind::Filesystem);
        assert_eq!(plan.caps[1].rights, RIGHT_READ | RIGHT_WRITE);
        assert_eq!(plan.caps[2].kind, CapKind::Ipc);
        assert_eq!(plan.caps[2].rights, RIGHT_WRITE);
        // No grant carries GRANT or REVOKE bits; no Device/Rollback/Memory.
        for c in &plan.caps {
            assert_eq!(c.rights & !(RIGHT_READ | RIGHT_WRITE), 0, "no GRANT/REVOKE bits");
            assert!(!matches!(c.kind, CapKind::Device | CapKind::Rollback | CapKind::Memory));
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fs_view_answers_abi_errnos() {
        let root = tmp("view");
        let dep = root.join("store/dd-dep-1.0");
        let other = root.join("store/zz-undeclared-1.0");
        fs::create_dir_all(&other).unwrap();
        let plan = plan_with(&root, vec![dep.clone()], vec![]);

        let dep_c = fs::canonicalize(&dep).unwrap();
        let other_c = fs::canonicalize(&other).unwrap();

        // Declared input: readable, not writable (EROFS — read-only mount).
        assert_eq!(plan.check_read(&dep_c.join("bin/dep")), Ok(()));
        assert_eq!(plan.check_write(&dep_c.join("bin/dep")), Err(errno::EROFS));
        // Undeclared store sibling: invisible — ENOPERM both ways.
        assert_eq!(plan.check_read(&other_c), Err(errno::ENOPERM));
        assert_eq!(plan.check_write(&other_c), Err(errno::ENOPERM));
        // Build dir: read+write.
        assert_eq!(plan.check_read(&plan.scratch.join("x")), Ok(()));
        assert_eq!(plan.check_write(&plan.staging.join("bin/demo")), Ok(()));
        // Anywhere else: ENOPERM.
        assert_eq!(plan.check_read(Path::new("/Users/nobody/secret")), Err(errno::ENOPERM));
        assert_eq!(plan.check_write(Path::new("/etc/hosts")), Err(errno::ENOPERM));
        // Network: no capability reaches it.
        assert_eq!(plan.check_network(), Err(errno::ENOPERM));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn plan_is_deterministic_and_mount_order_stable() {
        let root = tmp("det");
        let a = root.join("store/aa-a-1.0");
        let b = root.join("store/bb-b-1.0");
        let p1 = plan_with(&root, vec![a.clone(), b.clone()], vec![]);
        let p2 = plan_with(&root, vec![a.clone(), b.clone()], vec![]);

        assert_eq!(p1.mounts, p2.mounts, "same spec ⇒ same mounts, same order");
        assert_eq!(p1.env, p2.env, "same spec ⇒ same env, same order");
        assert_eq!(
            p1.seatbelt_profile().unwrap(),
            p2.seatbelt_profile().unwrap(),
            "same spec ⇒ byte-identical profile"
        );
        // dep order preserved, writable mount last.
        assert!(p1.mounts[0].target.ends_with("aa-a-1.0"));
        assert!(p1.mounts[1].target.ends_with("bb-b-1.0"));
        assert_eq!(p1.mounts[2].rights, RIGHT_READ | RIGHT_WRITE);
        assert!(p1.mounts[2].target.starts_with("/shade/build/"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn fixed_env_wins_over_recipe_collisions() {
        let root = tmp("envwin");
        let plan = plan_with(
            &root,
            vec![],
            vec![
                ("HOME".to_string(), "/tmp/evil".to_string()),
                ("TZ".to_string(), "PST8PDT".to_string()),
                ("GREETING".to_string(), "hello".to_string()),
            ],
        );
        // Later duplicate wins in Command::envs — fixed table is appended
        // after recipe env, so the *last* occurrence must be the fixed value.
        let last = |k: &str| {
            plan.env.iter().rev().find(|(key, _)| key == k).map(|(_, v)| v.clone()).unwrap()
        };
        assert_eq!(last("HOME"), SANDBOX_HOME);
        assert_eq!(last("TZ"), "UTC");
        assert_eq!(last("GREETING"), "hello", "non-colliding recipe env survives");
        // And the full fixed table is present.
        for k in ["PATH", "HOME", "OUT", "out", "TARGET", "TMPDIR", "SOURCE_DATE_EPOCH", "TZ", "LANG", "LC_ALL", "JOBS"] {
            assert!(plan.env.iter().any(|(key, _)| key == k), "missing fixed var {k}");
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn profile_denies_network_and_scopes_writes() {
        let root = tmp("sbpl");
        let dep = root.join("store/dd-dep-1.0");
        let plan = plan_with(&root, vec![dep], vec![]);
        let sbpl = plan.seatbelt_profile().unwrap();
        assert!(sbpl.contains("(deny default)"));
        assert!(sbpl.contains("(deny network*)"));
        // The scratch is the only plan-derived write subpath.
        let writes: Vec<&str> = sbpl
            .split("(allow file-write*")
            .nth(1)
            .unwrap()
            .lines()
            .filter(|l| l.contains("(subpath"))
            .collect();
        assert_eq!(writes.len(), 1, "exactly one writable subpath: {writes:?}");
        assert!(writes[0].contains("dd-dep-1.0") == false);
        let _ = fs::remove_dir_all(&root);
    }
}
