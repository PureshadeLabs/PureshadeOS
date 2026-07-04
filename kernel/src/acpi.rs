//! ACPI table parsing (RSDP → RSDT/XSDT → MADT) and QEMU shutdown.
//!
//! The MADT ("APIC" signature) supplies what the interrupt controllers need:
//!   - Local APIC physical base (cross-checked against the IA32_APIC_BASE MSR,
//!     which stays authoritative per the Intel SDM)
//!   - I/O APIC physical base + GSI base
//!   - Interrupt Source Overrides: ISA IRQ → GSI remaps with polarity/trigger
//!
//! Tables are read through the 0→1 GiB identity map (phys == virt), so
//! `init` must run after `vmm::init()`.  Tables above 1 GiB are skipped with
//! a warning — QEMU and typical hardware place them well below that.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

// ── Parsed MADT state ─────────────────────────────────────────────────────────

/// Local APIC physical base from the MADT (0 = not found).
static LAPIC_PHYS: AtomicU64 = AtomicU64::new(0);
/// First I/O APIC physical base from the MADT (0 = not found).
static IOAPIC_PHYS: AtomicU64 = AtomicU64::new(0);
/// GSI number of that I/O APIC's first redirection entry.
static IOAPIC_GSI_BASE: AtomicU32 = AtomicU32::new(0);

/// Interrupt Source Overrides for ISA IRQs 0–15.
/// Packed: bit 63 = present, bits 47:32 = MADT flags, bits 31:0 = GSI.
static ISA_OVERRIDES: [AtomicU64; 16] = [const { AtomicU64::new(0) }; 16];

const ISO_PRESENT: u64 = 1 << 63;

// ── Raw physical reads (identity map) ─────────────────────────────────────────

/// Highest physical address readable through the identity map.
const IDENTITY_LIMIT: u64 = 1 << 30;

#[inline]
unsafe fn rd_u8(phys: u64) -> u8 { unsafe { (phys as *const u8).read_volatile() } }
#[inline]
unsafe fn rd_u16(phys: u64) -> u16 { unsafe { (phys as *const u16).read_unaligned() } }
#[inline]
unsafe fn rd_u32(phys: u64) -> u32 { unsafe { (phys as *const u32).read_unaligned() } }
#[inline]
unsafe fn rd_u64(phys: u64) -> u64 { unsafe { (phys as *const u64).read_unaligned() } }

unsafe fn sig_matches(phys: u64, sig: &[u8; 4]) -> bool {
    (0..4).all(|i| unsafe { rd_u8(phys + i as u64) } == sig[i])
}

/// Sum `len` bytes starting at `phys` (ACPI checksums must total 0 mod 256).
unsafe fn checksum(phys: u64, len: u64) -> u8 {
    let mut sum = 0u8;
    for i in 0..len {
        sum = sum.wrapping_add(unsafe { rd_u8(phys + i) });
    }
    sum
}

// ── MADT parsing ──────────────────────────────────────────────────────────────

/// MADT entry types we consume.
const MADT_IOAPIC:         u8 = 1;
const MADT_ISO:            u8 = 2;
const MADT_LAPIC_OVERRIDE: u8 = 5;

/// Parse one MADT at `madt` (physical). Fills the module statics.
unsafe fn parse_madt(madt: u64) {
    let len = unsafe { rd_u32(madt + 4) } as u64;

    // Header field: 32-bit local APIC address at offset 36.
    LAPIC_PHYS.store(unsafe { rd_u32(madt + 36) } as u64, Ordering::Relaxed);

    // Variable-length entries start at offset 44: [type u8][len u8][payload].
    let mut p = madt + 44;
    let end = madt + len;
    while p + 2 <= end {
        let etype = unsafe { rd_u8(p) };
        let elen  = unsafe { rd_u8(p + 1) } as u64;
        if elen < 2 || p + elen > end { break; } // malformed — stop walking

        match etype {
            MADT_IOAPIC => {
                // [2]=id [3]=rsvd [4..8]=addr [8..12]=gsi_base
                let addr     = unsafe { rd_u32(p + 4) } as u64;
                let gsi_base = unsafe { rd_u32(p + 8) };
                // Keep the I/O APIC that serves GSI 0 (ISA range); ignore others.
                if IOAPIC_PHYS.load(Ordering::Relaxed) == 0 || gsi_base == 0 {
                    IOAPIC_PHYS.store(addr, Ordering::Relaxed);
                    IOAPIC_GSI_BASE.store(gsi_base, Ordering::Relaxed);
                }
            }
            MADT_ISO => {
                // [2]=bus (0=ISA) [3]=source IRQ [4..8]=GSI [8..10]=flags
                let source = unsafe { rd_u8(p + 3) };
                let gsi    = unsafe { rd_u32(p + 4) };
                let flags  = unsafe { rd_u16(p + 8) };
                if (source as usize) < ISA_OVERRIDES.len() {
                    ISA_OVERRIDES[source as usize].store(
                        ISO_PRESENT | ((flags as u64) << 32) | gsi as u64,
                        Ordering::Relaxed,
                    );
                }
            }
            MADT_LAPIC_OVERRIDE => {
                // [4..12] = 64-bit local APIC address, supersedes header field.
                LAPIC_PHYS.store(unsafe { rd_u64(p + 4) }, Ordering::Relaxed);
            }
            _ => {}
        }
        p += elen;
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Parse ACPI tables starting from the RSDP at `rsdp_phys`.
///
/// Returns `true` if a MADT was found and parsed.  Must be called after
/// `vmm::init()` (reads physical memory through the identity map).
pub fn init(rsdp_phys: u64) -> bool {
    if rsdp_phys == 0 || rsdp_phys >= IDENTITY_LIMIT {
        return false;
    }
    unsafe {
        if !sig_matches(rsdp_phys, b"RSD ") || !sig_matches(rsdp_phys + 4, b"PTR ") {
            return false;
        }
        if checksum(rsdp_phys, 20) != 0 {
            return false;
        }

        // Revision ≥ 2 → ACPI 2.0+ XSDT (64-bit entries); else RSDT (32-bit).
        let revision = rd_u8(rsdp_phys + 15);
        let (sdt, entry_size) = if revision >= 2 {
            (rd_u64(rsdp_phys + 24), 8u64)
        } else {
            (rd_u32(rsdp_phys + 16) as u64, 4u64)
        };
        if sdt == 0 || sdt >= IDENTITY_LIMIT {
            return false;
        }

        // Walk the R/XSDT entry pointers looking for the MADT ("APIC").
        let sdt_len = rd_u32(sdt + 4) as u64;
        let mut p = sdt + 36; // entries follow the 36-byte SDT header
        let end = sdt + sdt_len;
        while p + entry_size <= end {
            let table = if entry_size == 8 { rd_u64(p) } else { rd_u32(p) as u64 };
            if table != 0 && table < IDENTITY_LIMIT && sig_matches(table, b"APIC") {
                parse_madt(table);
                return true;
            }
            p += entry_size;
        }
    }
    false
}

/// Local APIC physical base from the MADT, if parsed.
pub fn lapic_phys() -> Option<u64> {
    match LAPIC_PHYS.load(Ordering::Relaxed) {
        0 => None,
        v => Some(v),
    }
}

/// First I/O APIC from the MADT: `(physical base, GSI base)`, if parsed.
pub fn ioapic_info() -> Option<(u64, u32)> {
    match IOAPIC_PHYS.load(Ordering::Relaxed) {
        0 => None,
        v => Some((v, IOAPIC_GSI_BASE.load(Ordering::Relaxed))),
    }
}

/// Resolve an ISA IRQ to `(gsi, ioapic_redirection_flags)`, honouring any
/// MADT Interrupt Source Override.
///
/// Without an override, ISA IRQs map identity to GSIs and are edge-triggered
/// active-high (flags 0).  Override flags follow ACPI §5.2.12.5: polarity in
/// bits [1:0] (0b11 = active low), trigger mode in bits [3:2] (0b11 = level).
pub fn isa_irq_route(irq: u8) -> (u32, u32) {
    let packed = ISA_OVERRIDES
        .get(irq as usize)
        .map(|a| a.load(Ordering::Relaxed))
        .unwrap_or(0);
    if packed & ISO_PRESENT == 0 {
        return (irq as u32, 0);
    }
    let gsi   = packed as u32;
    let flags = (packed >> 32) as u16;
    let mut redir = 0u32;
    if flags & 0b0011 == 0b0011 { redir |= crate::ioapic::IRQ_ACTIVE_LO; }
    if flags & 0b1100 == 0b1100 { redir |= crate::ioapic::IRQ_LEVEL; }
    (gsi, redir)
}

/// `true` if IRQ `irq` has an explicit MADT override entry.
pub fn isa_irq_overridden(irq: u8) -> bool {
    ISA_OVERRIDES
        .get(irq as usize)
        .map(|a| a.load(Ordering::Relaxed) & ISO_PRESENT != 0)
        .unwrap_or(false)
}

// ── QEMU shutdown ─────────────────────────────────────────────────────────────

/// Halt the machine by writing SLP_TYP=5 + SLP_EN to the QEMU PM1a control port.
///
/// Port 0x604 is QEMU's hardcoded PM1a_CNT_BLK. Writing 0x2000 sets
/// SLP_TYP = 0b101 (S5 soft-off) with SLP_EN, which QEMU interprets as a
/// clean power-off — equivalent to `quit` in the QEMU monitor.
pub fn shutdown() -> ! {
    unsafe {
        core::arch::asm!(
            "out dx, ax",
            in("dx") 0x604u16,
            in("ax") 0x2000u16,
            options(nomem, nostack, preserves_flags),
        );
    }
    // QEMU exits before this point; loop defensively in case the port write
    // is a no-op (e.g. running on hardware without this quirk).
    loop {
        unsafe { core::arch::asm!("hlt", options(nomem, nostack)) };
    }
}
