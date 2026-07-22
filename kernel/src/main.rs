#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::arch::global_asm;
use core::panic::PanicInfo;
use crate::serial::{OK, RST, STAT, TAG, VRB};
#[cfg(feature = "boot-tests")]
use crate::serial::WIN;

pub mod acpi;
pub mod apic;
pub mod cap;
pub mod console;
pub mod font8x16;
pub mod framebuffer;
pub mod kaslr;
pub mod keyboard;
pub mod ioapic;
pub mod smp;
pub mod pci;
pub mod virtio_blk;
pub mod device;
pub mod elf;
mod exceptions;
mod gdt;
pub mod heap;
mod idt;
pub mod ipc;
pub mod kdf;
pub mod log;
pub mod pmm;
pub mod serial;
pub mod rfs; // V1 driver — retired, superseded by vfs + fs/rfs2 (docs/rfs-v2/01 §4)
pub mod vfs;
pub mod syscall;
pub mod task;
pub mod time;
pub mod tss;
pub mod vmm;

// ISR stubs for vectors 0–31, gdt_flush helper, isr_stub_table.
global_asm!(include_str!("arch/x86_64/isr_stubs.s"), options(att_syntax));

// ── Limine boot protocol ──────────────────────────────────────────────────────
//
// Request statics tell Limine which responses to fill before jumping to
// kernel_main.  They must live in specific ELF sections so the bootloader
// can locate them; the linker script maps those sections at fixed addresses.
//
// ALL response pointers become invalid after vmm::init() installs a new CR3
// (which discards Limine's page tables).  kernel_main copies every needed
// value out of the responses BEFORE calling pmm::init_from_limine / vmm::init.

use limine::{BaseRevision, RequestsEndMarker, RequestsStartMarker};
use limine::request::{
    ExecutableAddressRequest, FramebufferRequest, HhdmRequest, MemmapRequest, RsdpRequest,
};

#[used] #[unsafe(link_section = ".requests_start")]
static REQUESTS_START: RequestsStartMarker = RequestsStartMarker::new();

#[used] #[unsafe(link_section = ".requests")]
static BASE_REVISION: BaseRevision = BaseRevision::new();

#[used] #[unsafe(link_section = ".requests")]
static FRAMEBUFFER_REQ: FramebufferRequest = FramebufferRequest::new();

#[used] #[unsafe(link_section = ".requests")]
static MEMMAP_REQ: MemmapRequest = MemmapRequest::new();

#[used] #[unsafe(link_section = ".requests")]
static HHDM_REQ: HhdmRequest = HhdmRequest::new();

#[used] #[unsafe(link_section = ".requests")]
static RSDP_REQ: RsdpRequest = RsdpRequest::new();

#[used] #[unsafe(link_section = ".requests")]
static KERNEL_ADDR_REQ: ExecutableAddressRequest = ExecutableAddressRequest::new();

#[used] #[unsafe(link_section = ".requests_end")]
static REQUESTS_END: RequestsEndMarker = RequestsEndMarker::new();

// ── Copied framebuffer info ───────────────────────────────────────────────────

/// Framebuffer fields copied from the Limine response before vmm::init().
struct BootFb {
    phys:   u64,
    pitch:  u64,
    width:  u64,
    height: u64,
    bpp:    u16,
}

// ── Kernel entry point ────────────────────────────────────────────────────────

/// Kernel entry point — called by Limine in 64-bit long mode.
///
/// On entry (Limine protocol guarantees):
///   - Interrupts disabled; SSE/SSE2 enabled.
///   - CR3: Limine's PML4 (identity 0→4 GiB + HHDM + kernel at ELF VAs).
///   - RSP: top of a ≥ 64 KiB stack in reclaimable memory.
///   - All request response pointers filled before this call.
#[unsafe(no_mangle)]
pub extern "C" fn kernel_main() -> ! {
    // ── Collect all Limine responses ─────────────────────────────────────────
    //
    // vmm::init() will install a new CR3, discarding Limine's page tables.
    // Every response pointer dereference must complete here, before pmm init.

    // HHDM offset: maps physical_addr → Limine virtual_addr via
    //   virt = hhdm_offset + phys  ⟹  phys = virt − hhdm_offset
    let hhdm_off: u64 = HHDM_REQ
        .response()
        .map(|r| r.offset)          // public field on Response<HhdmRespData>
        .unwrap_or_else(|| panic!("limine: no HHDM response — is the kernel in a Limine image?"));

    // Memory map: copy entries to a stack buffer (max 256; real systems have < 30).
    // r.entries() returns &[&Entry] — a slice of references already, no raw pointer needed.
    const MAX_MMAP: usize = 256;
    let mut mmap_entries = [(0u64, 0u64, 0u64); MAX_MMAP];
    let mmap_len: usize = {
        let r = MEMMAP_REQ
            .response()
            .unwrap_or_else(|| panic!("limine: no memory-map response"));
        let slice = r.entries();
        let n = slice.len().min(MAX_MMAP);
        for (i, e) in slice.iter().take(n).enumerate() {
            mmap_entries[i] = (e.base, e.length, e.type_ as u64);
        }
        n
    };

    // Framebuffer: copy fields; convert Limine's HHDM virtual address to physical.
    // fb.address() is a method returning the HHDM virtual address of the FB MMIO.
    let boot_fb: Option<BootFb> = FRAMEBUFFER_REQ.response().and_then(|r| {
        r.framebuffers().first().map(|fb| BootFb {
            phys:   fb.address() as u64 - hhdm_off,
            pitch:  fb.pitch,
            width:  fb.width,
            height: fb.height,
            bpp:    fb.bpp,
        })
    });

    // RSDP: physical address for the ACPI/MADT parser (acpi::init below).
    // Limine hands the RSDP out as an HHDM virtual address on older base
    // revisions and a physical address on newer ones — normalise to physical.
    let rsdp_phys: u64 = RSDP_REQ
        .response()
        .map(|r| {
            let addr = r.address as u64;
            if addr >= hhdm_off { addr - hhdm_off } else { addr }
        })
        .unwrap_or(0);

    // Kernel address: physical and virtual base of the first LOAD segment.
    // Used by pmm::init_from_limine (to re-mark kernel frames as used) and
    // vmm::init (to re-map the kernel at its higher-half VA in the new PML4).
    let (kernel_phys_base, kernel_virt_base) = KERNEL_ADDR_REQ
        .response()
        .map(|r| (r.physical_base, r.virtual_base))
        .unwrap_or_else(|| panic!("limine: no kernel address response"));

    // ── All Limine data copied.  Safe to replace CR3 after pmm::init. ───────

    serial::init();
    kprintln!();
    kprintln!("{TAG}  ██╗      ██╗   ██╗████████╗██╗  ██╗ ██████╗ ███████╗{RST}");
    kprintln!("{TAG}  ██║      ╚██╗ ██╔╝╚══██╔══╝██║  ██║██╔═══██╗██╔════╝{RST}");
    kprintln!("{TAG}  ██║       ╚████╔╝    ██║   ███████║██║   ██║███████╗{RST}");
    kprintln!("{TAG}  ██║        ╚██╔╝     ██║   ██╔══██║██║   ██║╚════██║{RST}");
    kprintln!("{TAG}  ███████╗    ██║      ██║   ██║  ██║╚██████╔╝███████║{RST}");
    kprintln!("{TAG}  ╚══════╝    ╚═╝      ╚═╝   ╚═╝  ╚═╝ ╚═════╝ ╚══════╝{RST}");
    kprintln!("  {VRB}x86_64 microkernel · capability-aware · Limine protocol{RST}");
    kprintln!();
    kprintln!("{TAG}lythos{RST} kernel initializing...");

    // Sanity-check that our BASE_REVISION request was honoured.
    if !BASE_REVISION.is_supported() {
        panic!("limine: bootloader does not support the requested base revision — update Limine");
    }

    kaslr::init();

    gdt::init();
    kprintln!("{TAG}[gdt]{RST} loaded");

    idt::init();
    kprintln!("{TAG}[idt]{RST} loaded {VRB}- exceptions active{RST}");

    // ── Physical memory manager ──────────────────────────────────────────────
    // Kernel physical range: [kernel_phys_base, kernel_phys_base + (KERNEL_END - KERNEL_START)).
    unsafe extern "C" { static KERNEL_START: u8; static KERNEL_END: u8; }
    let kernel_phys_end = kernel_phys_base
        + (&raw const KERNEL_END as u64).saturating_sub(&raw const KERNEL_START as u64);
    pmm::init_from_limine(&mmap_entries[..mmap_len], kernel_phys_base, kernel_phys_end);
    kprintln!(
        "{TAG}[pmm]{RST} initialized — {STAT}{}{RST} free frames ({STAT}{} MiB{RST})",
        pmm::free_frame_count(),
        pmm::free_frame_count() * 4 / 1024
    );

    // ── Smoke-test: alloc 1000 frames, free, re-alloc, verify same addrs ────
    let mut frames = [pmm::PhysAddr(0); 1000];
    for f in frames.iter_mut() {
        *f = pmm::alloc_frame().expect("pmm smoke-test: out of frames");
    }
    for &f in frames.iter().rev() {
        pmm::free_frame(f);
    }
    for (i, f) in frames.iter().enumerate() {
        let got = pmm::alloc_frame().expect("pmm smoke-test: out of frames on re-alloc");
        assert_eq!(got, *f, "pmm smoke-test: frame mismatch at index {}", i);
    }
    // Return the 1000 frames so they don't pollute later allocations.
    for &f in frames.iter().rev() {
        pmm::free_frame(f);
    }
    kprintln!("{TAG}[pmm]{RST} smoke-test {OK}passed{RST}");

    // ── Virtual memory manager ────────────────────────────────────────────────
    // Installs a new PML4 (identity 0→1 GiB + kernel at its higher-half VA).
    // After this call, Limine's page tables are gone; use only our own mappings.
    vmm::init(kernel_phys_base, kernel_virt_base, kernel_phys_end, hhdm_off);
    kprintln!("{TAG}[vmm]{RST} paging active {VRB}— identity 0–4MiB, higher-half kernel mapped{RST}");

    // ── VMM smoke-test: map a scratch page, write to it, unmap it ─────────────
    {
        let test_virt = vmm::VirtAddr(0xFFFF_A000_0001_0000); // higher-half scratch VA
        let test_phys = pmm::alloc_frame().expect("vmm smoke-test: no frame");
        vmm::map_page(test_virt, test_phys, vmm::PageFlags::KERNEL_RW);
        // Write a sentinel through the mapping and read it back.
        unsafe {
            let p = test_virt.as_u64() as *mut u64;
            p.write_volatile(0xDEAD_BEEF_CAFE_BABE);
            assert_eq!(
                p.read_volatile(),
                0xDEAD_BEEF_CAFE_BABE,
                "vmm smoke-test: sentinel mismatch"
            );
        }
        vmm::unmap_page(test_virt);
        pmm::free_frame(test_phys);
    }
    kprintln!("{TAG}[vmm]{RST} smoke-test {OK}passed{RST}");

    // ── Framebuffer ───────────────────────────────────────────────────────────
    // Uses the pre-copied BootFb (physical address + dimensions).
    // Falls back to Bochs VBE probe if Limine supplied no framebuffer response.
    {
        let (phys, pitch, width, height, bpp) = boot_fb
            .map(|f| (f.phys, f.pitch, f.width, f.height, f.bpp))
            .unwrap_or((0, 0, 0, 0, 0));

        if framebuffer::init_from_limine(phys, pitch, width, height, bpp) {
            let (fw, fh) = framebuffer::dimensions();
            let fpitch = pitch;
            kprintln!(
                "{TAG}[fb]{RST} {STAT}{}×{}{RST} px  pitch={STAT}{}{RST}  bpp={STAT}{}{RST}  phys={STAT}{:#x}{RST}",
                fw, fh, fpitch, bpp, phys
            );
            console::init();
            let (cc, cr) = console::dimensions();
            println!("PureshadeOS — Lythos kernel");
            println!("fb console {}x{} cells ({}x{} px)", cc, cr, fw, fh);
        } else {
            kprintln!("{VRB}[fb] no framebuffer — run with a Limine virtio-gpu or -vga std for display{RST}");
        }
    }

    // ── Heap allocator ────────────────────────────────────────────────────────
    heap::init();
    kprintln!(
        "{TAG}[heap]{RST} initialized — {STAT}{} KiB{RST} pre-mapped at {STAT}{:#x}{RST}",
        heap::HEAP_INIT_PAGES * 4,
        heap::heap_start(),
    );

    // ── Heap smoke-test ───────────────────────────────────────────────────────
    {
        // Box<T>: single heap allocation, dealloc on drop.
        let b = Box::new(0xDEAD_BEEF_u64);
        assert_eq!(*b, 0xDEAD_BEEF_u64, "heap smoke-test: Box value mismatch");
        drop(b);

        // Vec<T>: heap-backed growable array.
        let mut v = Vec::<u8>::with_capacity(256);
        for i in 0..64_u8 {
            v.push(i);
        }
        assert_eq!(v.len(), 64, "heap smoke-test: Vec length mismatch");
        assert_eq!(v[0], 0, "heap smoke-test: Vec[0] mismatch");
        assert_eq!(v[63], 63, "heap smoke-test: Vec[63] mismatch");
    }
    kprintln!("{TAG}[heap]{RST} smoke-test {OK}passed{RST}");

    // ── Scheduler ─────────────────────────────────────────────────────────────
    task::init();
    kprintln!("{TAG}[sched]{RST} initialized");

    task::spawn_kernel_task(task_b);

    // Cooperative yield smoke-test: task_a (this thread) and task_b alternate.
    // Expected interleaving:
    //   task A tick 0 → task B tick 0 → task A tick 1 → task B tick 1
    //   → task A tick 2 → task B tick 2 → task A: smoke-test passed
    for i in 0..3_u32 {
        kprintln!("{VRB}[task A] tick {}, yielding...{RST}", i);
        task::yield_task();
    }
    kprintln!("{TAG}[sched]{RST} smoke-test {OK}passed{RST}");

    // ── ACPI MADT — interrupt controller discovery ────────────────────────────
    // Reads RSDP → R/XSDT → MADT through the identity map. Supplies the
    // I/O APIC base + GSI base and any ISA Interrupt Source Overrides.
    if acpi::init(rsdp_phys) {
        match acpi::lapic_phys() {
            Some(base) => kprintln!("{TAG}[acpi]{RST} MADT: LAPIC base {STAT}{:#x}{RST}", base),
            None       => kprintln!("{VRB}[acpi] MADT: no LAPIC base entry{RST}"),
        }
        match acpi::ioapic_info() {
            Some((base, gsi0)) => kprintln!(
                "{TAG}[acpi]{RST} MADT: IOAPIC base {STAT}{:#x}{RST}, GSI base {STAT}{}{RST}",
                base, gsi0
            ),
            None => kprintln!("{VRB}[acpi] MADT: no IOAPIC entry — using default base{RST}"),
        }
    } else {
        kprintln!("{VRB}[acpi] no MADT (RSDP {:#x}) — IOAPIC defaults, identity ISA→GSI{RST}", rsdp_phys);
    }

    // ── APIC + preemptive timer ───────────────────────────────────────────────
    // apic::init() also remaps + fully masks both 8259 PICs, and enables the
    // Local APIC via SVR bit 8 (spurious vector 0xFF).
    apic::init();
    kprintln!("{TAG}[apic]{RST} timer active {VRB}— preemptive scheduling enabled, 8259 PICs masked{RST}");

    // ── Wall-clock anchor (requires apic::ticks() to be live) ────────────────
    time::init();

    if let Some((base, gsi0)) = acpi::ioapic_info() {
        ioapic::set_phys_base(base, gsi0);
    }
    ioapic::init();
    kprintln!(
        "{TAG}[ioapic]{RST} initialized — base {STAT}{:#x}{RST}, {STAT}{} GSIs{RST}, all masked",
        ioapic::phys_base(),
        ioapic::entry_count(),
    );

    match keyboard::init() {
        Some((gsi, flags)) => kprintln!(
            "{TAG}[kbd]{RST} i8042 armed — IRQ1 → GSI {STAT}{}{RST}{}, vector {STAT}{}{RST}, {}{}, set-2 decode",
            gsi,
            if acpi::isa_irq_overridden(1) { " (MADT override)" } else { "" },
            keyboard::VECTOR_KBD,
            if flags & ioapic::IRQ_LEVEL != 0 { "level" } else { "edge" },
            if flags & ioapic::IRQ_ACTIVE_LO != 0 { ", active-low" } else { ", active-high" },
        ),
        None => kprintln!("{VRB}[kbd] no i8042 controller detected{RST}"),
    }

    if virtio_blk::init() {
        let sects = virtio_blk::capacity_sectors();
        kprintln!(
            "{TAG}[virtio-blk]{RST} device ready — {STAT}{} sectors ({} MiB){RST}",
            sects,
            sects / 2048,
        );
        if vfs::init() {
            kprintln!("{TAG}[vfs]{RST} rfs2 mounted");
        } else {
            kprintln!("{VRB}[vfs] no RFS V2 volume on disk (pass -drive file=disk.img,... to QEMU){RST}");
        }
    } else {
        kprintln!("{VRB}[virtio-blk] no device found (pass -device virtio-blk-pci to QEMU){RST}");
    }

    // Secondary virtio-blk device (instance 1) backs the persistent
    // /shade/store volume (store.img). Probed here so it is ready when lythd
    // issues SYS_MOUNT with MOUNT_SRC_RFS2_BLK; the RFS2 mount/format happens
    // lazily at that mount, not now. Absence is non-fatal — the store mount
    // then fails loud in lythd (no persistent store this boot).
    if virtio_blk::init_store() {
        let sects = virtio_blk::capacity_sectors_dev(virtio_blk::DEV_STORE);
        kprintln!(
            "{TAG}[virtio-blk]{RST} store device ready — {STAT}{} sectors ({} MiB){RST}",
            sects,
            sects / 2048,
        );
    } else {
        kprintln!("{VRB}[virtio-blk] no persistent store device (pass a 2nd -device virtio-blk-pci for store.img){RST}");
    }

    // Enumerate PCI devices the kernel does NOT drive (e.g. virtio-net) into
    // the device registry. Each becomes claimable by lythd via SYS_DEV_CLAIM,
    // which mints a Device capability handed to the intended userspace driver
    // (e.g. `netd`). The kernel never touches a registered device's registers,
    // IRQ, or DMA — only the driver holding its Device cap does. virtio-blk is
    // deliberately NOT registered (kernel-owned root disk + persistent store).
    pci::init_device_registry();

    // Smoke-test: sleep ~50 ms by polling the tick counter.
    let t0 = apic::ticks();
    while apic::ticks() < t0 + 50 {
        unsafe { core::arch::asm!("hlt") };
    }
    kprintln!(
        "{TAG}[apic]{RST} smoke-test {OK}passed{RST} {VRB}— {} ticks elapsed{RST}",
        apic::ticks() - t0
    );

    // ── Syscall interface ─────────────────────────────────────────────────────
    syscall::init();
    kprintln!("{TAG}[syscall]{RST} initialized {VRB}— LSTAR/STAR/FMASK configured{RST}");

    // ── SMP — start Application Processors ───────────────────────────────────
    smp::init();

    // ── Capability system ─────────────────────────────────────────────────────
    {
        let mut alice = cap::CapabilityTable::new();
        let mut bob = cap::CapabilityTable::new();

        // Create a physical memory object and give alice a root cap (all rights).
        let obj = cap::create_object(cap::KernelObject::Memory {
            base_pa: 0x1000,
            frame_count: 1,
        })
        .expect("cap smoke-test: create_object");

        let h_alice =
            cap::create_root_cap(&mut alice, cap::CapKind::Memory, cap::CapRights::ALL, obj);

        // Alice grants Bob a read-only derived capability.
        let h_bob = cap::cap_grant(
            &mut alice,
            h_alice,
            99, // placeholder task id for bob
            &mut bob,
            cap::CapRights::READ,
        )
        .expect("cap smoke-test: cap_grant");

        // Bob's cap carries only READ — WRITE/GRANT/REVOKE were masked off.
        assert_eq!(
            bob.get(h_bob).expect("cap smoke-test: get bob cap").rights,
            cap::CapRights::READ,
            "cap smoke-test: rights mismatch",
        );

        // Bob cannot re-grant (no Grant right).
        assert!(
            cap::cap_grant(&mut bob, h_bob, 0, &mut alice, cap::CapRights::READ).is_err(),
            "cap smoke-test: Bob should not be able to grant",
        );

        // Alice revokes her root cap.
        cap::cap_revoke(&mut alice, h_alice).expect("cap smoke-test: revoke");

        // Alice's handle is now invalid.
        assert!(
            alice.get(h_alice).is_err(),
            "cap smoke-test: cap should be gone after revoke",
        );
        // Bob's derived cap is still present (cascading revocation is separate).
        assert!(
            bob.get(h_bob).is_ok(),
            "cap smoke-test: Bob's derived cap should survive a single-table revoke",
        );
    }
    kprintln!("{TAG}[cap]{RST} smoke-test {OK}passed{RST}");

    // ── Cascade-revoke smoke-test ─────────────────────────────────────────────
    {
        let mut alice = cap::CapabilityTable::new();
        let mut bob = cap::CapabilityTable::new();

        let obj = cap::create_object(cap::KernelObject::Memory {
            base_pa: 0x3000,
            frame_count: 1,
        })
        .expect("cascade smoke: create_object");

        let h_alice =
            cap::create_root_cap(&mut alice, cap::CapKind::Memory, cap::CapRights::ALL, obj);
        let h_bob = cap::cap_grant(&mut alice, h_alice, 99, &mut bob, cap::CapRights::READ)
            .expect("cascade smoke: cap_grant");

        // Alice cascade-revokes her root cap → bob's derived cap disappears too.
        let bob_ptr: *mut cap::CapabilityTable = &mut bob;
        cap::cap_cascade_revoke(&mut alice, h_alice, &mut |tid| {
            if tid == 99 {
                bob_ptr
            } else {
                core::ptr::null_mut()
            }
        })
        .expect("cascade smoke: revoke");

        assert!(
            alice.get(h_alice).is_err(),
            "cascade: alice's cap should be gone"
        );
        assert!(
            bob.get(h_bob).is_err(),
            "cascade: bob's derived cap should be gone"
        );
    }
    kprintln!("{TAG}[cap]{RST} cascade-revoke smoke-test {OK}passed{RST}");

    // ── IPC smoke-test ────────────────────────────────────────────────────────
    // Two kernel tasks share one endpoint.  The receiver spawns first and
    // blocks immediately (ring is empty).  The sender runs next, posts three
    // messages, and exits.  The receiver wakes on each message, verifies the
    // payload, then exits after the third.  kmain yields until both are done.
    {
        use core::sync::atomic::{AtomicUsize, Ordering as O};
        static IPC_EP: AtomicUsize = AtomicUsize::new(usize::MAX);
        static IPC_RECV_COUNT: AtomicUsize = AtomicUsize::new(0);

        let ep_idx = ipc::create_endpoint().expect("create_endpoint");
        IPC_EP.store(ep_idx, O::Relaxed);

        fn ipc_receiver() -> ! {
            use core::sync::atomic::Ordering as O;
            let ep = IPC_EP.load(O::Relaxed);
            for expected in 1u8..=3 {
                let mut buf = [0u8; ipc::MSG_SIZE];
                ipc::recv(ep, &mut buf);
                assert_eq!(buf[0], expected, "ipc smoke: wrong payload byte");
                IPC_RECV_COUNT.fetch_add(1, O::Relaxed);
            }
            kprintln!("{VRB}[ipc] receiver done{RST}");
            task::task_exit(task::exit_status_normal(0));
        }

        fn ipc_sender() -> ! {
            use core::sync::atomic::Ordering as O;
            let ep = IPC_EP.load(O::Relaxed);
            for i in 1u8..=3 {
                let mut msg = [0u8; ipc::MSG_SIZE];
                msg[0] = i;
                ipc::send(ep, &msg);
                kprintln!("{VRB}[ipc] sent message {}{RST}", i);
            }
            kprintln!("{VRB}[ipc] sender done{RST}");
            task::task_exit(task::exit_status_normal(0));
        }

        task::spawn_kernel_task(ipc_receiver);
        task::spawn_kernel_task(ipc_sender);

        // Yield until both tasks have exited (recv count reaches 3).
        while IPC_RECV_COUNT.load(core::sync::atomic::Ordering::Relaxed) < 3 {
            task::yield_task();
        }
    }
    kprintln!("{TAG}[ipc]{RST} smoke-test {OK}passed{RST}");

    // ── Boot test suite (feature-gated) ───────────────────────────────────────
    // Userspace-entry / ELF / integration / sweep probes cost several seconds
    // per boot; they run only with `--features boot-tests` (`make kernel-tests`).
    // The cheap init smoke tests above (pmm/vmm/heap/sched/apic/cap/ipc) stay
    // unconditional.
    #[cfg(feature = "boot-tests")]
    {
    // ── Userspace entry smoke-test ────────────────────────────────────────────
    // Spawn a kernel task that maps a user code page, writes `mov eax,1;
    // syscall` into it (SYS_TASK_EXIT = 1), and enters ring 3.  The syscall
    // handler calls task_exit(), marks the task Dead, and switches back to
    // kernel_main.
    let smoke_task_id = task::spawn_kernel_task(userspace_smoke_task);
    // Yield until the smoke task has been fully reaped by sweep_dead.
    // A bare yield_task() is not enough: the APIC timer may switch back to
    // kernel_main before the task stores SMOKE_STACK_PHYS, making the physaddr zero.
    while task::task_exists(smoke_task_id) {
        task::yield_task();
    }
    // Safe: task is reaped (Dead + swept), physaddrs were stored before ring-3 entry.
    {
        use core::sync::atomic::Ordering;
        vmm::unmap_page(vmm::VirtAddr(0x0000_0001_0000_0000));
        vmm::unmap_page(vmm::VirtAddr(0x0000_0002_0000_0000));
        pmm::free_frame(pmm::PhysAddr(SMOKE_CODE_PHYS.load(Ordering::Relaxed)));
        pmm::free_frame(pmm::PhysAddr(SMOKE_STACK_PHYS.load(Ordering::Relaxed)));
    }
    kprintln!("{TAG}[syscall]{RST} userspace entry smoke-test {OK}passed{RST}");

    // ── ELF exec smoke-test ───────────────────────────────────────────────────
    // Load SMOKE_ELF (hand-crafted ELF64 that calls SYS_TASK_EXIT) via the
    // full exec path: ELF parser → segment mapping → stack allocation →
    // ABI stack frame → exec_trampoline → ring-3 entry.
    // The task exits via SYS_TASK_EXIT, returning control to kernel_main.
    elf::exec(elf::SMOKE_ELF, &[], &[]).expect("elf smoke-test: exec failed");
    task::yield_task();
    kprintln!("{TAG}[elf]{RST} smoke-test {OK}passed{RST}");

    // ── Core integration smoke tests ──────────────────────────────────────────
    //
    // Run before spawning lythd so these kernel-level checks execute without
    // any concurrent userspace tasks.  None of the checks require lythd.
    core_smoke();
    kprintln!("{WIN}[integration] all checks passed{RST}");

    // ── sweep_dead resource-release probe ─────────────────────────────────────
    // Spawn tasks that exit immediately and wait for the reap; heap and PMM
    // free counts must return to their pre-spawn values or sweep_dead leaks.
    {
        let heap_before = heap::free_bytes() as i64;
        let pmm_before  = pmm::free_frame_count() as i64;
        for _ in 0..8 {
            let id = task::spawn_kernel_task(sweep_probe_task);
            while task::task_exists(id) {
                task::yield_task();
            }
        }
        let heap_delta = heap_before - heap::free_bytes() as i64;
        let pmm_delta  = pmm_before - pmm::free_frame_count() as i64;
        kprintln!(
            "{TAG}[sweep]{RST} 8 spawn/reap cycles: heap_leak={} B pmm_leak={} frames",
            heap_delta, pmm_delta
        );

        // Same probe through the userspace path: exec + exit exercises
        // free_user_page_table (user frames, PT pages) on top of the kernel
        // stack release.
        let heap_before = heap::free_bytes() as i64;
        let pmm_before  = pmm::free_frame_count() as i64;
        for i in 0..8 {
            let before = pmm::free_frame_count();
            let id = elf::exec(elf::SMOKE_ELF, &[], &[])
                .expect("sweep probe: exec failed");
            if i == 0 {
                kprintln!(
                    "{TAG}[sweep-user]{RST} single exec cost: {} frames",
                    before - pmm::free_frame_count()
                );
            }
            while task::task_exists(id) {
                task::yield_task();
            }
        }
        let heap_delta = heap_before - heap::free_bytes() as i64;
        let pmm_delta  = pmm_before - pmm::free_frame_count() as i64;
        kprintln!(
            "{TAG}[sweep-user]{RST} 8 exec/reap cycles: heap_leak={} B pmm_leak={} frames",
            heap_delta, pmm_delta
        );
    }

    // ── Mount syscall probe (stage 1, docs/plans/mount-syscall-shade-store.md) ─
    // Routing logic is host-tested in fs/vfs-core; this probe covers the glue
    // the host cannot: the real RFS2 root + RAM volume through vfs.rs, and the
    // capability gate on the syscall boundary via syscall_dispatch.
    if vfs::generation().is_some() {
        // Cap gate, deny path: a task with no Filesystem capability gets
        // ENOPERM before any argument is inspected (a1 deliberately null).
        let mut frame = syscall::SyscallFrame {
            r15: 0, r14: 0, r13: 0, r12: 0, rbx: 0, rbp: 0, r11: 0, rcx: 0,
            nr: syscall::SYS_MOUNT, a1: 0, a2: 0, a3: 0, a4: 0, a5: 0, a6: 0,
        };
        task::set_bootstrap_cap_table(cap::CapabilityTable::new());
        let r = syscall::syscall_dispatch(&mut frame);
        assert_eq!(r as i64, -3, "mount probe: no-cap call must be ENOPERM, got {}", r as i64);

        // Cap gate, pass path: with Filesystem+WRITE the gate opens and the
        // next check (bad user pointer) reports EINVAL — proving the deny
        // above came from the capability, not the arguments.
        let fs_probe_obj = cap::create_object(cap::KernelObject::Filesystem)
            .expect("mount probe: cap OOM");
        let mut probe_caps = cap::CapabilityTable::new();
        cap::create_root_cap(
            &mut probe_caps, cap::CapKind::Filesystem, cap::CapRights::ALL, fs_probe_obj,
        );
        task::set_bootstrap_cap_table(probe_caps);
        let r = syscall::syscall_dispatch(&mut frame);
        assert_eq!(r as i64, -4, "mount probe: with-cap bad-args must be EINVAL, got {}", r as i64);

        // Real mount: fresh RAM-backed RFS2 at /mnt (dir created on root if
        // absent; EEXIST from a previous test boot is fine).
        let root_gen_before = vfs::generation().expect("root mounted");
        let r = vfs::mkdir(b"/mnt", 0, 0);
        assert!(r == 0 || r == vfs::EEXIST, "mount probe: mkdir /mnt failed: {r}");
        let root_gen_mkdir = vfs::generation().expect("root mounted");
        let r = vfs::mount("/mnt", vfs::MOUNT_SRC_RFS2_RAM, 0);
        assert_eq!(r, 0, "mount probe: mount /mnt failed: {r}");
        assert!(vfs::is_mounted_at("/mnt"), "mount probe: /mnt not registered");

        // Mounting must not mutate the root volume.
        assert_eq!(
            vfs::generation().expect("root mounted"), root_gen_mkdir,
            "mount probe: mounting changed root generation"
        );

        // Resolution crosses the boundary: file created under /mnt lands on
        // the RAM volume and reads back through the mount.
        let fd = vfs::create(b"/mnt/probe.txt", 0, 0);
        assert!(fd >= 0, "mount probe: create failed: {fd}");
        assert_eq!(vfs::write(fd as u64, b"mount-probe"), 11);
        assert_eq!(vfs::close(fd as u64), 0);
        let fd = vfs::open(b"/mnt/probe.txt");
        assert!(fd >= 0, "mount probe: reopen failed: {fd}");
        let mut buf = [0u8; 16];
        let n = vfs::read(fd as u64, &mut buf);
        assert_eq!(&buf[..n as usize], b"mount-probe", "mount probe: readback mismatch");
        assert_eq!(vfs::close(fd as u64), 0);

        // The write went to the RAM volume, not root: root generation is
        // still where mkdir left it.
        assert_eq!(
            vfs::generation().expect("root mounted"), root_gen_mkdir,
            "mount probe: /mnt write leaked onto the root volume"
        );
        let _ = root_gen_before;

        // Distinct backend instance: the RAM volume has its own generation
        // counter (fresh volume, a handful of commits — root is far ahead).
        let ram_gen = vfs::generation_at("/mnt").expect("ram volume mounted");
        assert_ne!(
            ram_gen,
            vfs::generation().expect("root mounted"),
            "mount probe: /mnt reports the root volume's generation"
        );

        // Double mount is rejected without side effects.
        let r = vfs::mount("/mnt", vfs::MOUNT_SRC_RFS2_RAM, 0);
        assert_eq!(r, vfs::EMOUNTED, "mount probe: double mount must be EMOUNTED, got {r}");

        kprintln!("{TAG}[mount]{RST} stage-1 probe {OK}passed{RST} — cap gate, routing, root isolation");

        // ── Stage-1 hardening: RamDisk direct-map ceiling + cap gate across
        // the REAL ring-3 boundary (the dispatcher probe above never leaves
        // ring 0) ──────────────────────────────────────────────────────────
        {
            // Direct-map ceiling predicate — exercises the rejection boundary
            // without needing >1 GiB of guest RAM. RamDisk::new consults this
            // before the first touch of every frame, so an out-of-range frame
            // is a clean mount failure, never a page fault.
            assert!(vfs::frame_in_direct_map(0), "ceiling: frame 0 must be mapped");
            assert!(
                vfs::frame_in_direct_map(vmm::IDENTITY_MAP_LIMIT - 4096),
                "ceiling: last in-map frame must pass"
            );
            assert!(
                !vfs::frame_in_direct_map(vmm::IDENTITY_MAP_LIMIT),
                "ceiling: first out-of-map frame must be rejected"
            );
            assert!(
                !vfs::frame_in_direct_map(vmm::IDENTITY_MAP_LIMIT + 4096),
                "ceiling: high frame must be rejected"
            );
            assert!(
                !vfs::frame_in_direct_map(u64::MAX - 4095),
                "ceiling: overflow must be rejected"
            );
            assert!(!vfs::frame_in_direct_map(0x1234), "ceiling: unaligned must be rejected");

            // Ring-3 DENY: a genuine userspace task with an EMPTY cap set
            // makes a fully VALID mount request and must get ENOPERM. The
            // ELF exits only on ENOPERM — any other answer (EINVAL would
            // mean the gate is ordered after arg checks; 0 would mean an
            // unprivileged mount succeeded) leaves it spinning and the reap
            // deadline below fails.
            let deny_id = elf::exec(elf::MOUNT_DENIED_ELF, &[], &[])
                .expect("mount ring3: deny exec failed");
            let deadline = apic::ticks() + 500;
            while apic::ticks() < deadline && task::task_exists(deny_id) {
                task::yield_task();
            }
            assert!(
                !task::task_exists(deny_id),
                "mount ring3: capless SYS_MOUNT did not answer ENOPERM"
            );

            // Ring-3 HOLDER: with a Filesystem cap (handle 0), deliberately
            // bad args must answer EINVAL — proves the gate OPENED for the
            // holder and argument validation did the rejecting. Same
            // ordering assertion as the dispatcher probe, across the real
            // privilege boundary. Exits only on EINVAL.
            let fs_probe2_obj = cap::create_object(cap::KernelObject::Filesystem)
                .expect("mount ring3: cap OOM");
            let fs_probe2_cap = {
                let tbl = unsafe { &mut *task::cap_table_ptr(0) };
                cap::create_root_cap(
                    tbl, cap::CapKind::Filesystem, cap::CapRights::ALL, fs_probe2_obj,
                )
            };
            let holder_id = elf::exec(elf::MOUNT_EINVAL_ELF, &[fs_probe2_cap], &[])
                .expect("mount ring3: holder exec failed");
            let deadline = apic::ticks() + 500;
            while apic::ticks() < deadline && task::task_exists(holder_id) {
                task::yield_task();
            }
            assert!(
                !task::task_exists(holder_id),
                "mount ring3: cap-holder SYS_MOUNT bad-args did not answer EINVAL"
            );

            kprintln!(
                "{TAG}[mount]{RST} ring-3 hardening probe {OK}passed{RST} — capless ENOPERM, holder EINVAL, direct-map ceiling enforced"
            );
        }

        // ── Stage-2 probe: store mount + read-only-after-realize ──────────
        // (design §4; RealizeGuard logic host-tested in fs/vfs-core — this
        // covers the kernel wiring on a real MOUNT_STORE mount.)
        let r = vfs::mkdir(b"/mnt-store", 0, 0);
        assert!(r == 0 || r == vfs::EEXIST, "store probe: mkdir failed: {r}");
        let root_gen = vfs::generation().expect("root mounted");
        let r = vfs::mount("/mnt-store", vfs::MOUNT_SRC_RFS2_RAM, vfs::MOUNT_STORE);
        assert_eq!(r, 0, "store probe: MOUNT_STORE mount failed: {r}");

        // Distinct backend instance from root AND from the /mnt volume.
        let store_gen = vfs::generation_at("/mnt-store").expect("store mounted");
        assert_ne!(store_gen, vfs::generation().expect("root"), "store probe: not distinct from root");

        // Realize: stage a temp dir tree, write into it (temp is writable),
        // atomically rename onto the final store name.
        const STORE_NAME: &[u8] = b"/mnt-store/abcd1234-demo-1.0";
        assert_eq!(vfs::mkdir(b"/mnt-store/.tmp-w1", 0, 0), 0);
        let fd = vfs::create(b"/mnt-store/.tmp-w1/demo.bin", 0, 0);
        assert!(fd >= 0, "store probe: temp create failed: {fd}");
        assert_eq!(vfs::write(fd as u64, b"realized-bytes"), 14);
        assert_eq!(vfs::close(fd as u64), 0);
        assert_eq!(
            vfs::rename(b"/mnt-store/.tmp-w1", STORE_NAME), 0,
            "store probe: realize rename failed"
        );

        // Sealed: any mutation of the realized entry is EROFS.
        let r = vfs::create(b"/mnt-store/abcd1234-demo-1.0/inject", 0, 0);
        assert_eq!(r, vfs::EROFS, "store probe: create into sealed must be EROFS, got {r}");
        let r = vfs::mkdir(b"/mnt-store/abcd1234-demo-1.0/dir", 0, 0);
        assert_eq!(r, vfs::EROFS, "store probe: mkdir into sealed must be EROFS, got {r}");
        let r = vfs::unlink(b"/mnt-store/abcd1234-demo-1.0/demo.bin");
        assert_eq!(r, vfs::EROFS, "store probe: unlink in sealed must be EROFS, got {r}");
        let r = vfs::rename(STORE_NAME, b"/mnt-store/elsewhere");
        assert_eq!(r, vfs::EROFS, "store probe: moving sealed must be EROFS, got {r}");

        // Reads still work; content is the realized bytes.
        let fd = vfs::open(b"/mnt-store/abcd1234-demo-1.0/demo.bin");
        assert!(fd >= 0, "store probe: sealed open failed: {fd}");
        let mut buf = [0u8; 32];
        let n = vfs::read(fd as u64, &mut buf);
        assert_eq!(&buf[..n as usize], b"realized-bytes");
        assert_eq!(vfs::close(fd as u64), 0);

        // Re-realize (second writer, same digest): distinct temp, rename onto
        // the sealed name is a no-op success; the loser's temp survives for
        // the caller to clean, and the winner's content is untouched.
        assert_eq!(vfs::mkdir(b"/mnt-store/.tmp-w2", 0, 0), 0);
        let fd = vfs::create(b"/mnt-store/.tmp-w2/demo.bin", 0, 0);
        assert!(fd >= 0);
        assert_eq!(vfs::write(fd as u64, b"LOSER CONTENT!"), 14);
        // Keep this temp fd open across the no-op to test the stale-fd guard
        // below? No — the seal happened at w1's rename; this fd is on an
        // UNSEALED temp and stays writable. Close it normally.
        assert_eq!(vfs::close(fd as u64), 0);
        assert_eq!(
            vfs::rename(b"/mnt-store/.tmp-w2", STORE_NAME), 0,
            "store probe: re-realize must be a no-op success"
        );
        let fd = vfs::open(b"/mnt-store/abcd1234-demo-1.0/demo.bin");
        let n = vfs::read(fd as u64, &mut buf);
        assert_eq!(
            &buf[..n as usize], b"realized-bytes",
            "store probe: re-realize must not replace the winner's content"
        );
        assert_eq!(vfs::close(fd as u64), 0);
        // Loser cleans its redundant temp (unsealed — still mutable).
        assert_eq!(vfs::unlink(b"/mnt-store/.tmp-w2/demo.bin"), 0);

        // Stale-fd seal: a writable fd staged into an entry sealed afterwards
        // must be refused at write time.
        assert_eq!(vfs::mkdir(b"/mnt-store/.tmp-w3", 0, 0), 0);
        let stale = vfs::create(b"/mnt-store/.tmp-w3/f", 0, 0);
        assert!(stale >= 0);
        assert_eq!(vfs::write(stale as u64, b"pre-seal"), 8);
        assert_eq!(
            vfs::rename(b"/mnt-store/.tmp-w3", b"/mnt-store/ffff0000-late-2.0"), 0
        );
        let r = vfs::write(stale as u64, b"post-seal");
        assert_eq!(r, vfs::EROFS, "store probe: stale fd into sealed must be EROFS, got {r}");
        assert_eq!(vfs::close(stale as u64), 0);

        // Root volume untouched by all of it.
        assert_eq!(
            vfs::generation().expect("root mounted"), root_gen,
            "store probe: store activity leaked onto the root volume"
        );

        kprintln!("{TAG}[mount]{RST} stage-2 probe {OK}passed{RST} — store mount, realize seal, EROFS, re-realize no-op");

        // ── Exclusive-create probe (docs/spec/syscalls.md SYS_CREATE) ─────
        // SYS_CREATE is the atomic create-if-absent primitive: existence
        // check + creation happen inside one uninterrupted syscall, so of N
        // racing creators exactly one wins. The kernel is single-threaded —
        // the tightest race two creators can produce is back-to-back calls,
        // modeled here with the winner's fd still open. Runs on the /mnt RAM
        // volume so the root disk image is untouched.
        {
            let winner = vfs::create(b"/mnt/excl.lock", 0, 0);
            assert!(winner >= 0, "excl probe: first create failed: {winner}");
            let loser = vfs::create(b"/mnt/excl.lock", 0, 0);
            assert_eq!(loser, vfs::EEXIST, "excl probe: racing create must be EEXIST, got {loser}");
            assert_eq!(vfs::close(winner as u64), 0);
            // Still EEXIST after the winner's fd closes — the path's
            // existence excludes, not the open fd.
            let again = vfs::create(b"/mnt/excl.lock", 0, 0);
            assert_eq!(again, vfs::EEXIST, "excl probe: create on existing must be EEXIST, got {again}");
            // Unlink releases the path for the next creator (lock release).
            assert_eq!(vfs::unlink(b"/mnt/excl.lock"), 0);
            let retake = vfs::create(b"/mnt/excl.lock", 0, 0);
            assert!(retake >= 0, "excl probe: create after unlink failed: {retake}");
            assert_eq!(vfs::close(retake as u64), 0);
            assert_eq!(vfs::unlink(b"/mnt/excl.lock"), 0);

            // Errno canonicalization (followup item 10): open() on a
            // directory reports the ABI's EISDIR (-15), not the retired
            // V1 scheme's -7.
            let r = vfs::open(b"/mnt");
            assert_eq!(r, vfs::EISDIR, "excl probe: open(dir) must be EISDIR(-15), got {r}");

            kprintln!("{TAG}[vfs]{RST} exclusive-create probe {OK}passed{RST} — one winner, loser EEXIST, unlink releases, open(dir)=EISDIR(-15)");
        }

        // ── Symlink probe (SYS_SYMLINK/SYS_READLINK, docs/spec/syscalls.md) ──
        // On the /mnt RAM volume: create target file, absolute + relative
        // links, readlink round-trip, follow-through-open, EEXIST on an
        // occupied name, unlink removes the link not the target; on the
        // /mnt-store guarded mount: a link into a sealed entry is EROFS.
        {
            let fd = vfs::create(b"/mnt/ln-target", 0, 0);
            assert!(fd >= 0, "symlink probe: target create failed: {fd}");
            assert_eq!(vfs::write(fd as u64, b"link-bytes"), 10);
            assert_eq!(vfs::close(fd as u64), 0);

            // Absolute-target link + readlink round-trip.
            assert_eq!(vfs::symlink(b"/mnt/ln-target", b"/mnt/ln-abs"), 0);
            let mut buf = [0u8; 64];
            let n = vfs::readlink(b"/mnt/ln-abs", &mut buf);
            assert_eq!(&buf[..n as usize], b"/mnt/ln-target", "readlink target mismatch");

            // Follow: open() resolves the link to the target's bytes.
            let fd = vfs::open(b"/mnt/ln-abs");
            assert!(fd >= 0, "symlink probe: open through link failed: {fd}");
            let n = vfs::read(fd as u64, &mut buf);
            assert_eq!(&buf[..n as usize], b"link-bytes");
            assert_eq!(vfs::close(fd as u64), 0);

            // Relative target follows within the link's directory.
            assert_eq!(vfs::symlink(b"ln-target", b"/mnt/ln-rel"), 0);
            let fd = vfs::open(b"/mnt/ln-rel");
            assert!(fd >= 0, "symlink probe: relative link follow failed: {fd}");
            assert_eq!(vfs::close(fd as u64), 0);

            // Occupied name is EEXIST; readlink on a non-link is EINVAL.
            let r = vfs::symlink(b"/x", b"/mnt/ln-abs");
            assert_eq!(r, vfs::EEXIST, "symlink onto existing must be EEXIST, got {r}");
            let r = vfs::readlink(b"/mnt/ln-target", &mut buf);
            assert_eq!(r, vfs::EINVAL, "readlink(non-link) must be EINVAL, got {r}");

            // Unlink removes the link; the target survives.
            assert_eq!(vfs::unlink(b"/mnt/ln-abs"), 0);
            assert_eq!(vfs::unlink(b"/mnt/ln-rel"), 0);
            let fd = vfs::open(b"/mnt/ln-target");
            assert!(fd >= 0, "symlink probe: target must survive link unlink");
            assert_eq!(vfs::close(fd as u64), 0);

            // Store mount: link into a sealed entry is EROFS; a link at an
            // unsealed top-level name is allowed (roots/current live free).
            let r = vfs::symlink(b"/anywhere", b"/mnt-store/abcd1234-demo-1.0/ln");
            assert_eq!(r, vfs::EROFS, "symlink into sealed must be EROFS, got {r}");
            assert_eq!(vfs::symlink(b"abcd1234-demo-1.0", b"/mnt-store/ln-free"), 0);
            let n = vfs::readlink(b"/mnt-store/ln-free", &mut buf);
            assert_eq!(&buf[..n as usize], b"abcd1234-demo-1.0");

            kprintln!("{TAG}[vfs]{RST} symlink probe {OK}passed{RST} — create/readlink/follow, EEXIST, EROFS in sealed, unlink keeps target");
        }
    } else {
        kprintln!("{VRB}[mount] probe skipped — no root volume{RST}");
    }

    // ── Ring-3 argv probe (docs/spec/syscalls.md SYS_EXEC) ────────────────────
    // exec with argv=["probe","argv-ok!"]; ARGV_ECHO_ELF reads the initial
    // stack frame from ring 3, byte-compares both strings through the argv
    // pointers, SYS_LOGs the readback ("[argv-echo] …" on serial), and exits
    // only on an exact match — any mismatch spins and fails the reap deadline.
    {
        let argv_id = elf::exec(elf::ARGV_ECHO_ELF, &[], &["probe", "argv-ok!"])
            .expect("argv probe: exec failed");
        let deadline = apic::ticks() + 500;
        while apic::ticks() < deadline && task::task_exists(argv_id) {
            task::yield_task();
        }
        assert!(
            !task::task_exists(argv_id),
            "argv probe: ring-3 task did not read back its argv"
        );
        kprintln!("{TAG}[argv]{RST} ring-3 probe {OK}passed{RST} — argc/argv read back from the initial stack frame");
    }
    } // end #[cfg(feature = "boot-tests")]

    // ── Locate lythd ELF ────────────────────────────────────────────────────
    //
    // lythd (PID 1) lives at /lth/system/init (symlink → /lth/bin/lythd).
    // Populate via `make oros`.
    let lythd_elf = vfs::load_file("/lth/system/init")
        .expect("lythd: /lth/system/init not found — run `make oros` to populate rootfs/lth/bin/");

    // ── lythd bootstrap ───────────────────────────────────────────────────────
    //
    // Build the initial capability set and exec lythd with it.
    //
    // Cap layout handed to lythd (in exec order):
    //   handle 0 — root memory capability (all physical frames)
    //   handle 1 — rollback capability    (privileged SYS_ROLLBACK gate)
    //   handle 2 — boot-info IPC endpoint (one pre-queued BootInfo message)
    //   handle 3 — filesystem capability  (privileged SYS_MOUNT gate)

    // Root memory capability: covers the full physical address space.
    let mem_obj = cap::create_object(cap::KernelObject::Memory {
        base_pa: 0,
        frame_count: pmm::free_frame_count() as u64,
    })
    .expect("lythd bootstrap: mem cap OOM");

    // Rollback capability: exclusive privilege for lythd.
    let rollback_obj =
        cap::create_object(cap::KernelObject::Rollback).expect("lythd bootstrap: rollback cap OOM");

    // Boot-info IPC endpoint: pre-load one BootInfo message before exec.
    let boot_ep_idx = ipc::create_endpoint().expect("create_endpoint");
    let boot_info_bytes = build_boot_info();
    ipc::send(boot_ep_idx, &boot_info_bytes);

    let boot_ipc_obj = cap::create_object(cap::KernelObject::Ipc {
        endpoint_idx: boot_ep_idx,
    })
    .expect("lythd bootstrap: boot-info cap OOM");

    // Filesystem (mount authority) capability: exclusive privilege for lythd.
    let fs_obj =
        cap::create_object(cap::KernelObject::Filesystem).expect("lythd bootstrap: fs cap OOM");

    // Insert all four caps into kernel_main's table so spawn_userspace_task can inherit them.
    let mut kmain_caps = cap::CapabilityTable::new();
    let mem_cap = cap::create_root_cap(
        &mut kmain_caps,
        cap::CapKind::Memory,
        cap::CapRights::ALL,
        mem_obj,
    );
    let rollback_cap = cap::create_root_cap(
        &mut kmain_caps,
        cap::CapKind::Rollback,
        cap::CapRights::ALL,
        rollback_obj,
    );
    let boot_cap = cap::create_root_cap(
        &mut kmain_caps,
        cap::CapKind::Ipc,
        cap::CapRights::ALL,
        boot_ipc_obj,
    );
    let fs_cap = cap::create_root_cap(
        &mut kmain_caps,
        cap::CapKind::Filesystem,
        cap::CapRights::ALL,
        fs_obj,
    );

    // Temporarily give kernel_main's bootstrap task this cap table so
    // spawn_userspace_task can read from it during cap inheritance.
    task::set_bootstrap_cap_table(kmain_caps);

    let pf_before = pmm::free_frame_count();
    elf::exec(lythd_elf.as_slice(), &[mem_cap, rollback_cap, boot_cap, fs_cap], &[])
        .expect("lythd bootstrap: exec failed");
    kprintln!(
        "{TAG}[exec-cost]{RST} lythd: {} frames",
        pf_before - pmm::free_frame_count()
    );

    kprintln!("{TAG}[boot]{RST} lythd launched {VRB}— entering scheduler{RST}");
    // Periodic idle-RAM report: remaining PMM frames plus heap free-list
    // totals every ~2000 ticks.  The last report in a serial log is the
    // settled idle state.  Serial only (kdiagln!) — on the framebuffer
    // console these lines would scroll the screen forever on an idle system.
    let mut next_report = 2_000u64;
    loop {
        if apic::ticks() >= next_report {
            next_report = apic::ticks() + 2_000;
            let free = pmm::free_frame_count();
            kdiagln!(
                "{TAG}[ram-idle]{RST} ticks={} pmm_free_frames={} ({} MiB free)",
                apic::ticks(), free, free * 4 / 1024
            );
            heap::print_stats("idle");
            // TEMP DEBUG: serial RX health — bytes delivered to userspace and
            // whether a byte is currently waiting unread in the UART.
            // Evaluate BEFORE the print: format args run under the log lock,
            // which already holds SERIAL — locking it in-args deadlocks.
            let rx  = serial::RX_DELIVERED.load(core::sync::atomic::Ordering::Relaxed);
            let dr  = serial::SERIAL.lock().data_ready();
            let kst = keyboard::status_raw();
            let kbn = keyboard::buffered();
            let kiq = keyboard::irq_seen();
            kdiagln!(
                "{TAG}[serial-diag]{RST} rx_delivered={} lsr_dr={} i8042_status={:#04x} kbd_buf={} kbd_irq_seen={}",
                rx, dr, kst, kbn, kiq
            );
            // TEMP DEBUG: live task states (id:state, 1=Running 2=Ready 3=Blocked).
            let mut states = [0u8; 128];
            let mut si = 0usize;
            task::for_each_task(|_idx, id, state_raw, _kind| {
                if si + 12 >= states.len() { return; }
                let mut idv = id;
                if idv == 0 { states[si] = b'0'; si += 1; }
                else {
                    let mut tmp = [0u8; 8];
                    let mut n = 0;
                    while idv > 0 && n < 8 { tmp[n] = b'0' + (idv % 10) as u8; idv /= 10; n += 1; }
                    while n > 0 { n -= 1; states[si] = tmp[n]; si += 1; }
                }
                states[si] = b':'; si += 1;
                states[si] = b'0' + (state_raw as u8).min(9); si += 1;
                states[si] = b' '; si += 1;
            });
            if let Ok(s) = core::str::from_utf8(&states[..si]) {
                kdiagln!("{TAG}[task-diag]{RST} {}", s);
            }
        }
        unsafe { core::arch::asm!("hlt") };
    }
}

/// Probe task for the sweep_dead resource-release check: exits immediately.
#[cfg(feature = "boot-tests")]
fn sweep_probe_task() -> ! {
    task::task_exit(task::exit_status_normal(0))
}

/// Core integration checklist.  Runs before lythd is spawned so all checks
/// execute without concurrent userspace tasks.
#[cfg(feature = "boot-tests")]
fn core_smoke() {
    // ── Check 6: ENOCAP ──────────────────────────────────────────────────────
    // Call syscall_dispatch directly with a handle index that's way out of
    // range in the bootstrap task's cap table.
    {
        let mut frame: syscall::SyscallFrame = unsafe { core::mem::zeroed() };
        frame.nr = syscall::SYS_CAP_GRANT;
        frame.a1 = 0xDEAD_BEEF_0000_0000u64; // bogus CapHandle
        frame.a2 = 99u64; // target task id
        frame.a3 = 0xFFu64; // rights mask
        let result = syscall::syscall_dispatch(&mut frame);
        assert_eq!(result, syscall::ENOCAP, "step14: ENOCAP check failed");
    }
    kprintln!("{TAG}[integration]{RST} ENOCAP check {OK}passed{RST}");

    // ── Device-driver framework: no-Device-cap → ENOPERM (gate-before-args) ────
    // The bootstrap task holds no CapKind::Device capability, so every device
    // syscall must reject it up front — a process with no Device cap can map no
    // MMIO, wait on no IRQ, and allocate no DMA. Bogus argument values prove the
    // cap is checked BEFORE the arguments (gate-before-args, like SYS_MOUNT).
    {
        for &(nr, label) in &[
            (syscall::SYS_DEV_MMIO_MAP,  "MMIO_MAP"),
            (syscall::SYS_DEV_DMA_ALLOC, "DMA_ALLOC"),
            (syscall::SYS_DEV_IRQ_WAIT,  "IRQ_WAIT"),
            (syscall::SYS_DEV_CFG_READ,  "CFG_READ"),
        ] {
            let mut frame: syscall::SyscallFrame = unsafe { core::mem::zeroed() };
            frame.nr = nr;
            frame.a1 = 0xDEAD_BEEF_0000_0000u64; // bogus (non-Device) cap handle
            frame.a2 = 0xFFFF_FFFFu64;           // bogus arg — must not be reached
            frame.a3 = 0xFFFF_FFFFu64;
            let result = syscall::syscall_dispatch(&mut frame);
            assert_eq!(result, syscall::ENOPERM,
                "device gate: SYS_DEV_{} without Device cap must return ENOPERM", label);
        }
        // SYS_DEV_CLAIM is Rollback-gated: the bootstrap task holds no Rollback
        // cap here, so it too must be denied.
        let mut frame: syscall::SyscallFrame = unsafe { core::mem::zeroed() };
        frame.nr = syscall::SYS_DEV_CLAIM;
        frame.a1 = 0x1000u64; // any user ptr
        frame.a2 = 4u64;
        let result = syscall::syscall_dispatch(&mut frame);
        assert_eq!(result, syscall::ENOPERM,
            "device gate: SYS_DEV_CLAIM without Rollback cap must return ENOPERM");
    }
    kprintln!("{TAG}[integration]{RST} device-cap gate (ENOPERM) check {OK}passed{RST}");

    // ── Check 4 + 5: IPC between two userspace tasks ──────────────────────────
    // Create a fresh IPC endpoint and add a cap to the bootstrap task's table
    // so both exec'd tasks can inherit it.
    {
        let ep_idx = ipc::create_endpoint().expect("create_endpoint");
        let ep_obj = cap::create_object(cap::KernelObject::Ipc {
            endpoint_idx: ep_idx,
        })
        .expect("step14: IPC ep cap OOM");
        let ipc_cap = {
            // task 0 is the bootstrap (kernel_main) task; access its cap table directly.
            let tbl = unsafe { &mut *task::cap_table_ptr(0) };
            cap::create_root_cap(tbl, cap::CapKind::Ipc, cap::CapRights::ALL, ep_obj)
        };

        // Spawn receiver first — it will block on the empty ring.
        // Spawn sender second — it sends one message and exits.
        // Both inherit ipc_cap as handle 0 in their respective tables.
        let recv_id = elf::exec(elf::IPC_RECEIVER_ELF, &[ipc_cap], &[])
            .expect("step14: receiver exec failed");
        let send_id =
            elf::exec(elf::IPC_SENDER_ELF, &[ipc_cap], &[]).expect("step14: sender exec failed");

        // Yield until both tasks are reaped (or 500 ms timeout).
        let deadline = apic::ticks() + 500;
        while apic::ticks() < deadline && (task::task_exists(recv_id) || task::task_exists(send_id))
        {
            task::yield_task();
        }

        assert!(
            !task::task_exists(recv_id),
            "step14: IPC receiver task did not complete"
        );
        assert!(
            !task::task_exists(send_id),
            "step14: IPC sender task did not complete"
        );
    }
    kprintln!("{TAG}[integration]{RST} IPC userspace send/recv {OK}passed{RST}");
    kprintln!("{TAG}[integration]{RST} task_exit + scheduler reap {OK}verified{RST}");

    // ── Cap syscall: SYS_CAP_GRANT error paths + SYS_CAP_REVOKE ─────────────
    {
        // Create a fresh Memory cap in task 0's table for this test.
        let test_obj = cap::create_object(cap::KernelObject::Memory {
            base_pa: 0xA000,
            frame_count: 1,
        })
        .expect("cap syscall test: create object");
        let test_handle = {
            let tbl = unsafe { &mut *task::cap_table_ptr(0) };
            cap::create_root_cap(tbl, cap::CapKind::Memory, cap::CapRights::ALL, test_obj)
        };

        // Self-grant (target = current task) must return EINVAL.
        {
            let mut frame: syscall::SyscallFrame = unsafe { core::mem::zeroed() };
            frame.nr = syscall::SYS_CAP_GRANT;
            frame.a1 = test_handle.0;
            frame.a2 = 0; // task 0 = self
            frame.a3 = cap::CapRights::READ.0 as u64;
            assert_eq!(
                syscall::syscall_dispatch(&mut frame),
                syscall::EINVAL,
                "cap syscall: self-grant should be EINVAL",
            );
        }

        // Grant to a nonexistent task must return EINVAL.
        {
            let mut frame: syscall::SyscallFrame = unsafe { core::mem::zeroed() };
            frame.nr = syscall::SYS_CAP_GRANT;
            frame.a1 = test_handle.0;
            frame.a2 = 9999; // no such task
            frame.a3 = cap::CapRights::READ.0 as u64;
            assert_eq!(
                syscall::syscall_dispatch(&mut frame),
                syscall::EINVAL,
                "cap syscall: grant to nonexistent task should be EINVAL",
            );
        }

        // SYS_CAP_REVOKE on our own cap must succeed.
        {
            let mut frame: syscall::SyscallFrame = unsafe { core::mem::zeroed() };
            frame.nr = syscall::SYS_CAP_REVOKE;
            frame.a1 = test_handle.0;
            assert_eq!(
                syscall::syscall_dispatch(&mut frame),
                0,
                "cap syscall: revoke should return 0",
            );
        }

        // The cap must be gone after revoke.
        let tbl = unsafe { &*task::cap_table_ptr(0) };
        assert!(
            tbl.get(test_handle).is_err(),
            "cap syscall: cap should be gone after revoke"
        );
    }
    kprintln!("{TAG}[integration]{RST} cap grant/revoke syscall {OK}passed{RST}");

    // ── SYS_MMAP / SYS_MUNMAP full lifecycle ─────────────────────────────────
    // Exec MMAP_TEST_ELF — it maps a fresh frame at VA 0x5_0000_0000, writes
    // a sentinel, unmaps (freeing the frame), then exits.  The task has its
    // own page table; sweep_dead frees any remaining pages on exit.
    {
        // Create a fresh Memory cap for this test so core_smoke doesn't depend
        // on task 0's cap table being pre-populated by the lythd bootstrap.
        let mmap_mem_obj = cap::create_object(cap::KernelObject::Memory {
            base_pa: 0,
            frame_count: pmm::free_frame_count() as u64,
        })
        .expect("MMAP test: create mem obj");
        let mmap_mem_cap = {
            let tbl = unsafe { &mut *task::cap_table_ptr(0) };
            cap::create_root_cap(tbl, cap::CapKind::Memory, cap::CapRights::ALL, mmap_mem_obj)
        };
        let mmap_task =
            elf::exec(elf::MMAP_TEST_ELF, &[mmap_mem_cap], &[]).expect("MMAP test: exec failed");
        let deadline = apic::ticks() + 500;
        while apic::ticks() < deadline && task::task_exists(mmap_task) {
            task::yield_task();
        }
        assert!(
            !task::task_exists(mmap_task),
            "MMAP test: task did not complete"
        );
    }
    kprintln!("{TAG}[integration]{RST} SYS_MMAP/SYS_MUNMAP lifecycle {OK}passed{RST}");

    // ── SYS_IPC_SEND_CAP / SYS_IPC_RECV_CAP end-to-end ──────────────────────
    // Two kernel tasks share an endpoint.  The sender moves a Memory
    // capability through the ring buffer; the receiver verifies its kind.
    {
        use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering as O};

        static SCAP_EP: AtomicUsize = AtomicUsize::new(usize::MAX);
        static SCAP_DONE: AtomicU8 = AtomicU8::new(0);

        fn scap_sender() -> ! {
            let ep = SCAP_EP.load(core::sync::atomic::Ordering::Relaxed);
            // Build a read-only Memory cap to transfer.
            let ko = cap::create_object(cap::KernelObject::Memory {
                base_pa: 0x9000,
                frame_count: 1,
            })
            .expect("scap sender: create object");
            let mut tmp = cap::CapabilityTable::new();
            let h = cap::create_root_cap(&mut tmp, cap::CapKind::Memory, cap::CapRights::READ, ko);
            let the_cap = tmp.take(h).expect("scap sender: take cap");

            let msg = [0xCCu8; ipc::MSG_SIZE];
            ipc::send_cap(ep, &msg, the_cap);
            task::task_exit(task::exit_status_normal(0));
        }

        fn scap_receiver() -> ! {
            let ep = SCAP_EP.load(core::sync::atomic::Ordering::Relaxed);
            let mut buf = [0u8; ipc::MSG_SIZE];
            let (n, maybe_cap) = ipc::recv_cap(ep, &mut buf);
            assert_eq!(n, ipc::MSG_SIZE, "scap recv: wrong byte count");
            assert_eq!(buf[0], 0xCC, "scap recv: wrong payload");
            let received = maybe_cap.expect("scap recv: expected a capability");
            assert_eq!(received.kind, cap::CapKind::Memory, "scap recv: wrong kind");
            assert_eq!(
                received.rights,
                cap::CapRights::READ,
                "scap recv: wrong rights"
            );
            SCAP_DONE.store(1, core::sync::atomic::Ordering::Relaxed);
            task::task_exit(task::exit_status_normal(0));
        }

        let ep_idx = ipc::create_endpoint().expect("create_endpoint");
        SCAP_EP.store(ep_idx, O::Relaxed);

        task::spawn_kernel_task(scap_receiver); // blocks immediately (ring empty)
        task::spawn_kernel_task(scap_sender);

        while SCAP_DONE.load(O::Relaxed) == 0 {
            task::yield_task();
        }
    }
    kprintln!("{TAG}[integration]{RST} IPC_SEND_CAP/IPC_RECV_CAP {OK}passed{RST}");

    // ── Triangular IPC: blocked task woken by a third task ────────────────────
    // Task A blocks on ep1, task B blocks on ep2, task C sends to both.
    // Verifies that a task blocked in recv is correctly woken by an unrelated
    // third task (not the task that created the endpoint).
    {
        use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering as O};

        static TRI_EP1: AtomicUsize = AtomicUsize::new(usize::MAX);
        static TRI_EP2: AtomicUsize = AtomicUsize::new(usize::MAX);
        static TRI_DONE: AtomicU8 = AtomicU8::new(0); // bit 0 = A done, bit 1 = B done

        fn tri_task_a() -> ! {
            let ep = TRI_EP1.load(core::sync::atomic::Ordering::Relaxed);
            let mut buf = [0u8; ipc::MSG_SIZE];
            ipc::recv(ep, &mut buf);
            assert_eq!(buf[0], 0xAA, "triangular IPC: task A got wrong payload");
            TRI_DONE.fetch_or(1, core::sync::atomic::Ordering::Relaxed);
            task::task_exit(task::exit_status_normal(0));
        }

        fn tri_task_b() -> ! {
            let ep = TRI_EP2.load(core::sync::atomic::Ordering::Relaxed);
            let mut buf = [0u8; ipc::MSG_SIZE];
            ipc::recv(ep, &mut buf);
            assert_eq!(buf[0], 0xBB, "triangular IPC: task B got wrong payload");
            TRI_DONE.fetch_or(2, core::sync::atomic::Ordering::Relaxed);
            task::task_exit(task::exit_status_normal(0));
        }

        fn tri_task_c() -> ! {
            let ep1 = TRI_EP1.load(core::sync::atomic::Ordering::Relaxed);
            let ep2 = TRI_EP2.load(core::sync::atomic::Ordering::Relaxed);
            let mut m1 = [0u8; ipc::MSG_SIZE];
            m1[0] = 0xAA;
            let mut m2 = [0u8; ipc::MSG_SIZE];
            m2[0] = 0xBB;
            ipc::send(ep1, &m1); // wakes task A
            ipc::send(ep2, &m2); // wakes task B
            task::task_exit(task::exit_status_normal(0));
        }

        let ep1 = ipc::create_endpoint().expect("create_endpoint");
        let ep2 = ipc::create_endpoint().expect("create_endpoint");
        TRI_EP1.store(ep1, O::Relaxed);
        TRI_EP2.store(ep2, O::Relaxed);

        task::spawn_kernel_task(tri_task_a); // blocks on recv(ep1)
        task::spawn_kernel_task(tri_task_b); // blocks on recv(ep2)
        task::spawn_kernel_task(tri_task_c); // sends to both, waking A and B

        while TRI_DONE.load(O::Relaxed) != 3 {
            task::yield_task();
        }
    }
    kprintln!("{TAG}[integration]{RST} triangular IPC {OK}passed{RST}");

    // ── SYS_EXEC invoked from a userspace task ────────────────────────────────
    // EXEC_FROM_USER_ELF calls SYS_EXEC with an embedded SMOKE_ELF copy,
    // exercises user-pointer validation and the full exec syscall path.
    {
        let outer_task = elf::exec(elf::EXEC_FROM_USER_ELF, &[], &[])
            .expect("SYS_EXEC from userspace: exec failed");
        let deadline = apic::ticks() + 500;
        while apic::ticks() < deadline && task::task_exists(outer_task) {
            task::yield_task();
        }
        assert!(
            !task::task_exists(outer_task),
            "SYS_EXEC from userspace: outer task did not complete",
        );
    }
    kprintln!("{TAG}[integration]{RST} SYS_EXEC from userspace {OK}passed{RST}");

    // ── Syscall fuzz: boundary / invalid inputs must return error codes ───────
    // All cases are called directly via `syscall_dispatch` (no ring-3 needed).
    // A panic here means the kernel did not reject a bad input gracefully.
    {
        let mut f: syscall::SyscallFrame;
        for nr in [syscall::SYSCALL_MAX + 1, syscall::SYSCALL_MAX + 100, u64::MAX] {
            f = unsafe { core::mem::zeroed() };
            f.nr = nr;
            assert_eq!(
                syscall::syscall_dispatch(&mut f),
                syscall::ENOSYS,
                "fuzz: nr={nr} expected ENOSYS",
            );
        }

        // SYS_MMAP: unaligned VA → EINVAL
        f = unsafe { core::mem::zeroed() };
        f.nr = syscall::SYS_MMAP;
        f.a1 = 0x0000_0007_0000_0001; // not page-aligned
        assert_eq!(
            syscall::syscall_dispatch(&mut f),
            syscall::EINVAL,
            "fuzz: SYS_MMAP unaligned VA"
        );

        // SYS_MMAP: VA in 0→1 GiB identity-mapped huge-page range → EINVAL
        f = unsafe { core::mem::zeroed() };
        f.nr = syscall::SYS_MMAP;
        f.a1 = 0x0000_0000_0010_0000; // kernel load address — inside huge-page range
        assert_eq!(
            syscall::syscall_dispatch(&mut f),
            syscall::EINVAL,
            "fuzz: SYS_MMAP VA in 0→1GiB range"
        );
        f.a1 = 0x0000_0000_3FFF_F000; // top of identity range, aligned
        assert_eq!(
            syscall::syscall_dispatch(&mut f),
            syscall::EINVAL,
            "fuzz: SYS_MMAP VA at top of identity range"
        );

        // SYS_MMAP: kernel-space VA → EINVAL
        f = unsafe { core::mem::zeroed() };
        f.nr = syscall::SYS_MMAP;
        f.a1 = 0xFFFF_C000_0000_0000; // kernel heap VA
        assert_eq!(
            syscall::syscall_dispatch(&mut f),
            syscall::EINVAL,
            "fuzz: SYS_MMAP kernel-space VA"
        );

        // SYS_MUNMAP: unaligned VA → EINVAL
        f = unsafe { core::mem::zeroed() };
        f.nr = syscall::SYS_MUNMAP;
        f.a1 = 0x0000_0007_0000_0FFF;
        assert_eq!(
            syscall::syscall_dispatch(&mut f),
            syscall::EINVAL,
            "fuzz: SYS_MUNMAP unaligned VA"
        );

        // SYS_MUNMAP: VA in 0→1 GiB range → EINVAL
        f = unsafe { core::mem::zeroed() };
        f.nr = syscall::SYS_MUNMAP;
        f.a1 = 0x0000_0000_1000_0000;
        assert_eq!(
            syscall::syscall_dispatch(&mut f),
            syscall::EINVAL,
            "fuzz: SYS_MUNMAP VA in 0→1GiB range"
        );

        // SYS_MUNMAP: kernel-space VA → EINVAL
        f.a1 = 0xFFFF_D000_0000_0000; // IPC kernel window
        assert_eq!(
            syscall::syscall_dispatch(&mut f),
            syscall::EINVAL,
            "fuzz: SYS_MUNMAP kernel-space VA"
        );

        // SYS_MUNMAP: page-aligned user VA but not in vma_list → EINVAL
        f.a1 = 0x0000_0007_0000_0000;
        assert_eq!(
            syscall::syscall_dispatch(&mut f),
            syscall::EINVAL,
            "fuzz: SYS_MUNMAP unmapped VA"
        );

        // Null msg_ptr for all buffer syscalls → EINVAL
        for nr in [
            syscall::SYS_IPC_SEND,
            syscall::SYS_IPC_RECV,
            syscall::SYS_IPC_SEND_CAP,
            syscall::SYS_IPC_RECV_CAP,
        ] {
            f = unsafe { core::mem::zeroed() };
            f.nr = nr;
            f.a2 = 0; // null ptr
            f.a3 = 64; // non-zero len
            assert_eq!(
                syscall::syscall_dispatch(&mut f),
                syscall::EINVAL,
                "fuzz: nr={nr} null buf_ptr"
            );
        }

        // SYS_IPC_RECV_CAP: kernel-space out_handle_ptr → EINVAL
        f = unsafe { core::mem::zeroed() };
        f.nr = syscall::SYS_IPC_RECV_CAP;
        f.a2 = 0x0000_0007_0000_0000; // valid user buf
        f.a3 = 64;
        f.a4 = 0xFFFF_8000_0000_0000; // kernel-space handle ptr
        assert_eq!(
            syscall::syscall_dispatch(&mut f),
            syscall::EINVAL,
            "fuzz: SYS_IPC_RECV_CAP kernel-space out_handle_ptr"
        );

        // SYS_EXEC: null elf_ptr → EINVAL
        f = unsafe { core::mem::zeroed() };
        f.nr = syscall::SYS_EXEC;
        f.a1 = 0;
        f.a2 = 128;
        assert_eq!(
            syscall::syscall_dispatch(&mut f),
            syscall::EINVAL,
            "fuzz: SYS_EXEC null elf_ptr"
        );

        // SYS_EXEC: elf_len overflow (checked_add wraps) → EINVAL
        f.a1 = 1;
        f.a2 = u64::MAX;
        assert_eq!(
            syscall::syscall_dispatch(&mut f),
            syscall::EINVAL,
            "fuzz: SYS_EXEC overflow elf_len"
        );

        // SYS_LOG: null ptr → EINVAL
        f = unsafe { core::mem::zeroed() };
        f.nr = syscall::SYS_LOG;
        f.a1 = 0;
        f.a2 = 10;
        assert_eq!(
            syscall::syscall_dispatch(&mut f),
            syscall::EINVAL,
            "fuzz: SYS_LOG null ptr"
        );

        // SYS_LOG: kernel-space ptr → EINVAL
        f.a1 = 0xFFFF_8000_0000_0000;
        f.a2 = 4;
        assert_eq!(
            syscall::syscall_dispatch(&mut f),
            syscall::EINVAL,
            "fuzz: SYS_LOG kernel-space ptr"
        );

        // SYS_LOG: oversized length → EINVAL
        f.a1 = 0x0000_0007_0000_0000;
        f.a2 = 0x1001;
        assert_eq!(
            syscall::syscall_dispatch(&mut f),
            syscall::EINVAL,
            "fuzz: SYS_LOG oversized len"
        );

        // SYS_SERIAL_READ: null buf_ptr → EINVAL
        f = unsafe { core::mem::zeroed() };
        f.nr = syscall::SYS_SERIAL_READ;
        f.a1 = 0;     // null ptr
        f.a2 = 16;    // non-zero len
        assert_eq!(
            syscall::syscall_dispatch(&mut f),
            syscall::EINVAL,
            "fuzz: SYS_SERIAL_READ null buf_ptr"
        );

        // SYS_SERIAL_READ: kernel-space buf_ptr → EINVAL
        f.a1 = 0xFFFF_8000_0000_0000;
        f.a2 = 16;
        assert_eq!(
            syscall::syscall_dispatch(&mut f),
            syscall::EINVAL,
            "fuzz: SYS_SERIAL_READ kernel-space buf_ptr"
        );

        // Bogus cap handles → ENOCAP
        f = unsafe { core::mem::zeroed() };
        f.nr = syscall::SYS_CAP_GRANT;
        f.a1 = 0xDEAD_BEEF_DEAD_BEEF;
        f.a2 = 99;
        f.a3 = 0xFF;
        assert_eq!(
            syscall::syscall_dispatch(&mut f),
            syscall::ENOCAP,
            "fuzz: SYS_CAP_GRANT bogus handle"
        );

        f = unsafe { core::mem::zeroed() };
        f.nr = syscall::SYS_CAP_REVOKE;
        f.a1 = u64::MAX;
        assert_eq!(
            syscall::syscall_dispatch(&mut f),
            syscall::ENOCAP,
            "fuzz: SYS_CAP_REVOKE bogus handle"
        );

        // SYS_TIME: no args, always returns a millisecond count (never an error sentinel).
        f = unsafe { core::mem::zeroed() };
        f.nr = syscall::SYS_TIME;
        let t = syscall::syscall_dispatch(&mut f);
        assert!(
            t < syscall::EINVAL,
            "fuzz: SYS_TIME returned error sentinel {:#x}", t
        );

        // SYS_TASK_STATUS: nonexistent task ID → 0 (dead/missing), never an error.
        f = unsafe { core::mem::zeroed() };
        f.nr = syscall::SYS_TASK_STATUS;
        f.a1 = 0xDEAD_BEEF_DEAD_BEEF;
        assert_eq!(
            syscall::syscall_dispatch(&mut f),
            0,
            "fuzz: SYS_TASK_STATUS bogus task_id must return 0"
        );

        // SYS_TASK_STATUS: bootstrap task (id=0) is always alive → 1.
        f = unsafe { core::mem::zeroed() };
        f.nr = syscall::SYS_TASK_STATUS;
        f.a1 = 0; // kernel_main / bootstrap task
        assert_eq!(
            syscall::syscall_dispatch(&mut f),
            1,
            "fuzz: SYS_TASK_STATUS bootstrap task must be alive"
        );
    }
    kprintln!("{TAG}[integration]{RST} syscall fuzz {OK}passed{RST} {VRB}— all bad inputs rejected{RST}");

    // ── Exit-code round-trip: SYS_TASK_EXIT status → wait_for_task ────────────
    // The exit-code ABI (Part 2): a child's exit status is retained until a
    // waiter reaps it, and "exited cleanly with 0" is distinguishable from
    // "not found". Runs here (no concurrent userspace) so scheduling is
    // deterministic: the bootstrap task blocks in wait_for_task and the child
    // is the only other ready task.
    {
        fn exit_zero() -> ! { task::task_exit(task::exit_status_normal(0)) }
        fn exit_42()   -> ! { task::task_exit(task::exit_status_normal(42)) }

        // 1) Clean exit (code 0): wait must return 0 — not conflated with the
        //    old "not found ⇒ 0" overload.
        let child = task::spawn_kernel_task(exit_zero);
        assert_eq!(task::wait_for_task(child), 0,
            "exit-code: clean exit must wait→0");

        // 2) Nonzero exit: the exact code must round-trip.
        let child = task::spawn_kernel_task(exit_42);
        assert_eq!(task::wait_for_task(child), 42,
            "exit-code: code 42 must round-trip");

        // 3) Wait on an already-dead task: let the child exit and be reaped
        //    first, then wait — the retained record still answers with the
        //    code. A second wait on the same id finds no record (consumed) and
        //    returns ENOENT.
        let child = task::spawn_kernel_task(exit_42);
        while task::task_status_raw(child) != 0 { task::yield_task(); }
        assert_eq!(task::wait_for_task(child), 42,
            "exit-code: wait-on-already-dead returns the code");
        assert_eq!(task::wait_for_task(child), syscall::ENOENT,
            "exit-code: consumed record → ENOENT");

        // 4) Wait on a task that never existed → ENOENT (the disambiguation).
        assert_eq!(task::wait_for_task(0xDEAD_BEEF), syscall::ENOENT,
            "exit-code: nonexistent task → ENOENT");
    }
    kprintln!("{TAG}[integration]{RST} exit-code round-trip {OK}passed{RST}");

    // ── Exit-record retention: no eviction, spawner-death cascade ─────────────
    // Part 1: the exit-record table is a heap Vec with reap-required, spawner-
    // owned records — not a fixed round-robin ring. Two properties matter:
    //   (a) No eviction. Far more unreaped records than the old 64-entry ring
    //       held can coexist, and the *oldest* is still readable — a real exit
    //       status is never silently dropped to make room for a newer one (the
    //       regression that turned a builder's exit-5 into ENOENT).
    //   (b) Spawner-death cascade. When a task dies, the unreaped records of
    //       children IT spawned are freed, since no one is left to reap them.
    {
        fn exit_42() -> ! { task::task_exit(task::exit_status_normal(42)) }

        // (a) Accumulate N unreaped records (N well above the old ring size of
        //     64), letting each child exit and be reaped but NOT waiting on it.
        //     ids are contiguous: only these spawns advance next_id here.
        const N: task::TaskId = 80;
        let first = task::spawn_kernel_task(exit_42);
        while task::task_status_raw(first) != 0 { task::yield_task(); }
        for _ in 1..N {
            let c = task::spawn_kernel_task(exit_42);
            while task::task_status_raw(c) != 0 { task::yield_task(); }
        }
        // Every record from `first` onward is still present (the old ring would
        // have evicted the earliest N-64). Waiting drains them.
        for id in first..first + N {
            assert_eq!(task::wait_for_task(id), 42,
                "retention: unreaped record must survive with no eviction");
        }

        // (b) Spawner-death cascade. A parent task spawns a child that exits,
        //     never waits on it, then exits itself. The child's record (owned
        //     by the now-dead parent) is cascade-freed; a later wait sees it as
        //     gone. The parent's own record (owned by the bootstrap task) is
        //     unaffected and still readable.
        fn leaky_spawner() -> ! {
            // Spawn a child that exits, then yield enough for it to run and be
            // recorded before we exit (so the cascade has something to free).
            let _child = task::spawn_kernel_task(exit_42);
            for _ in 0..16 { task::yield_task(); }
            task::task_exit(task::exit_status_normal(7))
        }
        let parent = task::spawn_kernel_task(leaky_spawner);
        let child  = parent + 1; // the only next spawn is leaky_spawner's child
        while task::task_status_raw(parent) != 0 { task::yield_task(); }
        assert_eq!(task::wait_for_task(child), syscall::ENOENT,
            "retention: spawner death cascade-frees unreaped child record");
        assert_eq!(task::wait_for_task(parent), 7,
            "retention: spawner's own record (owned by bootstrap) survives");
    }
    kprintln!("{TAG}[integration]{RST} exit-record retention {OK}passed{RST}");
}

// ── Boot-info helpers ─────────────────────────────────────────────────────────

/// Boot-info message layout — exactly `ipc::MSG_SIZE` (64) bytes.
///
/// Passed to `lythd` via its boot-info IPC endpoint at handle 2.
#[repr(C, packed)]
struct BootInfo {
    signature: u64,   // 0xB007_1NFO_B007_1NFO
    mem_bytes: u64,   // total physical memory (free frames × 4096)
    free_frames: u64, // free physical frames at boot time
    vendor: [u8; 12], // CPUID leaf 0 vendor string
    _pad: [u8; 28],   // zero-pad to 64 bytes
}

const _: () = assert!(core::mem::size_of::<BootInfo>() == ipc::MSG_SIZE);

/// Build the initial `BootInfo` message for lythd.
fn build_boot_info() -> [u8; ipc::MSG_SIZE] {
    let free = pmm::free_frame_count() as u64;
    let info = BootInfo {
        signature: 0xB007_1000_B007_1000,
        mem_bytes: free * 4096,
        free_frames: free,
        vendor: cpuid_vendor(),
        _pad: [0u8; 28],
    };
    unsafe { core::mem::transmute(info) }
}

/// Read the CPU vendor string via CPUID leaf 0.
fn cpuid_vendor() -> [u8; 12] {
    let ebx: u32;
    let ecx: u32;
    let edx: u32;
    unsafe {
        // rbx is reserved by LLVM; save and restore it around cpuid.
        core::arch::asm!(
            "push rbx",
            "xor eax, eax",  // leaf 0
            "cpuid",
            "mov {0:e}, ebx",
            "pop rbx",
            out(reg) ebx,
            out("ecx") ecx,
            out("edx") edx,
            lateout("eax") _,
            options(nostack),
        );
    }
    let mut v = [0u8; 12];
    v[0..4].copy_from_slice(&ebx.to_le_bytes());
    v[4..8].copy_from_slice(&edx.to_le_bytes());
    v[8..12].copy_from_slice(&ecx.to_le_bytes());
    v
}

// Physical addresses of the two frames mapped by `userspace_smoke_task` into the
// global kernel PML4.  Stored here so kernel_main can unmap and free them after
// the task exits — otherwise they would leak permanently into the kernel page table.
#[cfg(feature = "boot-tests")]
static SMOKE_CODE_PHYS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
#[cfg(feature = "boot-tests")]
static SMOKE_STACK_PHYS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Kernel task for the userspace smoke-test.
///
/// Maps a user code page and a user stack page, writes two instructions
/// (`mov eax, SYS_TASK_EXIT; syscall`) into the code page, then enters
/// ring 3.  The syscall handler calls `task_exit()`, which marks this task
/// Dead and switches back to kernel_main.
#[cfg(feature = "boot-tests")]
fn userspace_smoke_task() -> ! {
    use core::sync::atomic::Ordering;

    // Allocate and map a user-executable code page.
    let code_phys = pmm::alloc_frame().expect("userspace smoke: no frame for code");
    SMOKE_CODE_PHYS.store(code_phys.as_u64(), Ordering::Relaxed);
    let code_va = vmm::VirtAddr(0x0000_0001_0000_0000);
    vmm::map_page(code_va, code_phys, vmm::PageFlags::USER_RX);

    // Write: `mov eax, 1` (SYS_TASK_EXIT); `syscall`
    // Written through the identity map (kernel RW), not the USER_RX mapping:
    // Limine sets CR0.WP=1, so ring-0 writes honour the R/W bit.
    unsafe {
        let p = code_phys.as_u64() as *mut u8;
        p.add(0).write(0xB8); // MOV EAX, imm32
        p.add(1).write(syscall::SYS_TASK_EXIT as u8); // imm32 byte 0
        p.add(2).write(0x00);
        p.add(3).write(0x00);
        p.add(4).write(0x00);
        p.add(5).write(0x0F); // SYSCALL (two-byte opcode)
        p.add(6).write(0x05);
    }

    // Allocate and map a user stack page.
    let stack_phys = pmm::alloc_frame().expect("userspace smoke: no frame for stack");
    SMOKE_STACK_PHYS.store(stack_phys.as_u64(), Ordering::Relaxed);
    let stack_va = vmm::VirtAddr(0x0000_0002_0000_0000);
    vmm::map_page(stack_va, stack_phys, vmm::PageFlags::USER_RW);
    let stack_top = vmm::VirtAddr(stack_va.as_u64() + 4096);

    // Enter ring 3 — never returns (user code calls SYS_TASK_EXIT).
    syscall::enter_userspace(code_va, stack_top);
}

/// Second kernel task: prints three ticks interleaved with task A, then exits.
fn task_b() -> ! {
    for i in 0..3_u32 {
        kprintln!("{VRB}[task B] tick {}, yielding...{RST}", i);
        task::yield_task();
    }
    kprintln!("{VRB}[task B] done, exiting{RST}");
    task::task_exit(task::exit_status_normal(0));
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let tid  = task::current_task_id();
    let name = task::current_task_name();
    let rsp: u64;
    unsafe { core::arch::asm!("mov {}, rsp", out(reg) rsp, options(nostack, nomem)); }
    kprintln!("[PANIC] task {} ({})  rsp={:#x}", tid, name, rsp);
    kprintln!("[PANIC] {}", info);
    loop {
        unsafe { core::arch::asm!("hlt") };
    }
}
