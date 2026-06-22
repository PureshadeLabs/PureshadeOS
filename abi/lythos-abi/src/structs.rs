//! Boundary structs — transcribed from `docs/spec/syscalls.md`.
//!
//! Every struct in this module is `#[repr(C)]` and has explicit `_pad` fields
//! matching the spec byte layout.  Size and offset assertions verify the
//! layout at compile time.
//!
//! Cross-checked against:
//! - Kernel serialisation in `kernel/src/syscall.rs`
//! - Spec tables in `docs/spec/syscalls.md`

// ── TaskInfo — 24 bytes ───────────────────────────────────────────────────────
//
// Spec § "TaskInfo — 24 bytes" (used by SYS_TASK_LIST, nr 17):
//   [0..8]   u64 LE  task_id
//   [8..16]  u64 LE  state   (1=running, 2=ready, 3=blocked)
//   [16]     u8      kind    (0=kernel, 1=userspace)
//   [17..24] [u8;7]  _pad
//
// Kernel serialisation (syscall.rs SYS_TASK_LIST):
//   write_unaligned(entry+0, id:u64)      ← offset 0  ✓
//   write_unaligned(entry+8, state:u64)   ← offset 8  ✓
//   write(entry+16, kind:u8)              ← offset 16 ✓
//   write_bytes(entry+17, 0, 7)           ← pad 7     ✓

/// One entry in the buffer filled by `SYS_TASK_LIST` (17).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct TaskInfo {
    /// Unique task identifier.
    pub id:    u64,
    /// Canonical task state: 1=running, 2=ready, 3=blocked.
    pub state: u64,
    /// Task kind: 0=kernel task, 1=userspace task.
    pub kind:  u8,
    pub _pad:  [u8; 7],
}

const _TASKINFO_SIZE:         () = assert!(core::mem::size_of::<TaskInfo>()       == 24);
const _TASKINFO_OFF_ID:       () = assert!(core::mem::offset_of!(TaskInfo, id)    ==  0);
const _TASKINFO_OFF_STATE:    () = assert!(core::mem::offset_of!(TaskInfo, state) ==  8);
const _TASKINFO_OFF_KIND:     () = assert!(core::mem::offset_of!(TaskInfo, kind)  == 16);

// ── PsEntry — 48 bytes ───────────────────────────────────────────────────────
//
// Spec § "PsEntry — 48 bytes" (used by SYS_PS, nr 37):
//   [0..8]   u64 LE   id
//   [8..16]  u64 LE   state
//   [16]     u8       kind
//   [17]     u8       priority  (0=low, 1=normal, 2=high)
//   [18]     u8       name_len  (0–16)
//   [19..24] [u8;5]   _pad
//   [24..40] [u8;16]  name      (first name_len bytes valid, rest zeroed)
//   [40..48] [u8;8]   _pad2
//
// Kernel serialisation (syscall.rs SYS_PS):
//   write_unaligned(entry+0,  id:u64)         ← offset 0   ✓
//   write_unaligned(entry+8,  state:u64)      ← offset 8   ✓
//   write(entry+16, kind:u8)                  ← offset 16  ✓
//   write(entry+17, priority:u8)              ← offset 17  ✓
//   write(entry+18, name_len:u8)              ← offset 18  ✓
//   write_bytes(entry+19, 0, 5)               ← pad 5      ✓
//   copy_nonoverlapping(name, entry+24, 16)   ← offset 24  ✓
//   write_bytes(entry+40, 0, 8)               ← pad 8      ✓
//
// First definition of PsEntry in userspace; previously only existed in the kernel.

/// One entry in the buffer filled by `SYS_PS` (37).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PsEntry {
    /// Unique task identifier.
    pub id:       u64,
    /// Canonical task state: 1=running, 2=ready, 3=blocked.
    pub state:    u64,
    /// Task kind: 0=kernel task, 1=userspace task.
    pub kind:     u8,
    /// Scheduling priority: 0=low, 1=normal, 2=high.
    pub priority: u8,
    /// Length of the name in bytes (0–16).
    pub name_len: u8,
    pub _pad:     [u8; 5],
    /// Task name — first `name_len` bytes valid, remainder zeroed.
    pub name:     [u8; 16],
    pub _pad2:    [u8; 8],
}

const _PSENTRY_SIZE:          () = assert!(core::mem::size_of::<PsEntry>()           == 48);
const _PSENTRY_OFF_ID:        () = assert!(core::mem::offset_of!(PsEntry, id)        ==  0);
const _PSENTRY_OFF_STATE:     () = assert!(core::mem::offset_of!(PsEntry, state)     ==  8);
const _PSENTRY_OFF_KIND:      () = assert!(core::mem::offset_of!(PsEntry, kind)      == 16);
const _PSENTRY_OFF_PRIORITY:  () = assert!(core::mem::offset_of!(PsEntry, priority)  == 17);
const _PSENTRY_OFF_NAME_LEN:  () = assert!(core::mem::offset_of!(PsEntry, name_len)  == 18);
const _PSENTRY_OFF_NAME:      () = assert!(core::mem::offset_of!(PsEntry, name)      == 24);

// ── Stat — 48 bytes ───────────────────────────────────────────────────────────
//
// Spec § "Stat — 48 bytes" (used by SYS_STAT, nr 26):
//   [0..8]   u64 LE  size
//   [8..16]  u64 LE  mtime  (Unix epoch ms, SYS_TIME_EPOCH epoch)
//   [16..24] u64 LE  ctime  (Unix epoch ms, SYS_TIME_EPOCH epoch)
//   [24..28] u32 LE  flags
//   [28..32] u32 LE  uid
//   [32..36] u32 LE  gid
//   [36..40] u32 LE  nlink
//   [40..42] u16 LE  mode
//   [42..48] [u8;6]  _pad
//
// Kernel serialisation (syscall.rs SYS_STAT):
//   buf[ 0.. 8] = size.to_le_bytes()    ← offset 0   ✓
//   buf[ 8..16] = mtime.to_le_bytes()   ← offset 8   ✓
//   buf[16..24] = ctime.to_le_bytes()   ← offset 16  ✓
//   buf[24..28] = flags.to_le_bytes()   ← offset 24  ✓
//   buf[28..32] = uid.to_le_bytes()     ← offset 28  ✓
//   buf[32..36] = gid.to_le_bytes()     ← offset 32  ✓
//   buf[36..40] = nlink.to_le_bytes()   ← offset 36  ✓
//   buf[40..42] = mode.to_le_bytes()    ← offset 40  ✓
//   (buf initialised to [0u8;48] so _pad is zero)

/// File/directory metadata returned by `SYS_STAT` (26).
///
/// Wire layout is 48 bytes, all fields little-endian, naturally aligned.
/// Safe to transmit as `[u8; 48]` and reinterpret with `from_bytes`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Stat {
    /// File size in bytes.
    pub size:  u64,
    /// Last-modified time (Unix epoch milliseconds, same epoch as `SYS_TIME_EPOCH`).
    pub mtime: u64,
    /// Creation time (Unix epoch milliseconds, same epoch as `SYS_TIME_EPOCH`).
    pub ctime: u64,
    /// Inode flags (see `inode_flags` module).
    pub flags: u32,
    /// Owner user ID.
    pub uid:   u32,
    /// Owner group ID.
    pub gid:   u32,
    /// Hard link count.
    pub nlink: u32,
    /// Unix permission bits (low 12 bits).
    pub mode:  u16,
    pub _pad:  [u8; 6],
}

const _STAT_SIZE:       () = assert!(core::mem::size_of::<Stat>()        == 48);
const _STAT_OFF_SIZE:   () = assert!(core::mem::offset_of!(Stat, size)   ==  0);
const _STAT_OFF_MTIME:  () = assert!(core::mem::offset_of!(Stat, mtime)  ==  8);
const _STAT_OFF_CTIME:  () = assert!(core::mem::offset_of!(Stat, ctime)  == 16);
const _STAT_OFF_FLAGS:  () = assert!(core::mem::offset_of!(Stat, flags)  == 24);
const _STAT_OFF_UID:    () = assert!(core::mem::offset_of!(Stat, uid)    == 28);
const _STAT_OFF_GID:    () = assert!(core::mem::offset_of!(Stat, gid)    == 32);
const _STAT_OFF_NLINK:  () = assert!(core::mem::offset_of!(Stat, nlink)  == 36);
const _STAT_OFF_MODE:   () = assert!(core::mem::offset_of!(Stat, mode)   == 40);

// ── Inode flags (Stat.flags bit meanings) ─────────────────────────────────────

pub mod inode_flags {
    /// Always set for a valid inode.
    pub const USED:     u32 = 1 << 0;  // 0x01
    /// Entry is a directory.
    pub const DIR:      u32 = 1 << 1;  // 0x02
    /// Entry is a symbolic link.
    pub const SYMLINK:  u32 = 1 << 2;  // 0x04
    /// Symlink name stored inline (kernel detail).
    pub const FAST_SYM: u32 = 1 << 3;  // 0x08
}

// ── DirEntry file_type values ─────────────────────────────────────────────────

pub mod file_type {
    /// Regular file.
    pub const REG:     u8 = 1;
    /// Directory.
    pub const DIR:     u8 = 2;
    /// Symbolic link.
    pub const SYMLINK: u8 = 3;
}

// ── DirEntry — 264 bytes ──────────────────────────────────────────────────────
//
// Spec § "DirEntry — 264 bytes" (used by SYS_READDIR, nr 27):
//   [0..4]    u32 LE    ino
//   [4]       u8        file_type
//   [5]       u8        name_len  (0–255)
//   [6..8]    [u8;2]    _pad
//   [8..264]  [u8;256]  name      (first name_len bytes valid, rest zeroed)
//
// Kernel serialisation (syscall.rs SYS_READDIR), buf zero-initialised:
//   kbuf[off..off+4] = e.ino.to_le_bytes()           ← offset 0   ✓
//   kbuf[off+4]      = e.file_type                    ← offset 4   ✓
//   kbuf[off+5]      = name_len as u8                 ← offset 5   ✓
//   (kbuf[off+6..off+8] zeroed by vec init)           ← pad 2      ✓
//   kbuf[off+8..off+8+name_len] = name bytes          ← offset 8   ✓

/// Wire size of one `DirEntry` in bytes.
pub const DIR_ENTRY_SIZE: usize = 264;

/// One directory entry returned by `SYS_READDIR` (27).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct DirEntry {
    /// Inode number.
    pub ino:       u32,
    /// Entry type (see `file_type` module).
    pub file_type: u8,
    /// Length of the name in bytes (0–255).
    pub name_len:  u8,
    pub _pad:      [u8; 2],
    /// Filename — first `name_len` bytes valid, remainder zeroed.
    pub name:      [u8; 256],
}

const _DIRENTRY_SIZE:         () = assert!(core::mem::size_of::<DirEntry>()           == 264);
const _DIRENTRY_OFF_INO:      () = assert!(core::mem::offset_of!(DirEntry, ino)       ==   0);
const _DIRENTRY_OFF_FTYPE:    () = assert!(core::mem::offset_of!(DirEntry, file_type) ==   4);
const _DIRENTRY_OFF_NAME_LEN: () = assert!(core::mem::offset_of!(DirEntry, name_len)  ==   5);
const _DIRENTRY_OFF_NAME:     () = assert!(core::mem::offset_of!(DirEntry, name)      ==   8);

impl DirEntry {
    /// Name as a `&str` (empty on UTF-8 decode failure).
    pub fn name_str(&self) -> &str {
        let len = self.name_len as usize;
        core::str::from_utf8(&self.name[..len.min(255)]).unwrap_or("")
    }
}
