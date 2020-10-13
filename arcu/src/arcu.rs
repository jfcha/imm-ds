use core::borrow;
use core::future::Future;
use core::isize;
use core::marker::{PhantomData, Unpin};
use core::ops::{Deref, Drop};
use core::pin::Pin;
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering::*};
use core::task::{Context, Poll, Waker};
use std::process;
use std::sync::Mutex;

const MAX_REFCOUNT: usize = (isize::MAX) as usize;

struct ArcuInner<T> {
    count: AtomicUsize,
    forward: AtomicPtr<ArcuInner<T>>,
    data: T,
    callbacks: Mutex<Vec<Waker>>,
}

pub struct Arcu<T> {
    ptr: NonNull<ArcuInner<T>>,
    phantom: PhantomData<ArcuInner<T>>,
}

impl<T> Future for Arcu<T> {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let iself = self.inner();
        if iself.count.load(Relaxed) == 1 && iself.forward.load(Relaxed) == ptr::null_mut() {
            Poll::Ready(())
        } else if iself.forward.load(Relaxed) == ptr::null_mut() {
            let mut lock = iself.callbacks.lock().expect("lock shouldn't fail?");
            lock.push(cx.waker().clone());
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }
}

impl<T> Clone for Arcu<T> {
    #[inline]
    fn clone(&self) -> Self {
        // SAFETY: We have a refence on this thread so it can't be deleted
        let old_size = self.inner().count.fetch_add(1, Relaxed);
        if old_size > MAX_REFCOUNT {
            process::abort();
        }
        Self {
            ptr: self.ptr,
            phantom: PhantomData::default(),
        }
    }
}

impl<T> Deref for Arcu<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.inner().data
    }
}

impl<T> Unpin for Arcu<T> {}

impl<T> borrow::Borrow<T> for Arcu<T> {
    fn borrow(&self) -> &T {
        &**self
    }
}

impl<T> AsRef<T> for Arcu<T> {
    fn as_ref(&self) -> &T {
        &**self
    }
}

impl<T> Drop for ArcuInner<T> {
    fn drop(&mut self) {
        // I think this can be relaxed because all callers would have acquired right before this
        let ptr = self.forward.load(Relaxed);
        if !ptr.is_null() {
            // drop ref creates a memory barrier
            unsafe { drop_ref(ptr) };
        }
    }
}

impl<T> Drop for Arcu<T> {
    fn drop(&mut self) {
        let ptr = self.ptr.as_ptr();
        unsafe { drop_ref(ptr) };
    }
}

#[inline]
unsafe fn drop_ref<T>(ptr: *mut ArcuInner<T>) {
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
unsafe fn drop_slow<T>(ptr: *mut ArcuInner<T>) {
    ptr::drop_in_place(ptr);
}

impl<T> Arcu<T> {
    pub fn pin(data: T) -> Pin<Arcu<T>> {
        unsafe { Pin::new_unchecked(Arcu::new(data)) }
    }

    #[inline]
    pub fn new(data: T) -> Arcu<T> {
        let x: Box<_> = Box::new(ArcuInner {
            count: AtomicUsize::new(1),
            forward: AtomicPtr::default(),
            data,
            callbacks: Mutex::new(Vec::new()),
        });
        Arcu {
            ptr: Box::leak(x).into(),
            phantom: PhantomData::default(),
        }
    }

    #[inline]
    fn inner(&self) -> &ArcuInner<T> {
        unsafe { self.ptr.as_ref() }
    }

    pub fn ref_count(&self) -> usize {
        self.inner().count.load(Relaxed)
    }

    pub fn has_update(&self) -> bool {
        let ptr = self.inner().forward.load(Relaxed);
        if ptr.is_null() {
            false
        } else {
            true
        }
    }

    pub fn update(&mut self) -> bool {
        let ptr = self.inner().forward.load(Acquire);
        if ptr.is_null() {
            false
        } else {
            self.update_ptr(ptr);
            true
        }
    }

    pub fn update_latest(&mut self) -> bool {
        let mut ptr = self.inner().forward.load(Acquire);
        if ptr.is_null() {
            false
        } else {
            loop {
                let n_ptr = unsafe { (*ptr).forward.load(Acquire) };
                if n_ptr.is_null() {
                    break;
                } else {
                    ptr = n_ptr;
                }
            }
            self.update_ptr(ptr);
            true
        }
    }

    #[inline]
    fn update_ptr(&mut self, ptr: *mut ArcuInner<T>) {
        let cur_ptr = self.ptr;
        let old_size = unsafe { (*ptr).count.fetch_add(1, Relaxed) };
        if old_size > MAX_REFCOUNT {
            process::abort();
        }
        self.ptr = NonNull::new(ptr).unwrap();
        // drop ref establishes a memory barrier
        unsafe {
            drop_ref(cur_ptr.as_ptr());
        }
    }

    #[inline]
    pub fn update_value(&mut self, data: T) {
        let x: Box<_> = Box::new(ArcuInner {
            count: AtomicUsize::new(1),
            forward: AtomicPtr::default(),
            data,
            callbacks: Mutex::new(Vec::new()),
        });
        let new_ptr = Box::leak(x);
        let mut cur_point = self.ptr.as_ptr();
        // we just update the forward pointer, updating self to point to the new reference will be done on deref
        loop {
            match unsafe {
                (*cur_point).forward.compare_exchange_weak(
                    ptr::null_mut(),
                    new_ptr,
                    Release,
                    Relaxed,
                )
            } {
                Ok(_) => {
                    let mut cbs = unsafe { (*cur_point).callbacks.lock().unwrap() };
                    while let Some(c) = cbs.pop() {
                        c.wake();
                    }
                    return;
                }
                Err(e) => {
                    cur_point = e;
                }
            }
        }
    }
}
