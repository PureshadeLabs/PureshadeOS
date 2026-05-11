//! Collection types, mirroring `std::collections`.

// B-tree collections from alloc — always available.
pub use _alloc::collections::{BTreeMap, BTreeSet, LinkedList, VecDeque};

// Vec and String live in the crate root via alloc re-exports.
pub use _alloc::vec::Vec;
pub use _alloc::string::String;

// HashMap / HashSet — requires hashbrown or std; on no_std we use hashbrown.
// Enable the `hashbrown` cargo feature to get these.
#[cfg(feature = "hashbrown")]
pub use hashbrown::{HashMap, HashSet};

// When hashbrown is not enabled, provide the type names as BTree aliases so
// code that just wants a map still compiles (with sorted rather than hashed
// semantics).  Programs that need hash semantics should enable the feature.
#[cfg(not(feature = "hashbrown"))]
pub use _alloc::collections::BTreeMap as HashMap;
#[cfg(not(feature = "hashbrown"))]
pub use _alloc::collections::BTreeSet as HashSet;

// Binary heap.
pub use _alloc::collections::BinaryHeap;
