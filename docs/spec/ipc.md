# IPC

## Design

Each IPC endpoint is a 4 KiB physical page shared between the kernel and
any tasks that hold a capability to it. The page holds a simple ring buffer.
All messaging is in-kernel copy: user data is copied from the sender's
address space into the ring, then from the ring into the receiver's address
space. There is no shared memory between tasks at the IPC layer.

---

## Ring buffer layout

The ring occupies exactly one 4 KiB page.

```
Offset  Size  Field
──────  ────  ─────────────────────────────────────────────────────
0       4     head (u32) — index of next slot to consume
4       4     tail (u32) — index of next slot to produce
8       4088  data — circular buffer of 64-byte message slots
```

Constants:

| Constant | Value | Meaning |
|----------|-------|---------|
| `MSG_SIZE` | 64 bytes | Fixed message size |
| `RING_CAPACITY` | 63 | Max messages in-flight (floor(4088 / 64)) |

The ring is empty when `head == tail`. It is full when `(tail + 1) % RING_CAPACITY == head`.

---

## Kernel mapping

The kernel maps each endpoint page at a fixed virtual address for its own
access:

```
0xFFFF_D000_0000_0000 + endpoint_index * 4096
```

User tasks do not have the endpoint page mapped into their address space.
All access goes through `SYS_IPC_SEND` / `SYS_IPC_RECV`.

---

## Send semantics

`SYS_IPC_SEND` (or `ipc::send`):

1. Check the IPC capability handle is valid and has `WRITE` right.
2. Validate user pointer `[msg_ptr, msg_ptr + msg_len)` is in user space.
3. If ring is **full**: mark task Blocked, yield. Resume when a receiver
   drains the ring and calls `wake_task`.
4. Copy up to `MSG_SIZE` bytes from user buffer into the next slot at
   `data[tail * MSG_SIZE]`, advance `tail`.
5. If a task is blocked waiting to receive on this endpoint, wake it.

---

## Recv semantics

`SYS_IPC_RECV` (or `ipc::recv`):

1. Check the IPC capability handle is valid and has `READ` right.
2. If ring is **empty**: mark task Blocked, yield. Resume when a sender
   writes a message and calls `wake_task`.
3. Copy `min(buf_len, MSG_SIZE)` bytes from `data[head * MSG_SIZE]` into
   user buffer, advance `head`.
4. Return the number of bytes copied.
5. If a task is blocked waiting to send on this endpoint, wake it.

---

## Blocking model

Tasks block on a per-endpoint wait queue. When a send unblocks a receiver
(or a recv unblocks a sender), it calls `wake_task(id)` which moves the
blocked task from `Blocked` back to `Ready` in the scheduler's queue. The
woken task does not run immediately — it is placed at the back of the ready
queue and scheduled normally.

---

## Message format: BootInfo

The kernel pre-queues one message on the boot IPC endpoint (handle 2 given
to lythd) before launching lythd. This message is exactly `MSG_SIZE` = 64
bytes.

```
Offset  Size  Field
──────  ────  ─────────────────────────────────────────────────────
0       8     signature  (u64) = 0xB007_1000_B007_1000
8       8     mem_bytes  (u64) = free_frames × 4096
16      8     free_frames (u64) = PMM free frame count at boot
24      12    vendor     ([u8; 12]) = CPUID leaf 0 vendor string
                          (EBX || EDX || ECX, 12 bytes)
36      28    _pad       ([u8; 28]) = zeroed
```

lythd's first syscall must be `SYS_IPC_RECV` on handle 2 to consume this
message and learn the system's memory size and CPU vendor.

---

## Limitations and future work

- **Fixed 64-byte message size.** Large data must be transferred by first
  sharing a memory capability and then exchanging addresses over IPC.
- **No multicast.** One sender unblocks one receiver at a time.
- **No message priority.** Ring is strictly FIFO.
- **No timeout.** A task that blocks on IPC with no counterpart waits
  indefinitely. A timeout / cancellation mechanism is a future addition.
