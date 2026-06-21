/// Local APIC and timer initialisation.
///
/// ## PIC disable
///
/// The legacy 8259 PIC is reinitialised (to remap its vectors away from CPU
/// exception space 0–31) and then fully masked.  All hardware IRQ delivery
/// goes through the Local APIC from this point on.
///
/// ## APIC MMIO
///
/// The Local APIC register file lives at a physical address reported by the
/// `IA32_APIC_BASE` MSR (default 0xFEE0_0000).  That physical address is
/// above the 1 GiB identity map, so we call `vmm::map_page` to create a
/// virtual mapping at `APIC_VIRT` before accessing any registers.
///
/// ## Timer calibration
///
/// The APIC timer frequency depends on the bus (or core-crystal) clock, which
/// varies by CPU and QEMU configuration.  We calibrate by counting APIC ticks
/// over a 10 ms PIT channel-2 reference window, then configure the timer for
/// a 1 ms periodic period.

use core::sync::atomic::{AtomicU64, Ordering};
use core::arch::global_asm;

// ── Virtual address for the APIC MMIO page ────────────────────────────────────
// Nominal higher-half base + KASLR offset applied at runtime.
const APIC_VIRT_NOMINAL: u64 = 0xFFFF_8000_FEE0_0000;

static APIC_VIRT: AtomicU64 = AtomicU64::new(0);

#[inline]
fn apic_virt() -> u64 {
    APIC_VIRT.load(Ordering::Relaxed)
}

// ── APIC register offsets (byte offsets into the MMIO page) ──────────────────
const REG_EOI:   usize = 0x0B0;
const REG_SVR:   usize = 0x0F0; // Spurious Interrupt Vector Register
const REG_TIMER: usize = 0x320; // LVT Timer
const REG_TICR:  usize = 0x380; // Timer Initial Count Register
const REG_TCCR:  usize = 0x390; // Timer Current Count Register (read-only)
const REG_TDCR:  usize = 0x3E0; // Timer Divide Configuration Register

// ── LVT Timer flags ───────────────────────────────────────────────────────────
const TIMER_PERIODIC: u32 = 1 << 17;
const TIMER_MASKED:   u32 = 1 << 16;

// ── Interrupt vectors ─────────────────────────────────────────────────────────
pub const VECTOR_TIMER:         u8 = 32;
/// IPI vector used for TLB shootdowns.  Sent to all CPUs except self when a
/// page mapping is removed; the handler reloads CR3 to flush the local TLB.
pub const VECTOR_TLB_SHOOTDOWN: u8 = 33;
pub const VECTOR_SPURIOUS:      u8 = 255;

// ── APIC register offsets (additional) ───────────────────────────────────────
/// Interrupt Command Register — low 32 bits.  Writing this triggers an IPI.
const REG_ICR_LOW:  usize = 0x300;
/// Interrupt Command Register — high 32 bits (destination field).
const REG_ICR_HIGH: usize = 0x310;

// ── MSR ───────────────────────────────────────────────────────────────────────
const IA32_APIC_BASE: u32 = 0x1B;

// ── Global tick counter ───────────────────────────────────────────────────────
static TICK_COUNT: AtomicU64 = AtomicU64::new(0);

// ── Calibrated timer period (ticks per ms) ────────────────────────────────────
// Written once by the BSP during `init()`; read by APs in `ap_apic_init()`.
static TICKS_PER_MS: AtomicU64 = AtomicU64::new(1);

// ── APIC online flag ──────────────────────────────────────────────────────────
/// Set to `true` at the end of `init()`.  `send_tlb_shootdown_ipi` checks
/// this so that early-boot `unmap_page` calls (before the IDT handler is
/// installed) are silent no-ops.
static APIC_ONLINE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

// ── ISR assembly stubs ────────────────────────────────────────────────────────
//
// timer_isr_stub (vector 32):
//   Saves ALL general-purpose registers before calling timer_interrupt_handler().
//   All 15 GPRs must be saved because this interrupt can fire while user-mode
//   (ring-3) code is running, where any register may hold a live value.
//   If yield_task performs a context switch, switch_context saves/restores the
//   *kernel* callee-saved registers on top of this frame — the user's callee-saved
//   registers (rbx, rbp, r12-r15) stay safe beneath that frame and are popped here
//   on the way back to iretq.
//
// spurious_isr_stub (vector 255):
//   Intel SDM §10.9: spurious interrupts must NOT be acknowledged (no EOI).
//   Just iretq.
global_asm!(r#"
.section .text

.global timer_isr_stub
.type   timer_isr_stub, @function
timer_isr_stub:
    pushq  %rax
    pushq  %rcx
    pushq  %rdx
    pushq  %rsi
    pushq  %rdi
    pushq  %r8
    pushq  %r9
    pushq  %r10
    pushq  %r11
    pushq  %rbx
    pushq  %rbp
    pushq  %r12
    pushq  %r13
    pushq  %r14
    pushq  %r15
    call   timer_interrupt_handler
    popq   %r15
    popq   %r14
    popq   %r13
    popq   %r12
    popq   %rbp
    popq   %rbx
    popq   %r11
    popq   %r10
    popq   %r9
    popq   %r8
    popq   %rdi
    popq   %rsi
    popq   %rdx
    popq   %rcx
    popq   %rax
    iretq

.global spurious_isr_stub
.type   spurious_isr_stub, @function
spurious_isr_stub:
    iretq

// tlb_shootdown_isr_stub (vector 33):
//   Saves caller-saved registers, calls tlb_shootdown_handler (which reloads
//   CR3 to flush the local TLB and sends EOI), then restores and iretq.
.global tlb_shootdown_isr_stub
.type   tlb_shootdown_isr_stub, @function
tlb_shootdown_isr_stub:
    pushq  %rax
    pushq  %rcx
    pushq  %rdx
    pushq  %rsi
    pushq  %rdi
    pushq  %r8
    pushq  %r9
    pushq  %r10
    pushq  %r11
    call   tlb_shootdown_handler
    popq   %r11
    popq   %r10
    popq   %r9
    popq   %r8
    popq   %rdi
    popq   %rsi
    popq   %rdx
    popq   %rcx
    popq   %rax
    iretq
"#, options(att_syntax));

unsafe extern "C" {
    fn timer_isr_stub();
    fn spurious_isr_stub();
    fn tlb_shootdown_isr_stub();
}

// ── MMIO helpers ──────────────────────────────────────────────────────────────

#[inline]
fn apic_read(offset: usize) -> u32 {
    unsafe {
        core::ptr::read_volatile((apic_virt() as usize + offset) as *const u32)
    }
}

#[inline]
fn apic_write(offset: usize, val: u32) {
    unsafe {
        core::ptr::write_volatile((apic_virt() as usize + offset) as *mut u32, val)
    }
}

// ── Port I/O helpers ──────────────────────────────────────────────────────────

#[inline]
unsafe fn outb(port: u16, val: u8) {
    unsafe {
        core::arch::asm!(
            "outb %al, %dx",
            in("dx") port, in("al") val,
            options(att_syntax, nostack, preserves_flags),
        );
    }
}

#[inline]
unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    unsafe {
        core::arch::asm!(
            "inb %dx, %al",
            in("dx") port, out("al") val,
            options(att_syntax, nostack, preserves_flags),
        );
    }
    val
}

// ── Submodule: legacy PIC ─────────────────────────────────────────────────────

/// Reinitialise the 8259 PIC (remapping vectors to 0x20–0x2F so they don't
/// overlap CPU exceptions), then mask every IRQ line on both chips.
fn disable_pic() {
    unsafe {
        // ICW1: start initialisation sequence, cascade mode
        outb(0x20, 0x11); // PIC1 command
        outb(0xA0, 0x11); // PIC2 command

        // ICW2: vector offsets
        outb(0x21, 0x20); // PIC1: IRQ0-7  → vectors 0x20-0x27
        outb(0xA1, 0x28); // PIC2: IRQ8-15 → vectors 0x28-0x2F

        // ICW3: cascade wiring
        outb(0x21, 0x04); // PIC1: IR2 connected to PIC2
        outb(0xA1, 0x02); // PIC2: cascade identity 2

        // ICW4: 8086 mode
        outb(0x21, 0x01);
        outb(0xA1, 0x01);

        // OCW1: mask all IRQ lines
        outb(0x21, 0xFF);
        outb(0xA1, 0xFF);
    }
}

// ── Submodule: PIT calibration ────────────────────────────────────────────────

/// Block for approximately `ms` milliseconds using PIT channel 2.
///
/// Channel 2 is gated via bit 0 of port 0x61.  Its OUT pin (bit 5 of port
/// 0x61) goes high when the one-shot count reaches zero, giving us a
/// polled timer that doesn't disturb the IDT or require IRQs.
unsafe fn pit_wait_ms(ms: u32) {
    unsafe {
        // Briefly clear gate to reset channel 2 OUT, then configure.
        let saved = inb(0x61);
        outb(0x61, saved & !0x03); // gate off, speaker off

        // Channel 2, lo/hi byte, mode 0 (terminal count), binary: 0xB0
        outb(0x43, 0xB0);

        // count ≈ ms × 1193  (PIT runs at 1,193,182 Hz)
        let count = (ms * 1193) as u16;
        outb(0x42, count as u8);
        outb(0x42, (count >> 8) as u8);

        // Enable gate (bit 0 = 1), keep speaker off (bit 1 = 0).
        outb(0x61, (saved & !0x02) | 0x01);

        // Poll until OUT goes high (bit 5 of port 0x61).
        while inb(0x61) & 0x20 == 0 {}
    }
}

// ── Submodule: APIC base ──────────────────────────────────────────────────────

fn read_apic_base() -> u64 {
    let lo: u32;
    let hi: u32;
    unsafe {
        core::arch::asm!(
            "rdmsr",
            in("ecx") IA32_APIC_BASE,
            out("eax") lo,
            out("edx") hi,
            options(nostack, nomem),
        );
    }
    ((hi as u64) << 32 | lo as u64) & 0x000F_FFFF_FFFF_F000
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Initialise the Local APIC and start the preemption timer.
///
/// Must be called after `vmm::init()` (needs map_page) and `idt::init()`
/// (needs register_irq).
pub fn init() {
    disable_pic();

    let apic_phys = read_apic_base();

    // Compute and store the KASLR-adjusted virtual address for the APIC MMIO page.
    let va = APIC_VIRT_NOMINAL + crate::kaslr::offset();
    APIC_VIRT.store(va, Ordering::Relaxed);

    // Map the APIC MMIO page into the kernel's virtual address space.
    // KERNEL_RW is sufficient under QEMU; on real hardware PWT|PCD (cache-
    // disable) bits should also be set to avoid speculative MMIO reads.
    crate::vmm::map_page(
        crate::vmm::VirtAddr(va),
        crate::pmm::PhysAddr(apic_phys),
        crate::vmm::PageFlags::KERNEL_RW,
    );

    // Register ISR stubs in the IDT.
    crate::idt::register_irq(VECTOR_TIMER,         timer_isr_stub         as *const () as u64);
    crate::idt::register_irq(VECTOR_SPURIOUS,      spurious_isr_stub      as *const () as u64);
    crate::idt::register_irq(VECTOR_TLB_SHOOTDOWN, tlb_shootdown_isr_stub as *const () as u64);

    // Enable the APIC: set SVR bit 8 (software enable) + spurious vector.
    apic_write(REG_SVR, (1 << 8) | VECTOR_SPURIOUS as u32);

    // ── Calibrate timer against PIT ───────────────────────────────────────
    // Set divide-by-16, start a masked one-shot count at max value, wait
    // 10 ms via PIT channel 2, then read back how many ticks elapsed.
    apic_write(REG_TDCR,  0x3);  // divide by 16
    apic_write(REG_TIMER, TIMER_MASKED | VECTOR_TIMER as u32);
    apic_write(REG_TICR,  0xFFFF_FFFF);

    unsafe { pit_wait_ms(10) };

    let remaining      = apic_read(REG_TCCR);
    let ticks_per_10ms = 0xFFFF_FFFFu32.wrapping_sub(remaining);
    let ticks_per_ms   = (ticks_per_10ms / 10).max(1); // guard against zero
    TICKS_PER_MS.store(ticks_per_ms as u64, Ordering::Relaxed);

    // ── Start periodic 1 ms timer ─────────────────────────────────────────
    apic_write(REG_TDCR,  0x3);
    apic_write(REG_TICR,  ticks_per_ms);
    apic_write(REG_TIMER, TIMER_PERIODIC | VECTOR_TIMER as u32); // unmasked

    // Mark APIC as live so send_tlb_shootdown_ipi stops being a no-op.
    APIC_ONLINE.store(true, Ordering::Release);
}

/// Initialise the Local APIC on an Application Processor.
///
/// The BSP has already mapped the APIC MMIO page and calibrated `TICKS_PER_MS`.
/// This function enables the AP's local APIC and starts its periodic 1 ms timer
/// using the same period, so all CPUs share one tick rate.
pub fn ap_apic_init() {
    let ticks_per_ms = TICKS_PER_MS.load(Ordering::Relaxed) as u32;
    apic_write(REG_SVR,   (1 << 8) | VECTOR_SPURIOUS as u32);
    apic_write(REG_TDCR,  0x3);
    apic_write(REG_TICR,  ticks_per_ms);
    apic_write(REG_TIMER, TIMER_PERIODIC | VECTOR_TIMER as u32);
}

/// Signal end-of-interrupt to the Local APIC.
/// Must be called from every hardware IRQ handler before returning.
#[inline]
pub fn eoi() {
    apic_write(REG_EOI, 0);
}

/// Send a TLB shootdown IPI to all CPUs except the current one.
///
/// Uses the APIC ICR "All Excluding Self" destination shorthand so no APIC
/// ID lookup is needed.  Polls the delivery-status bit until the IPI has
/// been accepted by the bus, then returns.
///
/// On a single-CPU system the IPI goes nowhere and the delivery-status bit
/// clears immediately — the call is effectively a no-op with a brief MMIO
/// read/write.  On SMP the receiving CPUs execute `tlb_shootdown_handler`,
/// which reloads CR3 to flush their TLB before sending EOI.
///
/// Safe to call with interrupts disabled (which is the typical case inside
/// syscall handlers and ISRs).
pub fn send_tlb_shootdown_ipi() {
    if !APIC_ONLINE.load(Ordering::Relaxed) { return; }

    // ICR low-dword encoding:
    //   [7:0]   = vector
    //   [10:8]  = 000 (Fixed delivery)
    //   [14]    = 1   (Level = Assert, required for edge-triggered fixed IPIs)
    //   [15]    = 0   (Trigger = Edge)
    //   [19:18] = 11  (Destination shorthand: All Excluding Self)
    let icr_low: u32 = (3 << 18) | (1 << 14) | VECTOR_TLB_SHOOTDOWN as u32;

    // Write ICR_HIGH first (destination field — ignored for shorthand, but
    // must be written before ICR_LOW which triggers the IPI).
    apic_write(REG_ICR_HIGH, 0);
    apic_write(REG_ICR_LOW,  icr_low);

    // Spin until the APIC clears the Send Pending bit (bit 12 = 0 → Idle).
    while apic_read(REG_ICR_LOW) & (1 << 12) != 0 {
        core::hint::spin_loop();
    }
}

/// Return the kernel virtual address of the APIC MMIO page.
///
/// Valid only after `apic::init()` has run; used by `smp` for ICR writes.
#[inline]
pub fn mmio_va() -> u64 {
    apic_virt()
}

/// Return the LAPIC ID of the calling processor (bits 31:24 of LAPIC[0x020]).
///
/// Uses the already-mapped APIC virtual address set by `init()`, so it is safe
/// to call from any CPU after `apic::init()` has run on the BSP.
#[inline]
pub fn lapic_id() -> u8 {
    (apic_read(0x020) >> 24) as u8
}

/// Return the number of 1 ms timer ticks since `init()`.
#[inline]
pub fn ticks() -> u64 {
    TICK_COUNT.load(Ordering::Relaxed)
}

// ── IRQ handlers (called from assembly stubs) ─────────────────────────────────

/// Called by `tlb_shootdown_isr_stub` on receipt of a TLB shootdown IPI.
///
/// Reloads CR3 with its current value, which flushes all non-global TLB
/// entries on this CPU.  Sends EOI before returning so the APIC can accept
/// further interrupts.
#[unsafe(no_mangle)]
pub extern "C" fn tlb_shootdown_handler() {
    unsafe {
        // Read CR3 and write it back — the CPU flushes non-global TLB entries
        // on any CR3 write, even when the value is unchanged.
        let cr3: u64;
        core::arch::asm!("mov {0}, cr3", out(reg) cr3, options(nostack, nomem));
        core::arch::asm!("mov cr3, {0}", in(reg) cr3, options(nostack));
    }
    eoi();
}

/// Called by `timer_isr_stub` on every APIC timer tick (~1 ms).
#[unsafe(no_mangle)]
pub extern "C" fn timer_interrupt_handler() {
    let tick = TICK_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    eoi(); // acknowledge before yielding so the next tick can be queued
    crate::task::wake_sleepers(tick);
    crate::task::yield_task();
}
