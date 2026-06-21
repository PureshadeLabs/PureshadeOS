/// IPC shared-memory ring buffer — Step 11.
///
/// ## Model
///
/// An IPC endpoint is a 4 KiB physical page shared between a sender and a
/// receiver.  The page holds a ring buffer of fixed 64-byte message slots.
/// Both user processes map this page directly into their address spaces; the
/// kernel keeps a permanent higher-half mapping at `IPC_KERN_BASE + idx*4096`
/// for blocking checks.
///
/// The kernel is involved only for **blocking and waking**:
/// - `send` blocks the caller when the ring is full; wakes any blocked recv.
/// - `recv` blocks the caller when the ring is empty; wakes any blocked send.
///
/// Data copying (user code → ring page → user code) goes through those same
/// kernel-VA mappings during the syscall window — no additional bounce buffer
/// is needed.
///
/// ## Ring layout (byte offsets within the 4 KiB page)
///
/// ```text
/// [0]   head : u32   — read cursor, advanced by the receiver
/// [4]   tail : u32   — write cursor, advanced by the sender
/// [8]   data : [u8; RING_DATA_BYTES]
/// ```
///
/// Counters are free-running `u32`s; position is `counter % RING_CAPACITY`.
/// The ring is full when `tail - head == RING_CAPACITY`.

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU32, Ordering};

// ── Ring buffer constants ──────────────────────────────────────────────────────

const RING_DATA_BYTES: usize = 4096 - 8; // 4-byte head + 4-byte tail

/// Fixed message slot size in bytes.
pub const MSG_SIZE: usize = 64;

/// Number of message slots in the ring.
pub const RING_CAPACITY: usize = RING_DATA_BYTES / MSG_SIZE; // 63

// ── Ring buffer layout ────────────────────────────────────────────────────────

/// The in-page ring buffer structure.  Exactly 4 096 bytes.
#[repr(C)]
struct RingBuffer {
    head: AtomicU32,
    tail: AtomicU32,
    data: [u8; RING_DATA_BYTES],
}

const _: () = assert!(core::mem::size_of::<RingBuffer>() == 4096);

// ── IPC endpoint ──────────────────────────────────────────────────────────────

/// Kernel state for one IPC endpoint.
pub struct IpcEndpoint {
    /// Physical address of the shared ring buffer page.
    pub phys_page: crate::pmm::PhysAddr,
    /// Kernel virtual address (permanent higher-half mapping).
    kern_virt_u64: u64,
    /// Task blocked waiting to send (ring full), if any.
    pub sender_waiting: Option<crate::task::TaskId>,
    /// Task blocked waiting to receive (ring empty), if any.
    pub receiver_waiting: Option<crate::task::TaskId>,
    /// A capability in transit from the last `send_cap` call.
    ///
    /// Only one cap can be in flight per endpoint at a time.  It is consumed
    /// (and moved into the receiver's cap table) by `recv_cap`.
    pub pending_cap: Option<crate::cap::Capability>,
}

impl IpcEndpoint {
    /// Raw pointer to the ring buffer page (kernel VA).
    #[inline]
    fn ring_ptr(&self) -> *mut RingBuffer {
        self.kern_virt_u64 as *mut RingBuffer
    }
}

// ── Global endpoint table ─────────────────────────────────────────────────────

/// Nominal (pre-KASLR) kernel VA window for IPC ring buffer pages.
const IPC_KERN_BASE_NOMINAL: u64 = 0xFFFF_D000_0000_0000;

/// Return the KASLR-adjusted IPC kernel window base.
#[inline]
fn ipc_kern_base() -> u64 {
    IPC_KERN_BASE_NOMINAL + crate::kaslr::offset()
}

struct EpTable(UnsafeCell<Option<Vec<IpcEndpoint>>>);
// SAFETY: single-threaded kernel.
unsafe impl Sync for EpTable {}
static EP_TABLE: EpTable = EpTable(UnsafeCell::new(None));

fn ep_table() -> &'static mut Vec<IpcEndpoint> {
    unsafe {
        let t = &mut *EP_TABLE.0.get();
        if t.is_none() {
            *t = Some(Vec::new());
        }
        t.as_mut().unwrap()
    }
}

// ── Named endpoint registry ───────────────────────────────────────────────────

struct NameTable(UnsafeCell<Option<BTreeMap<String, usize>>>);
unsafe impl Sync for NameTable {}
static NAME_TABLE: NameTable = NameTable(UnsafeCell::new(None));

fn name_table() -> &'static mut BTreeMap<String, usize> {
    unsafe {
        let t = &mut *NAME_TABLE.0.get();
        if t.is_none() {
            *t = Some(BTreeMap::new());
        }
        t.as_mut().unwrap()
    }
}

/// Bind a name to an endpoint index.  Returns `false` if the name is already
/// taken by a *different* endpoint (re-binding the same idx is a no-op).
pub fn bind_name(name: &str, idx: usize) -> bool {
    let table = name_table();
    match table.get(name) {
        Some(&existing) => existing == idx,
        None => { table.insert(String::from(name), idx); true }
    }
}

/// Look up an endpoint index by name.
pub fn lookup_name(name: &str) -> Option<usize> {
    name_table().get(name).copied()
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Maximum number of IPC endpoints that can exist simultaneously.
///
/// Each endpoint occupies one physical frame and one 4 KiB slot in the kernel
/// IPC window (`0xFFFF_D000_0000_0000`).  The cap prevents a runaway caller
/// from exhausting physical memory or overflowing the kernel VA window.
pub const MAX_ENDPOINTS: usize = 1024;

/// Create a new IPC endpoint.
///
/// Allocates a physical page, maps it into the kernel IPC window, zeroes the
/// ring buffer, and returns the endpoint index.  Returns `None` if the global
/// endpoint cap (`MAX_ENDPOINTS`) has been reached.  The caller is responsible
/// for creating a `KernelObject::Ipc { endpoint_idx }` capability.
pub fn create_endpoint() -> Option<usize> {
    let table = ep_table();
    if table.len() >= MAX_ENDPOINTS { return None; }

    let phys_page = crate::pmm::alloc_frame()
        .expect("ipc::create_endpoint: out of physical frames");

    let idx = table.len();
    let kern_virt = crate::vmm::VirtAddr(ipc_kern_base() + (idx as u64) * 4096);

    crate::vmm::map_page(kern_virt, phys_page, crate::vmm::PageFlags::KERNEL_RW);

    // Zero the ring buffer (head = tail = 0, data zeroed).
    unsafe {
        core::ptr::write_bytes(kern_virt.as_u64() as *mut u8, 0, 4096);
    }

    table.push(IpcEndpoint {
        phys_page,
        kern_virt_u64: kern_virt.as_u64(),
        sender_waiting:   None,
        receiver_waiting: None,
        pending_cap:      None,
    });

    Some(idx)
}

/// Return the physical address of endpoint `idx`'s shared page.
pub fn endpoint_phys(idx: usize) -> crate::pmm::PhysAddr {
    ep_table()[idx].phys_page
}

/// Send a message to endpoint `idx`.
///
/// Copies up to `MSG_SIZE` bytes from `msg` into the next ring slot and
/// advances the tail.  Blocks the calling task if the ring is full.
/// Wakes any task blocked in `recv` on this endpoint.
pub fn send(idx: usize, msg: &[u8]) {
    let len = msg.len().min(MSG_SIZE);

    loop {
        let ep = &mut ep_table()[idx];
        let ring_ptr = ep.ring_ptr();

        // Read counters through the kernel mapping.
        let head = unsafe { (*ring_ptr).head.load(Ordering::Acquire) };
        let tail = unsafe { (*ring_ptr).tail.load(Ordering::Acquire) };
        let used = tail.wrapping_sub(head) as usize;

        if used < RING_CAPACITY {
            let slot_off = (tail as usize % RING_CAPACITY) * MSG_SIZE;
            unsafe {
                // Copy payload; zero-pad the remainder of the slot.
                let dst = (*ring_ptr).data.as_mut_ptr().add(slot_off);
                core::ptr::copy_nonoverlapping(msg.as_ptr(), dst, len);
                if len < MSG_SIZE {
                    core::ptr::write_bytes(dst.add(len), 0, MSG_SIZE - len);
                }
                (*ring_ptr).tail.store(tail.wrapping_add(1), Ordering::Release);
            }

            // Wake any blocked receiver.
            if let Some(recv_id) = ep.receiver_waiting.take() {
                crate::task::wake_task(recv_id);
            }
            return;
        }

        // Ring full: block until a receiver consumes a slot.
        let current_id = crate::task::current_task_id();
        ep.sender_waiting = Some(current_id);
        crate::task::block_and_yield();
        // Woken — retry.
    }
}

/// Like `send` but also attaches a capability to the endpoint.
///
/// The capability is moved out of the caller's hands and stored in
/// `pending_cap`; the receiver retrieves it via `recv_cap`.  Only one
/// capability can be in flight per endpoint at a time.
pub fn send_cap(idx: usize, msg: &[u8], cap: crate::cap::Capability) {
    let len = msg.len().min(MSG_SIZE);

    loop {
        let ep       = &mut ep_table()[idx];
        let ring_ptr = ep.ring_ptr();

        let head = unsafe { (*ring_ptr).head.load(Ordering::Acquire) };
        let tail = unsafe { (*ring_ptr).tail.load(Ordering::Acquire) };
        let used = tail.wrapping_sub(head) as usize;

        if used < RING_CAPACITY {
            let slot_off = (tail as usize % RING_CAPACITY) * MSG_SIZE;
            unsafe {
                let dst = (*ring_ptr).data.as_mut_ptr().add(slot_off);
                core::ptr::copy_nonoverlapping(msg.as_ptr(), dst, len);
                if len < MSG_SIZE {
                    core::ptr::write_bytes(dst.add(len), 0, MSG_SIZE - len);
                }
                (*ring_ptr).tail.store(tail.wrapping_add(1), Ordering::Release);
            }
            ep.pending_cap = Some(cap);
            if let Some(recv_id) = ep.receiver_waiting.take() {
                crate::task::wake_task(recv_id);
            }
            return;
        }

        let current_id = crate::task::current_task_id();
        ep.sender_waiting = Some(current_id);
        crate::task::block_and_yield();
    }
}

/// Like `recv` but also returns any capability attached by `send_cap`.
///
/// Returns `(bytes_received, Option<Capability>)`.  If no capability was
/// attached the `Option` is `None`.
pub fn recv_cap(idx: usize, buf: &mut [u8]) -> (usize, Option<crate::cap::Capability>) {
    let out_len = buf.len().min(MSG_SIZE);

    loop {
        let ep       = &mut ep_table()[idx];
        let ring_ptr = ep.ring_ptr();

        let head = unsafe { (*ring_ptr).head.load(Ordering::Acquire) };
        let tail = unsafe { (*ring_ptr).tail.load(Ordering::Acquire) };

        if head != tail {
            let slot_off = (head as usize % RING_CAPACITY) * MSG_SIZE;
            unsafe {
                let src = (*ring_ptr).data.as_ptr().add(slot_off);
                core::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), out_len);
                (*ring_ptr).head.store(head.wrapping_add(1), Ordering::Release);
            }
            let cap = ep.pending_cap.take();
            if let Some(send_id) = ep.sender_waiting.take() {
                crate::task::wake_task(send_id);
            }
            return (out_len, cap);
        }

        let current_id = crate::task::current_task_id();
        ep.receiver_waiting = Some(current_id);
        crate::task::block_and_yield();
    }
}

/// Receive a message from endpoint `idx` with a timeout.
///
/// Blocks the calling task like `recv`, but returns `Err(())` if no message
/// arrives within `timeout_ms` milliseconds (EAGAIN at the syscall layer).
///
/// A zero `timeout_ms` is equivalent to `try_recv`.
pub fn recv_timeout(idx: usize, buf: &mut [u8], timeout_ms: u64) -> Result<usize, ()> {
    let out_len = buf.len().min(MSG_SIZE);
    let deadline = crate::apic::ticks() + timeout_ms;

    loop {
        let ep = &mut ep_table()[idx];
        let ring_ptr = ep.ring_ptr();

        let head = unsafe { (*ring_ptr).head.load(Ordering::Acquire) };
        let tail = unsafe { (*ring_ptr).tail.load(Ordering::Acquire) };

        if head != tail {
            let slot_off = (head as usize % RING_CAPACITY) * MSG_SIZE;
            unsafe {
                let src = (*ring_ptr).data.as_ptr().add(slot_off);
                core::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), out_len);
                (*ring_ptr).head.store(head.wrapping_add(1), Ordering::Release);
            }
            if let Some(send_id) = ep.sender_waiting.take() {
                crate::task::wake_task(send_id);
            }
            return Ok(out_len);
        }

        // Check deadline before blocking.
        if crate::apic::ticks() >= deadline {
            let current_id = crate::task::current_task_id();
            if ep.receiver_waiting == Some(current_id) {
                ep.receiver_waiting = None;
            }
            return Err(());
        }

        let current_id = crate::task::current_task_id();
        ep.receiver_waiting = Some(current_id);
        // Schedule a wakeup so we don't sleep past the deadline.
        crate::task::sleep_until(deadline);
        // block_and_yield is called by sleep_until.
        // On wake, loop again to check ring and deadline.
    }
}

/// Send to endpoint `idx` with a timeout.
///
/// Returns `Err(())` if the ring stays full for `timeout_ms` milliseconds.
pub fn send_timeout(idx: usize, msg: &[u8], timeout_ms: u64) -> Result<(), ()> {
    let len = msg.len().min(MSG_SIZE);
    let deadline = crate::apic::ticks() + timeout_ms;

    loop {
        let ep = &mut ep_table()[idx];
        let ring_ptr = ep.ring_ptr();

        let head = unsafe { (*ring_ptr).head.load(Ordering::Acquire) };
        let tail = unsafe { (*ring_ptr).tail.load(Ordering::Acquire) };
        let used = tail.wrapping_sub(head) as usize;

        if used < RING_CAPACITY {
            let slot_off = (tail as usize % RING_CAPACITY) * MSG_SIZE;
            unsafe {
                let dst = (*ring_ptr).data.as_mut_ptr().add(slot_off);
                core::ptr::copy_nonoverlapping(msg.as_ptr(), dst, len);
                if len < MSG_SIZE {
                    core::ptr::write_bytes(dst.add(len), 0, MSG_SIZE - len);
                }
                (*ring_ptr).tail.store(tail.wrapping_add(1), Ordering::Release);
            }
            if let Some(recv_id) = ep.receiver_waiting.take() {
                crate::task::wake_task(recv_id);
            }
            return Ok(());
        }

        if crate::apic::ticks() >= deadline {
            let current_id = crate::task::current_task_id();
            if ep.sender_waiting == Some(current_id) {
                ep.sender_waiting = None;
            }
            return Err(());
        }

        let current_id = crate::task::current_task_id();
        ep.sender_waiting = Some(current_id);
        crate::task::sleep_until(deadline);
    }
}

/// Non-blocking receive from endpoint `idx`.
///
/// Returns `Some(bytes)` if a message was available, `None` if the ring is
/// empty.  Never blocks or modifies the waiting-task fields.
pub fn try_recv(idx: usize, buf: &mut [u8]) -> Option<usize> {
    let out_len = buf.len().min(MSG_SIZE);
    let ep = &mut ep_table()[idx];
    let ring_ptr = ep.ring_ptr();

    let head = unsafe { (*ring_ptr).head.load(Ordering::Acquire) };
    let tail = unsafe { (*ring_ptr).tail.load(Ordering::Acquire) };

    if head == tail { return None; }

    let slot_off = (head as usize % RING_CAPACITY) * MSG_SIZE;
    unsafe {
        let src = (*ring_ptr).data.as_ptr().add(slot_off);
        core::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), out_len);
        (*ring_ptr).head.store(head.wrapping_add(1), Ordering::Release);
    }
    if let Some(send_id) = ep.sender_waiting.take() {
        crate::task::wake_task(send_id);
    }
    Some(out_len)
}

/// Receive a message from endpoint `idx` into `buf`.
///
/// Copies one message slot (up to `buf.len()` bytes, max `MSG_SIZE`) from the
/// head of the ring into `buf` and advances the head.  Blocks the calling task
/// if the ring is empty.  Wakes any task blocked in `send` on this endpoint.
///
/// Returns the number of bytes written into `buf`.
pub fn recv(idx: usize, buf: &mut [u8]) -> usize {
    let out_len = buf.len().min(MSG_SIZE);

    loop {
        let ep = &mut ep_table()[idx];
        let ring_ptr = ep.ring_ptr();

        let head = unsafe { (*ring_ptr).head.load(Ordering::Acquire) };
        let tail = unsafe { (*ring_ptr).tail.load(Ordering::Acquire) };

        if head != tail {
            let slot_off = (head as usize % RING_CAPACITY) * MSG_SIZE;
            unsafe {
                let src = (*ring_ptr).data.as_ptr().add(slot_off);
                core::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), out_len);
                (*ring_ptr).head.store(head.wrapping_add(1), Ordering::Release);
            }

            // Wake any blocked sender.
            if let Some(send_id) = ep.sender_waiting.take() {
                crate::task::wake_task(send_id);
            }
            return out_len;
        }

        // Ring empty: block until a sender posts a message.
        let current_id = crate::task::current_task_id();
        ep.receiver_waiting = Some(current_id);
        crate::task::block_and_yield();
        // Woken — retry.
    }
}
