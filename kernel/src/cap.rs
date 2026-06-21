/// Capability system — unforgeable tokens granting access to kernel objects.
///
/// ## Model
///
/// Every access to a kernel resource goes through a capability.  Processes
/// hold opaque `CapHandle` values; the kernel maps those to `Capability`
/// structs stored in per-task `CapabilityTable`s.  No capability can be
/// forged from userspace: all handles are indices into kernel-managed tables.
///
/// ## Kernel object table
///
/// Kernel objects (memory regions, IPC endpoints, devices) live in a global
/// arena.  Each slot carries a generation counter; the handle encodes both
/// the slot index and the generation so that stale handles are detected
/// immediately (`ENOCAP` in userspace terms).
///
/// ## Capability handles
///
/// A `CapHandle` similarly encodes a slot index and generation inside the
/// per-task capability table, giving the same stale-handle protection.
///
/// ## Propagation and revocation
///
/// `cap_grant` is the *only* way a capability can propagate: the new cap's
/// rights are `original & rights_mask`, and the new handle is recorded in the
/// parent's `children` list.  `cap_revoke` removes a cap from one table.
/// Cascading revocation (removing all derived copies) is performed at the
/// syscall layer in Step 10 using `Capability::children` + `find_children`.

extern crate alloc;

use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU64, Ordering};

// ── CapId ─────────────────────────────────────────────────────────────────────

/// Kernel-internal, monotonically increasing capability identity.
pub type CapId = u64;

static NEXT_CAP_ID: AtomicU64 = AtomicU64::new(1);

#[inline]
fn alloc_cap_id() -> CapId {
    NEXT_CAP_ID.fetch_add(1, Ordering::Relaxed)
}

// ── CapKind ───────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapKind {
    /// Contiguous physical frame range.
    Memory,
    /// IPC endpoint — fully populated in Step 11.
    Ipc,
    /// Hardware device (IRQ line, port range, etc.).
    Device,
    /// Privileged rollback trigger — granted only to `lythd` at boot.
    Rollback,
}

// ── CapRights ─────────────────────────────────────────────────────────────────

/// Bitfield of rights attached to a capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapRights(pub u8);

impl CapRights {
    pub const READ:   Self = Self(1 << 0);
    pub const WRITE:  Self = Self(1 << 1);
    pub const GRANT:  Self = Self(1 << 2);
    pub const REVOKE: Self = Self(1 << 3);
    pub const ALL:    Self = Self(0x0F);
    pub const NONE:   Self = Self(0x00);

    /// True if `self` includes all bits in `other`.
    #[inline] pub fn has(self, other: Self) -> bool { self.0 & other.0 == other.0 }
    /// Intersection of rights.
    #[inline] pub fn intersect(self, mask: Self) -> Self { Self(self.0 & mask.0) }
}

// ── KernelObject ──────────────────────────────────────────────────────────────

/// A kernel-managed resource that a capability refers to.
pub enum KernelObject {
    /// Contiguous run of physical frames starting at `base_pa`.
    Memory { base_pa: u64, frame_count: u64 },
    /// IPC endpoint — index into `ipc::EP_TABLE`.
    Ipc { endpoint_idx: usize },
    /// Hardware device identified by an optional IRQ line.
    Device { irq: Option<u8> },
    /// Privileged rollback trigger.
    Rollback,
}

// ── KernelObjectRef ───────────────────────────────────────────────────────────

/// Generation-tagged index into the global kernel object table.
///
/// Bits [63:32] = generation, bits [31:0] = slot index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KernelObjectRef(u64);

impl KernelObjectRef {
    fn pack(index: u32, generation: u32) -> Self {
        Self(((generation as u64) << 32) | index as u64)
    }
    fn index(self)      -> usize { (self.0 & 0xFFFF_FFFF) as usize }
    fn generation(self) -> u32   { (self.0 >> 32) as u32 }
}

// ── Global kernel object table ────────────────────────────────────────────────

struct KoSlot {
    generation: u32,
    object:     Option<KernelObject>,
}

struct KoTable {
    slots: Vec<KoSlot>,
}

impl KoTable {
    fn create(&mut self, obj: KernelObject) -> Result<KernelObjectRef, CapError> {
        // Reuse the first vacant slot.
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if slot.object.is_none() {
                slot.generation = slot.generation.wrapping_add(1);
                slot.object = Some(obj);
                return Ok(KernelObjectRef::pack(i as u32, slot.generation));
            }
        }
        // Append a new slot.
        let idx = self.slots.len();
        if idx > u32::MAX as usize { return Err(CapError::NoMemory); }
        self.slots.push(KoSlot { generation: 0, object: Some(obj) });
        Ok(KernelObjectRef::pack(idx as u32, 0))
    }

    fn get(&self, r: KernelObjectRef) -> Option<&KernelObject> {
        let slot = self.slots.get(r.index())?;
        if slot.generation != r.generation() { return None; }
        slot.object.as_ref()
    }

    fn destroy(&mut self, r: KernelObjectRef) -> Result<(), CapError> {
        let slot = self.slots.get_mut(r.index()).ok_or(CapError::InvalidHandle)?;
        if slot.generation != r.generation() { return Err(CapError::GenerationMismatch); }
        slot.object = None;
        // generation increments on the next create(), not here
        Ok(())
    }
}

struct GlobalKoTable(UnsafeCell<Option<KoTable>>);
// SAFETY: single-threaded kernel.
unsafe impl Sync for GlobalKoTable {}
static KO_TABLE: GlobalKoTable = GlobalKoTable(UnsafeCell::new(None));

fn ko_table() -> &'static mut KoTable {
    unsafe {
        let t = &mut *KO_TABLE.0.get();
        if t.is_none() { *t = Some(KoTable { slots: Vec::new() }); }
        t.as_mut().unwrap()
    }
}

/// Register a new kernel object and return an opaque reference to it.
pub fn create_object(obj: KernelObject) -> Result<KernelObjectRef, CapError> {
    ko_table().create(obj)
}

/// Destroy a kernel object.  Capabilities that still hold a reference to it
/// remain in their tables but will fail to resolve via `get_object`.
pub fn destroy_object(r: KernelObjectRef) -> Result<(), CapError> {
    ko_table().destroy(r)
}

/// Resolve a `KernelObjectRef`.  Returns `None` if the object has been
/// destroyed or the reference is stale.
pub fn get_object(r: KernelObjectRef) -> Option<&'static KernelObject> {
    ko_table().get(r)
}

// ── CapHandle ─────────────────────────────────────────────────────────────────

/// Opaque per-task capability handle, passed through the syscall interface.
///
/// Bits [63:32] = generation, bits [31:0] = slot index.
/// Passing an out-of-range or generation-mismatched handle returns `ENOCAP`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapHandle(pub u64);

impl CapHandle {
    fn pack(index: u32, generation: u32) -> Self {
        Self(((generation as u64) << 32) | index as u64)
    }
    fn index(self)      -> usize { (self.0 & 0xFFFF_FFFF) as usize }
    fn generation(self) -> u32   { (self.0 >> 32) as u32 }
}

// ── Capability ────────────────────────────────────────────────────────────────

pub struct Capability {
    /// Unique kernel-internal identity (monotonically increasing).
    pub id:        CapId,
    pub kind:      CapKind,
    pub rights:    CapRights,
    /// Reference to the underlying kernel object.
    pub object:    KernelObjectRef,
    /// `CapId` of the capability this was derived from (`None` = root).
    pub parent_id: Option<CapId>,
    /// `(task_id, handle)` pairs of derived capabilities granted from this one.
    /// The syscall layer uses this list for cascading revocation (Step 10).
    pub children:  Vec<(u64, CapHandle)>,
}

// ── CapabilityTable ───────────────────────────────────────────────────────────

struct CapSlot {
    generation: u32,
    cap:        Option<Capability>,
}

/// Per-task table mapping `CapHandle → Capability`.
pub struct CapabilityTable {
    slots: Vec<CapSlot>,
}

impl CapabilityTable {
    pub fn new() -> Self {
        Self { slots: Vec::new() }
    }

    /// Insert a capability and return its handle.
    pub fn insert(&mut self, cap: Capability) -> CapHandle {
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if slot.cap.is_none() {
                slot.generation = slot.generation.wrapping_add(1);
                slot.cap = Some(cap);
                return CapHandle::pack(i as u32, slot.generation);
            }
        }
        let idx = self.slots.len() as u32;
        self.slots.push(CapSlot { generation: 0, cap: Some(cap) });
        CapHandle::pack(idx, 0)
    }

    /// Borrow a capability by handle.
    pub fn get(&self, h: CapHandle) -> Result<&Capability, CapError> {
        let slot = self.slots.get(h.index()).ok_or(CapError::InvalidHandle)?;
        if slot.generation != h.generation() { return Err(CapError::GenerationMismatch); }
        slot.cap.as_ref().ok_or(CapError::InvalidHandle)
    }

    /// Mutably borrow a capability by handle.
    pub fn get_mut(&mut self, h: CapHandle) -> Result<&mut Capability, CapError> {
        let slot = self.slots.get_mut(h.index()).ok_or(CapError::InvalidHandle)?;
        if slot.generation != h.generation() { return Err(CapError::GenerationMismatch); }
        slot.cap.as_mut().ok_or(CapError::InvalidHandle)
    }

    /// Remove and return a capability by handle.
    pub fn take(&mut self, h: CapHandle) -> Result<Capability, CapError> {
        let slot = self.slots.get_mut(h.index()).ok_or(CapError::InvalidHandle)?;
        if slot.generation != h.generation() { return Err(CapError::GenerationMismatch); }
        slot.cap.take().ok_or(CapError::InvalidHandle)
    }

    /// Return `true` if the table contains at least one capability of `kind`.
    /// Used by the rollback syscall to verify the caller holds a Rollback cap.
    pub fn has_kind(&self, kind: CapKind) -> bool {
        self.slots.iter().any(|s| {
            s.cap.as_ref().map_or(false, |c| c.kind == kind)
        })
    }

    /// Return `true` if the table holds a capability of `kind` that includes
    /// all bits in `rights`.  Used by `SYS_MMAP` to gate physical-frame
    /// allocation on the caller holding a Memory capability with write access.
    pub fn has_kind_with_rights(&self, kind: CapKind, rights: CapRights) -> bool {
        self.slots.iter().any(|s| {
            s.cap.as_ref().map_or(false, |c| c.kind == kind && c.rights.has(rights))
        })
    }

    /// Return handles of all capabilities whose `parent_id` matches `id`.
    /// Used by the syscall layer to implement cascading revocation.
    pub fn find_children(&self, parent_id: CapId) -> Vec<CapHandle> {
        self.slots.iter().enumerate()
            .filter_map(|(i, slot)| {
                let cap = slot.cap.as_ref()?;
                if cap.parent_id == Some(parent_id) {
                    Some(CapHandle::pack(i as u32, slot.generation))
                } else {
                    None
                }
            })
            .collect()
    }
}

// ── CapError ──────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum CapError {
    /// Handle index out of range or slot empty.  Userspace error: `ENOCAP`.
    InvalidHandle,
    /// Handle's generation doesn't match the slot's current generation.
    GenerationMismatch,
    /// Caller's capability does not carry the `Grant` right.
    NoGrant,
    /// Caller's capability does not carry the `Revoke` right.
    NoRevoke,
    /// Kernel object table or heap allocation failed.
    NoMemory,
}

// ── Public operations ─────────────────────────────────────────────────────────

/// Create a root capability (kernel use only).
///
/// Root capabilities have no parent and are created directly by the kernel
/// at boot to seed the initial capability set handed to `lythd`.  They are
/// never derivable through the userspace `cap_grant` syscall.
pub fn create_root_cap(
    table:  &mut CapabilityTable,
    kind:   CapKind,
    rights: CapRights,
    object: KernelObjectRef,
) -> CapHandle {
    table.insert(Capability {
        id:        alloc_cap_id(),
        kind,
        rights,
        object,
        parent_id: None,
        children:  Vec::new(),
    })
}

/// Grant a derived capability from `from[handle]` into `to`.
///
/// Requires the `Grant` right.  The new capability's rights are
/// `original_rights & rights_mask`.  The child handle is appended to the
/// parent's `children` list so the syscall layer can perform cascading
/// revocation later.
///
/// `to_task_id` is the opaque identifier of the receiving task; it is stored
/// in the parent's child list for lookup during cascading revocation.
pub fn cap_grant(
    from:       &mut CapabilityTable,
    handle:     CapHandle,
    to_task_id: u64,
    to:         &mut CapabilityTable,
    rights_mask: CapRights,
) -> Result<CapHandle, CapError> {
    // Extract everything we need before releasing the borrow on `from`.
    let (parent_id, kind, object, new_rights) = {
        let cap = from.get(handle)?;
        if !cap.rights.has(CapRights::GRANT) { return Err(CapError::NoGrant); }
        (cap.id, cap.kind, cap.object, cap.rights.intersect(rights_mask))
    };

    let derived = Capability {
        id:        alloc_cap_id(),
        kind,
        rights:    new_rights,
        object,
        parent_id: Some(parent_id),
        children:  Vec::new(),
    };
    let new_handle = to.insert(derived);

    // Record the child so the parent can cascade-revoke later.
    from.get_mut(handle)?.children.push((to_task_id, new_handle));

    Ok(new_handle)
}

/// Revoke a capability and all transitively derived capabilities.
///
/// The `Revoke` right is checked only on the directly-targeted cap; derived
/// caps are forcibly removed by the kernel regardless of their rights.
/// `find_table(task_id)` must return a raw pointer to that task's
/// `CapabilityTable`, or null if the task is not found.  Stale or
/// already-revoked handles in the child list are silently ignored.
///
/// # Safety
/// `find_table` must return valid, non-aliased pointers for the duration of
/// the recursive call.  In a single-threaded kernel this is trivially true
/// when the pointer comes from a scheduler-owned `Box<Task>`.
pub fn cap_cascade_revoke(
    table:      &mut CapabilityTable,
    handle:     CapHandle,
    find_table: &mut dyn FnMut(u64) -> *mut CapabilityTable,
) -> Result<(), CapError> {
    // Check the Revoke right on the root of the cascade only.
    let cap = table.get(handle)?;
    if !cap.rights.has(CapRights::REVOKE) { return Err(CapError::NoRevoke); }
    cascade_force(table, handle, find_table);
    Ok(())
}

/// Forcibly remove a capability and all its descendants without checking rights.
/// Used as the recursive inner step of `cap_cascade_revoke`.
fn cascade_force(
    table:      &mut CapabilityTable,
    handle:     CapHandle,
    find_table: &mut dyn FnMut(u64) -> *mut CapabilityTable,
) {
    let children = match table.get(handle) {
        Ok(cap) => cap.children.clone(),
        Err(_)  => return, // already gone
    };
    let _ = table.take(handle);
    for (task_id, child_handle) in children {
        let ptr = find_table(task_id);
        if !ptr.is_null() {
            cascade_force(unsafe { &mut *ptr }, child_handle, find_table);
        }
    }
}

/// Inherit a capability into a new task's table during `exec`.
///
/// Kernel-only path: copies the capability with its original rights, without
/// checking the `Grant` right or recording a child link.  Used by the ELF
/// loader to seed the initial capability set of an exec'd process.
pub fn cap_inherit(
    from:   &CapabilityTable,
    handle: CapHandle,
    to:     &mut CapabilityTable,
) -> Result<CapHandle, CapError> {
    let src = from.get(handle)?;
    let derived = Capability {
        id:        alloc_cap_id(),
        kind:      src.kind,
        rights:    src.rights,
        object:    src.object,
        parent_id: src.parent_id,
        children:  Vec::new(),
    };
    Ok(to.insert(derived))
}

/// Revoke a capability from `table`.
///
/// Requires the `Revoke` right.  Removes the capability from the holder's
/// table only.  Cascading revocation (removing derived copies from other
/// tasks) must be performed by the caller using `Capability::children` and
/// `CapabilityTable::find_children`.
pub fn cap_revoke(
    table:  &mut CapabilityTable,
    handle: CapHandle,
) -> Result<(), CapError> {
    if !table.get(handle)?.rights.has(CapRights::REVOKE) {
        return Err(CapError::NoRevoke);
    }
    table.take(handle)?;
    Ok(())
}
