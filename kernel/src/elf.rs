/// ELF64 loader and `exec` — Step 12.
///
/// ## What this module does
///
/// `exec(elf_data, caps)` parses a static ELF64 binary, maps its `PT_LOAD`
/// segments into the current address space, allocates an 8 MiB user stack
/// (with a guard page at the bottom), writes an initial stack frame, spawns a
/// new kernel task that will enter ring-3 at the ELF entry point, and returns
/// the new task's ID.
///
/// ## Constraints
///
/// - Static ELF64 binaries only (`ET_EXEC`; dynamic linking deferred).
/// - Virtual addresses in `PT_LOAD` segments must be **above 1 GiB** — the
///   VMM identity-maps 0→1 GiB with 2 MiB huge pages and refuses to split them.
/// - All loaded segments share the kernel's page table (per-process isolation
///   deferred to a later step).
///
/// ## Stack layout at ring-3 entry
///
/// The initial stack frame follows the System V AMD64 ABI for `_start`.
/// String data for argv is placed immediately after the fixed table:
///
/// ```text
/// rsp+0                  argc
/// rsp+8 .. rsp+8*(N+1)  argv[0..N-1]  (pointers into the string area below)
/// rsp+8*(N+1)            NULL  (argv terminator)
/// rsp+8*(N+2)            NULL  (envp terminator)
/// rsp+8*(N+3)            6     (AT_PAGESZ type)
/// rsp+8*(N+4)            4096  (AT_PAGESZ value)
/// rsp+8*(N+5)            0     (AT_NULL type)
/// rsp+8*(N+6)            0     (AT_NULL value)
/// rsp+8*(N+7)            "argv[0]\0argv[1]\0…" (string data)
/// ```
///
/// `rsp` is 16-byte aligned at entry.  When no argv is passed (N=0) the
/// layout matches the previous fixed 56-byte frame.

extern crate alloc;

use core::sync::atomic::{AtomicU64, Ordering};

use crate::cap::CapHandle;
use crate::pmm::PhysAddr;
use crate::task::TaskId;
use crate::vmm::{PageFlags, VirtAddr};

// ── ELF64 constants ───────────────────────────────────────────────────────────

const ELFMAG:      [u8; 4] = [0x7F, b'E', b'L', b'F'];
const ELFCLASS64:  u8  = 2;
const ELFDATA2LSB: u8  = 1;
const ET_EXEC:     u16 = 2;
const EM_X86_64:   u16 = 62;
const PT_LOAD:     u32 = 1;
const PF_X:        u32 = 1;
const PF_W:        u32 = 2;

// ── ELF64 structs ─────────────────────────────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy)]
struct Elf64Ehdr {
    e_ident:     [u8; 16],
    e_type:      u16,
    e_machine:   u16,
    e_version:   u32,
    e_entry:     u64,
    e_phoff:     u64,
    e_shoff:     u64,
    e_flags:     u32,
    e_ehsize:    u16,
    e_phentsize: u16,
    e_phnum:     u16,
    e_shentsize: u16,
    e_shnum:     u16,
    e_shstrndx:  u16,
}

const ELF64_EHDR_SIZE: usize = core::mem::size_of::<Elf64Ehdr>();

#[repr(C)]
#[derive(Clone, Copy)]
struct Elf64Phdr {
    p_type:   u32,
    p_flags:  u32,
    p_offset: u64,
    p_vaddr:  u64,
    p_paddr:  u64,
    p_filesz: u64,
    p_memsz:  u64,
    p_align:  u64,
}

const ELF64_PHDR_SIZE: usize = core::mem::size_of::<Elf64Phdr>();

// ── Error ─────────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum ElfError {
    TooSmall,
    BadMagic,
    Not64Bit,
    NotLittleEndian,
    UnsupportedType,
    UnsupportedMachine,
    BadProgramHeader,
    SegmentOutOfBounds,
    OutOfMemory,
    StackExhausted,
    ArgvTooLarge,
}

// ── Stack constants ────────────────────────────────────────────────────────────

/// Base VA of the first user-mode stack slot.
/// Must be above 1 GiB (identity-mapped region) and below the kernel half.
const STACK_GUARD_VA:   u64   = 0x0000_7FFF_0000_0000;
/// 256 KiB = 64 × 4 KiB pages of usable stack above the guard.
///
/// Every page is allocated eagerly at exec, so this is a per-task physical
/// RAM cost — the previous 8 MiB value made each userspace task cost 8 MiB
/// idle.  256 KiB covers the current OROS binaries (no deep recursion; big
/// buffers live on the brk heap); overflow hits the guard page as a clean #PF.
const USER_STACK_PAGES: usize = 64;
/// Pages per stack slot: guard + usable + 1 gap.
const STACK_SLOT_PAGES: u64   = USER_STACK_PAGES as u64 + 2;

/// Monotonically increasing counter for stack slot allocation.
/// Each `alloc_user_stack` call claims one slot of `STACK_SLOT_PAGES` pages.
static NEXT_STACK_SLOT: AtomicU64 = AtomicU64::new(0);

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Read an `Elf64Ehdr` from `data` without assuming alignment.
fn read_ehdr(data: &[u8]) -> Result<Elf64Ehdr, ElfError> {
    if data.len() < ELF64_EHDR_SIZE { return Err(ElfError::TooSmall); }
    Ok(unsafe { (data.as_ptr() as *const Elf64Ehdr).read_unaligned() })
}

/// Read the `n`-th `Elf64Phdr` from `data` given base offset and entry size.
fn read_phdr(data: &[u8], phoff: usize, phentsize: usize, n: usize)
    -> Result<Elf64Phdr, ElfError>
{
    let off = phoff + n * phentsize;
    if off + ELF64_PHDR_SIZE > data.len() { return Err(ElfError::BadProgramHeader); }
    Ok(unsafe { (data.as_ptr().add(off) as *const Elf64Phdr).read_unaligned() })
}

/// Choose page flags for a `PT_LOAD` segment based on its `p_flags`.
fn segment_flags(p_flags: u32) -> PageFlags {
    let exec  = p_flags & PF_X != 0;
    let write = p_flags & PF_W != 0;
    match (exec, write) {
        (true,  false) => PageFlags::USER_RX,
        (false, true ) => PageFlags::USER_RW,
        // R/W/X: present + user + writable (no NX).
        (true,  true ) => PageFlags(
            PageFlags::PRESENT.0 | PageFlags::USER.0 | PageFlags::WRITABLE.0
        ),
        // R only: present + user + NX.
        (false, false) => PageFlags(
            PageFlags::PRESENT.0 | PageFlags::USER.0 | PageFlags::NX.0
        ),
    }
}

/// Load a single `PT_LOAD` segment into `user_pml4`.
///
/// All reads and writes go through the **physical** (identity-mapped) address
/// of each frame so that this function works while the kernel PML4 is the
/// active CR3 — the user PT is not yet loaded.  Page-aligned arithmetic is
/// used throughout to avoid spilling into unmapped adjacent pages.  If a page
/// was already mapped by an earlier overlapping segment the physical frame is
/// reused and only the flags are upgraded.
fn load_segment_into(data: &[u8], phdr: &Elf64Phdr, user_pml4: PhysAddr) -> Result<(), ElfError> {
    let file_off   = phdr.p_offset as usize;
    let file_size  = phdr.p_filesz as usize;
    let mem_size   = phdr.p_memsz  as usize;
    let vaddr_base = phdr.p_vaddr;
    let flags      = segment_flags(phdr.p_flags);

    if file_off + file_size > data.len() { return Err(ElfError::SegmentOutOfBounds); }
    if mem_size == 0 { return Ok(()); }

    let vaddr_end  = vaddr_base + mem_size as u64;
    let page_start = vaddr_base & !0xFFF;
    let page_end   = (vaddr_end + 0xFFF) & !0xFFF;
    let page_count = ((page_end - page_start) / 0x1000) as usize;

    for i in 0..page_count {
        let page_va = page_start + (i as u64) * 0x1000;

        // Resolve or allocate the physical frame for this page.
        let frame = if let Some(p) = crate::vmm::query_page_in(user_pml4, VirtAddr(page_va)) {
            crate::vmm::update_page_flags_in(user_pml4, VirtAddr(page_va), flags);
            p
        } else {
            let f = crate::pmm::alloc_frame()
                .ok_or(ElfError::OutOfMemory)?;
            crate::vmm::map_page_in(user_pml4, VirtAddr(page_va), f, flags);
            // Zero entirely through the identity map.
            unsafe { core::ptr::write_bytes(f.as_u64() as *mut u8, 0, 0x1000); }
            f
        };

        // Copy the file bytes for this page (if any) through the physical frame.
        let seg_file_end  = vaddr_base + file_size as u64;
        let copy_va_start = vaddr_base.max(page_va);
        let copy_va_end   = seg_file_end.min(page_va + 0x1000);

        if copy_va_start < copy_va_end {
            let frame_off    = (copy_va_start - page_va) as usize;
            let file_src_off = file_off + (copy_va_start - vaddr_base) as usize;
            let copy_len     = (copy_va_end - copy_va_start) as usize;
            unsafe {
                core::ptr::copy_nonoverlapping(
                    data.as_ptr().add(file_src_off),
                    (frame.as_u64() as *mut u8).add(frame_off),
                    copy_len,
                );
            }
        }
    }
    Ok(())
}

/// Allocate and map the user stack for one task into `user_pml4`.
///
/// Slot layout (STACK_SLOT_PAGES pages wide):
///   slot_base + 0              — guard page (no USER bit → #PF on access)
///   slot_base + 4 KiB          — first usable stack page
///   slot_base + N × 4 KiB      — last usable stack page  (N = USER_STACK_PAGES)
///   slot_base + (N+1) × 4 KiB  — stack top (returned; gap page for next slot)
///
/// Returns `(stack_top_va, last_usable_page_phys)`.  The physical address is
/// needed by `write_initial_stack_frame` to write the ABI frame through the
/// identity map while the kernel PML4 is still active.
fn alloc_user_stack_into(user_pml4: PhysAddr) -> Result<(VirtAddr, PhysAddr), ElfError> {
    // Available VA: 0x7FFF_0000_0000 → 0x8000_0000_0000 = 4 GiB.
    // Slot size: STACK_SLOT_PAGES × 4 KiB.
    const MAX_STACK_SLOTS: u64 =
        (0x1_0000_0000u64 / 0x1000) / STACK_SLOT_PAGES;
    let slot = NEXT_STACK_SLOT.fetch_add(1, Ordering::Relaxed);
    if slot >= MAX_STACK_SLOTS { return Err(ElfError::StackExhausted); }
    let slot_base = STACK_GUARD_VA + slot * STACK_SLOT_PAGES * 0x1000;

    // Guard page: kernel-only, NX.
    let guard_frame = crate::pmm::alloc_frame().ok_or(ElfError::OutOfMemory)?;
    crate::vmm::map_page_in(
        user_pml4,
        VirtAddr(slot_base),
        guard_frame,
        PageFlags(PageFlags::PRESENT.0 | PageFlags::NX.0),
    );
    unsafe { core::ptr::write_bytes(guard_frame.as_u64() as *mut u8, 0, 0x1000); }

    // Usable stack pages.
    let mut last_frame = PhysAddr(0);
    for i in 1..=(USER_STACK_PAGES as u64) {
        let frame = crate::pmm::alloc_frame().ok_or(ElfError::OutOfMemory)?;
        crate::vmm::map_page_in(user_pml4, VirtAddr(slot_base + i * 0x1000), frame, PageFlags::USER_RW);
        unsafe { core::ptr::write_bytes(frame.as_u64() as *mut u8, 0, 0x1000); }
        if i == USER_STACK_PAGES as u64 { last_frame = frame; }
    }

    let stack_top = VirtAddr(slot_base + (USER_STACK_PAGES as u64 + 1) * 0x1000);
    Ok((stack_top, last_frame))
}

/// Write the initial ABI stack frame below `stack_top` through `last_page_phys`.
///
/// The stack pages are mapped in the user PT, which is not the active CR3
/// during `exec`.  Writes go through the physical (identity-mapped) address
/// so no page-table switch is required.
///
/// `argv` holds the command-line arguments.  When empty the frame is exactly
/// 56 bytes (same as before), matching the previous behaviour.  All data must
/// fit within a single 4 KiB page; panics if `argv` is too large.
fn write_initial_stack_frame(
    stack_top: VirtAddr,
    last_page_phys: PhysAddr,
    argv: &[&str],
) -> Result<VirtAddr, ElfError> {
    let argc       = argv.len();
    let str_bytes: usize = argv.iter().map(|s| s.len() + 1).sum();
    // Fixed table: 1 (argc) + argc (ptrs) + 1 (argv NULL) + 1 (envp NULL) + 4 (auxv) = argc+7
    let header_bytes = 8 * (argc + 7);
    let total_bytes  = header_bytes + str_bytes;
    if total_bytes > 4080 { return Err(ElfError::ArgvTooLarge); }

    let rsp         = (stack_top.as_u64() - total_bytes as u64) & !0xF;
    let page_offset = (rsp & 0xFFF) as usize;
    // All writes go through the physical identity-mapped address of the last stack page.
    let base        = (last_page_phys.as_u64() as usize + page_offset) as *mut u8;

    let mut off = 0usize;

    // Helper: write a u64 at byte offset `off` through the physical pointer.
    macro_rules! w64 {
        ($val:expr) => {{
            unsafe { (base.add(off) as *mut u64).write_unaligned($val) };
            off += 8;
        }};
    }

    w64!(argc as u64);                          // argc

    // argv pointers — strings land just past the fixed table
    let strs_va = rsp + header_bytes as u64;
    let mut str_va = strs_va;
    for s in argv {
        w64!(str_va);
        str_va += (s.len() + 1) as u64;
    }

    w64!(0);        // argv NULL terminator
    w64!(0);        // envp NULL
    w64!(6);        // AT_PAGESZ type
    w64!(4096);     // AT_PAGESZ value
    w64!(0);        // AT_NULL type
    w64!(0);        // AT_NULL value

    // String data
    for s in argv {
        let bytes = s.as_bytes();
        unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), base.add(off), bytes.len()); }
        off += bytes.len();
        unsafe { base.add(off).write(0); }      // null terminator
        off += 1;
    }

    Ok(VirtAddr(rsp))
}

// ── exec trampoline ───────────────────────────────────────────────────────────

/// Kernel-mode entry point for tasks created by `exec`.
///
/// Reads the user entry point and user stack top from the current task's
/// stored fields (set by `spawn_userspace_task`) and transfers control to
/// ring-3 via `iretq`.  Never returns.
fn exec_trampoline() -> ! {
    let (entry, stack) = crate::task::current_entry_and_stack();
    crate::syscall::enter_userspace(entry, stack);
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Load and execute a static ELF64 binary.
///
/// Steps performed:
/// 1. Parse the ELF header and validate magic / class / type.
/// 2. For each `PT_LOAD` segment: allocate frames, map, copy file data,
///    zero-fill BSS.
/// 3. Allocate an 8 MiB user stack with a guard page.
/// 4. Write the initial ABI stack frame.
/// 5. Inherit `caps` into the new task's capability table.
/// 6. Spawn a kernel task that will enter ring-3 at the ELF entry point.
///
/// Returns the `TaskId` of the newly created task.  The task is enqueued in
/// the scheduler but does not run until the caller yields.
pub fn exec(elf_data: &[u8], caps: &[CapHandle], argv: &[&str]) -> Result<TaskId, ElfError> {
    // ── 1. Parse header ───────────────────────────────────────────────────
    let ehdr = read_ehdr(elf_data)?;

    if ehdr.e_ident[..4] != ELFMAG           { return Err(ElfError::BadMagic); }
    if ehdr.e_ident[4]   != ELFCLASS64       { return Err(ElfError::Not64Bit); }
    if ehdr.e_ident[5]   != ELFDATA2LSB      { return Err(ElfError::NotLittleEndian); }
    if ehdr.e_type        != ET_EXEC         { return Err(ElfError::UnsupportedType); }
    if ehdr.e_machine     != EM_X86_64       { return Err(ElfError::UnsupportedMachine); }

    let phoff     = ehdr.e_phoff     as usize;
    let phentsize = ehdr.e_phentsize as usize;
    let phnum     = ehdr.e_phnum     as usize;

    // ── 2. Create per-process page table ─────────────────────────────────
    let user_pml4 = crate::vmm::create_user_page_table();

    // ── 3. Load PT_LOAD segments into the new page table ─────────────────
    for i in 0..phnum {
        let phdr = read_phdr(elf_data, phoff, phentsize, i)?;
        if phdr.p_type != PT_LOAD { continue; }
        load_segment_into(elf_data, &phdr, user_pml4)?;
    }

    // ── 4. User stack ─────────────────────────────────────────────────────
    let (stack_top, last_page_phys) = alloc_user_stack_into(user_pml4)?;

    // ── 5. Initial stack frame (through physical address) ─────────────────
    let initial_sp = write_initial_stack_frame(stack_top, last_page_phys, argv)?;

    // ── 6. Spawn task with its own page table ─────────────────────────────
    let task_name = argv.first().copied().unwrap_or("user");
    let task_id = crate::task::spawn_userspace_task(
        VirtAddr(ehdr.e_entry),
        initial_sp,
        caps,
        exec_trampoline,
        user_pml4.as_u64(),
        task_name,
    );

    Ok(task_id)
}

// ── Smoke-test binary ─────────────────────────────────────────────────────────
//
// A hand-crafted ELF64 binary that calls SYS_TASK_EXIT (nr=1) and halts.
//
// Header layout:
//   [  0.. 63] ELF header
//   [ 64..119] PT_LOAD program header (p_vaddr=0x100000000, p_filesz/memsz=128)
//   [120..127] code: mov eax,1 ; syscall ; hlt
//
// Entry point: 0x0000_0001_0000_0078  (0x100000000 + 120)
//
// The load VA is chosen to be above the VMM's 0→1 GiB identity-mapped region.

pub static SMOKE_ELF: &[u8] = &[
    // ── ELF header (64 bytes) ─────────────────────────────────────────────
    0x7F, 0x45, 0x4C, 0x46,              // ELF magic
    0x02,                                 // EI_CLASS:   ELFCLASS64
    0x01,                                 // EI_DATA:    ELFDATA2LSB
    0x01,                                 // EI_VERSION: 1
    0x00,                                 // EI_OSABI:   System V
    0x00, 0x00, 0x00, 0x00,              // padding
    0x00, 0x00, 0x00, 0x00,              // padding
    0x02, 0x00,                           // e_type:      ET_EXEC
    0x3E, 0x00,                           // e_machine:   EM_X86_64
    0x01, 0x00, 0x00, 0x00,              // e_version:   1
    0x78, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,  // e_entry: 0x100000078
    0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // e_phoff: 64
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // e_shoff: 0
    0x00, 0x00, 0x00, 0x00,              // e_flags: 0
    0x40, 0x00,                           // e_ehsize:    64
    0x38, 0x00,                           // e_phentsize: 56
    0x01, 0x00,                           // e_phnum:     1
    0x40, 0x00,                           // e_shentsize: 64
    0x00, 0x00,                           // e_shnum:     0
    0x00, 0x00,                           // e_shstrndx:  0

    // ── PT_LOAD program header (56 bytes) ─────────────────────────────────
    0x01, 0x00, 0x00, 0x00,              // p_type:   PT_LOAD
    0x05, 0x00, 0x00, 0x00,              // p_flags:  PF_R | PF_X
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // p_offset: 0
    0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,  // p_vaddr:  0x100000000
    0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,  // p_paddr:  0x100000000
    0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // p_filesz: 128
    0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // p_memsz:  128
    0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // p_align:  0x1000

    // ── Code (8 bytes at file offset 120) ─────────────────────────────────
    0xB8, 0x01, 0x00, 0x00, 0x00,        // mov eax, 1   (SYS_TASK_EXIT)
    0x0F, 0x05,                           // syscall
    0xF4,                                 // hlt          (should not reach)
];

// ── Step 14 integration ELFs ──────────────────────────────────────────────────
//
// Two userspace tasks for the end-to-end IPC smoke test.
// They use a *shared* IPC capability at handle 0 (the only cap they inherit).
//
// Different p_vaddr values keep them from clobbering each other's code pages
// in the shared kernel page table.

/// Minimal IPC sender task (p_vaddr=0x200000000).
///
/// Assembly (entry at file offset 120 = VA 0x200000078):
/// ```asm
/// mov  eax, 6       ; SYS_IPC_SEND
/// xor  edi, edi     ; a1 = handle 0 (ipc_cap)
/// mov  rsi, rsp     ; a2 = buf (initial stack frame on rsp)
/// mov  edx, 64      ; a3 = len
/// syscall
/// mov  eax, 1       ; SYS_TASK_EXIT
/// syscall
/// hlt
/// ```
pub static IPC_SENDER_ELF: &[u8] = &[
    // ── ELF header (64 bytes) ─────────────────────────────────────────────
    0x7F, 0x45, 0x4C, 0x46, 0x02, 0x01, 0x01, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x02, 0x00, 0x3E, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x78, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,  // e_entry: 0x200000078
    0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // e_phoff: 64
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00,
    0x40, 0x00, 0x38, 0x00, 0x01, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00,
    // ── PT_LOAD (56 bytes) ────────────────────────────────────────────────
    0x01, 0x00, 0x00, 0x00,              // PT_LOAD
    0x05, 0x00, 0x00, 0x00,              // PF_R | PF_X
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,  // p_vaddr: 0x200000000
    0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,
    0x91, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // p_filesz: 145
    0x91, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // p_memsz:  145
    0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // ── Code (25 bytes at file offset 120) ───────────────────────────────
    0xB8, 0x06, 0x00, 0x00, 0x00,        // mov  eax, 6   (SYS_IPC_SEND)
    0x31, 0xFF,                           // xor  edi, edi (handle = 0)
    0x48, 0x89, 0xE6,                    // mov  rsi, rsp
    0xBA, 0x40, 0x00, 0x00, 0x00,        // mov  edx, 64
    0x0F, 0x05,                           // syscall
    0xB8, 0x01, 0x00, 0x00, 0x00,        // mov  eax, 1   (SYS_TASK_EXIT)
    0x0F, 0x05,                           // syscall
    0xF4,                                 // hlt
];

/// MMAP lifecycle test (p_vaddr=0x400000000).
///
/// Requires a Memory capability (any handle) with WRITE rights.
///
/// Assembly (entry at file offset 120 = VA 0x400000078):
/// ```asm
/// mov  eax, 2              ; SYS_MMAP
/// mov  rdi, 0x500000000    ; virt (page-aligned, above 1 GiB)
/// xor  esi, esi            ; phys (ignored; kernel allocates fresh frame)
/// mov  edx, 7              ; flags: PRESENT | WRITABLE | USER
/// syscall
/// mov  rdi, 0x500000000
/// mov  dword [rdi], 0x1234ABCD   ; write sentinel through new mapping
/// mov  eax, 3              ; SYS_MUNMAP
/// mov  rdi, 0x500000000
/// syscall                  ; frame freed, PTE cleared
/// mov  eax, 1              ; SYS_TASK_EXIT
/// syscall
/// hlt
/// ```
pub static MMAP_TEST_ELF: &[u8] = &[
    // ── ELF header (64 bytes) ─────────────────────────────────────────────
    0x7F, 0x45, 0x4C, 0x46, 0x02, 0x01, 0x01, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x02, 0x00, 0x3E, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x78, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00,  // e_entry: 0x400000078
    0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // e_phoff: 64
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00,
    0x40, 0x00, 0x38, 0x00, 0x01, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00,
    // ── PT_LOAD (56 bytes) ────────────────────────────────────────────────
    0x01, 0x00, 0x00, 0x00,              // PT_LOAD
    0x05, 0x00, 0x00, 0x00,              // PF_R | PF_X
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00,  // p_vaddr: 0x400000000
    0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00,
    0xB9, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // p_filesz: 185
    0xB9, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // p_memsz:  185
    0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // ── Code (65 bytes at file offset 120) ───────────────────────────────
    0xB8, 0x02, 0x00, 0x00, 0x00,        // mov  eax, 2   (SYS_MMAP)
    0x48, 0xBF, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, // mov rdi, 0x500000000
    0x31, 0xF6,                           // xor  esi, esi
    0xBA, 0x07, 0x00, 0x00, 0x00,        // mov  edx, 7
    0x0F, 0x05,                           // syscall → SYS_MMAP
    0x48, 0xBF, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, // mov rdi, 0x500000000
    0xC7, 0x07, 0xCD, 0xAB, 0x34, 0x12,  // mov  dword [rdi], 0x1234ABCD
    0xB8, 0x03, 0x00, 0x00, 0x00,        // mov  eax, 3   (SYS_MUNMAP)
    0x48, 0xBF, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00, // mov rdi, 0x500000000
    0x0F, 0x05,                           // syscall → SYS_MUNMAP
    0xB8, 0x01, 0x00, 0x00, 0x00,        // mov  eax, 1   (SYS_TASK_EXIT)
    0x0F, 0x05,                           // syscall
    0xF4,                                 // hlt
];

/// SYS_EXEC-from-userspace test (p_vaddr=0x600000000).
///
/// Calls SYS_EXEC with an embedded copy of SMOKE_ELF (128 bytes at file
/// offset 248 = VA 0x6000000F8), yields once to let the spawned task run,
/// then calls SYS_TASK_EXIT.
///
/// Assembly (entry at file offset 120 = VA 0x600000078):
/// ```asm
/// mov  eax, 10             ; SYS_EXEC
/// mov  rdi, 0x6000000F8    ; elf_ptr = VA of embedded SMOKE_ELF
/// mov  esi, 128            ; elf_len
/// xor  edx, edx            ; caps_ptr = 0
/// xor  r10d, r10d          ; caps_len = 0
/// syscall
/// mov  eax, 0              ; SYS_YIELD
/// syscall
/// mov  eax, 1              ; SYS_TASK_EXIT
/// syscall
/// hlt
/// ```
pub static EXEC_FROM_USER_ELF: &[u8] = &[
    // ── ELF header (64 bytes) ─────────────────────────────────────────────
    0x7F, 0x45, 0x4C, 0x46, 0x02, 0x01, 0x01, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x02, 0x00, 0x3E, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x78, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00,  // e_entry: 0x600000078
    0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // e_phoff: 64
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00,
    0x40, 0x00, 0x38, 0x00, 0x01, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00,
    // ── PT_LOAD (56 bytes) ────────────────────────────────────────────────
    0x01, 0x00, 0x00, 0x00,              // PT_LOAD
    0x05, 0x00, 0x00, 0x00,              // PF_R | PF_X
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00,  // p_vaddr: 0x600000000
    0x00, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00,
    0x78, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // p_filesz: 376
    0x78, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // p_memsz:  376
    0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // ── Code (42 bytes at file offset 120) ───────────────────────────────
    0xB8, 0x0A, 0x00, 0x00, 0x00,        // mov  eax, 10   (SYS_EXEC)
    0x48, 0xBF, 0xF8, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, // mov rdi, 0x6000000F8
    0xBE, 0x80, 0x00, 0x00, 0x00,        // mov  esi, 128  (elf_len)
    0x31, 0xD2,                           // xor  edx, edx  (caps_ptr=0)
    0x45, 0x31, 0xD2,                    // xor  r10d, r10d (caps_len=0)
    0x0F, 0x05,                           // syscall → SYS_EXEC
    0xB8, 0x00, 0x00, 0x00, 0x00,        // mov  eax, 0   (SYS_YIELD)
    0x0F, 0x05,                           // syscall → SYS_YIELD
    0xB8, 0x01, 0x00, 0x00, 0x00,        // mov  eax, 1   (SYS_TASK_EXIT)
    0x0F, 0x05,                           // syscall
    0xF4,                                 // hlt
    // ── Padding (86 bytes) — aligns embedded ELF to file offset 248 ──────
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // ── Embedded SMOKE_ELF (128 bytes at file offset 248 = VA 0x6000000F8) ─
    0x7F, 0x45, 0x4C, 0x46,              // ELF magic
    0x02, 0x01, 0x01, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x02, 0x00, 0x3E, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x78, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,  // e_entry: 0x100000078
    0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // e_phoff: 64
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00,
    0x40, 0x00, 0x38, 0x00, 0x01, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00,
    // PT_LOAD:
    0x01, 0x00, 0x00, 0x00,              // PT_LOAD
    0x05, 0x00, 0x00, 0x00,              // PF_R | PF_X
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,  // p_vaddr: 0x100000000
    0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // p_filesz: 128
    0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // p_memsz:  128
    0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // Code (8 bytes — mov eax,1; syscall; hlt):
    0xB8, 0x01, 0x00, 0x00, 0x00,        // mov  eax, 1   (SYS_TASK_EXIT)
    0x0F, 0x05,                           // syscall
    0xF4,                                 // hlt
];

/// Minimal IPC receiver task (p_vaddr=0x300000000).
///
/// Assembly (entry at file offset 120 = VA 0x300000078):
/// ```asm
/// sub  rsp, 72      ; room for recv buffer
/// mov  eax, 7       ; SYS_IPC_RECV
/// xor  edi, edi     ; a1 = handle 0 (ipc_cap)
/// mov  rsi, rsp     ; a2 = buf
/// mov  edx, 64      ; a3 = len
/// syscall
/// mov  eax, 1       ; SYS_TASK_EXIT
/// syscall
/// hlt
/// ```
pub static IPC_RECEIVER_ELF: &[u8] = &[
    // ── ELF header (64 bytes) ─────────────────────────────────────────────
    0x7F, 0x45, 0x4C, 0x46, 0x02, 0x01, 0x01, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x02, 0x00, 0x3E, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x78, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00,  // e_entry: 0x300000078
    0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00,
    0x40, 0x00, 0x38, 0x00, 0x01, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00,
    // ── PT_LOAD (56 bytes) ────────────────────────────────────────────────
    0x01, 0x00, 0x00, 0x00,
    0x05, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00,  // p_vaddr: 0x300000000
    0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00,
    0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // p_filesz: 149
    0x95, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // p_memsz:  149
    0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // ── Code (29 bytes at file offset 120) ───────────────────────────────
    0x48, 0x83, 0xEC, 0x48,              // sub  rsp, 72
    0xB8, 0x07, 0x00, 0x00, 0x00,        // mov  eax, 7   (SYS_IPC_RECV)
    0x31, 0xFF,                           // xor  edi, edi (handle = 0)
    0x48, 0x89, 0xE6,                    // mov  rsi, rsp
    0xBA, 0x40, 0x00, 0x00, 0x00,        // mov  edx, 64
    0x0F, 0x05,                           // syscall
    0xB8, 0x01, 0x00, 0x00, 0x00,        // mov  eax, 1   (SYS_TASK_EXIT)
    0x0F, 0x05,                           // syscall
    0xF4,                                 // hlt
];

/// Ring-3 mount cap-gate probe, DENY side (p_vaddr=0x600000000).
///
/// Executed with an EMPTY capability set. Calls SYS_MOUNT with fully valid
/// arguments (real path pointer into its own segment, source 0, flags 0) and
/// requires the answer to be ENOPERM (-3): the Filesystem-capability gate
/// must fire before any argument is considered, from a genuine ring-3
/// caller. On ENOPERM the task exits (kernel probe sees it reaped); on ANY
/// other result — including EINVAL (gate ordered after arg checks) or 0 (an
/// unprivileged mount succeeded!) — it spins forever and the probe's reap
/// deadline fails.
///
/// Assembly (entry at file offset 120 = VA 0x600000078):
/// ```asm
/// mov    eax, 56           ; SYS_MOUNT
/// movabs rdi, 0x6000000A2  ; -> "/mnt" (in this segment, below)
/// mov    esi, 4            ; path len
/// xor    edx, edx          ; source = MOUNT_SRC_RFS2_RAM
/// xor    r10d, r10d        ; flags = 0
/// syscall
/// cmp    rax, -3           ; ENOPERM?
/// jne    fail
/// mov    eax, 1            ; SYS_TASK_EXIT
/// syscall
/// fail: jmp fail
/// db "/mnt"
/// ```
pub static MOUNT_DENIED_ELF: &[u8] = &[
    // ── ELF header (64 bytes) ─────────────────────────────────────────────
    0x7F, 0x45, 0x4C, 0x46, 0x02, 0x01, 0x01, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x02, 0x00, 0x3E, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x78, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00,  // e_entry: 0x600000078
    0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // e_phoff: 64
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00,
    0x40, 0x00, 0x38, 0x00, 0x01, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00,
    // ── PT_LOAD (56 bytes) ────────────────────────────────────────────────
    0x01, 0x00, 0x00, 0x00,              // PT_LOAD
    0x05, 0x00, 0x00, 0x00,              // PF_R | PF_X
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00,  // p_vaddr: 0x600000000
    0x00, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00,
    0xA6, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // p_filesz: 166
    0xA6, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // p_memsz:  166
    0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // ── Code (42 bytes at file offset 120) + "/mnt" (4 bytes) ────────────
    0xB8, 0x38, 0x00, 0x00, 0x00,                    // mov  eax, 56
    0x48, 0xBF, 0xA2, 0x00, 0x00, 0x00, 0x06, 0x00, 0x00, 0x00, // movabs rdi, 0x6000000A2
    0xBE, 0x04, 0x00, 0x00, 0x00,                    // mov  esi, 4
    0x31, 0xD2,                                       // xor  edx, edx
    0x45, 0x31, 0xD2,                                 // xor  r10d, r10d
    0x0F, 0x05,                                       // syscall
    0x48, 0x83, 0xF8, 0xFD,                           // cmp  rax, -3 (ENOPERM)
    0x75, 0x07,                                       // jne  fail (+7)
    0xB8, 0x01, 0x00, 0x00, 0x00,                    // mov  eax, 1 (SYS_TASK_EXIT)
    0x0F, 0x05,                                       // syscall
    0xEB, 0xFE,                                       // fail: jmp fail
    0x2F, 0x6D, 0x6E, 0x74,                           // "/mnt"
];

/// Ring-3 mount cap-gate probe, HOLDER side (p_vaddr=0x700000000).
///
/// Executed WITH a Filesystem capability (handle 0). Calls SYS_MOUNT with
/// deliberately bad arguments (null path pointer, zero length) and requires
/// EINVAL (-4): proves the gate OPENED for the cap holder (a broken gate
/// would answer ENOPERM) and that argument validation is what rejected the
/// call — the same gate-ordering assertion as the dispatcher probe, but
/// across the real ring-3/ring-0 boundary. Exits on EINVAL; spins on
/// anything else.
///
/// Assembly (entry at file offset 120 = VA 0x700000078):
/// ```asm
/// mov  eax, 56             ; SYS_MOUNT
/// xor  edi, edi            ; path ptr = NULL
/// xor  esi, esi            ; path len = 0
/// xor  edx, edx            ; source 0
/// xor  r10d, r10d          ; flags 0
/// syscall
/// cmp  rax, -4             ; EINVAL?
/// jne  fail
/// mov  eax, 1              ; SYS_TASK_EXIT
/// syscall
/// fail: jmp fail
/// ```
pub static MOUNT_EINVAL_ELF: &[u8] = &[
    // ── ELF header (64 bytes) ─────────────────────────────────────────────
    0x7F, 0x45, 0x4C, 0x46, 0x02, 0x01, 0x01, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x02, 0x00, 0x3E, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x78, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00,  // e_entry: 0x700000078
    0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // e_phoff: 64
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00,
    0x40, 0x00, 0x38, 0x00, 0x01, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00,
    // ── PT_LOAD (56 bytes) ────────────────────────────────────────────────
    0x01, 0x00, 0x00, 0x00,              // PT_LOAD
    0x05, 0x00, 0x00, 0x00,              // PF_R | PF_X
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00,  // p_vaddr: 0x700000000
    0x00, 0x00, 0x00, 0x00, 0x07, 0x00, 0x00, 0x00,
    0x97, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // p_filesz: 151
    0x97, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // p_memsz:  151
    0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // ── Code (31 bytes at file offset 120) ────────────────────────────────
    0xB8, 0x38, 0x00, 0x00, 0x00,                    // mov  eax, 56
    0x31, 0xFF,                                       // xor  edi, edi
    0x31, 0xF6,                                       // xor  esi, esi
    0x31, 0xD2,                                       // xor  edx, edx
    0x45, 0x31, 0xD2,                                 // xor  r10d, r10d
    0x0F, 0x05,                                       // syscall
    0x48, 0x83, 0xF8, 0xFC,                           // cmp  rax, -4 (EINVAL)
    0x75, 0x07,                                       // jne  fail (+7)
    0xB8, 0x01, 0x00, 0x00, 0x00,                    // mov  eax, 1 (SYS_TASK_EXIT)
    0x0F, 0x05,                                       // syscall
    0xEB, 0xFE,                                       // fail: jmp fail
];

/// Ring-3 argv probe (p_vaddr=0x800000000).
///
/// Executed with argv = `["probe", "argv-ok!"]`. Reads the initial stack
/// frame the kernel wrote (`write_initial_stack_frame`): requires argc == 2,
/// argv[0] == "probe\0", argv[1] == "argv-ok!\0" — pointers followed and
/// every byte compared, from genuine ring 3. On match it SYS_LOGs a report
/// line (so the readback is visible on serial) and exits; on ANY mismatch it
/// spins forever and the probe's reap deadline fails.
///
/// Assembly (entry at file offset 120 = VA 0x800000078):
/// ```asm
/// mov    rax, [rsp]          ; argc
/// cmp    rax, 2
/// jne    fail
/// mov    rdi, [rsp+8]        ; argv[0]
/// cmp    dword [rdi], 'prob'
/// jne    fail
/// cmp    word [rdi+4], 'e\0'
/// jne    fail
/// mov    rdi, [rsp+16]       ; argv[1]
/// mov    rax, [rdi]
/// movabs rbx, 'argv-ok!'
/// cmp    rax, rbx
/// jne    fail
/// cmp    byte [rdi+8], 0
/// jne    fail
/// mov    eax, 11             ; SYS_LOG
/// movabs rdi, 0x8000000D3    ; -> msg (in this segment, below)
/// mov    esi, 57             ; msg len
/// syscall
/// mov    eax, 1              ; SYS_TASK_EXIT
/// syscall
/// fail: jmp fail
/// db "[argv-echo] ring-3 argc=2 argv[0]=probe argv[1]=argv-ok!\n"
/// ```
pub static ARGV_ECHO_ELF: &[u8] = &[
    // ── ELF header (64 bytes) ─────────────────────────────────────────────
    0x7F, 0x45, 0x4C, 0x46, 0x02, 0x01, 0x01, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x02, 0x00, 0x3E, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x78, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00,  // e_entry: 0x800000078
    0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // e_phoff: 64
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00,
    0x40, 0x00, 0x38, 0x00, 0x01, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00,
    // ── PT_LOAD (56 bytes) ────────────────────────────────────────────────
    0x01, 0x00, 0x00, 0x00,              // PT_LOAD
    0x05, 0x00, 0x00, 0x00,              // PF_R | PF_X
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00,  // p_vaddr: 0x800000000
    0x00, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00,
    0x0C, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // p_filesz: 268
    0x0C, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  // p_memsz:  268
    0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    // ── Code (91 bytes at file offset 120) ────────────────────────────────
    0x48, 0x8B, 0x04, 0x24,                           // mov  rax, [rsp]
    0x48, 0x83, 0xF8, 0x02,                           // cmp  rax, 2
    0x75, 0x4F,                                       // jne  fail (@89)
    0x48, 0x8B, 0x7C, 0x24, 0x08,                    // mov  rdi, [rsp+8]
    0x81, 0x3F, 0x70, 0x72, 0x6F, 0x62,              // cmp  dword [rdi], "prob"
    0x75, 0x42,                                       // jne  fail
    0x66, 0x81, 0x7F, 0x04, 0x65, 0x00,              // cmp  word [rdi+4], "e\0"
    0x75, 0x3A,                                       // jne  fail
    0x48, 0x8B, 0x7C, 0x24, 0x10,                    // mov  rdi, [rsp+16]
    0x48, 0x8B, 0x07,                                 // mov  rax, [rdi]
    0x48, 0xBB, 0x61, 0x72, 0x67, 0x76, 0x2D, 0x6F, 0x6B, 0x21, // movabs rbx, "argv-ok!"
    0x48, 0x39, 0xD8,                                 // cmp  rax, rbx
    0x75, 0x23,                                       // jne  fail
    0x80, 0x7F, 0x08, 0x00,                           // cmp  byte [rdi+8], 0
    0x75, 0x1D,                                       // jne  fail
    0xB8, 0x0B, 0x00, 0x00, 0x00,                    // mov  eax, 11 (SYS_LOG)
    0x48, 0xBF, 0xD3, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00, // movabs rdi, 0x8000000D3
    0xBE, 0x39, 0x00, 0x00, 0x00,                    // mov  esi, 57
    0x0F, 0x05,                                       // syscall
    0xB8, 0x01, 0x00, 0x00, 0x00,                    // mov  eax, 1 (SYS_TASK_EXIT)
    0x0F, 0x05,                                       // syscall
    0xEB, 0xFE,                                       // fail: jmp fail
    // ── Message (57 bytes at file offset 211 = VA 0x8000000D3) ───────────
    b'[', b'a', b'r', b'g', b'v', b'-', b'e', b'c', b'h', b'o', b']', b' ',
    b'r', b'i', b'n', b'g', b'-', b'3', b' ',
    b'a', b'r', b'g', b'c', b'=', b'2', b' ',
    b'a', b'r', b'g', b'v', b'[', b'0', b']', b'=', b'p', b'r', b'o', b'b', b'e', b' ',
    b'a', b'r', b'g', b'v', b'[', b'1', b']', b'=', b'a', b'r', b'g', b'v', b'-', b'o', b'k', b'!',
    b'\n',
];
