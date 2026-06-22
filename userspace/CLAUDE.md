# OROS userspace — development reference

Userspace for Lythos. All programs are `no_std` static ELF64 binaries targeting `x86_64-oros.json`. No dynamic linker, no libc from the host toolchain.

## Build

```bash
make oros              # build all userspace + copy to rootfs/lth/bin/
# Single crate (from repo root):
cargo +nightly build --target targets/x86_64-oros.json \
  -Z build-std=core,alloc,compiler_builtins \
  -Z build-std-features=compiler-builtins-mem \
  -Z json-target-spec \
  -p <crate-name>
```

`userspace/.cargo/config.toml` sets `[build] target = "../targets/x86_64-oros.json"` and the build-std flags — these apply automatically when building inside the `userspace/` subtree. The userspace linker script is `userspace/lib/lythos-rt/userspace.ld`, wired via rustflags in the root `.cargo/config.toml`.

## Runtime crate dependencies

New userspace programs use these crates:

| Crate | Role |
|-------|------|
| `lythos-rt` | Runtime: `_start` entry point, heap allocator, panic handler, `#[no_std]` glue |
| `lythos-libstd` | Native stdlib wrappers: `fs`, `io`, `net`, `process`, `sync`, `time` |
| `lythos-syscall` | Raw syscall instruction stubs (`syscall0`–`syscall6`, x86_64 only) |
| `lythos-abi` | ABI types: syscall numbers, errno, `CapHandle`, boundary structs |

For the syscall/errno/struct contract, read `abi/lythos-abi/src/` and `docs/spec/syscalls.md`. Do not restate ABI values in userspace code — import from `lythos-abi`.

## SYS_MMAP — no physical address

`SYS_MMAP` allocates an anonymous page from the kernel PMM. **There is no physical address argument.** The kernel always picks the frame.

```
a1 = virtual address (4 KiB-aligned, ≥ 0x4000_0000, < canonical split)
a2 = reserved — must be 0
a3 = page flags (bit 0 = Present, bit 1 = Writable, bit 63 = NX); User bit forced set
```

Returns 0 on success. Caller must hold a `CapKind::Memory` cap with `WRITE` right. Passing a physical address in a2 is silently ignored (it is reserved).

## Syscall and errno contract

The full syscall table (numbers 0–55), error sentinel values, register conventions, and struct byte layouts are in `docs/spec/syscalls.md`. The machine-readable version is `abi/lythos-abi`. Do not hardcode syscall numbers or errno values — use the constants from `lythos-abi`.

## lythd boot protocol

lythd (PID-1) receives three capability handles at fixed slots from `kmain`:

| Handle | Kind | Rights | Contents |
|--------|------|--------|----------|
| 0 | `Memory` | ALL | All free physical frames at boot |
| 1 | `Rollback` | ALL | `SYS_ROLLBACK` gate — only lythd holds this |
| 2 | `Ipc` | ALL | Boot IPC endpoint with one pre-queued `BootInfo` message |

**First action must be** `SYS_IPC_RECV` on handle 2 to consume the `BootInfo` message (64 bytes). The `BootInfo` layout is in `docs/spec/ipc.md` and `abi/lythos-abi/src/structs.rs`.

## Install paths

Built binaries go to `rootfs/lth/bin/` (copied by `make oros`). At runtime they live at `/lth/bin/` — see `docs/spec/fhs.md`. Do not hardcode `/bin/`.

## Address space

| Region | Address |
|--------|---------|
| User code | `0x0000_0001_0000_0000`+ (above kernel 0→1 GiB identity map) |
| User stacks | `0x0000_7FFF_0000_0000`+ (allocated by kernel on `SYS_EXEC`) |
| Kernel (inaccessible) | `0xFFFF_8000_0000_0000`+ |

Avoid placing code below `0x4000_0000` — the kernel's identity map covers 0→1 GiB.
