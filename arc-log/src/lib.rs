#![feature(
    allocator_api,
    slice_ptr_get,
    try_reserve,
    optin_builtin_traits,
    negative_impls
)]
#![no_std]

extern crate alloc;
pub mod arc_log;
pub use arc_log::*;

