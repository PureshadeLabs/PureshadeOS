/// Global Descriptor Table — null, kernel code/data, user data/code, and TSS.
///
/// ## Selector layout
///
/// ```
/// 0x00  null
/// 0x08  64-bit kernel code, DPL 0
/// 0x10  kernel data, DPL 0
/// 0x18  user data, DPL 3   ← data before code for STAR/sysretq alignment
/// 0x20  64-bit user code, DPL 3
/// 0x28  TSS low  (16-byte system descriptor)
/// 0x30  TSS high
/// ```
///
/// ## STAR alignment
///
/// `IA32_STAR[63:48]` is set to `0x10` (kernel data selector base).  On
/// `sysretq` the CPU adds 16 for CS and 8 for SS:
/// ```
///   CS = STAR[63:48] + 16 = 0x20 | RPL=3  (user code, DPL 3) ✓
///   SS = STAR[63:48] +  8 = 0x18 | RPL=3  (user data, DPL 3) ✓
/// ```

use core::cell::UnsafeCell;
use core::mem;

// Segment selector values (index * 8 | RPL).  Used by the assembly stubs as
// hardcoded immediates; kept here as documentation and for future Rust callers.
#[allow(dead_code)] pub const KERNEL_CODE_SEL: u16 = 0x08;
#[allow(dead_code)] pub const KERNEL_DATA_SEL: u16 = 0x10;
#[allow(dead_code)] pub const USER_DATA_SEL:   u16 = 0x18;
#[allow(dead_code)] pub const USER_CODE_SEL:   u16 = 0x20;
pub const TSS_SEL:         u16 = 0x28;

// GDT entries 5–6 (the TSS descriptor) are filled at runtime because the TSS
// base address is only known then.  Use UnsafeCell to avoid `static mut`.
struct GlobalGdt(UnsafeCell<[u64; 7]>);
// SAFETY: single-threaded kernel.
unsafe impl Sync for GlobalGdt {}

static GDT: GlobalGdt = GlobalGdt(UnsafeCell::new([
    0x0000_0000_0000_0000, // 0x00 — null
    0x00AF_9A00_0000_FFFF, // 0x08 — 64-bit code, DPL 0 (L=1, P=1, type=0xA)
    0x00CF_9200_0000_FFFF, // 0x10 — data, DPL 0
    0x00CF_F200_0000_FFFF, // 0x18 — data, DPL 3 (F2: P=1, DPL=3, S=1, type=2)
    0x00AF_FA00_0000_FFFF, // 0x20 — 64-bit code, DPL 3 (FA: P=1, DPL=3, S=1, type=0xA)
    0x0000_0000_0000_0000, // 0x28 — TSS low  (filled by init())
    0x0000_0000_0000_0000, // 0x30 — TSS high (filled by init())
]));

/// Encode a 16-byte TSS descriptor (Intel SDM Vol. 3A §7.2.3, Table 7-4).
///
/// Returns `(low_qword, high_qword)`.
fn encode_tss_descriptor(base: u64, limit: u32) -> (u64, u64) {
    let low =
          (limit as u64 & 0xFFFF)               // bits 15:0  = limit[15:0]
        | ((base & 0x00FF_FFFF) << 16)           // bits 39:16 = base[23:0]
        | (0x89u64 << 40)                        // bits 47:40 = P=1 DPL=0 S=0 type=9
        | (((limit as u64 >> 16) & 0xF) << 48)  // bits 51:48 = limit[19:16]
        | (((base >> 24) & 0xFF) << 56);         // bits 63:56 = base[31:24]
    let high = base >> 32;                       // bits 31:0  = base[63:32]
    (low, high)
}

/// The 10-byte operand for `lgdt` (limit:base).
#[repr(C, packed)]
struct GdtPtr {
    limit: u16,
    base:  u64,
}

unsafe extern "C" {
    fn gdt_flush(ptr: *const GdtPtr);
}

pub fn init() {
    // Fill the TSS descriptor with the runtime base address of the TSS.
    let tss_base  = crate::tss::tss_addr();
    let tss_limit = (mem::size_of::<crate::tss::Tss>() - 1) as u32; // 103 = 0x67 ✓
    let (lo, hi) = encode_tss_descriptor(tss_base, tss_limit);

    unsafe {
        let gdt = &mut *GDT.0.get();
        gdt[5] = lo;
        gdt[6] = hi;
    }

    let ptr = GdtPtr {
        limit: (mem::size_of::<[u64; 7]>() - 1) as u16,
        base:  unsafe { (*GDT.0.get()).as_ptr() as u64 },
    };
    unsafe { gdt_flush(&raw const ptr); }

    // Load the TSS into the Task Register.
    crate::tss::load(TSS_SEL);
}
