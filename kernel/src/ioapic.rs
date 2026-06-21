//! I/O APIC driver.
//!
//! The I/O APIC routes external hardware interrupts (GSIs) to Local APIC
//! vectors, replacing the legacy 8259 PIC for all interrupt delivery.
//!
//! ## Physical address
//!
//! `IOAPIC_PHYS` is hardcoded to `0xFEC0_0000`, the QEMU default.  On real
//! hardware the address comes from the ACPI MADT table.  A future ACPI parser
//! can call `ioapic::set_phys_base` before `ioapic::init`.
//!
//! ## Register access
//!
//! The I/O APIC uses an indirect scheme: write the register index to
//! `IOREGSEL` (offset 0x00), then read/write the value through `IOWIN`
//! (offset 0x10).
//!
//! ## Usage
//!
//! 1. Call `ioapic::init()` after `vmm::init()` and `apic::init()`.
//! 2. Per device: register a handler with `idt::register_irq`, then call
//!    `ioapic::map_irq(gsi, vector, flags)` to unmask the line.
//! 3. Every ISR must call `apic::eoi()` before returning.

use core::sync::atomic::{AtomicU64, AtomicU8, Ordering};

// ── Physical / virtual addresses ──────────────────────────────────────────────

/// Default I/O APIC physical address (QEMU; real hardware: read from ACPI MADT).
static IOAPIC_PHYS: AtomicU64 = AtomicU64::new(0xFEC0_0000);

/// Virtual address where the I/O APIC MMIO page is mapped.
/// Higher-half base (0xFFFF_8000_0000_0000) + default physical address.
const IOAPIC_VIRT: u64 = 0xFFFF_8000_FEC0_0000;

// ── Indirect register indices ─────────────────────────────────────────────────

const IOAPICVER: u8 = 0x01; // version register: [23:16] = max redir entry index
const IOREDTBL:  u8 = 0x10; // redirection table base (2 dwords per entry)

// ── MMIO offsets within the IOAPIC page ──────────────────────────────────────

const IOREGSEL: usize = 0x00; // index register (write to select)
const IOWIN:    usize = 0x10; // data window (read/write selected register)

// ── Redirection entry flag bits (low dword) ───────────────────────────────────

/// Mask the IRQ line — no interrupt delivered while set.
pub const IRQ_MASKED:    u32 = 1 << 16;
/// Level-triggered mode (default 0 = edge-triggered).
pub const IRQ_LEVEL:     u32 = 1 << 15;
/// Active-low polarity (default 0 = active-high).
/// ISA IRQs are active-high edge; PCI IRQs are active-low level.
pub const IRQ_ACTIVE_LO: u32 = 1 << 13;

// ── State ─────────────────────────────────────────────────────────────────────

/// Maximum redirection entry index (entries = this + 1). Set during `init`.
static MAX_ENTRY: AtomicU8 = AtomicU8::new(0);

// ── MMIO helpers ──────────────────────────────────────────────────────────────

#[inline]
fn ioapic_write(reg: u8, val: u32) {
    unsafe {
        core::ptr::write_volatile(
            (IOAPIC_VIRT as usize + IOREGSEL) as *mut u32,
            reg as u32,
        );
        core::ptr::write_volatile(
            (IOAPIC_VIRT as usize + IOWIN) as *mut u32,
            val,
        );
    }
}

#[inline]
fn ioapic_read(reg: u8) -> u32 {
    unsafe {
        core::ptr::write_volatile(
            (IOAPIC_VIRT as usize + IOREGSEL) as *mut u32,
            reg as u32,
        );
        core::ptr::read_volatile(
            (IOAPIC_VIRT as usize + IOWIN) as *const u32,
        )
    }
}

// ── Redirection entry index helper ───────────────────────────────────────────

/// Return the IOREDTBL register index for the low dword of GSI `gsi`.
/// High dword is at `redir_lo_reg(gsi) + 1`.
#[inline]
fn redir_lo_reg(gsi: u8) -> u8 {
    // IOREDTBL = 0x10; each entry occupies two consecutive dword registers.
    // Max GSI = 23 → index 0x10 + 46 = 0x3E; fits in u8.
    (IOREDTBL as u32 + gsi as u32 * 2) as u8
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Override the I/O APIC physical base address before calling `init`.
///
/// Only needed when the ACPI MADT reports a non-default address.
/// Must be called before `ioapic::init()`.
pub fn set_phys_base(phys: u64) {
    IOAPIC_PHYS.store(phys, Ordering::Relaxed);
}

/// Initialise the I/O APIC.
///
/// Maps the MMIO page, reads the entry count from `IOAPICVER`, and masks
/// every redirection table entry so no spurious interrupts fire before
/// drivers call `map_irq`.
///
/// Must be called after `vmm::init()` and `apic::init()`.
pub fn init() {
    let phys = IOAPIC_PHYS.load(Ordering::Relaxed);

    crate::vmm::map_page(
        crate::vmm::VirtAddr(IOAPIC_VIRT),
        crate::pmm::PhysAddr(phys),
        crate::vmm::PageFlags::KERNEL_RW,
    );

    // IOAPICVER[23:16] = maximum redirection entry index (zero-based).
    let ver = ioapic_read(IOAPICVER);
    let max = ((ver >> 16) & 0xFF) as u8;
    MAX_ENTRY.store(max, Ordering::Relaxed);

    // Mask every entry; leave the vector field as zero (safe: masked = no delivery).
    for gsi in 0..=max {
        ioapic_write(redir_lo_reg(gsi),     IRQ_MASKED);
        ioapic_write(redir_lo_reg(gsi) + 1, 0);
    }
}

/// Program a redirection table entry and unmask the line.
///
/// # Arguments
///
/// * `gsi`    — Global System Interrupt (0-based). Must be within `entry_count()`.
/// * `vector` — IDT vector to deliver on this GSI. Register the handler with
///              `idt::register_irq(vector, handler)` before calling this.
/// * `flags`  — Bitwise OR of `IRQ_LEVEL`, `IRQ_ACTIVE_LO`, etc.
///              `IRQ_MASKED` in `flags` is ignored — this call always unmasks.
///
/// Delivery mode is Fixed; destination is the BSP (physical APIC ID 0).
/// ISA IRQs (0–15) are active-high edge-triggered; pass `flags = 0`.
/// PCI IRQs are active-low level-triggered; pass `IRQ_LEVEL | IRQ_ACTIVE_LO`.
pub fn map_irq(gsi: u8, vector: u8, flags: u32) {
    let lo = (vector as u32) | (flags & !IRQ_MASKED); // mask bit clear → unmasked
    let hi = 0u32; // physical mode, destination APIC ID 0 (BSP)

    ioapic_write(redir_lo_reg(gsi),     lo);
    ioapic_write(redir_lo_reg(gsi) + 1, hi);
}

/// Mask one GSI — stop delivering its interrupts.
pub fn mask_irq(gsi: u8) {
    let reg = redir_lo_reg(gsi);
    let lo = ioapic_read(reg);
    ioapic_write(reg, lo | IRQ_MASKED);
}

/// Unmask one GSI — resume delivering its interrupts.
pub fn unmask_irq(gsi: u8) {
    let reg = redir_lo_reg(gsi);
    let lo = ioapic_read(reg);
    ioapic_write(reg, lo & !IRQ_MASKED);
}

/// Number of redirection table entries supported by this I/O APIC.
/// Returns 0 if called before `init`.
pub fn entry_count() -> u8 {
    MAX_ENTRY.load(Ordering::Relaxed).saturating_add(1)
}
