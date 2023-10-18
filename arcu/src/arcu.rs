use core::{borrow, fmt};
use core::future::Future;
use core::isize;
use core::fmt;
use core::marker::{PhantomData, Unpin};
use core::ops::{Deref, Drop};
use core::pin::Pin;
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering::*};
use core::task::{Context, Poll, Waker};
use std::process;
use std::sync::Mutex;

//use tracing::{event, instrument, Level};

const MAX_REFCOUNT: usize = (isize::MAX) as usize;

struct ArcuInner<T> {
    count: AtomicUsize,
    forward: AtomicPtr<ArcuInner<T>>,
    data: T,
    // you only need to keep track of latest waker
    callback: NonNull<Mutex<Option<Waker>>>,
    cb_phantom: PhantomData<Mutex<Option<Waker>>>
}

pub struct Arcu<T> {
    ptr: NonNull<ArcuInner<T>>,
    phantom: PhantomData<ArcuInner<T>>,
}

impl<T> Future for Arcu<T> {
    type Output = Self;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let updated = self.update();
        if updated {
            Poll::Ready(self.clone())
        } else {
            let mut lock = unsafe { self.inner().callback.as_ref().lock().expect("lock shouldn't fail")};
            *lock = Some(cx.waker().clone());
            Poll::Pending
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

impl<T: fmt::Debug> fmt::Debug for Arcu<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = self.inner();
        f.debug_struct("Arcu")
            .field("forward", &inner.forward.load(Relaxed))
            .field("count", &inner.count.load(Relaxed))
            .field("data", &&**self)
            .finish()
    }    
}

impl<T> Drop for Arcu<T> {
    fn drop(&mut self) {
        let ptr = self.ptr.as_ptr();
        unsafe { drop_ref(ptr) };
    }
}

impl<T> Drop for ArcuInner<T> {
    fn drop(&mut self) {
        // I think this can be relaxed because all callers would have acquired right before this
        let ptr = self.forward.load(Relaxed);
        if !ptr.is_null() {
            // drop ref creates a memory barrier
            unsafe { drop_ref(ptr) };
        } else {
            unsafe { ptr::drop_in_place(self.callback.as_ptr()) };
        }
    }
}



#[inline]
unsafe fn drop_ref<T>(ptr: *mut ArcuInner<T>) {
    // release here because we want all of the changes before running the destructor
    if (*ptr).count.fetch_sub(1, Release) != 1 {
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
        let cb = Box::new(Mutex::new(None));
        let x: Box<_> = Box::new(ArcuInner {
            count: AtomicUsize::new(1),
            forward: AtomicPtr::default(),
            data,
            callback: Box::leak(cb).into(),
            cb_phantom: PhantomData::default()
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
        !ptr.is_null()
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
        let inner = self.inner();
        let x: Box<_> = Box::new(ArcuInner {
            count: AtomicUsize::new(1),
            forward: AtomicPtr::default(),
            data,
            callback: inner.callback,
            cb_phantom: PhantomData::default()
        });
        let new_ptr = Box::leak(x);
        //let orig_ptr 
        let mut cur_point = inner as *const _ as *mut ArcuInner<T>;
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
                    let mut cbs = unsafe { inner.callback.as_ref().lock().unwrap() };
                    if let Some(c) = cbs.take() {
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
// there is a decision to make as to whether we should include Ts
// and have bound, or ignore them so you can always debug
impl<T: fmt::Debug> fmt::Debug for Arcu<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = self.inner();
        f.debug_struct("Arcu")
            .field("ptr", &self.ptr)
            .field("forward", &inner.forward.load(Relaxed))
            .field("count", &inner.count.load(Relaxed))
            .field("data", &inner.data)
            .finish()
    }
}