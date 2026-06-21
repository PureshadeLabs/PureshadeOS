use crate::kprintln;

/// CPU exception handling.
///
/// `exception_handler` is called by the common ISR dispatcher in isr_stubs.s
/// with a pointer to the full saved CPU state on the stack.

/// Saved register state at exception entry.
///
/// Layout must exactly match the push sequence in `exception_common`
/// (isr_stubs.s).  Fields are ordered from lowest stack address (last push /
/// at RSP when exception_handler is entered) to highest (CPU-pushed frame).
#[repr(C)]
pub struct ExceptionFrame {
    // Saved by exception_common (pushed last → lowest address)
    pub r15: u64, pub r14: u64, pub r13: u64, pub r12: u64,
    pub r11: u64, pub r10: u64, pub r9:  u64, pub r8:  u64,
    pub rbp: u64, pub rdi: u64, pub rsi: u64,
    pub rdx: u64, pub rcx: u64, pub rbx: u64, pub rax: u64,
    // Pushed by the ISR stub
    pub vector:     u64,
    pub error_code: u64,
    // Pushed by the CPU on exception entry (same-privilege: no rsp/ss)
    pub rip:    u64,
    pub cs:     u64,
    pub rflags: u64,
}

fn dump_regs(f: &ExceptionFrame) {
    kprintln!("  rax={:#018x}  rbx={:#018x}  rcx={:#018x}  rdx={:#018x}",
              f.rax, f.rbx, f.rcx, f.rdx);
    kprintln!("  rsi={:#018x}  rdi={:#018x}  rbp={:#018x}",
              f.rsi, f.rdi, f.rbp);
    kprintln!("  r8 ={:#018x}  r9 ={:#018x}  r10={:#018x}  r11={:#018x}",
              f.r8,  f.r9,  f.r10, f.r11);
    kprintln!("  r12={:#018x}  r13={:#018x}  r14={:#018x}  r15={:#018x}",
              f.r12, f.r13, f.r14, f.r15);
    kprintln!("  rip={:#018x}  cs={:#06x}  rflags={:#018x}",
              f.rip, f.cs, f.rflags);
}

/// Page fault handler (vector 14).
///
/// If the fault originated in ring-3 (CPL=3), logs the fault address and
/// terminates only the faulting task — the kernel and all other tasks keep
/// running.  A kernel-mode #PF is unrecoverable and halts the CPU.
fn page_fault_handler(frame: &ExceptionFrame) -> ! {
    let cr2: u64;
    unsafe {
        core::arch::asm!("mov {}, cr2", out(reg) cr2, options(nostack, nomem));
    }
    let p = frame.error_code & 1 != 0;
    let w = frame.error_code & 2 != 0;
    let u = frame.error_code & 4 != 0;

    let tid  = crate::task::current_task_id();
    let name = crate::task::current_task_name();

    kprintln!("[#PF] task {} ({})  faulting_va={:#x}  error={:#x}  {} {} {}",
        tid, name, cr2, frame.error_code,
        if p { "protection-violation" } else { "not-present" },
        if w { "write" }               else { "read" },
        if u { "user"  }               else { "kernel" },
    );
    dump_regs(frame);

    if frame.cs & 3 == 3 {
        let frame_ptr  = frame as *const ExceptionFrame as *const u64;
        let struct_u64s = core::mem::size_of::<ExceptionFrame>() / 8;
        let user_rsp   = unsafe { *frame_ptr.add(struct_u64s) };
        let user_ss    = unsafe { *frame_ptr.add(struct_u64s + 1) };
        kprintln!("[#PF] user rsp={:#x}  ss={:#x}  → killing task", user_rsp, user_ss);
        crate::task::task_exit();
    }

    loop { unsafe { core::arch::asm!("hlt") }; }
}

/// Common exception entry point, called from `exception_common` in isr_stubs.s.
#[unsafe(no_mangle)]
pub extern "C" fn exception_handler(frame: *const ExceptionFrame) {
    let f = unsafe { &*frame };

    if f.vector == 14 {
        page_fault_handler(f);
    }

    let tid  = crate::task::current_task_id();
    let name = crate::task::current_task_name();

    kprintln!("[EXCEPTION] vec={:#x}  err={:#x}  task {} ({})",
              f.vector, f.error_code, tid, name);
    dump_regs(f);

    if f.cs & 3 == 3 {
        kprintln!("[exception] killing user task {} ({})", tid, name);
        crate::task::task_exit();
    }

    loop { unsafe { core::arch::asm!("hlt") }; }
}
