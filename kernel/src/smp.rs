/// Multi-processor support — AP startup and per-CPU initialisation.
///
/// ## AP startup sequence
///
/// 1. `smp::init()` (called by the BSP after all single-core subsystems are
///    up) discovers how many logical processors the package has via CPUID.
/// 2. It copies the AP trampoline to physical 0x8000, writes per-AP data
///    (PML4 address, kernel stack pointer, and the Rust `ap_entry` address)
///    into the trampoline page's data area, then sends INIT–SIPI–SIPI IPIs
///    to all non-BSP processors using the APIC "All Excluding Self"
///    destination shorthand.
/// 3. Each AP executes the trampoline: 16-bit real mode → 32-bit protected
///    mode → 64-bit long mode → `ap_entry()`.
/// 4. `ap_entry()` initialises per-AP state (GDT/TSS, syscall MSRs, local
///    APIC) and enters an idle loop.  Task migration to APs is deferred to
///    a future step.
///
/// ## Per-CPU data
///
/// Each AP (and the BSP) has a `PerCpu` slot indexed by LAPIC ID.  The BSP
/// populates slot 0 during `smp::init()`.  APs populate their slots inside
/// `ap_entry()`.

use core::arch::global_asm;
use core::sync::atomic::{AtomicUsize, Ordering};

// ── Trampoline assembly (included at compile time) ────────────────────────

global_asm!(
    include_str!("arch/x86_64/ap_trampoline.s"),
    options(att_syntax)
);

unsafe extern "C" {
    static ap_trampoline_start: u8;
    static ap_trampoline_end:   u8;
}

// ── Trampoline page layout (when code runs at physical 0x8000) ────────────

const TRAMPOLINE_PHYS: u64 = 0x8000;

/// Offset within the page of the `ap_entry` fn-pointer slot.
const OFF_AP_ENTRY: usize = 0xFD0;
/// Offset within the page of the per-AP kernel stack-top slot.
const OFF_AP_STACK: usize = 0xFD8;
/// Offset within the page of the BSP PML4 physical address slot.
const OFF_BSP_CR3:  usize = 0xFE0;
/// Offset within the page of the AP-online byte (written by each AP).
const OFF_AP_ONLINE: usize = 0xFE8;

// ── Per-CPU data ──────────────────────────────────────────────────────────

pub const MAX_CPUS: usize = 8;

/// Per-logical-processor data, indexed by LAPIC ID.
#[repr(C)]
pub struct PerCpu {
    /// LAPIC ID of this processor.
    pub lapic_id: u8,
    /// Set to 1 when this CPU has completed `ap_entry` initialisation.
    pub online: bool,
}

impl PerCpu {
    const fn zero() -> Self { Self { lapic_id: 0, online: false } }
}

// `static mut` is safe here: each slot is written exactly once (by its own
// CPU) before it is ever read.  The kernel is single-threaded until APs
// start, and after that each AP writes only its own slot.
static mut PER_CPU: [PerCpu; MAX_CPUS] = [
    PerCpu::zero(), PerCpu::zero(), PerCpu::zero(), PerCpu::zero(),
    PerCpu::zero(), PerCpu::zero(), PerCpu::zero(), PerCpu::zero(),
];

/// Count of APs that have completed `ap_entry` and signalled online.
static AP_ONLINE_COUNT: AtomicUsize = AtomicUsize::new(0);

// ── AP kernel stacks ──────────────────────────────────────────────────────

const AP_STACK_PAGES: usize = 16; // 64 KiB per AP
const AP_STACK_BYTES: usize = AP_STACK_PAGES * 4096;
const MAX_APS: usize = MAX_CPUS - 1;

static mut AP_STACKS: [[u8; AP_STACK_BYTES]; MAX_APS] =
    [[0u8; AP_STACK_BYTES]; MAX_APS];

// ── LAPIC MSR / register helpers ──────────────────────────────────────────


/// Read the LAPIC ID of the calling processor.
///
/// Delegates to `apic::lapic_id()` which uses the kernel-mapped APIC virtual
/// address rather than the physical base — safe above the 1 GiB identity map.
pub fn lapic_id() -> u8 {
    crate::apic::lapic_id()
}

/// Write `val` to the APIC ICR (triggers IPI).
///
/// Writes ICR_HIGH first (destination), then ICR_LOW (delivery — this triggers
/// the IPI).  Polls the delivery-status bit (bit 12 of ICR_LOW) until idle.
fn icr_write(base: u64, high: u32, low: u32) {
    unsafe {
        core::ptr::write_volatile((base + 0x310) as *mut u32, high);
        core::ptr::write_volatile((base + 0x300) as *mut u32, low);
        // Poll until Send Pending (bit 12) clears.
        while core::ptr::read_volatile((base + 0x300) as *const u32) & (1 << 12) != 0 {
            core::hint::spin_loop();
        }
    }
}

/// Busy-wait for approximately `ms` milliseconds using the APIC tick counter.
fn wait_ms(ms: u64) {
    let t0 = crate::apic::ticks();
    while crate::apic::ticks() < t0 + ms {
        core::hint::spin_loop();
    }
}

// ── AP entry (called from the trampoline in 64-bit mode) ──────────────────

/// Entry point for each Application Processor after the trampoline.
///
/// `ap_idx` is the zero-based AP index (0 = first AP, 1 = second, …).
/// It is passed by value in %rdi per the SysV AMD64 ABI.
///
/// This function must never return.
#[unsafe(no_mangle)]
pub extern "C" fn ap_entry(ap_idx: u64) -> ! {
    // ── IDT ───────────────────────────────────────────────────────────────
    // Load the BSP's already-initialised IDT so exceptions and timer
    // interrupts are dispatched correctly on this AP.
    crate::idt::ap_load();

    // ── Local APIC ────────────────────────────────────────────────────────
    // Enable the AP's local APIC and start its 1 ms periodic timer using
    // the calibration stored by the BSP during apic::init().
    crate::apic::ap_apic_init();

    // ── Per-CPU data ──────────────────────────────────────────────────────
    let id = lapic_id();
    let idx = id as usize;
    if idx < MAX_CPUS {
        unsafe {
            PER_CPU[idx].lapic_id = id;
            PER_CPU[idx].online   = true;
        }
    }
    AP_ONLINE_COUNT.fetch_add(1, Ordering::Release);

    crate::kprintln!("[smp] AP {} (LAPIC {}) online", ap_idx, id);

    // Enable interrupts and idle until the scheduler assigns work to this AP.
    loop {
        unsafe { core::arch::asm!("sti; hlt", options(nostack)) };
    }
}

// ── BSP-side AP startup ───────────────────────────────────────────────────

/// Detect the number of logical processors via CPUID and start all APs.
///
/// Must be called after `apic::init()` (APIC online), `task::init()`
/// (scheduler ready), `gdt::init()`, and `syscall::init()` (so APs can
/// inherit the configuration).
pub fn init() {
    // CPUID leaf 1 EBX[23:16] = "Maximum number of addressable logical
    // processor IDs in the physical package".  Subtract 1 for the BSP.
    let ebx: u32;
    unsafe {
        core::arch::asm!(
            "push rbx",
            "mov eax, 1",
            "cpuid",
            "mov {:e}, ebx",
            "pop rbx",
            out(reg) ebx,
            lateout("eax") _,
            lateout("ecx") _,
            lateout("edx") _,
            options(nostack),
        );
    }
    let total_cpus = ((ebx >> 16) & 0xFF) as usize;
    let ap_count   = total_cpus.saturating_sub(1).min(MAX_APS);

    // Record BSP per-CPU data (LAPIC ID 0 on QEMU).
    let bsp_id = lapic_id() as usize;
    if bsp_id < MAX_CPUS {
        unsafe {
            PER_CPU[bsp_id].lapic_id = bsp_id as u8;
            PER_CPU[bsp_id].online   = true;
        }
    }

    if ap_count == 0 {
        crate::kprintln!("[smp] uniprocessor — no APs to start");
        return;
    }

    crate::kprintln!("[smp] starting {} AP(s)…", ap_count);

    // ── Copy trampoline to physical 0x8000 ───────────────────────────────
    let src_start = &raw const ap_trampoline_start as *const u8;
    let src_end   = &raw const ap_trampoline_end   as *const u8;
    let tlen      = src_end as usize - src_start as usize;
    assert!(tlen <= 4096, "smp: trampoline too large ({} bytes)", tlen);

    unsafe {
        core::ptr::copy_nonoverlapping(src_start, TRAMPOLINE_PHYS as *mut u8, tlen);
        // Zero the per-AP data area so stale bytes don't mislead the AP.
        core::ptr::write_bytes(
            (TRAMPOLINE_PHYS as usize + 0xFD0) as *mut u8,
            0,
            0x30,
        );
    }

    // Write the BSP's PML4 physical address into the trampoline data area.
    let pml4_phys = crate::vmm::kernel_pml4().as_u64();
    unsafe {
        core::ptr::write_volatile(
            (TRAMPOLINE_PHYS as usize + OFF_BSP_CR3) as *mut u64,
            pml4_phys,
        );
    }

    // Write the `ap_entry` Rust function pointer.
    unsafe {
        core::ptr::write_volatile(
            (TRAMPOLINE_PHYS as usize + OFF_AP_ENTRY) as *mut u64,
            ap_entry as *const () as u64,
        );
    }

    // Use the already-mapped APIC virtual address for ICR writes.
    // The physical LAPIC base (0xFEE00000) is above the 1 GiB identity map
    // and must not be accessed directly.
    let lapic_base = crate::apic::mmio_va();

    // SIPI vector = physical_page >> 12.  For 0x8000: vector = 0x08.
    let sipi_vec: u32 = (TRAMPOLINE_PHYS >> 12) as u32;

    // ── Start each AP ────────────────────────────────────────────────────
    // Each AP is started individually so we can give it a unique stack and
    // wait for it to signal online before starting the next one.
    for ap_idx in 0..ap_count {
        // Allocate a stack: point the AP at the top of its stack page.
        let stack_top = unsafe {
            let base = AP_STACKS[ap_idx].as_ptr() as usize;
            (base + AP_STACK_BYTES) as u64
        };

        // Write the per-AP stack and argument into the trampoline data area.
        // We abuse OFF_AP_STACK for the actual stack pointer and store
        // the ap_idx in a scratchpad word at OFF_AP_ONLINE − 8.
        unsafe {
            core::ptr::write_volatile(
                (TRAMPOLINE_PHYS as usize + OFF_AP_STACK) as *mut u64,
                stack_top,
            );
            // Pass ap_idx to ap_entry via a word in the trampoline page.
            // ap_entry reads it via the RDI convention set up in the 64-bit
            // trampoline stub (see ap_trampoline.s, where we load RDI from 0x8FD0).
            // Actually, ap_entry receives ap_idx via the arg we push below.
            // For simplicity, store ap_idx at 0x8FE8 and load it from there.
            core::ptr::write_volatile(
                (TRAMPOLINE_PHYS as usize + OFF_AP_ONLINE) as *mut u64,
                ap_idx as u64,
            );
        }

        // ── INIT IPI (All Excluding Self broadcast) ───────────────────────
        // ICR encoding: delivery=INIT(5), level=assert, trigger=level,
        // destination shorthand = All Excluding Self (3).
        let init_low: u32 = (3 << 18) | (1 << 15) | (1 << 14) | (5 << 8);
        icr_write(lapic_base, 0, init_low);
        wait_ms(10);

        // De-assert INIT IPI (level=deassert, trigger=level).
        let init_deassert: u32 = (3 << 18) | (1 << 15) | (5 << 8);
        icr_write(lapic_base, 0, init_deassert);
        wait_ms(1);

        // ── SIPI × 2 ─────────────────────────────────────────────────────
        // ICR: delivery=StartUp(6), vector=sipi_vec, no shorthand needed for
        // single AP; use All Excluding Self for simplicity.
        let sipi_low: u32 = (3 << 18) | (1 << 14) | (6 << 8) | sipi_vec;
        icr_write(lapic_base, 0, sipi_low);
        wait_ms(1);
        icr_write(lapic_base, 0, sipi_low);

        // Wait up to 200 ms for the AP to signal online.
        let deadline = crate::apic::ticks() + 200;
        while crate::apic::ticks() < deadline
            && AP_ONLINE_COUNT.load(Ordering::Acquire) <= ap_idx
        {
            core::hint::spin_loop();
        }

        if AP_ONLINE_COUNT.load(Ordering::Acquire) > ap_idx {
            crate::kprintln!("[smp] AP {} started", ap_idx);
        } else {
            crate::kprintln!("[smp] AP {} did not respond — skipping", ap_idx);
        }
    }

    crate::kprintln!(
        "[smp] {} of {} AP(s) online",
        AP_ONLINE_COUNT.load(Ordering::Relaxed),
        ap_count,
    );
}
