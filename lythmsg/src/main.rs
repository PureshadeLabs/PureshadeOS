//! lythmsg — IPC bus daemon for OROS (Open Runtime Operating System).
//!
//! lythmsg is the system-wide publish/subscribe message broker.  Services
//! discover it by name via `SYS_IPC_LOOKUP("lythmsg")`, subscribe to topics,
//! and publish messages that are fanned out to all subscribers.
//!
//! ## Capability handles at entry
//!
//! | Handle | Kind   | Contents                                       |
//! |--------|--------|------------------------------------------------|
//! | 0      | Memory | Memory cap from lythdist (reserved; unused v1) |
//! | 1      | Ipc    | Control endpoint — clients send requests here  |
//! | 2      | Ipc    | lythd service registry — register at startup   |
//!
//! ## Client protocol (64-byte IPC messages)
//!
//! **SUBSCRIBE (kind = 0x01)** — attach a delivery endpoint to a topic:
//!
//! | Bytes  | Field       | Meaning                               |
//! |--------|-------------|---------------------------------------|
//! | 0      | kind        | 0x01                                  |
//! | 1      | topic_len   | length of topic string (≤16)          |
//! | 2..18  | topic       | topic name, ASCII, zero-padded        |
//! | 18..26 | sub_id      | client-chosen u64 subscription ID     |
//! | 26..64 | _pad        | zero                                  |
//! | (cap)  | delivery_ep | cap transferred via SYS_IPC_SEND_CAP  |
//!
//! lythmsg stores the delivery cap and fans DELIVER messages to it on publish.
//! No acknowledgement is sent; SUBSCRIBE is fire-and-forget.
//!
//! **UNSUBSCRIBE (kind = 0x02)**:
//!
//! | Bytes | Field  | Meaning                             |
//! |-------|--------|-------------------------------------|
//! | 0     | kind   | 0x02                                |
//! | 1..9  | sub_id | subscription ID to cancel (u64 LE)  |
//! | 9..64 | _pad   | zero                                |
//!
//! **PUBLISH (kind = 0x03)**:
//!
//! | Bytes  | Field       | Meaning                            |
//! |--------|-------------|------------------------------------|
//! | 0      | kind        | 0x03                               |
//! | 1      | topic_len   | length of topic string (≤16)       |
//! | 2..18  | topic       | topic name                         |
//! | 18     | payload_len | payload byte count (≤44)           |
//! | 19..63 | payload     | message bytes                      |
//! | 63     | _pad        | zero                               |
//!
//! **DELIVER (kind = 0x20)** — sent by lythmsg to each subscriber's delivery ep:
//!
//! Same layout as PUBLISH with kind = 0x20.
//!
//! ## Eviction policy
//!
//! If a subscriber's delivery ring stays full for more than `EVICT_MISS_LIMIT`
//! consecutive PUBLISH events to that topic, lythmsg drops the subscription.
//! If the delivery cap itself becomes invalid (subscriber died), the sub is
//! removed on the next attempted delivery.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::vec::Vec;
use lythos_std::{
    ipc::Endpoint,
    println, eprintln,
    sys_ipc_bind, sys_ipc_send_timeout,
    sys_task_exit, SysError,
};

// ── Capability handles at entry ───────────────────────────────────────────────

const CTRL_EP:      u64 = 1;
const REGISTRY_CAP: u64 = 2;

// ── Message kind constants ────────────────────────────────────────────────────

const KIND_SUBSCRIBE:   u8 = 0x01;
const KIND_UNSUBSCRIBE: u8 = 0x02;
const KIND_PUBLISH:     u8 = 0x03;
const KIND_DELIVER:     u8 = 0x20;

// ── Service registry protocol ─────────────────────────────────────────────────

const REG_KIND_REGISTER: u8 = 0;

// ── Layout constants ──────────────────────────────────────────────────────────

const TOPIC_MAX:   usize = 16;
const PAYLOAD_MAX: usize = 44;

// ── Slow-subscriber eviction threshold ───────────────────────────────────────

const EVICT_MISS_LIMIT: u32 = 64;

// ── Subscription record ───────────────────────────────────────────────────────

struct Sub {
    sub_id:       u64,
    topic:        [u8; TOPIC_MAX],
    topic_len:    u8,
    delivery_cap: u64,
    miss_count:   u32,
}

impl Sub {
    fn topic_matches(&self, topic: &[u8], len: u8) -> bool {
        if self.topic_len != len { return false; }
        self.topic[..len as usize] == topic[..len as usize]
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    println!("[lythmsg] IPC bus starting (ctrl={})", CTRL_EP);

    // ── Bind control endpoint to "lythmsg" ────────────────────────────────────
    match sys_ipc_bind(CTRL_EP, "lythmsg") {
        Ok(())               => println!("[lythmsg] bound control endpoint as 'lythmsg'"),
        Err(SysError::NoSys) => eprintln!("[lythmsg] warn: 'lythmsg' name already taken (restart?)"),
        Err(e)               => eprintln!("[lythmsg] warn: ipc_bind failed: {:?}", e),
    }

    // ── Register with lythd service registry ──────────────────────────────────
    {
        let reg_ep = Endpoint::from_raw(REGISTRY_CAP);
        let name   = b"lythmsg";
        let mut frame = [0u8; 64];
        frame[0] = REG_KIND_REGISTER;
        frame[1] = name.len() as u8;
        frame[2..2 + name.len()].copy_from_slice(name);
        frame[42..50].copy_from_slice(&CTRL_EP.to_le_bytes());
        reg_ep.send_frame(&frame).expect("[lythmsg] registry register send failed");
        println!("[lythmsg] registered with service registry");
    }

    let ctrl_ep = Endpoint::from_raw(CTRL_EP);
    let mut subs: Vec<Sub> = Vec::new();

    println!("[lythmsg] entering event loop");

    // ── Event loop ────────────────────────────────────────────────────────────
    loop {
        let (frame, maybe_cap) = match ctrl_ep.recv_frame_with_cap() {
            Ok(r)  => r,
            Err(e) => {
                eprintln!("[lythmsg] ctrl recv error: {:?}", e);
                continue;
            }
        };

        match frame[0] {
            KIND_SUBSCRIBE   => handle_subscribe(&frame, maybe_cap, &mut subs),
            KIND_UNSUBSCRIBE => handle_unsubscribe(&frame, &mut subs),
            KIND_PUBLISH     => handle_publish(&frame, &mut subs),
            kind             => eprintln!("[lythmsg] unknown kind=0x{:02x}", kind),
        }
    }
}

// ── SUBSCRIBE handler ─────────────────────────────────────────────────────────

fn handle_subscribe(frame: &[u8; 64], maybe_cap: Option<u64>, subs: &mut Vec<Sub>) {
    let delivery_cap = match maybe_cap {
        Some(c) => c,
        None => {
            eprintln!("[lythmsg] SUBSCRIBE: missing delivery cap");
            return;
        }
    };

    let topic_len = frame[1].min(TOPIC_MAX as u8);
    let mut topic = [0u8; TOPIC_MAX];
    topic[..topic_len as usize].copy_from_slice(&frame[2..2 + topic_len as usize]);
    let sub_id = u64::from_le_bytes(frame[18..26].try_into().unwrap_or([0; 8]));

    if sub_id == 0 {
        eprintln!("[lythmsg] SUBSCRIBE: sub_id=0 rejected");
        return;
    }

    // Idempotent: replace if same sub_id already registered.
    subs.retain(|s| s.sub_id != sub_id);

    let topic_str = core::str::from_utf8(&topic[..topic_len as usize]).unwrap_or("?");
    println!("[lythmsg] subscribe: id={} topic='{}' delivery_cap={}", sub_id, topic_str, delivery_cap);

    subs.push(Sub { sub_id, topic, topic_len, delivery_cap, miss_count: 0 });
}

// ── UNSUBSCRIBE handler ───────────────────────────────────────────────────────

fn handle_unsubscribe(frame: &[u8; 64], subs: &mut Vec<Sub>) {
    let sub_id = u64::from_le_bytes(frame[1..9].try_into().unwrap_or([0; 8]));
    let before = subs.len();
    subs.retain(|s| s.sub_id != sub_id);
    if subs.len() < before {
        println!("[lythmsg] unsubscribe: id={} removed", sub_id);
    }
}

// ── PUBLISH handler ───────────────────────────────────────────────────────────

fn handle_publish(frame: &[u8; 64], subs: &mut Vec<Sub>) {
    let topic_len   = frame[1].min(TOPIC_MAX as u8);
    let payload_len = frame[18].min(PAYLOAD_MAX as u8);

    let topic   = &frame[2..2 + topic_len as usize];
    let payload = &frame[19..19 + payload_len as usize];

    let mut deliver = [0u8; 64];
    deliver[0] = KIND_DELIVER;
    deliver[1] = topic_len;
    deliver[2..2 + topic_len as usize].copy_from_slice(topic);
    deliver[18] = payload_len;
    deliver[19..19 + payload_len as usize].copy_from_slice(payload);

    let mut dead_ids: Vec<u64> = Vec::new();

    for sub in subs.iter_mut() {
        if !sub.topic_matches(topic, topic_len) { continue; }

        match sys_ipc_send_timeout(sub.delivery_cap, &deliver, 0) {
            Ok(()) => {
                sub.miss_count = 0;
            }
            Err(SysError::Again) => {
                sub.miss_count = sub.miss_count.saturating_add(1);
                if sub.miss_count >= EVICT_MISS_LIMIT {
                    eprintln!("[lythmsg] sub_id={} evicted (slow subscriber)", sub.sub_id);
                    dead_ids.push(sub.sub_id);
                }
            }
            Err(_) => {
                dead_ids.push(sub.sub_id);
            }
        }
    }

    if !dead_ids.is_empty() {
        subs.retain(|s| !dead_ids.iter().any(|&d| d == s.sub_id));
    }
}

// ── Panic handler ─────────────────────────────────────────────────────────────

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    lythos_std::sys_log("[lythmsg] PANIC");
    if let Some(msg) = info.message().as_str() {
        lythos_std::sys_log(": ");
        lythos_std::sys_log(msg);
    }
    if let Some(loc) = info.location() {
        lythos_std::sys_log(" at ");
        lythos_std::sys_log(loc.file());
        lythos_std::sys_log(":");
        let line = loc.line();
        let mut buf = [0u8; 10];
        let mut n = 0usize;
        let mut v = line;
        if v == 0 {
            buf[0] = b'0';
            n = 1;
        } else {
            while v > 0 {
                buf[n] = b'0' + (v % 10) as u8;
                n += 1;
                v /= 10;
            }
            buf[..n].reverse();
        }
        if let Ok(s) = core::str::from_utf8(&buf[..n]) { lythos_std::sys_log(s); }
        lythos_std::sys_log("\n");
    } else {
        lythos_std::sys_log("\n");
    }
    sys_task_exit()
}
