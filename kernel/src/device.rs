//! Userspace device-driver framework — kernel side.
//!
//! Lets a ring-3 driver own exactly one PCI device via an unforgeable
//! `CapKind::Device` capability. The kernel enumerates devices it does not
//! drive into `pci`'s registry (see `pci::init_device_registry`); lythd claims
//! each by name (`SYS_DEV_CLAIM`) and hands the resulting cap to the intended
//! driver. From then on the driver — and only the driver holding that cap — may:
//!
//! * read the device's PCI config space (`SYS_DEV_CFG_READ`) — so it can walk
//!   the modern virtio-pci capability list without port-I/O authority,
//! * map the device's MMIO BARs uncacheable (`SYS_DEV_MMIO_MAP`),
//! * allocate DMA buffers handed to the device (`SYS_DEV_DMA_ALLOC`),
//! * block on and acknowledge the device IRQ (`SYS_DEV_IRQ_WAIT` / `_ACK`).
//!
//! Every entry point is gate-before-args: the Device cap is resolved first, and
//! a caller without it gets `ENOPERM` before any argument is examined. No
//! ambient authority — a process with no Device cap can touch no device.
//!
//! ## DMA trust model
//!
//! Without an IOMMU a driver that programs DMA can address any physical memory,
//! so a Device-cap holder is trusted-for-DMA. Buffers are nonetheless minted
//! through `SYS_DEV_DMA_ALLOC` (zeroed on alloc) so an IOMMU domain could later
//! be programmed at this one chokepoint.
//!
//! ## IRQ model (level-triggered PCI INTx)
//!
//! On the device IRQ the kernel masks the IOAPIC line (so the still-asserted
//! level does not storm before userspace clears the device ISR), records a
//! pending flag, and wakes any driver blocked in `SYS_DEV_IRQ_WAIT`. The driver
//! services the interrupt (reading the device's own ISR register to deassert
//! the line) and calls `SYS_DEV_IRQ_ACK`, which unmasks the line.

use core::arch::global_asm;
use core::cell::UnsafeCell;

use crate::cap::{CapHandle, CapKind, CapRights, KernelObject};
use crate::syscall::{EINVAL, ENOENT, ENOPERM};
use crate::task::TaskId;

// ── IRQ vectors ───────────────────────────────────────────────────────────────
//
// 32 = APIC timer, 33 = TLB shootdown, 34 = virtio-blk. 35 was the retired
// in-kernel virtio-net vector; device IRQs start at 36.

const DEV_IRQ_VEC_BASE: u8 = 36;

/// Maximum simultaneously-claimed IRQ-driven devices.
pub const MAX_DEV_IRQS: usize = 4;

// ── Per-device IRQ state ──────────────────────────────────────────────────────

struct DevIrq {
    active:       bool,
    registry_idx: usize,
    gsi:          u32,
    pending:      bool,
    waiter:       Option<TaskId>,
}

impl DevIrq {
    const NONE: Self = Self {
        active: false, registry_idx: 0, gsi: 0, pending: false, waiter: None,
    };
}

struct IrqTable(UnsafeCell<[DevIrq; MAX_DEV_IRQS]>);
// SAFETY: single-threaded kernel; the ISR and syscall paths coordinate through
// the IF-off discipline in `sys_dev_irq_wait`.
unsafe impl Sync for IrqTable {}

static DEV_IRQS: IrqTable = IrqTable(UnsafeCell::new(
    [const { DevIrq::NONE }; MAX_DEV_IRQS],
));

#[inline]
fn irqs() -> &'static mut [DevIrq; MAX_DEV_IRQS] {
    unsafe { &mut *DEV_IRQS.0.get() }
}

/// Find the IRQ slot serving registry device `idx`.
fn irq_slot_for(registry_idx: usize) -> Option<usize> {
    irqs().iter().position(|s| s.active && s.registry_idx == registry_idx)
}

// ── IRQ stubs ─────────────────────────────────────────────────────────────────
//
// One stub per possible device IRQ slot; each loads its slot index and calls
// the shared dispatcher. Register conventions mirror the virtio-blk stub.

global_asm!(r#"
.section .text
.macro DEV_ISR_STUB name, slot
.global \name
.type   \name, @function
\name:
    pushq %rax
    pushq %rcx
    pushq %rdx
    pushq %rsi
    pushq %rdi
    pushq %r8
    pushq %r9
    pushq %r10
    pushq %r11
    movl  $\slot, %edi
    call  device_irq_dispatch
    popq  %r11
    popq  %r10
    popq  %r9
    popq  %r8
    popq  %rdi
    popq  %rsi
    popq  %rdx
    popq  %rcx
    popq  %rax
    iretq
.endm

DEV_ISR_STUB dev_isr_stub_0, 0
DEV_ISR_STUB dev_isr_stub_1, 1
DEV_ISR_STUB dev_isr_stub_2, 2
DEV_ISR_STUB dev_isr_stub_3, 3
"#, options(att_syntax));

unsafe extern "C" {
    fn dev_isr_stub_0();
    fn dev_isr_stub_1();
    fn dev_isr_stub_2();
    fn dev_isr_stub_3();
}

fn stub_addr(slot: usize) -> u64 {
    let f: unsafe extern "C" fn() = match slot {
        0 => dev_isr_stub_0,
        1 => dev_isr_stub_1,
        2 => dev_isr_stub_2,
        _ => dev_isr_stub_3,
    };
    f as *const () as u64
}

/// Shared device-IRQ handler. Runs in interrupt context: mask the line (so a
/// level-triggered PCI IRQ does not re-fire before the driver clears the device
/// ISR), latch pending, wake the waiting driver, EOI.
#[unsafe(no_mangle)]
pub extern "C" fn device_irq_dispatch(slot: u32) {
    let slot = slot as usize;
    if let Some(st) = irqs().get_mut(slot) {
        if st.active {
            crate::ioapic::mask_irq(st.gsi);
            st.pending = true;
            if let Some(id) = st.waiter.take() {
                crate::task::wake_task(id);
            }
        }
    }
    crate::apic::eoi();
}

// ── Cap resolution (gate-before-args) ─────────────────────────────────────────

/// Resolve a Device capability handle held by the current task to the registry
/// index it names, checking `need` rights. Returns `Err(ENOPERM)` when the
/// caller does not hold a matching Device cap — the framework gate.
fn resolve_device(handle: u64, need: CapRights) -> Result<usize, u64> {
    let table_ptr = crate::task::cap_table_ptr(crate::task::current_task_id());
    if table_ptr.is_null() { return Err(ENOPERM); }
    let table = unsafe { &*table_ptr };

    let cap = table.get(CapHandle(handle)).map_err(|_| ENOPERM)?;
    if cap.kind != CapKind::Device { return Err(ENOPERM); }
    if !cap.rights.has(need) { return Err(ENOPERM); }

    match crate::cap::get_object(cap.object) {
        Some(KernelObject::Device { registry_idx }) => Ok(*registry_idx),
        _ => Err(ENOPERM),
    }
}

// ── Syscall handlers ──────────────────────────────────────────────────────────

/// `SYS_DEV_CLAIM` — claim a registered device by name, minting a Device cap.
///
/// Gated on the Rollback capability so only lythd (which alone holds it) claims
/// devices from the registry; lythd then delegates the cap to the driver.
pub fn sys_dev_claim(name_ptr: u64, name_len: u64) -> u64 {
    // Gate: caller must hold a Rollback cap (lythd-exclusive).
    let table_ptr = crate::task::cap_table_ptr(crate::task::current_task_id());
    if table_ptr.is_null() { return ENOPERM; }
    if !unsafe { &*table_ptr }.has_kind(CapKind::Rollback) { return ENOPERM; }

    // Validate + copy the name from user memory.
    if name_len == 0 || name_len > 64 { return EINVAL; }
    if !crate::syscall::valid_user_range(name_ptr, name_len) { return EINVAL; }
    let mut buf = [0u8; 64];
    unsafe {
        crate::syscall::with_user_access(|| core::ptr::copy_nonoverlapping(
            name_ptr as *const u8, buf.as_mut_ptr(), name_len as usize,
        ));
    }
    let name = match core::str::from_utf8(&buf[..name_len as usize]) {
        Ok(s) => s,
        Err(_) => return EINVAL,
    };

    let idx = match crate::pci::find_registered(name) {
        Some(i) => i,
        None => return ENOENT,
    };
    // Claim-once: a device belongs to exactly one driver.
    if !crate::pci::mark_claimed(idx) { return EINVAL; }

    // Wire the device IRQ: allocate a slot, install the stub, unmask the line.
    if let Some(dev) = crate::pci::registry_get(idx) {
        if let Some(slot) = irqs().iter().position(|s| !s.active) {
            let vector = DEV_IRQ_VEC_BASE + slot as u8;
            irqs()[slot] = DevIrq {
                active: true, registry_idx: idx, gsi: dev.irq_line as u32,
                pending: false, waiter: None,
            };
            crate::idt::register_irq(vector, stub_addr(slot));
            crate::ioapic::map_irq(
                dev.irq_line as u32,
                vector,
                crate::ioapic::IRQ_LEVEL | crate::ioapic::IRQ_ACTIVE_LO,
            );
        }
    }

    // Mint the Device cap into the caller's (lythd's) table.
    let obj = match crate::cap::create_object(KernelObject::Device { registry_idx: idx }) {
        Ok(o) => o,
        Err(_) => return EINVAL,
    };
    let handle = crate::cap::create_root_cap(
        unsafe { &mut *table_ptr }, CapKind::Device, CapRights::ALL, obj,
    );
    handle.0
}

/// `SYS_DEV_CFG_READ` — read one dword from the device's PCI config space.
pub fn sys_dev_cfg_read(dev_cap: u64, offset: u64) -> u64 {
    let idx = match resolve_device(dev_cap, CapRights::READ) {
        Ok(i) => i,
        Err(e) => return e,
    };
    if offset >= 256 || offset & 0x3 != 0 { return EINVAL; }
    match crate::pci::registry_cfg_read(idx, offset as u8) {
        Some(v) => v as u64,
        None => EINVAL,
    }
}

/// `SYS_DEV_MMIO_MAP` — map device MMIO BAR `bar` uncacheable at `virt`.
pub fn sys_dev_mmio_map(dev_cap: u64, bar: u64, virt: u64) -> u64 {
    let idx = match resolve_device(dev_cap, CapRights::WRITE) {
        Ok(i) => i,
        Err(e) => return e,
    };
    if bar >= 6 { return EINVAL; }
    if virt & 0xFFF != 0 || virt < 0x4000_0000 || virt >= 0x0000_8000_0000_0000 {
        return EINVAL;
    }
    let dev = match crate::pci::registry_get(idx) { Some(d) => d, None => return EINVAL };
    let barinfo = dev.bars[bar as usize];
    if !barinfo.is_mem || barinfo.size == 0 { return EINVAL; }
    if barinfo.base & 0xFFF != 0 { return EINVAL; }

    let pages = ((barinfo.size + 0xFFF) / 0x1000) as u64;
    // Guard the user virtual range end.
    if virt.checked_add(pages * 0x1000).map_or(true, |e| e > 0x0000_8000_0000_0000) {
        return EINVAL;
    }

    let flags = crate::vmm::PageFlags(
        crate::vmm::PageFlags::PRESENT.0
            | crate::vmm::PageFlags::WRITABLE.0
            | crate::vmm::PageFlags::USER.0
            | crate::vmm::PageFlags::NX.0
            | crate::vmm::PageFlags::NO_CACHE.0
            | crate::vmm::PageFlags::MMIO_NOFREE.0,
    );
    let pml4 = crate::task::current_page_table();
    for i in 0..pages {
        let v = crate::vmm::VirtAddr(virt + i * 0x1000);
        let p = crate::pmm::PhysAddr(barinfo.base + i * 0x1000);
        match pml4 {
            Some(t) => crate::vmm::map_page_in(crate::pmm::PhysAddr(t), v, p, flags),
            None    => crate::vmm::map_page(v, p, flags),
        }
    }
    barinfo.size
}

/// `SYS_DEV_DMA_ALLOC` — allocate a zeroed contiguous DMA buffer at `virt`,
/// return its physical address via `out_phys_ptr`.
pub fn sys_dev_dma_alloc(dev_cap: u64, virt: u64, size: u64, out_phys_ptr: u64) -> u64 {
    let _idx = match resolve_device(dev_cap, CapRights::WRITE) {
        Ok(i) => i,
        Err(e) => return e,
    };
    if size == 0 || size > 0x40_0000 { return EINVAL; } // cap at 4 MiB per buffer
    if virt & 0xFFF != 0 || virt < 0x4000_0000 || virt >= 0x0000_8000_0000_0000 {
        return EINVAL;
    }
    if out_phys_ptr == 0 || !crate::syscall::valid_user_range(out_phys_ptr, 8) {
        return EINVAL;
    }
    let pages = ((size + 0xFFF) / 0x1000) as usize;
    if virt.checked_add((pages as u64) * 0x1000).map_or(true, |e| e > 0x0000_8000_0000_0000) {
        return EINVAL;
    }

    // Physically-contiguous frames (the device sees one linear buffer).
    let phys = match crate::pmm::alloc_frames_contiguous(pages) {
        Some(p) => p.as_u64(),
        None => return EINVAL,
    };
    // Zero on alloc — no stale RAM contents leak to the device or a later owner.
    unsafe { core::ptr::write_bytes(phys as *mut u8, 0, pages * 0x1000); }

    // DMA buffers are ordinary cacheable RAM (QEMU/PCI DMA is coherent) and are
    // PMM-owned, so they are freed normally on task teardown (no MMIO_NOFREE).
    let flags = crate::vmm::PageFlags::USER_RW;
    let pml4 = crate::task::current_page_table();
    for i in 0..pages as u64 {
        let v = crate::vmm::VirtAddr(virt + i * 0x1000);
        let p = crate::pmm::PhysAddr(phys + i * 0x1000);
        match pml4 {
            Some(t) => crate::vmm::map_page_in(crate::pmm::PhysAddr(t), v, p, flags),
            None    => crate::vmm::map_page(v, p, flags),
        }
    }

    unsafe {
        crate::syscall::with_user_access(|| {
            (out_phys_ptr as *mut u64).write_unaligned(phys);
        });
    }
    0
}

/// `SYS_DEV_IRQ_WAIT` — block until the device raises its IRQ.
pub fn sys_dev_irq_wait(dev_cap: u64) -> u64 {
    let idx = match resolve_device(dev_cap, CapRights::READ) {
        Ok(i) => i,
        Err(e) => return e,
    };
    let slot = match irq_slot_for(idx) { Some(s) => s, None => return ENOPERM };

    loop {
        // IF off across the check-and-block window so the ISR cannot race
        // between our pending test and the task going Blocked (lost wakeup).
        unsafe { core::arch::asm!("cli", options(nomem, nostack)) };
        if irqs()[slot].pending {
            irqs()[slot].pending = false;
            // sysretq restores the caller's RFLAGS (IF set); return with IF off.
            return 0;
        }
        irqs()[slot].waiter = Some(crate::task::current_task_id());
        // block_and_yield marks us Blocked and switches away with IF still off,
        // re-enabling interrupts only when we are resumed.
        crate::task::block_and_yield();
    }
}

/// `SYS_DEV_IRQ_ACK` — unmask the device IRQ after the driver has serviced it.
pub fn sys_dev_irq_ack(dev_cap: u64) -> u64 {
    let idx = match resolve_device(dev_cap, CapRights::READ) {
        Ok(i) => i,
        Err(e) => return e,
    };
    let slot = match irq_slot_for(idx) { Some(s) => s, None => return ENOPERM };
    crate::ioapic::unmask_irq(irqs()[slot].gsi);
    0
}
