use core::marker::PhantomData;
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering::*};
use core::ops::{Drop, Deref};
use core::isize;
use std::process;
use core::borrow;

const MAX_REFCOUNT: usize = (isize::MAX) as usize;

struct RefInner<T> {
    count: AtomicUsize,
    forward: AtomicPtr<RefInner<T>>,
    data: T,
}


pub struct Ref<T> {
    ptr: NonNull<RefInner<T>>,
    phantom: PhantomData<RefInner<T>>,
}

pub struct UpdatingRef<T> {
    ptr: AtomicPtr<RefInner<T>>,
    phantom: PhantomData<RefInner<T>>,
}


impl <T> Clone for Ref<T> {
    #[inline]
    fn clone(&self) -> Self {
        /// SAFETY: We have a refence on this thread so it can't be deleted
        let old_size = self.inner().count.fetch_add(1, Relaxed);
        if old_size > MAX_REFCOUNT {
            process::abort();
        }
        Self {
            ptr: self.ptr,
            phantom: PhantomData::default()
        }
    }
}

impl <T> Clone for UpdatingRef<T> {
    #[inline]
    fn clone(&self) -> Self {
        /// SAFETY: We have a refence on this thread so it can't be deleted
        let old_size = self.inner().count.fetch_add(1, Relaxed);
        if old_size > MAX_REFCOUNT {
            process::abort();
        }
        Self {
            ptr: AtomicPtr::new(self.ptr.load(Relaxed)),
            phantom: PhantomData::default()
        }
    }
}

impl<T> Deref for Ref<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.inner().data
    }
}


impl<T> borrow::Borrow<T> for Ref<T> {
    fn borrow(&self) -> &T {
        &**self
    }
}

impl<T> AsRef<T> for Ref<T> {
    fn as_ref(&self) -> &T {
        &**self
    }
}

impl<T> Unpin for Ref<T> {}

impl<T> Deref for UpdatingRef<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.inner().data
    }
}


impl<T> borrow::Borrow<T> for UpdatingRef<T> {
    fn borrow(&self) -> &T {
        &**self
    }
}

impl<T> AsRef<T> for UpdatingRef<T> {
    fn as_ref(&self) -> &T {
        &**self
    }
}

impl<T> Unpin for UpdatingRef<T> {}


impl <T> Drop for RefInner<T> {
    fn drop(&mut self) {
        // I think this can be relaxed because all callers would have acquired right before this
        let ptr = self.forward.load(Relaxed);
        if !ptr.is_null() {
            unsafe { drop_ref(ptr) };
        }
    }
}


impl <T> Drop for Ref<T> {
    fn drop(&mut self) {
        let ptr = self.ptr.as_ptr();
        unsafe { drop_ref(ptr) };
    }
}

impl <T> Drop for UpdatingRef<T> {
    fn drop(&mut self) {
        let ptr = self.ptr.load(Relaxed);
        unsafe { drop_ref(ptr) };
    }
}

#[inline]
unsafe fn drop_ref<T>(ptr: *mut RefInner<T>) {
    // release here because we want all of the changes before running the destructor
    if (*ptr).count.fetch_sub(1, Release) != 1 {
        return;
    } else {
        // 
        // this acquire is to prevent destructor code from creeping above
        (*ptr).count.load(Acquire);
        drop_slow(ptr)
    }
}

#[inline(never)]
unsafe fn drop_slow<T>(ptr: *mut RefInner<T>) {
    ptr::drop_in_place(ptr);
}

impl<T> Ref<T> {
    
    #[inline]
    pub fn new(data: T) -> Ref<T> {
        let x: Box<_> = Box::new(RefInner {
            count: AtomicUsize::new(1),
            forward: AtomicPtr::default(),
            data,
        });
        Ref {
            ptr: Box::leak(x).into(),
            phantom: PhantomData::default(),
        }
    }
    
    pub fn clone_to_updating(&self) -> UpdatingRef<T> {
        let old_size = self.inner().count.fetch_add(1, Relaxed);
        if old_size > MAX_REFCOUNT {
            process::abort();
        }
        UpdatingRef {
            ptr: AtomicPtr::new(self.ptr.as_ptr()),
            phantom: PhantomData::default()
        }
    }

    #[inline]
    fn inner(&self) -> &RefInner<T> {
        unsafe { self.ptr.as_ref() }
    }

    pub fn ref_count(&self) -> usize {
        self.inner().count.load(Relaxed)
    }
}


impl<T> UpdatingRef<T> {
    
    #[inline]
    pub fn new(data: T) -> UpdatingRef<T> {
        let x: Box<_> = Box::new(RefInner {
            count: AtomicUsize::new(1),
            forward: AtomicPtr::default(),
            data,
        });
        UpdatingRef {
            ptr: AtomicPtr::new(Box::leak(x)),
            phantom: PhantomData::default(),
        }
    }


    #[inline]
    pub fn update_to(&self, data: T) {
        let x: Box<_> = Box::new(RefInner {
            count: AtomicUsize::new(1),
            forward: AtomicPtr::default(),
            data,
        });
        let new_ptr = Box::leak(x);
        let mut cur_point = self.ptr.load(Relaxed);
        // we just update the forward pointer, updating self to point to the new reference will be done on deref
        loop {
            match unsafe { (*cur_point).forward.compare_exchange(ptr::null_mut(), new_ptr, Release, Relaxed)} {
                Ok(_) => {
                    return;
                },
                Err(e) => {
                    cur_point = e;
                }
            }
        }
    }

    pub fn clone_to_ref(&self) -> Ref<T> {
        let old_size = self.inner().count.fetch_add(1, Relaxed);
        if old_size > MAX_REFCOUNT {
            process::abort();
        }
        Ref {
            ptr: unsafe { NonNull::new_unchecked(self.ptr.load(Relaxed)) },
            phantom: PhantomData::default()
        }
    }

    #[inline]
    fn inner(&self) -> &RefInner<T> {
        unsafe { &*self.ptr.load(Relaxed) }
    }

    pub fn ref_count(&self) -> usize {
        self.inner().count.load(Relaxed)
    }

    #[inline]
    fn update_ptr(&mut self) {
        let old_ptr = self.ptr.load(Relaxed);
        let ptr = unsafe { (*old_ptr).forward.load(Relaxed) };
        if ptr.is_null() {
            return;
        }
        self.follow_ptr(old_ptr, ptr);
    }

    // next must not be null
    #[inline(never)]
    fn follow_ptr(&self, mut orig: *mut RefInner<T>, mut next: *mut RefInner<T>){
        loop {
            loop {
                let n_ptr = unsafe { (*next).forward.load(Relaxed) };
                if n_ptr.is_null() {
                    break;
                } else {
                    next = n_ptr;
                }
            }
            if orig == next {
                return;
            } else {
                // th
                match self.ptr.compare_exchange(orig, next, Relaxed, Relaxed) {
                    Ok(orig) => { 
                        // we updated the pointer so we need to update the count and potentially drop the old
                        unsafe {
                            let old_size = (*next).count.fetch_add(1, Relaxed);
                            if old_size > MAX_REFCOUNT {
                                process::abort();
                            }
                            // drop ref creates an Release barrier as all changes are captured  
                            drop_ref(orig);
                        }
                        return;
                    }
                    Err(e) => {
                        orig = e;
                        next = unsafe { (*orig).forward.load(Relaxed) };
                        if next.is_null() {
                            return;
                        }
                    }
                }
            }
        }
    }


    // fn update_ptr(&self) {
    //     let mut old_ptr = self.ptr.load(Relaxed);
    //     let mut ptr = unsafe { (*old_ptr).forward.load(Relaxed) };
    //     if ptr.is_null() {
    //         return;
    //     }
    //     loop {
    //         loop {
    //             let n_ptr = unsafe { (*ptr).forward.load(Relaxed) };
    //             if n_ptr.is_null() {
    //                 break;
    //             } else {
    //                 ptr = n_ptr;
    //             }
    //         }
    //         if old_ptr == ptr {
    //             return;
    //         } else {
    //             match self.ptr.compare_exchange(old_ptr, ptr, Relaxed, Relaxed) {
    //                 Ok(old_ptr) => {
    //                     unsafe {
    //                         drop_in_place(old_ptr);
    //                     }
    //                     return;
    //                 }
    //                 Err(e) => {
    //                     old_ptr = e;
    //                     ptr = unsafe { (*old_ptr).forward.load(Relaxed) };
    //                     if ptr.is_null() {
    //                         return;
    //                     }
    //                 }
    //             }
    //         }
    //     }
    // }

    // This is cleaner, but if loops don't unroll this may be slower in the common case
    // fn update_ptr(&self) {
    //     let mut old_ptr = self.ptr.load(Relaxed);
    //     loop {
    //         let mut ptr = old_ptr;
    //         loop {
    //             let n_ptr = unsafe { (*ptr).forward.load(Relaxed) };
    //             if n_ptr.is_null() {
    //                 break;
    //             } else {
    //                 ptr = n_ptr;
    //             }
    //         }
    //         if old_ptr == ptr {
    //             return;
    //         } else {
    //             match self.ptr.compare_exchange(old_ptr, ptr, Relaxed, Relaxed) {
    //                 Ok(old_ptr) => {
    //                     unsafe {
    //                         drop_in_place(old_ptr);
    //                     }
    //                     return;
    //                 }
    //                 Err(e) => {
    //                     old_ptr = e;
    //                 }
    //             }
    //         }
    //     }
    // }
}