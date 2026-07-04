# Lythos UEFI / bare-metal bring-up plan

Target: first boot on real x86_64 UEFI hardware via the **Limine** native boot
protocol.  All steps must be validated under QEMU + OVMF before touching
physical iron.

---

## 1. Current state (Multiboot-based QEMU-only boot)

### 1.1 Boot entry — `kernel/src/arch/x86_64/boot.s`

The kernel image carries **two** Multiboot headers, placed inside the first
32 KiB of the `.boot` section so every compliant loader finds them.

| Header | Magic | Loader |
|--------|-------|--------|
| MB1 (flags `0x00010004`) | `0x1BADB002` | QEMU SeaBIOS / GRUB legacy |
| MB2 (arch=i386, only end tag) | `0xE85250D6` | GRUB 2 |

The MB1 header uses **bit 16 (a.out kludge)**: it supplies `header_addr`,
`load_addr`, `load_end_addr`, and `entry_addr` directly.  QEMU's SeaBIOS uses
these to load the flat binary image at physical `0x100000` without parsing the
ELF headers.  `load_end_addr = KERNEL_END` trims debug sections from the
loaded image.

At `_start` (32-bit protected mode, flat segments, interrupts off) the stub:

1. Saves `EAX` (mb_magic) and `EBX` (mb_info_ptr) before zeroing BSS.
2. Zeroes BSS (`__bss_start`–`__bss_end`) by REP STOSD.
3. Builds a minimal 4-level page table in BSS (P4/P3/P2, each one 4 KiB page):
   - P2 contains 512 × 2 MiB huge-page entries (PS=1) covering physical 0–1 GiB.
   - This identity map is the only mapping active until `vmm::init()`.
4. Enables PAE → sets EFER.LME → enables paging → enters long mode.
5. Far-jumps to a 64-bit entry stub that reloads segment registers and calls
   `kmain(mb_magic: u32, mb_info: u64)`.

`mb_magic` is passed in RDI, `mb_info` (zero-extended 32-bit physical address)
in RSI, matching the System V AMD64 calling convention.

### 1.2 Physical memory manager — `kernel/src/pmm.rs`

`pmm::init(mb_magic, mb_info)` is the primary consumer of the Multiboot info
block.  It handles **both** wire formats:

**MB1 path** (`mb_magic == 0x2BADB002`, flag bit 6 set):
- Reads `mmap_length` at `mb_info + 44` and `mmap_addr` at `mb_info + 48`
  (both 32-bit).
- Iterates variable-length entries: 4-byte `size` field, 8-byte `base_addr`,
  8-byte `length`, 4-byte `type` (1 = usable RAM).
- Fallback if bit 6 clear: uses `mem_lower` (offset +4) and `mem_upper`
  (offset +8) in KiB.

**MB2 path** (`mb_magic == 0x36D76289`):
- Walks the tag list starting at `mb_info + 8`.
- Finds tag type 6 (memory map): fixed 24-byte entries (8-byte base,
  8-byte length, 4-byte type, 4-byte reserved).

After marking available RAM, PMM re-marks:
- The kernel image (`KERNEL_START`–`KERNEL_END`) as used.
- Frame 0 (BIOS data area) as used.
- Physical range `0x400000`–`0x480000` (`LYTHD_MODULE_ADDR`, 512 KiB) as
  used — lythd is loaded there by QEMU via `-device loader,addr=0x400000`.

### 1.3 Framebuffer — `kernel/src/framebuffer.rs`

`framebuffer::init(mb_magic, mb_info)` checks two sources in order:

**MB1** (flag bit 12 at `mb_info + 0`):
- `framebuffer_addr` at `mb_info + 88` (u64)
- `framebuffer_pitch` at `mb_info + 96` (u32, bytes per scanline)
- `framebuffer_width` / `_height` at `+100` / `+104` (u32 pixels)
- `framebuffer_bpp` at `+108` (u8), `framebuffer_type` at `+109`
  (1 = RGB direct-colour)

**MB2** (tag type 8):
- Same fields at fixed offsets within the tag.

**Bochs VBE fallback** (QEMU `-vga std` / stdvga, PCI 0x1234:0x1111):
- Probe PCI config space for vendor/device match.
- Program resolution via I/O ports 0x01CE/0x01CF (VBE_INDEX/VBE_DATA).
- Read framebuffer physical address from BAR0.

All three paths produce the same result: a physical address, width, height,
pitch, and bpp.  The kernel maps each 4 KiB page at `FB_VA_BASE =
0xFFFF_E000_0000_0000` with `KERNEL_RW | PCD` (cache-disable for MMIO).

### 1.4 TTY output — `kernel/src/serial.rs`

All kernel text (`kprint!` / `kprintln!`) goes to **COM1 (0x3F8)**, a 16550
UART at 115200 8N1.  No path today writes to VGA text memory (0xB8000).  The
framebuffer is used only for the graphical splash screen; it is not a text
console.

The UART is the only reliable output path on bare metal without a display
driver.  It must be preserved at least through early bring-up.

### 1.5 Keyboard — `keyboard::init()` / `kernel/src/keyboard.rs`

The i8042 PS/2 controller is initialised once, after the I/O APIC:

1. Flush the output buffer (read 0x60 until status bit 0 is clear).
2. Disable both PS/2 ports (commands 0xAD / 0xA7 to 0x64).
3. Read config byte (command 0x20), clear translation bit (bit 6, which
   would convert scan-code set 2 → set 1), set P1 IRQ enable (bit 0).
4. Write config byte back (command 0x60).
5. Re-enable port 1 (command 0xAE).
6. Register `kbd_isr_stub` at IDT vector 36.
7. Wire IOAPIC GSI 1 → vector 36, edge-triggered, active-high (ISA default).

The IRQ handler reads a scan code from port 0x60, runs a state machine for
prefix bytes (0xE0 extended, 0xF0 break), translates via a 256-entry set-2
lookup table, and pushes the decoded ASCII / control byte into a 256-byte
circular ring buffer.  Extended keys (arrows, Del, Home, End, PgUp, PgDn)
emit ANSI escape sequences.  EOI is sent to the local APIC after processing.

`SYS_SERIAL_READ` in the syscall dispatcher reads first from this ring buffer,
then from the UART RX FIFO; it yields if both are empty.

### 1.6 Interrupt topology

```
8259 PIC (legacy)
  idt::init()   — remap IRQ0-15 → vectors 0x20-0x2F, then mask all
  apic::init()  — disable_pic(): remap again (same vectors), mask all
  Result: PIC fully masked, all interrupt delivery through APIC

Local APIC
  Base: IA32_APIC_BASE MSR (hardware-reported, mapped at APIC_VIRT_NOMINAL + kaslr_offset)
  Timer: periodic, 1 ms tick, vector 32
  Spurious: vector 255 (no EOI)

I/O APIC
  Base: hardcoded 0xFEC00000 (QEMU default; ioapic.rs line ~20)
  GSI 1 → vector 36 (keyboard, edge/active-high)
  All other GSIs masked at init

RSDP: not read; ACPI only used for QEMU PM1a power-off (port 0x604)
```

---

## 2. Migration checklist — every Multiboot touch point

The following are all locations that must change or be removed to drop
Multiboot support.  Each line is a discrete migration item.

### A. Boot assembly — `kernel/src/arch/x86_64/boot.s`

| Item | Detail |
|------|--------|
| A1 | MB1 header block (`0x1BADB002`, flags, checksum, a.out kludge fields) |
| A2 | MB2 header block (`0xE85250D6`, architecture, size, end tag) |
| A3 | Save `EAX`/`EBX` → `ESI`/`EBP` at `_start` entry (mb_magic, mb_info_ptr) |
| A4 | `mb_magic` / `mb_info_ptr` BSS storage slots |
| A5 | Load `mb_magic`/`mb_info_ptr` into `RDI`/`RSI` before calling `kmain` |
| A6 | Boot-time 0–1 GiB identity map (P4/P3/P2 in BSS) — replace with Limine's HHDM or keep for transition |
| A7 | MB1 video mode request fields (1024×768×32) — unused but present in the header |

### B. Linker script — `kernel/boot/linker/x86_64.ld`

| Item | Detail |
|------|--------|
| B1 | Physical load address `0x100000` (ORIGIN of `.boot`) |
| B2 | `.boot` section (first in link order, within first 32 KiB) — Limine uses `.limine_requests` |
| B3 | `KERNEL_END` symbol (used by MB1 a.out kludge to skip debug sections) |
| B4 | Add `.limine_requests` section (must be present, readable, writable) |

### C. Physical memory manager — `kernel/src/pmm.rs`

| Item | Detail |
|------|--------|
| C1 | `init(mb_magic: u32, mb_info: u64)` signature → `init(mmap: &[LimineMmapEntry])` |
| C2 | `MB1_MAGIC = 0x2BADB002` constant |
| C3 | `MB2_MAGIC = 0x36D76289` constant |
| C4 | MB1 mmap parsing (flag bit 6, offsets +44/+48, variable-size entries) |
| C5 | MB1 fallback mem_lower/mem_upper parsing (flag bit 0, offsets +4/+8) |
| C6 | MB2 tag-list walker and tag-6 mmap parser |
| C7 | `LYTHD_MODULE_ADDR = 0x400000` / `LYTHD_MODULE_MAX` — replace with Limine module response |
| C8 | PMM re-mark of lythd physical region — replace with PMM re-mark of Limine module(s) |

### D. Framebuffer — `kernel/src/framebuffer.rs`

| Item | Detail |
|------|--------|
| D1 | `init(mb_magic: u32, mb_info: u64)` signature |
| D2 | MB1 flag bit 12 check and field offsets (+88 addr, +96 pitch, +100 w, +104 h, +108 bpp) |
| D3 | MB2 tag-type-8 parser |
| D4 | Bochs VBE fallback path — can keep for `-vga std` QEMU testing, remove for pure-Limine path |

### E. Kernel entry point — `kernel/src/main.rs`

| Item | Detail |
|------|--------|
| E1 | `kmain(mb_magic: u32, mb_info: u64)` signature |
| E2 | `pmm::init(mb_magic, mb_info)` call |
| E3 | `framebuffer::init(mb_magic, mb_info)` call |

### F. PIC (8259) — two locations

| Item | Detail |
|------|--------|
| F1 | `idt::init()`: initial PIC remap to 0x20–0x2F + full mask (required before APIC to silence spurious IRQs) |
| F2 | `apic::init()` → `disable_pic()`: second remap + full mask (belt-and-suspenders) |

Both are correct and safe; no change needed for Limine.  Limine does **not**
guarantee PIC state on entry.  The double-remap-and-mask pattern must be kept.

### G. Not present (confirmed by grep)

- No VGA text-mode writes to 0xB8000.
- No use of 0xB8000 in any `.rs` file.
- No Multiboot module list parsing beyond the lythd reserve in PMM.

---

## 3. Target state — Limine native boot protocol

### 3.1 Why Limine

Limine is the only actively maintained bootloader with a stable UEFI-native
boot protocol that:
- Delivers the kernel already in 64-bit long mode with a valid page table.
- Provides a structured response system (no raw struct-offset arithmetic).
- Supports QEMU + OVMF for pre-hardware validation.
- Gives us the framebuffer, memory map, HHDM offset, and RSDP as typed
  responses, eliminating all the Multiboot tag-walk and offset arithmetic.

### 3.2 What Limine does for us at entry

By the time Limine jumps to the kernel entry point:

- CPU is in 64-bit long mode, interrupts off, SSE/SSE2 enabled.
- A valid 4-level page table is installed; Limine's HHDM maps all physical RAM
  at a fixed virtual offset (returned via the HHDM response).
- The kernel image itself is mapped at the virtual address specified in the
  linker script (or at physical + HHDM if using the default).
- A small boot-services stack is live; we must switch to our own kernel stack
  before enabling interrupts.
- UEFI firmware has already configured the IOAPIC and other ACPI tables; the
  RSDP pointer is provided in the Limine RSDP response.

The boot.s 32→64 stub (A1–A7) becomes **entirely unnecessary** and should be
deleted.

### 3.3 Limine request/response contract

All requests are static variables in `.limine_requests` (or any section
Limine can find; convention is a dedicated section).  Limine fills the
`response` pointer before calling the entry point.  Any response that remains
null means the feature is unsupported or the request tag was not found.

```rust
// All requests must be present in a section Limine can see.
// Use #[link_section = ".limine_requests"] and #[used].

// Required: base revision (must match bootloader)
static BASE_REVISION: LimineBaseRevision = LimineBaseRevision::new(2);

// Required: entry point (tells Limine where to jump after filling responses)
static ENTRY_POINT: LimineEntryPointRequest = LimineEntryPointRequest::new(kernel_main_limine);

// Framebuffer
static FRAMEBUFFER: LimineFramebufferRequest = LimineFramebufferRequest::new();
// → response.framebuffers[0]:
//     address: *mut u8  (physical, already in HHDM)
//     width, height:    u64 (pixels)
//     pitch:            u64 (bytes per scanline)
//     bpp:              u16 (bits per pixel, expect 32)
//     memory_model:     u8  (1 = RGB)
//     red/green/blue:   {mask_size: u8, mask_shift: u8}

// Memory map
static MEMMAP: LimineMemmapRequest = LimineMemmapRequest::new();
// → response.entries: &[*mut LimineMemmapEntry]
//     base:  u64  (physical address)
//     length: u64
//     typ:   u64  (0=usable, 1=reserved, 3=ACPI reclaimable, 4=NVS,
//                  5=bad, 10=bootloader-reclaimable, 11=kernel/modules,
//                  12=framebuffer)

// Higher-half direct map offset
static HHDM: LimineHhdmRequest = LimineHhdmRequest::new();
// → response.offset: u64
//     phys_addr + offset = virtual addr of that physical page in the HHDM
//     Typically 0xFFFF800000000000 but MUST be read from the response.

// RSDP
static RSDP: LimineRsdpRequest = LimineRsdpRequest::new();
// → response.address: u64 (virtual address of the RSDP structure)
//     Parse RSDP → RSDT/XSDT → MADT to find the IOAPIC physical base.

// Kernel modules (replaces QEMU -device loader for lythd)
static MODULE: LimineModuleRequest = LimineModuleRequest::new();
// → response.modules[]: {path, cmdline, address: *mut u8, size: u64}
//     lythd binary embedded in the Limine config as a module at path /lythd
```

### 3.4 New boot entry flow

```
Limine fills responses
  → kernel_main_limine() [replaces kmain, no mb_magic/mb_info args]
      serial::init()                    — COM1 still first
      kaslr::init()                     — unchanged
      gdt::init()                       — unchanged
      idt::init()                       — unchanged (includes PIC remap+mask)
      pmm::init_from_limine(memmap)     — replaces pmm::init(mb_magic, mb_info)
      vmm::init_from_limine(hhdm_off)   — reconcile with Limine's page tables
      framebuffer::init_from_limine(fb) — replaces framebuffer::init(mb,mb)
      heap::init()                      — unchanged
      ... rest of kmain unchanged ...
      apic::init()                      — includes disable_pic(), unchanged
      ioapic::init_from_madt(rsdp)      — replaces hardcoded 0xFEC00000
      keyboard::init()                  — unchanged
      lythd_elf = limine_module(MODULE) — replaces rfs::load_file + 0x400000
      elf::exec(lythd_elf, caps, &[])   — unchanged
```

### 3.5 Framebuffer console (target)

The splash screen already uses the linear framebuffer.  The target adds a
simple text renderer on top:

- A fixed-width bitmap font (8×16 or similar, embedded in `.rodata`).
- A `con_putchar(ch: u8)` that blits one glyph at the cursor position using
  the framebuffer pitch and bpp from the Limine response.
- ANSI colour support is optional; monochrome white-on-black is sufficient
  for bring-up.
- `kprint!` / `kprintln!` continue to drive COM1.  The framebuffer console
  is a parallel output added to the same macros (or a separate `fbprint!`).
- No heap required: cursor position is a pair of `AtomicU32` statics; glyph
  blit uses the already-mapped `FB_VA_BASE`.

Until a font renderer exists, the framebuffer outputs only the splash screen.
COM1 remains the authoritative debug output path throughout bring-up.

### 3.6 i8042 keyboard after Limine

The i8042 controller and its interrupt wiring do not change:
- GSI 1 is still the PS/2 keyboard IRQ on all PC-compatible hardware.
- IOAPIC redirection entry for GSI 1 is edge-triggered, active-high (ISA).
- `keyboard::init()` remains unchanged.

The only bring-up risk is the IOAPIC base address (see §4, risk R3).

### 3.7 lythd delivery — replacing the 0x400000 hack

Currently lythd is loaded by QEMU at physical 0x400000 via:
```
-device loader,file=rootfs/lth/system/init,addr=0x400000,force-raw=on
```
and the PMM explicitly reserves `0x400000`–`0x480000`.

Under Limine, lythd is specified as a module in `limine.conf`:
```
MODULE_PATH=boot:///lythd
```
Limine loads it at an arbitrary physical address within usable RAM and reports
`address` + `size` in the module response.  PMM must re-mark that region as
used instead of hardcoding 0x400000.

The disk-image builder (`tools/mkrfs`) places lythd in the RFS filesystem;
that path (`/lth/system/init` via `rfs::load_file`) can serve as an
alternative to Limine modules once RFS is mounted.  Either works; the Limine
module route is simpler for early bring-up because it does not require a
working block driver.

---

## 4. Acceptance criteria by step

Each step must pass under **QEMU + OVMF** before proceeding.  OVMF provides a
UEFI firmware environment equivalent to real hardware (I/O APIC, ACPI tables,
no SeaBIOS Multiboot loader).

### Step 1 — Limine responds, kernel enters, COM1 outputs

**What changes:** Add Limine request structs; replace `boot.s` with a minimal
Limine-compatible entry (no 32→64 stub, no MB headers); update linker script.

**Accept when:**
- `qemu-system-x86_64 -bios /usr/share/OVMF/OVMF.fd -kernel <image>` (with
  Limine loader wrapping) produces COM1 output containing the Lythos banner.
- All existing QEMU + SeaBIOS `make run` tests still pass (keep both builds
  until Step 2 completes).

### Step 2 — PMM initialised from Limine memory map

**What changes:** `pmm::init_from_limine(entries)` replaces MB1/MB2 parsing;
lythd module address replaces 0x400000 constant.

**Accept when:**
- PMM reports a plausible free-frame count (≥ 32 MiB usable for a 128 MiB
  QEMU VM).
- PMM smoke test (alloc/free 1000 frames, verify addresses) passes on COM1.
- No frame 0 allocated (BIOS data area protection).

### Step 3 — Framebuffer mapped from Limine response

**What changes:** `framebuffer::init_from_limine(fb)` reads address/pitch/
width/height/bpp directly from the Limine response; no MB field offsets.

**Accept when:**
- Splash screen renders correctly in `qemu-system-x86_64 ... -vga virtio`
  (Limine will provide a framebuffer even without `-vga std`).
- `framebuffer::dimensions()` returns the correct pixel dimensions.

### Step 4 — IOAPIC base from ACPI MADT

**What changes:** `ioapic::init()` reads the IOAPIC physical base from the
MADT (parse RSDP → XSDT/RSDT → MADT, find type-1 record).

**Accept when:**
- IOAPIC initialises without kernel panic.
- Timer ticks (APIC timer vector 32) fire and `apic::ticks()` advances.
- `ioapic::entry_count()` returns the expected GSI count (≥ 24 for QEMU).

### Step 5 — Keyboard working end-to-end

**What changes:** None to `keyboard.rs`; depends on Step 4 IOAPIC init being
correct.

**Accept when:**
- lysh login prompt appears on the framebuffer console (or COM1 with
  the socket bridge).
- Keystrokes typed at the QEMU window (or via the socket bridge) are received
  by `SYS_SERIAL_READ` (ring buffer non-empty).
- Login completes successfully.

### Step 6 — lythd loaded via Limine module (removes 0x400000 dependency)

**What changes:** Remove QEMU `-device loader` from Makefile; PMM marks the
Limine module region; `kmain` reads lythd ELF bytes from the module response
instead of `rfs::load_file`.

**Accept when:**
- lythd boots and the supervisor loop starts (COM1 prints
  `[lythd] entering supervisor loop`).
- All managed services spawn correctly.

### Step 7 — Framebuffer text console (optional for initial bare-metal boot)

**What changes:** Add bitmap font renderer; `kprint!` / `kprintln!` additionally
write to framebuffer at `FB_VA_BASE`.

**Accept when:**
- Kernel boot messages visible in QEMU window without needing COM1 capture.
- lysh login prompt visible on framebuffer.

### Step 8 — First bare-metal boot

**Prerequisites:** Steps 1–6 green under QEMU + OVMF.

**Accept when:**
- Hardware boots from Limine USB/disk, COM1 output (USB-serial adapter)
  shows the full boot sequence through `[boot] lythd launched`.
- No triple fault, no unexpected reset.

---

## 5. Open risks

### R1 — Boot-time page table ownership (HIGH)

Limine installs its own PML4 with an HHDM mapping.  Our `vmm::init()` currently
builds a fresh PML4 and installs it via CR3, discarding the boot tables
entirely.  If any Limine response pointer lives in a page that was only mapped
in the Limine tables and not in our new PML4, writing CR3 in `vmm::init()`
will cause an immediate page fault.

Mitigation: read all Limine response data before `vmm::init()` and copy it to
kernel-owned structures.  Alternatively, build the new PML4 by copying Limine's
kernel mappings rather than starting from scratch.  This needs a careful audit
of when `vmm::init()` fires relative to the last dereference of a Limine
response pointer.

### R2 — lythd OROX scan of `/bin` (MEDIUM)

`lythd` scans `/bin/` via `sys_readdir` to find OROX-prefixed binaries.  The
current FHS spec places system binaries in `/lth/bin/`; `/bin/` is a compat
symlink.  If RFS is not mounted (early bring-up without a block device),
`load_orox_manifests()` silently returns an empty list.  This is benign but the
error message `[lythd] /bin not found — no OROX binaries` is confusing.  Track
separately; not blocking.

### R3 — IOAPIC base address on real hardware (HIGH)

`kernel/src/ioapic.rs` hardcodes `0xFEC00000` as the I/O APIC MMIO base.  On
QEMU this is always correct.  On real hardware the IOAPIC may be at a different
address (recorded in the ACPI MADT type-1 record).  Without MADT parsing, the
IOAPIC init will map the wrong page, the IOAPIC register reads/writes will
produce garbage, and GSI 1 (keyboard) will never fire.

Step 4 above fixes this.  It is the single highest-risk item for bare-metal
bring-up because a wrong IOAPIC base produces no obvious diagnostic — the
kernel will appear to boot but keyboard input will be dead and no useful error
will print.

### R4 — KASLR entropy before heap (LOW-MEDIUM)

`kaslr::init()` calls RDRAND or falls back to RDTSC.  Under Limine, RDRAND is
available (Limine does not strip CPU features).  The concern is that RDTSC at
entry gives very low entropy (counter may be near zero after reset).  The
current 15-bit range (0–128 MiB) keeps the risk acceptable for bring-up but
should be extended when RDRAND is unavailable on older hardware.

### R5 — PIC spurious interrupts during transition (LOW)

`idt::init()` remaps the 8259 to 0x20–0x2F and masks all lines.  `apic::init()`
remaps and masks again.  Between the two calls (GDT/IDT loaded but APIC not yet
active), a spurious PIC IRQ on vector 7 or 15 could fire an unhandled exception.
In practice this window is < 1 µs and never observed, but it is a latent risk
on noisy real hardware.  Adding an explicit `cli` barrier at the start of
`apic::init()` and an `sti` at the end (after APIC timer is live) would close
it cleanly.

### R6 — Limine page-fault on null response pointer (MEDIUM)

If a Limine request is silently ignored (wrong base revision, unsupported
feature), the response pointer remains null.  The current init code dereferences
response pointers unconditionally.  Each init function must check for null and
panic with a descriptive message rather than faulting at an opaque address.

### R7 — SMP AP trampoline at physical 0x8000 (LOW)

`smp::init()` copies the AP trampoline to physical address 0x8000 (SIPI vector
`0x08 << 12`).  Limine's memory map may report 0x8000 as reserved (conventional
memory below 1 MiB often is).  PMM must be queried before writing the trampoline
— if Limine marks 0x8000 as unusable, the trampoline must use a different
sub-1-MiB page.  Check the Limine memory map type for `base=0x0000, length=640K`
(conventional low RAM, typically reported as usable).
