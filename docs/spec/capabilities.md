# Capability system

## Concept

A capability is an unforgeable token that grants a task the right to perform
a specific operation on a specific resource. User code never touches the
underlying resource directly — it holds an opaque `CapHandle(u64)` and passes
it to a syscall. The kernel looks up the handle in the task's `CapabilityTable`
and checks whether the requested operation is allowed by the rights field.

This design means privilege separation is enforced at the kernel boundary:
even if a task's code is compromised, it cannot escalate beyond the
capabilities it holds.

---

## Capability kinds

| Kind | Resource | Grants access to |
|------|----------|-----------------|
| `Memory` | Physical memory pool | `SYS_MMAP` — map arbitrary physical frames |
| `Ipc` | One IPC endpoint | `SYS_IPC_SEND` / `SYS_IPC_RECV` |
| `Rollback` | Kernel checkpoint | `SYS_ROLLBACK` |

---

## Rights bitfield

Rights are a 4-bit mask stored as `u32`.

| Bit | Constant | Meaning |
|-----|----------|---------|
| 0 | `READ = 1` | Can read / recv from the resource |
| 1 | `WRITE = 2` | Can write / send to the resource |
| 2 | `GRANT = 4` | Can grant a derived cap to another task |
| 3 | `REVOKE = 8` | Can revoke a cap from self or descendants |
| — | `ALL = 15` | All four rights combined |

When a capability is granted to another task via `SYS_CAP_GRANT`, the new
capability's rights are `original_rights & mask` — rights can only be
reduced, never amplified, in transit.

---

## CapabilityTable

Each task has one `CapabilityTable`. It is a `Vec<Option<Capability>>`,
indexed by `CapHandle`. A `None` slot means the handle has been revoked or
was never allocated. Handle numbers are assigned sequentially and never
reused within a table (to prevent use-after-revoke races).

---

## Operations

### cap_grant

```
cap_grant(from_table, handle, target_task_id, to_table, rights_mask)
    → new_handle | ENOCAP | ENOPERM | EINVAL
```

1. Look up `handle` in `from_table`. Fail with `ENOCAP` if absent.
2. Check the source has `GRANT` right. Fail with `ENOPERM` if not.
3. Compute new rights = `source.rights & rights_mask`.
4. Append a new `Capability` to `to_table` with the reduced rights.
5. Return the new handle index.

A task cannot grant to itself (`EINVAL`). The target task must exist
(`EINVAL` if not found by ID).

### cap_revoke

```
cap_revoke(table, handle) → 0 | ENOCAP | ENOPERM
```

Removes the capability from `table` by setting the slot to `None`. The
caller must hold the `REVOKE` right on the capability being revoked.

### cap_cascade_revoke

```
cap_cascade_revoke(table, handle, resolver)
```

Removes the capability from `table` and then revokes all capabilities
derived from it (i.e., granted by this task to others). Requires a
`resolver` callback that maps task IDs to their capability tables.

Currently cascade revoke does a linear walk over all live tasks. This is
acceptable for the small task counts expected in OROS.

---

## Initial capabilities at boot

`kmain` creates three capabilities and passes them to lythd via `exec`:

| Handle | Kind | Rights | Description |
|--------|------|--------|-------------|
| 0 | `Memory` | `ALL` | All free physical frames at boot time |
| 1 | `Rollback` | `ALL` | Access to the kernel rollback mechanism |
| 2 | `Ipc` | `ALL` | Boot IPC endpoint with pre-queued `BootInfo` |

lythd is expected to split and delegate these capabilities to child processes
as appropriate. For example, it may grant child processes restricted `Memory`
capabilities that only allow mapping within a specific range.

---

## Security model notes

- **No ambient authority.** A task with no capabilities is genuinely
  powerless: it cannot allocate memory, communicate with other tasks, or
  spawn children.
- **Rights attenuation only.** The grant operation can only reduce rights.
  There is no privilege escalation path through the capability system.
- **Handle opacity.** `CapHandle` values are meaningful only within the
  task that holds them. Sending a handle value over IPC grants nothing —
  only `SYS_CAP_GRANT` transfers the actual capability.
- **Planned: range-restricted Memory caps.** Currently a Memory capability
  grants access to all of physical memory. A planned improvement adds a
  `[base, limit)` range field and enforces it in `SYS_MMAP`.
