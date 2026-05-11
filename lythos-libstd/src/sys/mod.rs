// sys — Platform Abstraction Layer for Lythos.
//
// All platform-specific logic lives here.  Public modules in the crate call
// into this layer rather than touching lythos_std syscalls directly.

mod lythos;

pub use lythos::alloc   as alloc_impl;
pub use lythos::io      as io_impl;
pub use lythos::process as process_impl;
pub use lythos::thread  as thread_impl;
pub use lythos::time    as time_impl;
