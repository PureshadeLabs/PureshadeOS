/// Kernel Address Space Layout Randomisation (KASLR).
///
/// The kernel image is loaded at a fixed physical address by the Multiboot
/// bootloader, so we cannot randomise the load address itself.  What we CAN
/// randomise is the virtual window used for every dynamically-managed kernel
/// region — the heap, the IPC ring-buffer window, and the APIC MMIO mapping.
///
/// A single 4 KiB-aligned random offset (0 → 128 MiB) is generated at boot
/// from RDRAND (with RDTSC as fallback) and added to each region's nominal
/// base address.  All three regions are shifted by the same delta so their
/// relative spacing is preserved; the 1 TiB gaps between regions make
/// collision impossible.
///
/// `init()` must be called before `heap::init()`, `apic::init()`, and the
/// first `ipc::create_endpoint()` call.

use core::sync::atomic::{AtomicU64, Ordering};

static KASLR_OFFSET: AtomicU64 = AtomicU64::new(0);

/// Return the raw entropy value for KASLR seeding.
///
/// Tries RDRAND up to 10 times (CPUID leaf 1 ECX[30] must be set).
/// Falls back to RDTSC if RDRAND is unavailable or persistently fails.
fn get_entropy() -> u64 {
    // Check CPUID leaf 1 ECX bit 30 for RDRAND support.
    let ecx: u32;
    unsafe {
        core::arch::asm!(
            "push rbx",
            "mov eax, 1",
            "cpuid",
            "pop rbx",
            out("ecx") ecx,
            lateout("eax") _,
            lateout("edx") _,
            options(nostack),
        );
    }

    if ecx & (1 << 30) != 0 {
        for _ in 0..10u32 {
            let val: u64;
            let cf: u8;
            unsafe {
                core::arch::asm!(
                    "rdrand {val}",
                    "setc {cf}",
                    val = out(reg) val,
                    cf = out(reg_byte) cf,
                    options(nostack, nomem),
                );
            }
            if cf != 0 {
                return val;
            }
        }
    }

    // Fallback: RDTSC (low entropy but better than a fixed offset).
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!("rdtsc", out("eax") lo, out("edx") hi, options(nostack, nomem));
    }
    ((hi as u64) << 32) | lo as u64
}

/// Initialise KASLR.
///
/// Generates a random 4 KiB-aligned offset in the range `[0, 128 MiB)` and
/// stores it for later use by the heap, IPC, and APIC subsystems.
/// Prints the chosen offset so boot logs include enough information to debug
/// an ASLR-related crash without compromising security (offset is still
/// unknown to userspace).
pub fn init() {
    let entropy = get_entropy();
    // 128 MiB / 4 KiB = 32 768 = 0x8000 distinct positions.
    let offset = (entropy % 0x8000) * 0x1000;
    KASLR_OFFSET.store(offset, Ordering::Relaxed);
    crate::kprintln!(
        "[kaslr] offset = {:#x} ({} MiB)",
        offset,
        offset / (1024 * 1024),
    );
}

/// Return the KASLR offset (page-aligned, 0 → 128 MiB).
///
/// Will be 0 until `init()` has run, but all callers initialise their
/// subsystems after `kaslr::init()` so this is always correct in practice.
#[inline]
pub fn offset() -> u64 {
    KASLR_OFFSET.load(Ordering::Relaxed)
}
