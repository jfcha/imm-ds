#![feature(
    allocator_api,
    slice_ptr_get,
    try_reserve,
    auto_traits,
    negative_impls,
    ptr_metadata,
    new_uninit,
    maybe_uninit_slice,
    maybe_uninit_uninit_array
)]
#![no_std]

extern crate alloc;
pub mod arc_log;
//pub mod log_fragment;
pub use crate::arc_log::*;
pub mod waker_list;
pub use waker_list::*;

