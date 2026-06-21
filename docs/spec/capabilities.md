# Capability system

## Concept

A capability is an unforgeable token that grants a task the right to perform
a specific operation on a specific resource. User code never touches the
underlying resource directly — it holds an opaque `CapHandle(u64)` and passes
it to a syscall. The kernel looks up the handle in the task's `CapabilityTable`
and checks whether the requested operation is allowed by the rights field.

Privilege separation is enforced at the kernel boundary: even if a task's code
is compromised, it cannot escalate beyond the capabilities it holds.

---

## Capability kinds

| Kind | Resource | Grants access to |
|------|----------|-----------------|
| `Memory` | Physical memory pool | `SYS_MMAP` — map anonymous frames from the PMM |
| `Ipc` | One IPC endpoint | `SYS_IPC_SEND` / `SYS_IPC_RECV` and all IPC variants |
| `Device` | Hardware device (IRQ line, port range, MMIO region) | Physical and MMIO memory mapping for the device's assigned range — this is the mechanism that keeps `SYS_MMAP` from accepting a physical-address argument |
| `Rollback` | Kernel checkpoint | `SYS_ROLLBACK` |

---

## Rights

Rights are carried in the low bits of a `u64` register argument (e.g. `a3` in
`SYS_CAP_GRANT`). Valid range is 0–15.

| Bit | Value | Constant | Meaning |
|-----|-------|----------|---------|
| 0 | 1 | `READ` | Can read / recv from the resource |
| 1 | 2 | `WRITE` | Can write / send to the resource |
| 2 | 4 | `GRANT` | Can grant a derived cap to another task |
| 3 | 8 | `REVOKE` | Can revoke a cap from self or descendants |
| — | 15 | `ALL` | All four rights combined |
| — | 0 | `NONE` | No rights; handle is effectively unusable |

Rights can only be reduced in transit: `SYS_CAP_GRANT` computes new rights as
`original_rights & mask`. There is no privilege-amplification path in the
capability system.

The kernel's internal `CapRights` storage type is an implementation detail and
is not part of the ABI. Rights do not currently appear in any boundary struct;
they are passed only as register values in syscall arguments.

---

## Handle encoding

`CapHandle` values are opaque `u64`s. Userspace must never parse, forge, or
arithmetic-manipulate handle values. The only valid operations are:

- Pass a handle received from a syscall back to a syscall.
- Compare two handles for equality.
- Check whether a handle equals `u64::MAX` (`0xFFFF_FFFF_FFFF_FFFF`), which is
  the sentinel for "no capability present" in `SYS_IPC_RECV_CAP`.

**Generation-tagging guarantees:** The kernel embeds a generation counter in
each handle value. This provides the following observable guarantees:

- **Stale-handle detection.** If a capability is revoked and its slot is later
  reallocated, the old handle value does not match the new slot's generation and
  is rejected immediately with `ENOCAP`. Stale handles never alias live
  capabilities.
- **No silent aliasing.** Two handles with different values in the same task's
  table always refer to different capabilities.
- **Wrap-around.** The generation counter is finite. Behavior after `u32::MAX`
  reuses of the same slot is not currently defined as an error; avoid relying on
  handles across billions of allocation cycles in the same slot.

The internal bit layout of the generation and slot fields is not part of the
ABI and may change without notice.

---

## CapabilityTable

Each task has one `CapabilityTable`. Handles are assigned sequentially within
the table. A revoked slot becomes vacant; future allocations in that slot carry
a new generation so old handles are rejected. Handle numbers are not reused
within the same generation.

---

## Operations

### cap_grant

```
cap_grant(from_table, handle, target_task_id, rights_mask)
    → new_handle | ENOCAP | ENOPERM | EINVAL
```

1. Look up `handle` in `from_table`. Fail `ENOCAP` if absent.
2. Check source has `GRANT` right. Fail `ENOPERM` if not.
3. Compute new rights = `source.rights & rights_mask`.
4. Append a new `Capability` to the target task's table with reduced rights.
5. Return the new handle index.

A task cannot grant to itself (`EINVAL`). The target task must exist
(`EINVAL` if not found by ID).

### cap_revoke

```
cap_revoke(table, handle) → 0 | ENOCAP | ENOPERM
```

Removes one capability from `table` by setting the slot to vacant. Not
directly exposed as a syscall — `SYS_CAP_REVOKE` always invokes the cascading
variant below.

### cap_cascade_revoke

```
cap_cascade_revoke(table, handle, resolver)
```

Removes the capability from `table`, then walks all live tasks and revokes all
capabilities derived from it — that is, all handles granted (directly or
transitively) from this one. This is the operation invoked by `SYS_CAP_REVOKE`.

The walk is linear over live tasks, which is acceptable for the task counts
expected in OROS.

---

## Initial capabilities at boot

`kmain` creates three capabilities and passes them to `lythd` via `SYS_EXEC`:

| Handle | Kind | Rights | Description |
|--------|------|--------|-------------|
| 0 | `Memory` | `ALL` | All free physical frames at boot time |
| 1 | `Rollback` | `ALL` | Access to the kernel rollback mechanism |
| 2 | `Ipc` | `ALL` | Boot IPC endpoint with pre-queued `BootInfo` message |

`lythd` splits and delegates these to child processes as appropriate. For
example, it may grant child processes a `Memory` capability restricted to a
specific frame range (planned improvement — currently the whole PMM).

---

## Security model notes

- **No ambient authority.** A task with no capabilities cannot allocate memory,
  communicate, or spawn children.
- **Rights attenuation only.** Grant can only reduce rights; there is no
  privilege escalation path.
- **Handle opacity.** `CapHandle` values are meaningful only within the holding
  task. Sending a raw handle value over IPC grants nothing — only
  `SYS_CAP_GRANT` and `SYS_IPC_SEND_CAP` transfer the actual capability.
- **Planned: range-restricted Memory caps.** Currently a `Memory` capability
  grants access to the entire PMM pool. A planned improvement adds a
  `[base, limit)` range field enforced in `SYS_MMAP`.
