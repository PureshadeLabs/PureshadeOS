/// Interrupt Descriptor Table — 256 interrupt-gate entries.
///
/// Vectors 0–31 are wired to the ISR stubs defined in isr_stubs.s.
/// All other entries are left absent (not-present); they will be filled in
/// as hardware IRQ handlers are added in later steps.

/// A single 16-byte IDT entry (64-bit interrupt gate).
#[repr(C)]
#[derive(Clone, Copy)]
struct IdtEntry {
    offset_low:  u16, // handler[15:0]
    selector:    u16, // code segment selector
    ist:         u8,  // interrupt stack table index (0 = legacy stack)
    type_attr:   u8,  // P | DPL | 0 | type (0x8E = present, ring 0, int gate)
    offset_mid:  u16, // handler[31:16]
    offset_high: u32, // handler[63:32]
    _reserved:   u32,
}

impl IdtEntry {
    const fn absent() -> Self {
        Self {
            offset_low: 0, selector: 0, ist: 0, type_attr: 0,
            offset_mid: 0, offset_high: 0, _reserved: 0,
        }
    }

    fn interrupt(handler: u64) -> Self {
        Self {
            offset_low:  handler as u16,
            selector:    0x08,
            ist:         0,
            type_attr:   0x8E, // P=1, DPL=0, type=0b1110 (interrupt gate)
            offset_mid:  (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            _reserved:   0,
        }
    }
}

/// The 10-byte operand for `lidt` (limit:base).
#[repr(C, packed)]
struct IdtPtr {
    limit: u16,
    base:  u64,
}

unsafe extern "C" {
    /// Array of ISR stub entry-point addresses, built in isr_stubs.s.
    static isr_stub_table: [u64; 32];
}

static mut IDT: [IdtEntry; 256] = [IdtEntry::absent(); 256];

/// Remap the 8259 PIC so its IRQ vectors (0x20–0x2F) don't overlap CPU
/// exceptions (0x00–0x1F), then mask every line.
///
/// Must be called before any `sti` — the PIC's power-on default maps
/// IRQ 0 (timer) to vector 8, which is the CPU's #DF (double-fault) slot.
unsafe fn remap_and_mask_pic() {
    // Helper: write one byte to an I/O port.
    #[inline]
    unsafe fn outb(port: u16, val: u8) {
        unsafe {
            core::arch::asm!(
                "out dx, al",
                in("dx") port, in("al") val,
                options(nomem, nostack, preserves_flags),
            );
        }
    }

    unsafe {
        // ICW1: cascade mode, ICW4 required
        outb(0x20, 0x11);
        outb(0xA0, 0x11);
        // ICW2: remap PIC1 → 0x20–0x27, PIC2 → 0x28–0x2F
        outb(0x21, 0x20);
        outb(0xA1, 0x28);
        // ICW3: wiring
        outb(0x21, 0x04);
        outb(0xA1, 0x02);
        // ICW4: 8086 mode
        outb(0x21, 0x01);
        outb(0xA1, 0x01);
        // OCW1: mask all IRQ lines on both chips
        outb(0x21, 0xFF);
        outb(0xA1, 0xFF);
    }
}

pub fn init() {
    // Remap the 8259 PIC before loading the IDT and before any sti.
    unsafe { remap_and_mask_pic() };

    // Wire vectors 0–31 to their ISR stubs.
    for i in 0..32_usize {
        let handler = unsafe { isr_stub_table[i] };
        unsafe { IDT[i] = IdtEntry::interrupt(handler) };
    }

    let ptr = IdtPtr {
        limit: (core::mem::size_of::<[IdtEntry; 256]>() - 1) as u16,
        base:  &raw const IDT as u64,
    };
    unsafe {
        core::arch::asm!("lidt [{}]", in(reg) &raw const ptr, options(nostack, readonly));
    }
}

/// Install a handler for a hardware IRQ vector (32–255).
/// Called by device drivers after `idt::init()`.
pub fn register_irq(vector: u8, handler: u64) {
    unsafe { IDT[vector as usize] = IdtEntry::interrupt(handler) };
}

/// Load the already-initialised IDT on the calling CPU.
///
/// Called by APs during startup so that interrupts are correctly dispatched
/// without re-running the full `init()` sequence (which would re-remap the
/// 8259 PIC and overwrite ISR entries that are already correct).
pub fn ap_load() {
    let ptr = IdtPtr {
        limit: (core::mem::size_of::<[IdtEntry; 256]>() - 1) as u16,
        base:  &raw const IDT as u64,
    };
    unsafe {
        core::arch::asm!("lidt [{}]", in(reg) &raw const ptr, options(nostack, readonly));
    }
}
