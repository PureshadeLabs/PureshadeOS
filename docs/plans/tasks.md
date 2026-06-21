# Task model and scheduler

## Task

A task is the kernel's unit of execution. Every task has:

- A unique `TaskId` (`u64`). The bootstrap task (kmain) is task 0.
- A state: `Ready`, `Running`, `Blocked`, or `Dead`.
- A kernel stack (64 KiB per task).
- A saved register context (`Context` struct: RSP + callee-saved registers).
- A capability table (`CapabilityTable`).
- A page table root (PML4 physical address).

Kernel tasks (spawned with `spawn_kernel_task`) run entirely in ring 0.
User tasks (spawned via `exec`) switch to ring 3 via `enter_userspace`.

---

## Scheduler

Round-robin over a `VecDeque<TaskId>` ready queue. There is no priority.
Pre-emption is driven by the LAPIC timer ISR, which calls `yield_task` at
each tick.

### State transitions

```
              spawn
               │
               ▼
      ┌──── Ready ◄──── wake_task ──── Blocked
      │        │                           ▲
      │    scheduled                       │
      │        ▼                    block_and_yield
      │    Running ──── yield_task ──► Ready
      │        │
      │   task_exit
      │        ▼
      └──────  Dead
```

### Functions

| Function | Description |
|----------|-------------|
| `spawn_kernel_task(fn)` | Create a kernel-mode task, add to ready queue |
| `spawn_userspace_task(entry, stack, caps, trampoline)` | Create a ring-3 task |
| `yield_task()` | Cooperative yield; also called from SYS_YIELD and timer ISR |
| `task_exit()` | Mark task Dead, switch to next; never returns |
| `block_and_yield()` | Mark task Blocked, yield; used by IPC when ring is empty/full |
| `wake_task(id)` | Move task from Blocked to Ready |

---

## Context switch

Context switching is purely software. The `switch` function (in `task.rs`)
saves callee-saved registers (RBX, R12–R15, RBP) plus RSP for the outgoing
task, then restores them for the incoming task.

The function pointer trick: the saved RIP is implicitly the return address
pushed when `switch` was called, so restoring RSP and returning resumes
the new task exactly where it last yielded.

### Kernel stack per task

Each task has a 64 KiB kernel stack allocated on the kernel heap. The stack
pointer stored in `Context.rsp` always points into this buffer when the task
is not running.

### Ring 3 → ring 0 on syscall / interrupt

When a user task takes a syscall or interrupt, the CPU switches to the ring-0
stack pointer in `TSS.RSP0`. `tss::init` sets RSP0 to the top of the current
task's kernel stack. Since only one task runs at a time, RSP0 is updated on
every context switch.

---

## User stack layout

`alloc_user_stack` assigns slots numbered from 0 upward, starting at virtual
address `0x0000_7FFF_0000_0000`. Each slot is 2050 pages wide:

```
┌─────────────────┐  ← slot_base
│   guard page    │  1 page (unmapped — causes fault on stack overflow)
├─────────────────┤
│   usable stack  │  2048 pages (8 MiB)
├─────────────────┤
│   gap page      │  1 page (unmapped — separates slots)
└─────────────────┘  ← next slot_base
```

The initial stack top is at `slot_base + 1 + 2048` pages = top of usable
region. `exec` writes an ABI-compatible initial frame (argc=0, NULL argv,
NULL envp) below the stack top before launching.

`NEXT_STACK_SLOT` is a global `u64` that increments monotonically. It is
**not** locked; concurrent `exec` calls would race. Currently safe because
the kernel is single-threaded.

---

## LAPIC timer

The LAPIC timer ISR fires at a calibrated rate (measured against the PIT in
`apic::init`). Each tick increments a global `TICK_COUNT` atomic and calls
`yield_task`, which implements pre-emptive scheduling.

The LAPIC timer interrupt is vector 32 (first available after the two
reserved ranges). It is an edge-triggered, locally-delivered interrupt and
does not go through the IOAPIC.
