# IPC

## Design

Each IPC endpoint is a 4 KiB physical page managed by the kernel. All
messaging is an in-kernel copy: user data is copied from the sender's address
space into the ring buffer, then from the ring into the receiver's address
space. There is no shared memory between tasks at the IPC layer.

---

## Ring buffer layout

The ring occupies exactly one 4 KiB page.

```
Offset  Size  Field
──────  ────  ─────────────────────────────────────────────────────
0       4     head (u32) — free-running consume counter
4       4     tail (u32) — free-running produce counter
8       4088  data — circular buffer of 64-byte message slots
```

Constants:

| Constant | Value | Meaning |
|----------|-------|---------|
| `MSG_SIZE` | 64 bytes | Fixed message size |
| `RING_CAPACITY` | 63 | Max messages in-flight (`floor(4088 / 64)`) |

`head` and `tail` are free-running `u32` counters that never reset (they wrap
at `u32::MAX`). The slot index for a given counter value is
`counter % RING_CAPACITY`.

- **Empty:** `head == tail`
- **Full:** `tail.wrapping_sub(head) >= RING_CAPACITY`

---

## Kernel mapping

The kernel maps each endpoint page at a virtual address derived from the
**nominal base** `0xFFFF_D000_0000_0000` plus a per-boot KASLR offset:

```
actual_base = 0xFFFF_D000_0000_0000 + kaslr_offset()
endpoint_VA = actual_base + endpoint_index * 4096
```

The nominal base is a documentation constant only — the actual address varies
per boot. User tasks do not have endpoint pages mapped at any address; all
access goes through the IPC syscalls (`SYS_IPC_SEND`, `SYS_IPC_RECV`, etc.).
The `IPC_KERN_BASE_NOMINAL` constant in `lythos-abi` reflects the pre-KASLR
value; kernel-internal code calls `ipc_kern_base()` to obtain the adjusted
address.

---

## Send semantics

`SYS_IPC_SEND` (6):

1. Check IPC capability handle is valid with `WRITE` right.
2. Validate user pointer `[msg_ptr, msg_ptr + msg_len)` is in user space.
3. If ring is **full**: mark task Blocked, yield. Resume when a receiver
   drains the ring and calls `wake_task`.
4. Copy up to `MSG_SIZE` bytes from user buffer into
   `data[(tail % RING_CAPACITY) * MSG_SIZE]`; advance `tail`.
5. If a task is blocked waiting to recv on this endpoint, wake it.

---

## Recv semantics

`SYS_IPC_RECV` (7):

1. Check IPC capability handle is valid with `READ` right.
2. If ring is **empty**: mark task Blocked, yield. Resume when a sender writes
   a message and calls `wake_task`.
3. Copy `min(buf_len, MSG_SIZE)` bytes from
   `data[(head % RING_CAPACITY) * MSG_SIZE]` into user buffer; advance `head`.
4. Return bytes copied.
5. If a task is blocked waiting to send on this endpoint, wake it.

---

## Blocking model

Tasks block on a per-endpoint wait queue. When a send unblocks a receiver (or
a recv unblocks a sender), `wake_task(id)` moves the blocked task from
`Blocked` to `Ready`. The woken task does not run immediately — it is placed at
the back of the ready queue and scheduled normally.

---

## Non-blocking receive

`SYS_IPC_POLL` (39):

Same semantics as `SYS_IPC_RECV` but returns `EAGAIN` immediately if the ring
is empty rather than blocking.

---

## Timed operations

`SYS_IPC_RECV_TIMEOUT` (42) / `SYS_IPC_SEND_TIMEOUT` (43):

Same semantics as blocking recv/send but with a millisecond deadline
(`a4 = timeout_ms`). Returns `EAGAIN` if the deadline expires before the
operation can complete.

---

## Capability transfer

`SYS_IPC_SEND_CAP` (12) / `SYS_IPC_RECV_CAP` (13):

A capability can be transferred alongside a message in one atomic operation.

**Send side (`SYS_IPC_SEND_CAP`):**
- Arguments: `a1 = ipc_cap`, `a2 = msg_ptr`, `a3 = msg_len`, `a4 = cap_handle`.
- Semantics are the same as `SYS_IPC_SEND` plus capability transfer. The
  capability identified by `a4` is detached from the sender's table and held
  in-flight with the message.

**Recv side (`SYS_IPC_RECV_CAP`):**
- Arguments: `a1 = ipc_cap`, `a2 = buf_ptr`, `a3 = buf_len`,
  `a4 = out_handle_ptr` (pointer to `u64` in user address space).
- Semantics are the same as `SYS_IPC_RECV`. On return, `*a4` contains the
  new `CapHandle` in the receiver's table if a capability was transferred, or
  `u64::MAX` (`0xFFFF_FFFF_FFFF_FFFF`) if the message carried no capability.

---

## Named endpoints

`SYS_IPC_BIND` (40) / `SYS_IPC_LOOKUP` (41):

Endpoints can be registered by name so peers can look them up without a
pre-shared handle.

**Bind (`SYS_IPC_BIND`):**
- Arguments: `a1 = ipc_cap`, `a2 = name_ptr`, `a3 = name_len` (max 128 bytes,
  UTF-8).
- Requires `GRANT` right on the IPC cap.
- Returns 0 on success; `ENOSYS` if the name is already registered.
- Names are global and persist until kernel restart. They are not
  garbage-collected when the endpoint cap is revoked.

**Lookup (`SYS_IPC_LOOKUP`):**
- Arguments: `a1 = name_ptr`, `a2 = name_len`, `a3 = requested_rights_mask`.
- The kernel strips `GRANT` and `REVOKE` from `a3` before creating the
  returned handle. Callers cannot self-grant elevated rights via lookup.
- Returns new `CapHandle` on success; `ENOENT` if the name is not registered.

---

## Message format: BootInfo

The kernel pre-queues one message on the boot IPC endpoint (handle 2 given
to `lythd` at boot). This message is exactly `MSG_SIZE` = 64 bytes.

```
Offset  Size  Field
──────  ────  ─────────────────────────────────────────────────────
0       8     signature   (u64) = 0xB007_1000_B007_1000
8       8     mem_bytes   (u64) = free_frames × 4096
16      8     free_frames (u64) = PMM free frame count at boot
24      12    vendor      ([u8; 12]) = CPUID leaf 0 vendor string
                           (EBX || EDX || ECX, 12 bytes)
36      28    _pad        ([u8; 28]) = zeroed
```

`lythd`'s first syscall must be `SYS_IPC_RECV` on handle 2 to consume this
message and learn the system's memory size and CPU vendor.

---

## Limitations

- **Fixed 64-byte message size.** Large data transfers require sharing a
  `Memory` capability and exchanging the mapped address over IPC.
- **No multicast.** One sender unblocks one receiver at a time.
- **No message priority.** Ring is strictly FIFO.
