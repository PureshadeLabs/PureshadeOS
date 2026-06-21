#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::arch::global_asm;
use core::panic::PanicInfo;
use crate::serial::{OK, RST, STAT, TAG, VRB, WIN};

pub mod acpi;
pub mod apic;
pub mod cap;
pub mod framebuffer;
pub mod kaslr;
pub mod keyboard;
pub mod ioapic;
pub mod smp;
pub mod pci;
pub mod virtio_blk;
pub mod virtio_net;
pub mod net;
pub mod elf;
mod exceptions;
mod gdt;
pub mod heap;
mod idt;
pub mod ipc;
pub mod pmm;
pub mod serial;
pub mod rfs;
pub mod syscall;
pub mod task;
pub mod time;
pub mod tss;
pub mod vmm;

// Boot stub: Multiboot headers + 32-bit → 64-bit long-mode transition.
global_asm!(include_str!("arch/x86_64/boot.s"), options(att_syntax));

// ISR stubs for vectors 0–31, gdt_flush helper, isr_stub_table.
global_asm!(include_str!("arch/x86_64/isr_stubs.s"), options(att_syntax));

/// Kernel entry point — called by the boot stub in 64-bit long mode.
///
/// `mb_magic`: Multiboot magic (0x2BADB002 for MB1, 0x36D76289 for MB2).
/// `mb_info`:  Physical address of the Multiboot info structure.
#[unsafe(no_mangle)]
pub extern "C" fn kmain(mb_magic: u32, mb_info: u64) -> ! {
    serial::init();
    kprintln!();
    kprintln!("{TAG}  ██╗      ██╗   ██╗████████╗██╗  ██╗ ██████╗ ███████╗{RST}");
    kprintln!("{TAG}  ██║      ╚██╗ ██╔╝╚══██╔══╝██║  ██║██╔═══██╗██╔════╝{RST}");
    kprintln!("{TAG}  ██║       ╚████╔╝    ██║   ███████║██║   ██║███████╗{RST}");
    kprintln!("{TAG}  ██║        ╚██╔╝     ██║   ██╔══██║██║   ██║╚════██║{RST}");
    kprintln!("{TAG}  ███████╗    ██║      ██║   ██║  ██║╚██████╔╝███████║{RST}");
    kprintln!("{TAG}  ╚══════╝    ╚═╝      ╚═╝   ╚═╝  ╚═╝ ╚═════╝ ╚══════╝{RST}");
    kprintln!("  {VRB}x86_64 microkernel · capability-aware{RST}");
    kprintln!();
    kprintln!("{TAG}lythos{RST} kernel initializing...");

    kaslr::init();

    gdt::init();
    kprintln!("{TAG}[gdt]{RST} loaded");

    idt::init();
    kprintln!("{TAG}[idt]{RST} loaded {VRB}- exceptions active{RST}");

    // ── Physical memory manager ──────────────────────────────────────────
    pmm::init(mb_magic, mb_info);
    kprintln!(
        "{TAG}[pmm]{RST} initialized — {STAT}{}{RST} free frames ({STAT}{} MiB{RST})",
        pmm::free_frame_count(),
        pmm::free_frame_count() * 4 / 1024
    );

    // ── Smoke-test: alloc 1000 frames, free, re-alloc, verify same addrs ─
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

    // ── Virtual memory manager ────────────────────────────────────────────
    vmm::init();
    kprintln!("{TAG}[vmm]{RST} paging active {VRB}— identity 0–4MiB, higher-half kernel mapped{RST}");

    // ── VMM smoke-test: map a scratch page, write to it, unmap it ─────────
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

    // ── Framebuffer ───────────────────────────────────────────────────────
    if framebuffer::init(mb_magic, mb_info) {
        let (fw, fh) = framebuffer::dimensions();
        kprintln!("{TAG}[fb]{RST} {STAT}{}×{}{RST} linear framebuffer mapped", fw, fh);
        framebuffer::draw_splash();
    } else {
        kprintln!("{VRB}[fb] no framebuffer — run `make run-gui` for display{RST}");
    }

    // ── Heap allocator ────────────────────────────────────────────────────
    heap::init();
    kprintln!(
        "{TAG}[heap]{RST} initialized — {STAT}{} KiB{RST} pre-mapped at {STAT}{:#x}{RST}",
        heap::HEAP_INIT_PAGES * 4,
        heap::heap_start(),
    );

    // ── Heap smoke-test ───────────────────────────────────────────────────
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

    // ── Scheduler ─────────────────────────────────────────────────────────
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

    // ── APIC + preemptive timer ───────────────────────────────────────────
    apic::init();
    kprintln!("{TAG}[apic]{RST} timer active {VRB}— preemptive scheduling enabled{RST}");

    // ── Wall-clock anchor (requires apic::ticks() to be live) ────────────
    time::init();

    ioapic::init();
    kprintln!(
        "{TAG}[ioapic]{RST} initialized — {STAT}{} GSIs{RST}, all masked",
        ioapic::entry_count(),
    );

    keyboard::init();
    kprintln!("{TAG}[kbd]{RST} PS/2 keyboard initialized {VRB}— GSI 1, vector {}, set-2 decode{RST}", crate::keyboard::VECTOR_KBD);

    if virtio_blk::init() {
        let sects = virtio_blk::capacity_sectors();
        kprintln!(
            "{TAG}[virtio-blk]{RST} device ready — {STAT}{} sectors ({} MiB){RST}",
            sects,
            sects / 2048,
        );
        if rfs::init() {
            kprintln!("{TAG}[rfs]{RST} mounted");
        } else {
            kprintln!("{VRB}[rfs] no RFS_V1 image on disk (pass -drive file=disk.img,... to QEMU){RST}");
        }
    } else {
        kprintln!("{VRB}[virtio-blk] no device found (pass -device virtio-blk-pci to QEMU){RST}");
    }

    if virtio_net::init() {
        let mac = virtio_net::mac_addr();
        kprintln!(
            "{TAG}[virtio-net]{RST} device ready — MAC {STAT}{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}{RST}  IP 10.0.2.15",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5],
        );
        net::init();
        kprintln!("{TAG}[net]{RST} stack initialised");
    } else {
        kprintln!("{VRB}[virtio-net] no device found (pass -netdev user,id=net0 -device virtio-net-pci,netdev=net0){RST}");
    }

    // Smoke-test: sleep ~50 ms by polling the tick counter.
    let t0 = apic::ticks();
    while apic::ticks() < t0 + 50 {
        unsafe { core::arch::asm!("hlt") };
    }
    kprintln!(
        "{TAG}[apic]{RST} smoke-test {OK}passed{RST} {VRB}— {} ticks elapsed{RST}",
        apic::ticks() - t0
    );

    // ── Syscall interface ─────────────────────────────────────────────────
    syscall::init();
    kprintln!("{TAG}[syscall]{RST} initialized {VRB}— LSTAR/STAR/FMASK configured{RST}");

    // ── SMP — start Application Processors ───────────────────────────────
    smp::init();

    // ── Capability system ─────────────────────────────────────────────────
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

    // ── Cascade-revoke smoke-test ─────────────────────────────────────────
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

    // ── IPC smoke-test ────────────────────────────────────────────────────
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
            task::task_exit();
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
            task::task_exit();
        }

        task::spawn_kernel_task(ipc_receiver);
        task::spawn_kernel_task(ipc_sender);

        // Yield until both tasks have exited (recv count reaches 3).
        while IPC_RECV_COUNT.load(core::sync::atomic::Ordering::Relaxed) < 3 {
            task::yield_task();
        }
    }
    kprintln!("{TAG}[ipc]{RST} smoke-test {OK}passed{RST}");

    // ── Userspace entry smoke-test ────────────────────────────────────────
    // Spawn a kernel task that maps a user code page, writes `mov eax,1;
    // syscall` into it (SYS_TASK_EXIT = 1), and enters ring 3.  The syscall
    // handler calls task_exit(), marks the task Dead, and switches back to
    // kmain.
    let smoke_task_id = task::spawn_kernel_task(userspace_smoke_task);
    // Yield until the smoke task has been fully reaped by sweep_dead.
    // A bare yield_task() is not enough: the APIC timer may switch back to
    // kmain before the task stores SMOKE_STACK_PHYS, making the physaddr zero.
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

    // ── ELF exec smoke-test ───────────────────────────────────────────────
    // Load SMOKE_ELF (hand-crafted ELF64 that calls SYS_TASK_EXIT) via the
    // full exec path: ELF parser → segment mapping → stack allocation →
    // ABI stack frame → exec_trampoline → ring-3 entry.
    // The task exits via SYS_TASK_EXIT, returning control to kmain.
    elf::exec(elf::SMOKE_ELF, &[], &[]).expect("elf smoke-test: exec failed");
    task::yield_task();
    kprintln!("{TAG}[elf]{RST} smoke-test {OK}passed{RST}");

    // ── Core integration smoke tests ──────────────────────────────────────
    //
    // Run before spawning lythd so these kernel-level checks execute without
    // any concurrent userspace tasks.  None of the checks require lythd.
    core_smoke();
    kprintln!("{WIN}[integration] all checks passed{RST}");

    // ── Locate lythd ELF ────────────────────────────────────────────────
    //
    // lythd (PID 1) lives at /lth/system/init (symlink → /lth/bin/lythd).
    // Populate via `make oros`.
    let lythd_elf = rfs::load_file("/lth/system/init")
        .expect("lythd: /lth/system/init not found — run `make oros` to populate rootfs/lth/bin/");

    // ── lythd bootstrap ───────────────────────────────────────────────────
    //
    // Build the initial capability set and exec lythd with it.
    //
    // Cap layout handed to lythd (in exec order):
    //   handle 0 — root memory capability (all physical frames)
    //   handle 1 — rollback capability    (privileged SYS_ROLLBACK gate)
    //   handle 2 — boot-info IPC endpoint (one pre-queued BootInfo message)

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

    // Insert all three caps into kmain's table so spawn_userspace_task can inherit them.
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

    // Temporarily give kmain's bootstrap task this cap table so spawn_userspace_task
    // can read from it during cap inheritance.
    task::set_bootstrap_cap_table(kmain_caps);

    elf::exec(lythd_elf.as_slice(), &[mem_cap, rollback_cap, boot_cap], &[])
        .expect("lythd bootstrap: exec failed");

    kprintln!("{TAG}[boot]{RST} lythd launched {VRB}— entering scheduler{RST}");
    loop {
        unsafe { core::arch::asm!("hlt") };
    }
}

/// Core integration checklist.  Runs before lythd is spawned so all checks
/// execute without concurrent userspace tasks.
///
/// Checks verified here:
///   1. Boot completes without triple fault          (implicit — we reached this line)
///   2. IPC send/recv between two userspace tasks    (active test below)
///   3. task_exit reaps correctly                    (active test below)
///   4. Unauthorized cap access returns ENOCAP       (active test below)
///   5. QEMU `-d int,cpu_reset` shows no faults      (manual — run: make qemu)
fn core_smoke() {
    // ── Check 6: ENOCAP ──────────────────────────────────────────────────
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

    // ── Check 4 + 5: IPC between two userspace tasks ─────────────────────
    // Create a fresh IPC endpoint and add a cap to the bootstrap task's table
    // so both exec'd tasks can inherit it.
    {
        let ep_idx = ipc::create_endpoint().expect("create_endpoint");
        let ep_obj = cap::create_object(cap::KernelObject::Ipc {
            endpoint_idx: ep_idx,
        })
        .expect("step14: IPC ep cap OOM");
        let ipc_cap = {
            // task 0 is the bootstrap (kmain) task; access its cap table directly.
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

    // ── Cap syscall: SYS_CAP_GRANT error paths + SYS_CAP_REVOKE ─────────
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

    // ── SYS_MMAP / SYS_MUNMAP full lifecycle ─────────────────────────────
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

    // ── SYS_IPC_SEND_CAP / SYS_IPC_RECV_CAP end-to-end ──────────────────
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
            task::task_exit();
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
            task::task_exit();
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

    // ── Triangular IPC: blocked task woken by a third task ────────────────
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
            task::task_exit();
        }

        fn tri_task_b() -> ! {
            let ep = TRI_EP2.load(core::sync::atomic::Ordering::Relaxed);
            let mut buf = [0u8; ipc::MSG_SIZE];
            ipc::recv(ep, &mut buf);
            assert_eq!(buf[0], 0xBB, "triangular IPC: task B got wrong payload");
            TRI_DONE.fetch_or(2, core::sync::atomic::Ordering::Relaxed);
            task::task_exit();
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
            task::task_exit();
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

    // ── SYS_EXEC invoked from a userspace task ────────────────────────────
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

    // ── Syscall fuzz: boundary / invalid inputs must return error codes ───
    // All cases are called directly via `syscall_dispatch` (no ring-3 needed).
    // A panic here means the kernel did not reject a bad input gracefully.
    {
        // Unknown syscall numbers → ENOSYS.
        // syscall::SYSCALL_MAX is kept in sync with the highest defined syscall.
        // 20/21 return ENOSYS when no VirtIO block device is present, which is the
        // case during kernel smoke tests run without a disk image.
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
        // Error sentinels are the top four u64 values (EINVAL..ENOSYS).
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
        f.a1 = 0; // kmain / bootstrap task
        assert_eq!(
            syscall::syscall_dispatch(&mut f),
            1,
            "fuzz: SYS_TASK_STATUS bootstrap task must be alive"
        );
    }
    kprintln!("{TAG}[integration]{RST} syscall fuzz {OK}passed{RST} {VRB}— all bad inputs rejected{RST}");
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
// global kernel PML4.  Stored here so kmain can unmap and free them after the task
// exits — otherwise they would leak permanently into the kernel page table.
static SMOKE_CODE_PHYS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static SMOKE_STACK_PHYS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Kernel task for the Step 10 userspace smoke-test.
///
/// Maps a user code page and a user stack page, writes two instructions
/// (`mov eax, SYS_TASK_EXIT; syscall`) into the code page, then enters
/// ring 3.  The syscall handler calls `task_exit()`, which marks this task
/// Dead and switches back to kmain.
///
/// The physical frames are saved to `SMOKE_CODE_PHYS` / `SMOKE_STACK_PHYS`
/// so kmain can unmap and free them once the task has exited.
fn userspace_smoke_task() -> ! {
    use core::sync::atomic::Ordering;

    // Allocate and map a user-executable code page.
    let code_phys = pmm::alloc_frame().expect("userspace smoke: no frame for code");
    SMOKE_CODE_PHYS.store(code_phys.as_u64(), Ordering::Relaxed);
    let code_va = vmm::VirtAddr(0x0000_0001_0000_0000);
    vmm::map_page(code_va, code_phys, vmm::PageFlags::USER_RX);

    // Write: `mov eax, 1` (SYS_TASK_EXIT); `syscall`
    unsafe {
        let p = code_va.as_u64() as *mut u8;
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
    task::task_exit();
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
