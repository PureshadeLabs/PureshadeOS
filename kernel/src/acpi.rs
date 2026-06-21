//! ACPI power management — QEMU-specific shutdown via PM1a control register.

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
