use crate::updating_ref::Arcu;

use core::sync::atomic::{AtomicUsize, Ordering::*};

use core::alloc::LayoutErr;
use core::cmp;
use core::mem::{self, ManuallyDrop, MaybeUninit};
use core::ops::Drop;
use core::ptr::{self, NonNull, Unique};
use core::slice;


use std::alloc::{handle_alloc_error, AllocRef, Global, Layout};
use std::boxed::Box;
use std::collections::TryReserveError::{self, *};


struct SRawVec<T, A: AllocRef = Global> {
    ptr: Unique<T>,
    cap: AtomicUsize,
    len: AtomicUsize,
    alloc: A
}


pub struct SVecWriter<T, A: AllocRef = Global> {
    data: Arcu<SRawVec<T, A>>
}

impl <T> SVecWriter<T> {
    pub fn new() -> Self {
        SVecWriter {
            data: Arcu::new(SRawVec::NEW)
        }
    }
    pub fn with_capacity(size: usize) -> Self {
        SVecWriter {
            data: Arcu::new(SRawVec::with_capacity(size))
        }
    }
    pub fn len(&self) -> usize {
        self.data.len.load(Relaxed)
    }

    pub fn new_reader(&self) -> SVecReader<T> {
        SVecReader { data: self.data.clone()}
    }

    
    pub fn push(&mut self, value: T){
        //self.data.reserve(len, additional)
        // This will panic or abort if we would allocate > isize::MAX bytes
        // or if the length increment would overflow for zero-sized types.
        if self.data.needs_to_grow(1) {
            unsafe {
                let end = self.data.ptr().add(self.data.len());
                ptr::write(end, value);
                self.data.len.fetch_add(1, Release);
            }
        } else {
            self.reserve(1);
        } 
        
    }
}

pub struct SVecReader<T, A: AllocRef = Global> {
    data: Arcu<SRawVec<T, A>>
}

impl <T> SVecReader<T> {
    pub fn len(&self) -> usize {
        self.data.len.load(Relaxed)
    }
}


// enum AllocInit {
//     /// The contents of the new memory are uninitialized.
//     Uninitialized,
//     /// The new memory is guaranteed to be zeroed.
//     Zeroed,
// }


impl<T> SRawVec<T, Global> {
    /// HACK(Centril): This exists because `#[unstable]` `const fn`s needn't conform
    /// to `min_const_fn` and so they cannot be called in `min_const_fn`s either.
    ///
    /// If you change `RawVec<T>::new` or dependencies, please take care to not
    /// introduce anything that would truly violate `min_const_fn`.
    ///
    /// NOTE: We could avoid this hack and check conformance with some
    /// `#[rustc_force_min_const_fn]` attribute which requires conformance
    /// with `min_const_fn` but does not necessarily allow calling it in
    /// `stable(...) const fn` / user code not enabling `foo` when
    /// `#[rustc_const_unstable(feature = "foo", issue = "01234")]` is present.
    pub const NEW: Self = Self::new();

    /// Creates the biggest possible `RawVec` (on the system heap)
    /// without allocating. If `T` has positive size, then this makes a
    /// `RawVec` with capacity `0`. If `T` is zero-sized, then it makes a
    /// `RawVec` with capacity `usize::MAX`. Useful for implementing
    /// delayed allocation.
    pub const fn new() -> Self {
        Self::new_in(Global)
    }

    /// Creates a `RawVec` (on the system heap) with exactly the
    /// capacity and alignment requirements for a `[T; capacity]`. This is
    /// equivalent to calling `RawVec::new` when `capacity` is `0` or `T` is
    /// zero-sized. Note that if `T` is zero-sized this means you will
    /// *not* get a `RawVec` with the requested capacity.
    ///
    /// # Panics
    ///
    /// Panics if the requested capacity exceeds `isize::MAX` bytes.
    ///
    /// # Aborts
    ///
    /// Aborts on OOM.
    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        Self::with_capacity_in(capacity, Global)
    }

    /// Reconstitutes a `RawVec` from a pointer and capacity.
    ///
    /// # Safety
    ///
    /// The `ptr` must be allocated (on the system heap), and with the given `capacity`.
    /// The `capacity` cannot exceed `isize::MAX` for sized types. (only a concern on 32-bit
    /// systems). ZST vectors may have a capacity up to `usize::MAX`.
    /// If the `ptr` and `capacity` come from a `RawVec`, then this is guaranteed.
    #[inline]
    pub unsafe fn from_raw_parts(ptr: *mut T, capacity: usize) -> Self {
        unsafe { Self::from_raw_parts_in(ptr, capacity, Global) }
    }

    /// Converts a `Box<[T]>` into a `RawVec<T>`.
    pub fn from_box(slice: Box<[T]>) -> Self {
        unsafe {
            let mut slice = ManuallyDrop::new(slice);
            SRawVec::from_raw_parts(slice.as_mut_ptr(), slice.len())
        }
    }

    /// Converts the entire buffer into `Box<[MaybeUninit<T>]>` with the specified `len`.
    ///
    /// Note that this will correctly reconstitute any `cap` changes
    /// that may have been performed. (See description of type for details.)
    ///
    /// # Safety
    ///
    /// * `len` must be greater than or equal to the most recently requested capacity, and
    /// * `len` must be less than or equal to `self.capacity()`.
    ///
    /// Note, that the requested capacity and `self.capacity()` could differ, as
    /// an allocator could overallocate and return a greater memory block than requested.
    pub unsafe fn into_box(self, len: usize) -> Box<[MaybeUninit<T>]> {
        // Sanity-check one half of the safety requirement (we cannot check the other half).
        debug_assert!(
            len <= self.capacity(),
            "`len` must be smaller than or equal to `self.capacity()`"
        );

        let me = ManuallyDrop::new(self);
        unsafe {
            let slice = slice::from_raw_parts_mut(me.ptr() as *mut MaybeUninit<T>, len);
            Box::from_raw(slice)
        }
    }
}

impl<T, A: AllocRef> SRawVec<T, A> {
    /// Like `new`, but parameterized over the choice of allocator for
    /// the returned `RawVec`.
    pub const fn new_in(alloc: A) -> Self {
        // `cap: 0` means "unallocated". zero-sized types are ignored.
        Self { ptr: Unique::dangling(), cap: AtomicUsize::new(0), len: AtomicUsize::new(0), alloc }
    }

    /// Like `with_capacity`, but parameterized over the choice of
    /// allocator for the returned `RawVec`.
    #[inline]
    pub fn with_capacity_in(capacity: usize, alloc: A) -> Self {
        Self::allocate_in(capacity, alloc)
    }

    fn allocate_in(capacity: usize, mut alloc: A) -> Self {
        if mem::size_of::<T>() == 0 {
            Self::new_in(alloc)
        } else {
            // We avoid `unwrap_or_else` here because it bloats the amount of
            // LLVM IR generated.
            let layout = match Layout::array::<T>(capacity) {
                Ok(layout) => layout,
                Err(_) => capacity_overflow(),
            };
            match alloc_guard(layout.size()) {
                Ok(_) => {}
                Err(_) => capacity_overflow(),
            }
            let result = alloc.alloc(layout);
            let ptr = match result {
                Ok(ptr) => ptr,
                Err(_) => handle_alloc_error(layout),
            };

            Self {
                ptr: unsafe { Unique::new_unchecked(ptr.cast().as_ptr()) },
                cap: AtomicUsize::new(Self::capacity_from_bytes(ptr.len())),
                len: AtomicUsize::new(0),
                alloc,
            }
        }
    }

    /// Reconstitutes a `RawVec` from a pointer, capacity, and allocator.
    ///
    /// # Safety
    ///
    /// The `ptr` must be allocated (via the given allocator `alloc`), and with the given
    /// `capacity`.
    /// The `capacity` cannot exceed `isize::MAX` for sized types. (only a concern on 32-bit
    /// systems). ZST vectors may have a capacity up to `usize::MAX`.
    /// If the `ptr` and `capacity` come from a `RawVec` created via `alloc`, then this is
    /// guaranteed.
    #[inline]
    pub unsafe fn from_raw_parts_in(ptr: *mut T, capacity: usize, alloc: A) -> Self {
        Self { ptr: unsafe { Unique::new_unchecked(ptr) }, cap: AtomicUsize::new(capacity), len: AtomicUsize::new(0), alloc }
    }

    /// Gets a raw pointer to the start of the allocation. Note that this is
    /// `Unique::dangling()` if `capacity == 0` or `T` is zero-sized. In the former case, you must
    /// be careful.
    pub fn ptr(&self) -> *mut T {
        self.ptr.as_ptr()
    }

    pub fn len(&self) -> usize {
        self.len.load(Relaxed);
    }

    /// Gets the capacity of the allocation.
    ///
    /// This will always be `usize::MAX` if `T` is zero-sized.
    #[inline(always)]
    pub fn capacity(&self) -> usize {
        if mem::size_of::<T>() == 0 { usize::MAX } else { self.cap.load(Relaxed) }
    }

    /// Returns a shared reference to the allocator backing this `RawVec`.
    pub fn alloc(&self) -> &A {
        &self.alloc
    }

    /// Returns a mutable reference to the allocator backing this `RawVec`.
    pub fn alloc_mut(&mut self) -> &mut A {
        &mut self.alloc
    }

    fn current_memory(&self) -> Option<(NonNull<u8>, Layout)> {
        if mem::size_of::<T>() == 0 || self.cap.load(Relaxed) == 0 {
            None
        } else {
            // We have an allocated chunk of memory, so we can bypass runtime
            // checks to get our current layout.
            unsafe {
                let align = mem::align_of::<T>();
                let size = mem::size_of::<T>() * self.cap.load(Relaxed);
                let layout = Layout::from_size_align_unchecked(size, align);
                Some((self.ptr.cast().into(), layout))
            }
        }
    }

    /// Ensures that the buffer contains at least enough space to hold `len +
    /// additional` elements. If it doesn't already have enough capacity, will
    /// reallocate enough space plus comfortable slack space to get amortized
    /// `O(1)` behavior. Will limit this behavior if it would needlessly cause
    /// itself to panic.
    ///
    /// If `len` exceeds `self.capacity()`, this may fail to actually allocate
    /// the requested space. This is not really unsafe, but the unsafe
    /// code *you* write that relies on the behavior of this function may break.
    ///
    /// This is ideal for implementing a bulk-push operation like `extend`.
    ///
    /// # Panics
    ///
    /// Panics if the new capacity exceeds `isize::MAX` bytes.
    ///
    /// # Aborts
    ///
    /// Aborts on OOM.
    ///
    /// # Examples
    ///
    /// ```
    /// # #![feature(raw_vec_internals)]
    /// # extern crate alloc;
    /// # use std::ptr;
    /// # use alloc::raw_vec::RawVec;
    /// struct MyVec<T> {
    ///     buf: RawVec<T>,
    ///     len: usize,
    /// }
    ///
    /// impl<T: Clone> MyVec<T> {
    ///     pub fn push_all(&mut self, elems: &[T]) {
    ///         self.buf.reserve(self.len, elems.len());
    ///         // reserve would have aborted or panicked if the len exceeded
    ///         // `isize::MAX` so this is safe to do unchecked now.
    ///         for x in elems {
    ///             unsafe {
    ///                 ptr::write(self.buf.ptr().add(self.len), x.clone());
    ///             }
    ///             self.len += 1;
    ///         }
    ///     }
    /// }
    /// # fn main() {
    /// #   let mut vector = MyVec { buf: RawVec::new(), len: 0 };
    /// #   vector.push_all(&[1, 3, 5, 7, 9]);
    /// # }
    /// ```
    pub fn reserve_one(&mut self, elem: T) -> Result<NonNull<[T]>, TryReserveError> {
        match self.try_reserve(self.len(), 1) {
            Err(CapacityOverflow) => capacity_overflow(),
            Err(AllocError { layout, .. }) => handle_alloc_error(layout),
            Ok(x) => Ok(x)
        }
    }
    pub fn reserve(&mut self, elems: &mut [T]) -> Result<NonNull<[T]>, TryReserveError> {
        match self.try_reserve(self.len(), elems.len()) {
            Err(CapacityOverflow) => capacity_overflow(),
            Err(AllocError { layout, .. }) => handle_alloc_error(layout),
            Ok(x) => Ok(x)
        }
    }


    /// The same as `reserve`, but returns on errors instead of panicking or aborting.
    pub fn try_reserve(&mut self, len: usize, additional: usize) -> Result<NonNull<[T]>, TryReserveError> {
        if self.needs_to_grow(additional) {
            self.grow_amortized(len, additional)
        } else {
            Ok(())
        }
    }

    /// Ensures that the buffer contains at least enough space to hold `len +
    /// additional` elements. If it doesn't already, will reallocate the
    /// minimum possible amount of memory necessary. Generally this will be
    /// exactly the amount of memory necessary, but in principle the allocator
    /// is free to give back more than we asked for.
    ///
    /// If `len` exceeds `self.capacity()`, this may fail to actually allocate
    /// the requested space. This is not really unsafe, but the unsafe code
    /// *you* write that relies on the behavior of this function may break.
    ///
    /// # Panics
    ///
    /// Panics if the new capacity exceeds `isize::MAX` bytes.
    ///
    /// # Aborts
    ///
    /// Aborts on OOM.
    pub fn reserve_exact(&mut self, len: usize, additional: usize) {
        match self.try_reserve_exact(len, additional) {
            Err(CapacityOverflow) => capacity_overflow(),
            Err(AllocError { layout, .. }) => handle_alloc_error(layout),
            Ok(()) => { /* yay */ }
        }
    }

    /// The same as `reserve_exact`, but returns on errors instead of panicking or aborting.
    pub fn try_reserve_exact(
        &mut self,
        len: usize,
        additional: usize,
    ) -> Result<(), TryReserveError> {
        if self.needs_to_grow(additional) { self.grow_exact(len, additional) } else { Ok(()) }
    }

    /// Shrinks the allocation down to the specified amount. If the given amount
    /// is 0, actually completely deallocates.
    ///
    /// # Panics
    ///
    /// Panics if the given amount is *larger* than the current capacity.
    ///
    /// # Aborts
    ///
    /// Aborts on OOM.
    pub fn shrink_to_fit(&mut self, amount: usize) {
        match self.shrink(amount) {
            Err(CapacityOverflow) => capacity_overflow(),
            Err(AllocError { layout, .. }) => handle_alloc_error(layout),
            Ok(()) => { /* yay */ }
        }
    }
}

impl<T, A: AllocRef> SRawVec<T, A> {
    /// Returns if the buffer needs to grow to fulfill the needed extra capacity.
    /// Mainly used to make inlining reserve-calls possible without inlining `grow`.
    fn needs_to_grow(&self, additional: usize) -> bool {
        additional > self.capacity().wrapping_sub(self.len.load(Relaxed))
    }

    fn capacity_from_bytes(excess: usize) -> usize {
        debug_assert_ne!(mem::size_of::<T>(), 0);
        excess / mem::size_of::<T>()
    }

    // This method is usually instantiated many times. So we want it to be as
    // small as possible, to improve compile times. But we also want as much of
    // its contents to be statically computable as possible, to make the
    // generated code run faster. Therefore, this method is carefully written
    // so that all of the code that depends on `T` is within it, while as much
    // of the code that doesn't depend on `T` as possible is in functions that
    // are non-generic over `T`.
    fn grow_amortized(&mut self, len: usize, additional: usize) -> Result<(), TryReserveError> {
        // This is ensured by the calling contexts.
        debug_assert!(additional > 0);

        if mem::size_of::<T>() == 0 {
            // Since we return a capacity of `usize::MAX` when `elem_size` is
            // 0, getting to here necessarily means the `RawVec` is overfull.
            return Err(CapacityOverflow);
        }

        // Nothing we can really do about these checks, sadly.
        let required_cap = len.checked_add(additional).ok_or(CapacityOverflow)?;

        // This guarantees exponential growth. The doubling cannot overflow
        // because `cap <= isize::MAX` and the type of `cap` is `usize`.
        let cap = cmp::max(self.cap.load(Relaxed) * 2, required_cap);

        // Tiny Vecs are dumb. Skip to:
        // - 8 if the element size is 1, because any heap allocators is likely
        //   to round up a request of less than 8 bytes to at least 8 bytes.
        // - 4 if elements are moderate-sized (<= 1 KiB).
        // - 1 otherwise, to avoid wasting too much space for very short Vecs.
        // Note that `min_non_zero_cap` is computed statically.
        let elem_size = mem::size_of::<T>();
        let min_non_zero_cap = if elem_size == 1 {
            8
        } else if elem_size <= 1024 {
            4
        } else {
            1
        };
        let cap = cmp::max(min_non_zero_cap, cap);

        let new_layout = Layout::array::<T>(cap);

        // `finish_grow` is non-generic over `T`.
        let ptr = finish_grow(new_layout, self.current_memory(), &mut self.alloc)?;
        self.set_ptr(ptr);
        Ok(())
    }

    // The constraints on this method are much the same as those on
    // `grow_amortized`, but this method is usually instantiated less often so
    // it's less critical.
    fn grow_exact(&mut self, len: usize, additional: usize) -> Result<(), TryReserveError> {
        if mem::size_of::<T>() == 0 {
            // Since we return a capacity of `usize::MAX` when the type size is
            // 0, getting to here necessarily means the `RawVec` is overfull.
            return Err(CapacityOverflow);
        }

        let cap = len.checked_add(additional).ok_or(CapacityOverflow)?;
        let new_layout = Layout::array::<T>(cap);

        // `finish_grow` is non-generic over `T`.
        let ptr = finish_grow(new_layout, self.current_memory(), &mut self.alloc)?;
        self.set_ptr(ptr);
        Ok(())
    }

    fn shrink(&mut self, amount: usize) -> Result<(), TryReserveError> {
        assert!(amount <= self.capacity(), "Tried to shrink to a larger capacity");

        let (ptr, layout) = if let Some(mem) = self.current_memory() { mem } else { return Ok(()) };
        let new_size = amount * mem::size_of::<T>();

        let ptr = unsafe {
            self.alloc.shrink(ptr, layout, new_size).map_err(|_| TryReserveError::AllocError {
                layout: Layout::from_size_align_unchecked(new_size, layout.align()),
                non_exhaustive: (),
            })?
        };
        self.set_ptr(ptr);
        Ok(())
    }
}

// This function is outside `RawVec` to minimize compile times. See the comment
// above `RawVec::grow_amortized` for details. (The `A` parameter isn't
// significant, because the number of different `A` types seen in practice is
// much smaller than the number of `T` types.)
fn finish_grow<A>(
    new_layout: Result<Layout, LayoutErr>,
    current_memory: Option<(NonNull<u8>, Layout)>,
    alloc: &mut A,
) -> Result<NonNull<[u8]>, TryReserveError>
where
    A: AllocRef,
{
    // Check for the error here to minimize the size of `RawVec::grow_*`.
    let new_layout = new_layout.map_err(|_| CapacityOverflow)?;

    alloc_guard(new_layout.size())?;

    let memory = if let Some((ptr, old_layout)) = current_memory {
        debug_assert_eq!(old_layout.align(), new_layout.align());
        unsafe { alloc.grow(ptr, old_layout, new_layout.size()) }
    } else {
        alloc.alloc(new_layout)
    }
    .map_err(|_| AllocError { layout: new_layout, non_exhaustive: () })?;

    Ok(memory)
}

unsafe impl<#[may_dangle] T, A: AllocRef> Drop for SRawVec<T, A> {
    /// Frees the memory owned by the `RawVec` *without* trying to drop its contents.
    fn drop(&mut self) {
        if let Some((ptr, layout)) = self.current_memory() {
            unsafe { self.alloc.dealloc(ptr, layout) }
        }
    }
}

// We need to guarantee the following:
// * We don't ever allocate `> isize::MAX` byte-size objects.
// * We don't overflow `usize::MAX` and actually allocate too little.
//
// On 64-bit we just need to check for overflow since trying to allocate
// `> isize::MAX` bytes will surely fail. On 32-bit and 16-bit we need to add
// an extra guard for this in case we're running on a platform which can use
// all 4GB in user-space, e.g., PAE or x32.

#[inline]
fn alloc_guard(alloc_size: usize) -> Result<(), TryReserveError> {
    if mem::size_of::<usize>() < 8 && alloc_size > isize::MAX as usize {
        Err(CapacityOverflow)
    } else {
        Ok(())
    }
}

// One central function responsible for reporting capacity overflows. This'll
// ensure that the code generation related to these panics is minimal as there's
// only one location which panics rather than a bunch throughout the module.
fn capacity_overflow() -> ! {
    panic!("capacity overflow");
}