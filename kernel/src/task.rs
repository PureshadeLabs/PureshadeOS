/// Cooperative task scheduler — kernel tasks and context switching.
///
/// ## Context switch mechanics
///
/// `switch_context(from, to)` is a small assembly routine (AT&T syntax).
/// Calling convention: SysV AMD64 — `rdi = from`, `rsi = to`.
///
/// On entry the `call` has already pushed the return address.  The routine
/// pushes `{rbp, rbx, r12, r13, r14, r15}`, saves `rsp → from->rsp`, loads
/// `to->rsp`, pops the six registers in reverse order, and `ret`s — which
/// pops the next task's `rip` from the top of its stack.
///
/// ## Initial stack frame for new tasks
///
/// A new task's stack is pre-populated so the first restore looks identical
/// to every subsequent one.  After `switch_context` runs 6 pops (48 bytes)
/// and `ret` (8 bytes):
///
///   `rsp_entry = initial_rsp + 56`
///
/// SysV ABI requires `rsp_entry % 16 == 8`
/// → `initial_rsp % 16 == 0`  (since 56 % 16 == 8).
///
/// `KERNEL_STACK_SIZE` is a multiple of 16, and our heap allocator guarantees
/// 16-byte alignment, so `stack_top` is always 16-byte aligned.
/// `initial_rsp = stack_top − 64` (64 % 16 == 0) satisfies the constraint.
///
/// ```text
/// [initial_rsp +  0]  r15 = 0           ← context.rsp points here
/// [initial_rsp +  8]  r14 = 0
/// [initial_rsp + 16]  r13 = 0
/// [initial_rsp + 24]  r12 = 0
/// [initial_rsp + 32]  rbx = 0
/// [initial_rsp + 40]  rbp = 0
/// [initial_rsp + 48]  entry address     ← popped by `ret` → rip
/// [initial_rsp + 56]  (8-byte padding)
/// ```

extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use core::arch::global_asm;
use core::cell::UnsafeCell;

use crate::vmm::VirtAddr;

// ── Assembly: context switch ──────────────────────────────────────────────────

global_asm!(r#"
.global switch_context
.type   switch_context, @function
switch_context:
    pushq  %rbp
    pushq  %rbx
    pushq  %r12
    pushq  %r13
    pushq  %r14
    pushq  %r15
    movq   %rsp, (%rdi)
    movq   (%rsi), %rsp
    popq   %r15
    popq   %r14
    popq   %r13
    popq   %r12
    popq   %rbx
    popq   %rbp
    retq
"#, options(att_syntax));

unsafe extern "C" {
    fn switch_context(from: *mut TaskContext, to: *const TaskContext);
}

// ── Types ─────────────────────────────────────────────────────────────────────

pub type TaskId = u64;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskState {
    Running,
    Ready,
    /// Waiting on an event (e.g. IPC). Will not be scheduled until wake_task is called.
    Blocked,
    /// Exited. Will be freed on the next yield_task sweep.
    Dead,
}

/// Per-task saved scheduler state.  Only `rsp` is stored; all other
/// callee-saved registers live on the task's kernel stack between switches.
#[repr(C)]
pub struct TaskContext {
    pub rsp: u64,
}

pub const KERNEL_STACK_SIZE: usize = 64 * 1024; // 64 KiB per task

/// Magic value written at the bottom of every kernel stack (the first usable
/// word above the guard page).  Checked at every yield / exit.  A corrupted
/// canary means the stack overflowed far enough to smash the canary word
/// without hitting the guard page — a shallow overflow the #PF wouldn't catch.
const STACK_CANARY: u64 = 0x5AFE_C0DE_DEAD_BEEF;

pub struct Task {
    pub id:        TaskId,
    pub state:     TaskState,
    pub context:   TaskContext,
    /// Scheduling priority: 0=low, 1=normal (default), 2=high.
    pub priority:  u8,
    /// Short null-terminated task name (max 15 chars + NUL).
    pub name:      [u8; 16],
    /// Per-task capability table.
    pub cap_table: crate::cap::CapabilityTable,
    /// Heap allocation backing this task's kernel stack.  Must not be resized
    /// after spawn; `context.rsp` points into this buffer.
    _stack: Vec<u8>,
    /// Top of the usable kernel stack (highest address).  Set to 0 for the
    /// bootstrap task, which uses the existing boot stack.
    _stack_top: u64,
    /// Guard page below the kernel stack: (virtual address, physical frame).
    /// The physical frame is valid but the PTE is cleared (not-present) so that
    /// stack overflow takes a clean #PF.  Remapped to KERNEL_RW in sweep_dead
    /// before the Vec is dropped so that the heap free-list can write safely.
    _guard_page: Option<(u64, crate::pmm::PhysAddr)>,
    /// Ring-3 entry point set by `spawn_userspace_task`; None for kernel tasks.
    pub entry_point:    Option<VirtAddr>,
    /// User-mode stack top set by `spawn_userspace_task`; None for kernel tasks.
    pub user_stack_top: Option<VirtAddr>,
    /// Physical address of this task's PML4, or `None` for kernel tasks that
    /// share the global kernel page table.
    pub page_table: Option<u64>,
    /// Sorted list of mapped user VA ranges `[va_start, va_end)`.
    /// Only populated for user tasks (page_table is Some).
    /// Used by SYS_MMAP/SYS_MUNMAP to detect double-map and invalid unmap.
    pub vma_list: Vec<(u64, u64)>,
    /// Current program break (top of the contiguous heap region).
    /// 0 means uninitialized; initialized to USERSPACE_HEAP_BASE on first SYS_BRK call.
    pub brk: u64,
}

fn make_name(s: &str) -> [u8; 16] {
    let mut n = [0u8; 16];
    let bytes = s.as_bytes();
    let len   = bytes.len().min(15);
    n[..len].copy_from_slice(&bytes[..len]);
    n
}

// ── Scheduler ─────────────────────────────────────────────────────────────────

struct Scheduler {
    /// All tasks.  Stored as `Box<Task>` so each Task's heap address is stable
    /// even if the Vec reallocates.
    tasks:   Vec<Box<Task>>,
    current: usize,
    next_id: TaskId,
}

// Use UnsafeCell + a newtype to avoid `static mut` (which Rust 2024 makes
// harder to use safely with the `static_mut_refs` lint).
struct GlobalSched(UnsafeCell<Option<Scheduler>>);
// SAFETY: single-threaded kernel; no concurrent accesses.
unsafe impl Sync for GlobalSched {}

static SCHED: GlobalSched = GlobalSched(UnsafeCell::new(None));

// ── Sleep queue ───────────────────────────────────────────────────────────────
//
// Each entry is (task_id, wake_tick): the task is Blocked until the APIC tick
// counter reaches wake_tick, at which point wake_sleepers wakes it.

const MAX_SLEEPERS: usize = 64;
static SLEEP_QUEUE: crate::serial::SpinLock<[Option<(TaskId, u64)>; MAX_SLEEPERS]> =
    crate::serial::SpinLock::new([None; MAX_SLEEPERS]);

// ── Task wait table ───────────────────────────────────────────────────────────
//
// Each entry is (waiter_id, target_id): the waiter is Blocked until the target
// calls task_exit (or is killed).  task_exit and kill_task scan this table and
// wake matching waiters.  32 slots is enough for any realistic workload.

const MAX_WAITERS: usize = 32;
static TASK_WAITERS: crate::serial::SpinLock<[Option<(TaskId, TaskId)>; MAX_WAITERS]> =
    crate::serial::SpinLock::new([None; MAX_WAITERS]);

/// Get a `&mut Scheduler`, panicking if `init()` has not been called.
#[inline]
unsafe fn get_sched() -> &'static mut Scheduler {
    unsafe { (*SCHED.0.get()).as_mut().expect("task: scheduler not initialised") }
}

/// Find the next Ready task to run, starting from `start_idx` (wrapping).
/// Scans in priority order (high→normal→low) to ensure higher-priority tasks
/// preempt lower ones; within a priority tier the round-robin start index
/// preserves fairness.  Returns `None` if no Ready task exists.
fn find_next_ready(sched: &Scheduler, start_idx: usize) -> Option<usize> {
    let n = sched.tasks.len();
    if n == 0 { return None; }
    for priority in (0u8..=2).rev() {
        let mut i = start_idx % n;
        let mut checked = 0;
        while checked < n {
            if sched.tasks[i].state == TaskState::Ready && sched.tasks[i].priority == priority {
                return Some(i);
            }
            i = (i + 1) % n;
            checked += 1;
        }
    }
    None
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Initialise the scheduler.  Creates a bootstrap task for the running
/// `kmain` thread.  Must be called before `spawn_kernel_task` / `yield_task`.
pub fn init() {
    let bootstrap = Box::new(Task {
        id:             0,
        state:          TaskState::Running,
        context:        TaskContext { rsp: 0 }, // filled by the first switch_context
        priority:       1,
        name:           make_name("kmain"),
        cap_table:      crate::cap::CapabilityTable::new(),
        _stack:         Vec::new(),             // kmain uses the existing boot stack
        _stack_top:     0,                      // boot stack; ring-3 entry not supported
        _guard_page:    None,
        entry_point:    None,
        user_stack_top: None,
        page_table:     None,
        vma_list:       Vec::new(),
        brk:            0,
    });

    let mut tasks = Vec::new();
    tasks.push(bootstrap);

    unsafe {
        *SCHED.0.get() = Some(Scheduler { tasks, current: 0, next_id: 1 });
    }
}

/// Spawn a new kernel-mode task beginning execution at `entry`.
///
/// `entry` must never return; call `task_exit()` when done.
pub fn spawn_kernel_task(entry: fn() -> !) -> TaskId {
    const PAGE: usize = 4096;

    // Allocate KERNEL_STACK_SIZE + 2×PAGE bytes.  The extra page provides
    // alignment headroom: the heap gives ≥16-byte alignment, so rounding up
    // to the next page boundary wastes at most PAGE−16 = 4080 bytes, and we
    // still have a full KERNEL_STACK_SIZE of usable stack above the guard page.
    let mut stack = Vec::with_capacity(KERNEL_STACK_SIZE + 2 * PAGE);
    stack.resize(KERNEL_STACK_SIZE + 2 * PAGE, 0u8);

    let base     = stack.as_ptr() as usize;
    // Round up base to the nearest page boundary — this is the guard page VA.
    let guard_va = (base + PAGE - 1) & !(PAGE - 1);
    // Usable kernel stack lives immediately above the guard page.
    let stack_top   = guard_va + PAGE + KERNEL_STACK_SIZE;
    let initial_rsp = stack_top - 64; // 64 % 16 == 0 → 16-byte aligned

    // Capture the physical frame backing the guard page so we can restore
    // the mapping in sweep_dead before the Vec is dropped.
    let guard_phys = crate::vmm::query_page(crate::vmm::VirtAddr(guard_va as u64))
        .expect("spawn_kernel_task: guard page not mapped");
    // Clear the PTE — the physical frame is NOT freed.
    crate::vmm::unmap_page(crate::vmm::VirtAddr(guard_va as u64));

    // Write the stack canary at the first usable word above the guard page.
    // The stack grows down, so this is the last word consumed before the guard
    // page triggers a #PF.  The canary catches shallower overflows first.
    unsafe { ((guard_va + PAGE) as *mut u64).write(STACK_CANARY); }

    // Layout: 8 × u64 slots from initial_rsp upward (see module doc).
    // Vec is zeroed so r15..rbp are already 0; only rip and padding need writing.
    unsafe {
        let p = initial_rsp as *mut u64;
        p.add(6).write(entry as u64);   // rip — popped by `ret`
        // p.add(7) is padding, already 0
    }

    let sched = unsafe { get_sched() };
    let id    = sched.next_id;
    sched.next_id += 1;

    sched.tasks.push(Box::new(Task {
        id,
        state:          TaskState::Ready,
        context:        TaskContext { rsp: initial_rsp as u64 },
        priority:       1,
        name:           make_name("kthread"),
        cap_table:      crate::cap::CapabilityTable::new(),
        _stack:         stack,
        _stack_top:     stack_top as u64,
        _guard_page:    Some((guard_va as u64, guard_phys)),
        entry_point:    None,
        user_stack_top: None,
        page_table:     None,
        vma_list:       Vec::new(),
        brk:            0,
    }));

    id
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Panic if `task`'s stack canary word has been overwritten.
///
/// No-op for the bootstrap task (no guard page).  Called at every
/// `yield_task` / `block_and_yield` / `task_exit` and in `sweep_dead`.
#[inline]
fn check_stack_canary(task: &Task) {
    if let Some((guard_va, _)) = task._guard_page {
        let canary = unsafe { *((guard_va + 4096) as *const u64) };
        if canary != STACK_CANARY {
            panic!(
                "kernel stack overflow: task {} canary at {:#x} is {:#x} (expected {:#x})",
                task.id,
                guard_va + 4096,
                canary,
                STACK_CANARY,
            );
        }
    }
}

/// Load `cr3` with the given page table, or the kernel PML4 when `None`.
///
/// Also updates `SYSCALL_KERN_RSP` and `tss::RSP0` to `stack_top` when
/// non-zero (i.e. the incoming task is a userspace task).  This must happen
/// on every context switch so that when the incoming task next executes a
/// `syscall` instruction the kernel stack is correctly identified regardless
/// of what other tasks called `enter_userspace` in the meantime.
///
/// Must be called immediately before `switch_context` so that the incoming
/// task executes with its own address space from the very first instruction.
#[inline]
fn switch_cr3(page_table: Option<u64>, stack_top: u64) {
    let cr3 = page_table.unwrap_or_else(|| crate::vmm::kernel_pml4().as_u64());
    unsafe {
        core::arch::asm!(
            "mov cr3, {cr3}",
            cr3 = in(reg) cr3,
            options(nostack),
        );
    }
    if stack_top != 0 {
        crate::tss::set_rsp0(stack_top);
        unsafe { crate::syscall::SYSCALL_KERN_RSP = stack_top; }
    }
}

/// Remove all Dead tasks from the queue, adjusting `current` accordingly.
/// Must be called when the current task is Running (never Dead).
fn sweep_dead(sched: &mut Scheduler) {
    // Save RFLAGS and disable interrupts for the entire sweep.  The APIC
    // timer ISR calls yield_task → sweep_dead; if it fires while we are
    // inside free_user_page_table (which iterates thousands of frames), the
    // re-entrant sweep sees the same dead task with page_table still set and
    // calls free_user_page_table a second time → double free.  CLI prevents
    // this; POPFQ restores the original IF state when we are done.
    let rflags: u64;
    unsafe {
        core::arch::asm!(
            "pushfq",
            "pop {f}",
            "cli",
            f = out(reg) rflags,
            options(nostack),
        );
    }

    let mut i = 0;
    while i < sched.tasks.len() {
        if sched.tasks[i].state == TaskState::Dead {
            // Last-chance canary check before the stack is freed.  Catches
            // overflows in tasks that exit without ever calling yield_task
            // (e.g. a task that overflows and then falls through to task_exit
            // without any intervening yield).
            check_stack_canary(&sched.tasks[i]);
            // Free the per-process page table (and all mapped user frames)
            // before dropping the task.  The current CR3 is the running
            // task's page table, not this dead task's, so it is safe to free.
            if let Some(pml4) = sched.tasks[i].page_table {
                crate::vmm::free_user_page_table(crate::pmm::PhysAddr(pml4));
            }
            // Restore the guard page PTE before dropping the kernel stack Vec.
            // The physical frame was never freed — just unmapped — so we can
            // re-establish the mapping so the heap free-list can write safely.
            if let Some((guard_va, guard_phys)) = sched.tasks[i]._guard_page {
                crate::vmm::map_page(
                    crate::vmm::VirtAddr(guard_va),
                    guard_phys,
                    crate::vmm::PageFlags::KERNEL_RW,
                );
            }
            sched.tasks.remove(i);
            if i < sched.current {
                sched.current -= 1;
            }
        } else {
            i += 1;
        }
    }

    unsafe {
        core::arch::asm!(
            "push {f}",
            "popfq",
            f = in(reg) rflags,
            options(nostack),
        );
    }
}

// ── Public API (continued) ────────────────────────────────────────────────────

/// Cooperative yield: save the current task and switch to the next ready task
/// in round-robin order.  Returns when this task is switched back to.
/// No-op if there are no other ready tasks.  Frees any Dead tasks first.
pub fn yield_task() {
    let sched = unsafe { get_sched() };
    check_stack_canary(&sched.tasks[sched.current]);
    sweep_dead(sched);

    let n       = sched.tasks.len();
    let current = sched.current;

    if n <= 1 { return; }

    let next = match find_next_ready(sched, (current + 1) % n) {
        Some(idx) => idx,
        None      => return,
    };
    if next == current { return; }

    // Capture raw pointers to both TaskContexts *before* mutating state.
    // Box<Task> is heap-allocated; the Task address is stable across Vec ops.
    let from_ctx: *mut   TaskContext = &mut sched.tasks[current].context;
    let to_ctx:   *const TaskContext = &    sched.tasks[next].context;

    sched.tasks[current].state = TaskState::Ready;
    sched.tasks[next].state    = TaskState::Running;
    sched.current              = next;

    // Disable interrupts before switch_cr3 so the APIC timer cannot fire
    // between switch_cr3 (which sets RSP0 = next's stack) and switch_context.
    // If it fired there the ISR would run on next's stack; its yield_task would
    // overwrite next.context.rsp (making to_ctx stale) and sweep_dead could
    // free current's stack (making from_ctx dangling). The sti below re-enables
    // interrupts when this task is next resumed.
    unsafe { core::arch::asm!("cli", options(nostack)) };
    switch_cr3(sched.tasks[next].page_table, sched.tasks[next]._stack_top);
    // After this call returns we are back in `current`'s context.
    unsafe { switch_context(from_ctx, to_ctx); }
    // Re-enable interrupts: covers both the cooperative-yield case (IF was 1
    // before cli above) and the timer-ISR case (IF was already 0; sti here
    // fires before iretq restores the original RFLAGS, which is harmless).
    unsafe { core::arch::asm!("sti", options(nostack)) };
}

/// Terminate the current task.  Marks it Dead, switches to the next ready task,
/// and never returns.  If no other task is ready, halts the CPU.
pub fn task_exit() -> ! {
    let sched = unsafe { get_sched() };
    check_stack_canary(&sched.tasks[sched.current]);
    let current = sched.current;
    let dying_id = sched.tasks[current].id;

    sched.tasks[current].state = TaskState::Dead;

    // Disable interrupts now so the APIC timer cannot fire between here and
    // switch_context.  SpinLock::lock saves RFLAGS and does cli; its drop does
    // popfq — which restores the RFLAGS we save here (IF=0), keeping interrupts
    // off through the switch.  Without this cli the lock-drop re-enables
    // interrupts, yield_task runs sweep_dead, frees the dying stack, and
    // switch_context returns into freed memory.
    unsafe { core::arch::asm!("cli", options(nostack)) };

    // Wake any tasks blocked in wait_for_task on this task.
    {
        let mut waiters = TASK_WAITERS.lock();
        for slot in waiters.iter_mut() {
            if let Some((waiter_id, target_id)) = *slot {
                if target_id == dying_id {
                    if let Some(t) = sched.tasks.iter_mut().find(|t| t.id == waiter_id) {
                        if t.state == TaskState::Blocked {
                            t.state = TaskState::Ready;
                        }
                    }
                    *slot = None;
                }
            }
        }
    }

    let n = sched.tasks.len();
    let next = match find_next_ready(sched, (current + 1) % n) {
        Some(idx) => idx,
        None      => loop { unsafe { core::arch::asm!("hlt") }; },
    };

    // from_ctx is written by switch_context but never read again — the Dead
    // task's stack stays valid until sweep_dead drops it on the next yield.
    let from_ctx: *mut   TaskContext = &mut sched.tasks[current].context;
    let to_ctx:   *const TaskContext = &    sched.tasks[next].context;

    sched.tasks[next].state = TaskState::Running;
    sched.current           = next;

    // Disable interrupts so the APIC cannot fire between switch_cr3 and
    // switch_context (same race as yield_task — see comment there). Interrupts
    // stay disabled until the new task executes sti in its yield_task/
    // block_and_yield continuation, or until iretq restores RFLAGS on the
    // ISR path. task_exit never returns so no matching sti is needed here.
    unsafe { core::arch::asm!("cli", options(nostack)) };
    switch_cr3(sched.tasks[next].page_table, sched.tasks[next]._stack_top);
    unsafe { switch_context(from_ctx, to_ctx); }

    unreachable!("task_exit: returned from switch_context")
}

/// Replace the bootstrap task's (id=0) capability table.
///
/// Called by `kmain` just before exec'ing lythd so that `spawn_userspace_task`
/// can inherit the root capabilities from the bootstrap task.
pub fn set_bootstrap_cap_table(table: crate::cap::CapabilityTable) {
    let sched = unsafe { get_sched() };
    for task in sched.tasks.iter_mut() {
        if task.id == 0 {
            task.cap_table = table;
            return;
        }
    }
    panic!("set_bootstrap_cap_table: bootstrap task not found");
}

/// Return `true` if a task with `id` is still in the scheduler queue.
///
/// Returns `false` once the task has been reaped by `sweep_dead`.  Used by
/// the Step 14 smoke test to wait for userspace tasks to complete.
pub fn task_exists(id: TaskId) -> bool {
    let sched = unsafe { get_sched() };
    sched.tasks.iter().any(|t| t.id == id)
}

/// Return the raw task status for `SYS_TASK_STATUS`:
/// - `0` — task not found or already reaped (Dead)
/// - `1` — task is Running or Ready
/// - `2` — task is Blocked
pub fn task_status_raw(id: TaskId) -> u64 {
    let sched = unsafe { get_sched() };
    match sched.tasks.iter().find(|t| t.id == id) {
        None    => 0,
        Some(t) => match t.state {
            TaskState::Dead    => 0,
            TaskState::Running => 1,
            TaskState::Ready   => 2,
            TaskState::Blocked => 3,
        },
    }
}

/// Iterate live (non-Dead) tasks, calling `f(index, id, state_raw, kind)` for each.
/// `state_raw`: canonical encoding — 1=running, 2=ready, 3=blocked.
/// `kind`: 0=kernel, 1=userspace.
/// Returns the number of live tasks visited.
pub fn for_each_task<F>(mut f: F) -> usize
where
    F: FnMut(usize, TaskId, u64, u8),
{
    let sched = unsafe { get_sched() };
    let mut idx = 0;
    for t in sched.tasks.iter() {
        if t.state == TaskState::Dead { continue; }
        let state_raw = match t.state {
            TaskState::Running => 1,
            TaskState::Ready   => 2,
            TaskState::Blocked => 3,
            TaskState::Dead    => continue,
        };
        let kind: u8 = if t.page_table.is_some() { 1 } else { 0 };
        f(idx, t.id, state_raw, kind);
        idx += 1;
    }
    idx
}

/// Kill a task by ID.
///
/// Marks the target task Dead so the scheduler reaps it on the next sweep.
/// Returns `false` if the task is not found, already dead, or is task 0 (lythd).
/// Killing the current task is also rejected (use `task_exit` instead).
pub fn kill_task(id: TaskId) -> bool {
    // Refuse to kill task 0 (lythd / bootstrap) or ourselves.
    if id == 0 { return false; }
    let sched = unsafe { get_sched() };
    let current_id = sched.tasks[sched.current].id;
    if id == current_id { return false; }

    let killed = if let Some(t) = sched.tasks.iter_mut().find(|t| t.id == id && t.state != TaskState::Dead) {
        t.state = TaskState::Dead;
        true
    } else {
        false
    };

    if killed {
        let mut waiters = TASK_WAITERS.lock();
        for slot in waiters.iter_mut() {
            if let Some((waiter_id, target_id)) = *slot {
                if target_id == id {
                    if let Some(t) = sched.tasks.iter_mut().find(|t| t.id == waiter_id) {
                        if t.state == TaskState::Blocked { t.state = TaskState::Ready; }
                    }
                    *slot = None;
                }
            }
        }
    }
    killed
}

/// Block the current task until task `target_id` exits.
///
/// Returns immediately if `target_id` is not found or already Dead.
/// Used by `SYS_TASK_WAIT` to let a parent block on a child without polling.
pub fn wait_for_task(target_id: TaskId) {
    {
        let mut waiters = TASK_WAITERS.lock();
        // Re-check status under the lock: target_id cannot exit between this
        // check and registration because task_exit also takes TASK_WAITERS.
        if task_status_raw(target_id) == 0 { return; }
        let current_id = current_task_id();
        for slot in waiters.iter_mut() {
            if slot.is_none() {
                *slot = Some((current_id, target_id));
                break;
            }
        }
    }
    block_and_yield();
}

/// Return the ID of the currently running task.
pub fn current_task_id() -> TaskId {
    let sched = unsafe { get_sched() };
    sched.tasks[sched.current].id
}

/// Return the name of the currently running task as a `&str` (valid for the call duration).
pub fn current_task_name() -> &'static str {
    let sched = unsafe { get_sched() };
    let name  = &sched.tasks[sched.current].name;
    let len   = name.iter().position(|&b| b == 0).unwrap_or(16);
    core::str::from_utf8(&name[..len]).unwrap_or("?")
}

/// Iterate live tasks with extended info for SYS_PS.
/// Calls `f(index, id, state_raw, kind, priority, name_buf)` for each non-Dead task.
pub fn for_each_task_ps<F>(mut f: F) -> usize
where
    F: FnMut(usize, TaskId, u64, u8, u8, &[u8; 16]),
{
    let sched = unsafe { get_sched() };
    let mut idx = 0;
    for t in sched.tasks.iter() {
        if t.state == TaskState::Dead { continue; }
        let state_raw = match t.state {
            TaskState::Running => 1,
            TaskState::Ready   => 2,
            TaskState::Blocked => 3,
            TaskState::Dead    => continue,
        };
        let kind: u8  = if t.page_table.is_some() { 1 } else { 0 };
        f(idx, t.id, state_raw, kind, t.priority, &t.name);
        idx += 1;
    }
    idx
}

/// Spawn a new userspace task.
///
/// Creates a kernel stack with a `trampoline` entry point (typically
/// `elf::exec_trampoline`), stores `entry` and `stack_top` in the task so the
/// trampoline can read them, and inherits each capability in `caps` from the
/// current task's table into the new task's table.
///
/// The task is enqueued as Ready but does not run until the caller yields.
pub fn spawn_userspace_task(
    entry:       VirtAddr,
    stack_top:   VirtAddr,
    caps:        &[crate::cap::CapHandle],
    trampoline:  fn() -> !,
    page_table:  u64,
    name:        &str,
) -> TaskId {
    const PAGE: usize = 4096;

    let mut stack = Vec::with_capacity(KERNEL_STACK_SIZE + 2 * PAGE);
    stack.resize(KERNEL_STACK_SIZE + 2 * PAGE, 0u8);

    let base        = stack.as_ptr() as usize;
    let guard_va    = (base + PAGE - 1) & !(PAGE - 1);
    let kstack_top  = guard_va + PAGE + KERNEL_STACK_SIZE;
    let initial_rsp = kstack_top - 64;

    let guard_phys = crate::vmm::query_page(crate::vmm::VirtAddr(guard_va as u64))
        .expect("spawn_userspace_task: guard page not mapped");
    crate::vmm::unmap_page(crate::vmm::VirtAddr(guard_va as u64));

    // Stack canary at the first usable word above the guard page.
    unsafe { ((guard_va + PAGE) as *mut u64).write(STACK_CANARY); }

    unsafe {
        let p = initial_rsp as *mut u64;
        p.add(0).write(0);               // r15
        p.add(1).write(0);               // r14
        p.add(2).write(0);               // r13
        p.add(3).write(0);               // r12
        p.add(4).write(0);               // rbx
        p.add(5).write(0);               // rbp
        p.add(6).write(trampoline as u64); // rip
        p.add(7).write(0);               // padding
    }

    let sched = unsafe { get_sched() };
    let id    = sched.next_id;
    sched.next_id += 1;

    // Inherit capabilities from the current (spawning) task.
    let mut cap_table = crate::cap::CapabilityTable::new();
    {
        let caller_ptr: *const crate::cap::CapabilityTable =
            &sched.tasks[sched.current].cap_table;
        let caller = unsafe { &*caller_ptr };
        for &handle in caps {
            let _ = crate::cap::cap_inherit(caller, handle, &mut cap_table);
        }
    }

    sched.tasks.push(Box::new(Task {
        id,
        state:          TaskState::Ready,
        context:        TaskContext { rsp: initial_rsp as u64 },
        priority:       1,
        name:           make_name(name),
        cap_table,
        _stack:         stack,
        _stack_top:     kstack_top as u64,
        _guard_page:    Some((guard_va as u64, guard_phys)),
        entry_point:    Some(entry),
        user_stack_top: Some(stack_top),
        page_table:     Some(page_table),
        vma_list:       Vec::new(),
        brk:            0,
    }));

    id
}

/// Return the ring-3 entry point and user stack top for the current task.
///
/// Called by `exec_trampoline` immediately before entering ring-3.
/// Panics if the current task has no stored userspace entry (i.e. it is a
/// kernel-only task).
pub fn current_entry_and_stack() -> (VirtAddr, VirtAddr) {
    let sched = unsafe { get_sched() };
    let task  = &sched.tasks[sched.current];
    (
        task.entry_point   .expect("current_entry_and_stack: not a userspace task"),
        task.user_stack_top.expect("current_entry_and_stack: not a userspace task"),
    )
}

/// Return the physical address of the current task's PML4, or `None` if it
/// is a kernel task sharing the global page table.
pub fn current_page_table() -> Option<u64> {
    let sched = unsafe { get_sched() };
    sched.tasks[sched.current].page_table
}

/// Return a raw pointer to the capability table of the task with the given ID,
/// or null if no such task exists.
///
/// # Safety
/// The caller must ensure no other mutable reference to the same table exists.
/// In the single-threaded kernel this is trivially satisfied between syscalls.
pub fn cap_table_ptr(id: TaskId) -> *mut crate::cap::CapabilityTable {
    let sched = unsafe { get_sched() };
    for task in sched.tasks.iter_mut() {
        if task.id == id {
            return &mut task.cap_table;
        }
    }
    core::ptr::null_mut()
}

/// Record a new page-aligned VMA `[va, va+4096)` for the current task.
///
/// Returns `true` on success; `false` if the range overlaps an existing
/// mapping (double-map attempt).  The list is kept sorted by `va_start`.
pub fn vma_insert(va: u64) -> bool {
    const PAGE: u64 = 0x1000;
    let end = va + PAGE;
    let sched = unsafe { get_sched() };
    let vmas = &mut sched.tasks[sched.current].vma_list;
    let pos = vmas.partition_point(|&(s, _)| s < va);
    // Check overlap with the preceding range.
    if pos > 0 && vmas[pos - 1].1 > va { return false; }
    // Check overlap with the following range.
    if pos < vmas.len() && vmas[pos].0 < end { return false; }
    vmas.insert(pos, (va, end));
    true
}

/// Remove the VMA whose `va_start == va` from the current task's list.
///
/// Returns `true` on success; `false` if no such mapping exists (invalid
/// unmap attempt).
pub fn vma_remove(va: u64) -> bool {
    let sched = unsafe { get_sched() };
    let vmas = &mut sched.tasks[sched.current].vma_list;
    match vmas.binary_search_by_key(&va, |&(s, _)| s) {
        Ok(pos) => { vmas.remove(pos); true }
        Err(_)  => false,
    }
}

/// Block the current task until the APIC tick counter reaches `wake_tick`.
///
/// Records (task_id, wake_tick) in SLEEP_QUEUE, then calls block_and_yield.
/// The timer ISR calls wake_sleepers on each tick to check for expired entries.
pub fn sleep_until(wake_tick: u64) {
    let id = current_task_id();
    {
        let mut q = SLEEP_QUEUE.lock();
        for slot in q.iter_mut() {
            if slot.is_none() {
                *slot = Some((id, wake_tick));
                break;
            }
        }
    }
    block_and_yield();
}

/// Wake any tasks in SLEEP_QUEUE whose wake_tick <= current_tick.
///
/// Called from the APIC timer ISR on every tick.  Collects expired entries
/// without holding the lock while calling wake_task.
pub fn wake_sleepers(current_tick: u64) {
    let mut to_wake = [0u64; MAX_SLEEPERS];
    let mut n = 0usize;
    {
        let mut q = SLEEP_QUEUE.lock();
        for slot in q.iter_mut() {
            if let Some((id, wake_tick)) = *slot {
                if current_tick >= wake_tick {
                    to_wake[n] = id;
                    n += 1;
                    *slot = None;
                }
            }
        }
    }
    for &id in &to_wake[..n] {
        wake_task(id);
    }
}

/// Set the scheduling priority of task `id` (0=low, 1=normal, 2=high).
///
/// Returns true on success, false if id not found or priority out of range.
pub fn set_task_priority(id: TaskId, priority: u8) -> bool {
    if priority > 2 { return false; }
    let sched = unsafe { get_sched() };
    if let Some(t) = sched.tasks.iter_mut().find(|t| t.id == id && t.state != TaskState::Dead) {
        t.priority = priority;
        true
    } else {
        false
    }
}

/// Return the top of the current task's kernel stack.
///
/// Returns 0 for the bootstrap task, which uses the existing boot stack and
/// has no heap-allocated `_stack`.  `syscall::enter_userspace` skips the
/// TSS/KERN_RSP update in that case (ring-3 entry from the boot task is not
/// supported).
pub fn current_kernel_stack_top() -> u64 {
    let sched = unsafe { get_sched() };
    sched.tasks[sched.current]._stack_top
}

/// Mark the current task `Blocked` and switch to the next ready task.
///
/// Returns when another task calls `wake_task` on this task's ID.  If there
/// are no other ready tasks the CPU halts (deadlock — shouldn't happen in a
/// well-formed system where the waker is still running).
pub fn block_and_yield() {
    let sched = unsafe { get_sched() };
    check_stack_canary(&sched.tasks[sched.current]);
    sweep_dead(sched);

    let current = sched.current;
    let n = sched.tasks.len();

    // Mark the current task Blocked before searching for the next task so
    // that the round-robin scan skips it correctly.
    sched.tasks[current].state = TaskState::Blocked;

    let next = match find_next_ready(sched, (current + 1) % n) {
        Some(idx) => idx,
        None      => loop { unsafe { core::arch::asm!("hlt") }; },
    };

    let from_ctx: *mut   TaskContext = &mut sched.tasks[current].context;
    let to_ctx:   *const TaskContext = &    sched.tasks[next].context;

    sched.tasks[next].state = TaskState::Running;
    sched.current           = next;

    unsafe { core::arch::asm!("cli", options(nostack)) };
    switch_cr3(sched.tasks[next].page_table, sched.tasks[next]._stack_top);
    unsafe { switch_context(from_ctx, to_ctx); }
    // Resumed here when wake_task + a subsequent schedule picks us back up.
    unsafe { core::arch::asm!("sti", options(nostack)) };
}

/// Block the task with `id`, removing it from scheduling until `wake_task` is called.
/// No-op if the task is not in the Ready state.
pub fn block_task(id: TaskId) {
    let sched = unsafe { get_sched() };
    if let Some(t) = sched.tasks.iter_mut().find(|t| t.id == id) {
        if t.state == TaskState::Ready {
            t.state = TaskState::Blocked;
        }
    }
}

/// Wake a blocked task, making it eligible to be scheduled again.
/// No-op if the task is not in the Blocked state.
pub fn wake_task(id: TaskId) {
    let sched = unsafe { get_sched() };
    if let Some(t) = sched.tasks.iter_mut().find(|t| t.id == id) {
        if t.state == TaskState::Blocked {
            t.state = TaskState::Ready;
        }
    }
}

/// Return the current program break for the running task (0 = uninitialized).
pub fn current_task_brk() -> u64 {
    let sched = unsafe { get_sched() };
    sched.tasks[sched.current].brk
}

/// Update the current program break for the running task.
pub fn set_current_task_brk(addr: u64) {
    let sched = unsafe { get_sched() };
    sched.tasks[sched.current].brk = addr;
}
