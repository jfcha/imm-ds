use alloc::alloc::{handle_alloc_error, AllocRef, Layout, Global};

use core::ops::Index;
use core::slice::SliceIndex;

use core::cell::UnsafeCell;
use core::cmp;
use core::marker::PhantomData;
use core::mem::{self, ManuallyDrop};
use core::ops::Deref;
use core::ptr::{self, NonNull};
use core::slice;
use core::sync::atomic::{AtomicIsize, AtomicPtr, AtomicUsize, Ordering::*};
use core::fmt;
use tracing::{event, instrument, Level};

pub unsafe auto trait Freeze {}
impl<T: ?Sized> !Freeze for UnsafeCell<T> {}
unsafe impl<T: ?Sized> Freeze for &T {}
unsafe impl<T: ?Sized> Freeze for &mut T {}
unsafe impl<T: ?Sized> Freeze for *const T {}
unsafe impl<T: ?Sized> Freeze for *mut T {}
unsafe impl<T: ?Sized> Freeze for PhantomData<T> {}

pub struct ArcLog<T, A: AllocRef + Freeze = Global> {
    ptr: NonNull<ArcLogInner<T, A>>,
    pd: PhantomData<ArcLogInner<T, A>>,
}

impl<T: Freeze + Sync> ArcLog<T> {
    pub fn new() -> Self {
        ArcLog::new_in(Global)
    }
    pub fn with_capacity(capacity: usize) -> Self {
        ArcLog::with_capacity_in(capacity, Global)
    }
}

unsafe impl<T, A: AllocRef + Freeze> Send for ArcLog<T, A> {}
// nothing prevents ArcLog from being Sync, but we may way to reserve
// this for future optimization, like keeping a local len value
//unsafe impl<T, A: AllocRef + Freeze> Sync for ArcLog<T, A> {}
impl<T, A: AllocRef + Freeze> Unpin for ArcLog<T, A> {}

impl<T, A: AllocRef + Freeze> Drop for ArcLog<T, A> {
    fn drop(&mut self) {
        drop_ref(self.ptr);
    }
}

// there is likely a decision to make as to whether we should include Ts
// and have bound, or ignore them so you can always debug
impl<T: fmt::Debug, A: AllocRef + Freeze> fmt::Debug for ArcLog<T,A>{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = self.get_inner();
        let len_raw = inner.header.len.load(Relaxed);
        let len = if len_raw.is_negative() { !len_raw } else { len_raw };
        f.debug_struct("ArcLog")
        .field("count", &inner.header.count.load(Relaxed))
        .field("len_raw", &len_raw)
        .field("len", &len)
        .field("forward", &inner.header.forward.load(Relaxed))
        .field("data",  &&**self)
        .finish()
    }
}

impl<T, A: AllocRef + Freeze, I: SliceIndex<[T]>> Index<I> for ArcLog<T, A> {
    type Output = I::Output;
    #[inline]
    fn index(&self, index: I) -> &Self::Output {
        Index::index(&**self, index)
    }
}

impl<T, A: AllocRef + Freeze> Clone for ArcLog<T, A> {
    fn clone(&self) -> Self {
        self.get_inner().header.count.fetch_add(1, Relaxed);
        ArcLog {
            ptr: self.ptr,
            pd: PhantomData,
        }
    }
}

impl<T, A: AllocRef + Freeze> Deref for ArcLog<T, A> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        let inner = self.get_inner();
        let mut len = inner.header.len.load(Acquire);
        if len.is_negative() {
            len = !len;
        }
        unsafe { slice::from_raw_parts(&inner.data as *const T, len as usize) }
    }
}

#[inline(never)]
#[instrument]
fn drop_ref<T, A: AllocRef + Freeze>(ptr: NonNull<ArcLogInner<T, A>>) {
    event!(Level::TRACE, "enter drop_ref");
    let ref_ptr = unsafe { ptr.as_ref() };
    // this is release because we need to capture all loads before we deallocate.
    // in theory we only need need to do this for the last subtraction, but we don't
    // have another clear place to do this, and this is what Arc does...
    let prev_val = ref_ptr.header.count.fetch_sub(1, Release);
    if prev_val != 1 {
        event!(Level::TRACE, "more than one ref, no need to drop");
        return;
    }
    event!(Level::TRACE, "last ref so have to drop");
    // the previous check guarantees we have exclusive access
    let forward_ptr = ref_ptr.header.forward.load(Acquire);
    // functionally, this does nothing, but we need a release barrier here so writes don't creep up from the destructors
    // ref_ptr.header.count.store(0, Acquire);

    if forward_ptr.is_null() {
        let len_to_drop = ref_ptr.header.len.load(Relaxed) as usize;
        event!(
            Level::TRACE,
            "forward pointer is null, with {:?} items to drop",
            len_to_drop
        );
        unsafe {
            ptr::drop_in_place(ptr::slice_from_raw_parts_mut(
                ref_ptr.data.as_ptr() as *mut T,
                len_to_drop,
            ));
        }
        event!(Level::TRACE, "drop items");
    } else {
        // if there is a forwarding address, we can just deallocate because the forward is responsible for dropping the inners
        event!(Level::TRACE, "forward isn't null so we reenter drop_ref");
        // SAFETY: We just checked it was null and a forward pointer should always be valid
        drop_ref(unsafe { NonNull::new_unchecked(forward_ptr) });
        event!(Level::TRACE, "finished reentry of drop_ref");
    }
    let alloc_ref = unsafe { ManuallyDrop::take(&mut (*ptr.as_ptr()).header.alloc) };

    unsafe {
        event!(Level::TRACE, "calling dealloc");
        // SAFETY: We are the last reference so we need to deallocate
        alloc_ref.dealloc(
            NonNull::new_unchecked(ptr.as_ptr() as *mut u8),
            ArcLogInner::<T, A>::get_layout(ref_ptr.header.cap.load(Relaxed)),
        );
        event!(Level::TRACE, "dealloc completed");
    }
    if !forward_ptr.is_null() {
        let _ = ManuallyDrop::new(alloc_ref);
    }
}

impl<T, A: AllocRef + Freeze> ArcLog<T, A> {
    fn get_inner(&self) -> &ArcLogInner<T, A> {
        // SAFETY: This should always be safe as we keep reference counts
        unsafe { self.ptr.as_ref() }
    }
}

impl<T: Freeze + Sync, A: AllocRef + Freeze> ArcLog<T, A> {
    pub fn new_in(alloc: A) -> Self {
        ArcLog {
            ptr: ArcLogInner::with_capacity(0, alloc),
            pd: PhantomData,
        }
    }

    pub fn with_capacity_in(capacity: usize, alloc: A) -> Self {
        ArcLog {
            ptr: ArcLogInner::with_capacity(capacity, alloc),
            pd: PhantomData,
        }
    }

    #[instrument(skip(self))]
    pub fn update(&mut self) -> bool {
        let mut new_this = self.get_inner().header.forward.load(Acquire);
        if new_this.is_null() {
            false
        } else {
            // SAFETY: We just checked for null, and forward must be valid if it exists
            let mut this = unsafe { &*new_this };
            loop {
                // this has to be acquire, because we may access data after this forward
                new_this = this.header.forward.load(Acquire);
                if new_this.is_null() {
                    event!(Level::TRACE, "at end of forward change");
                    break;
                } else {
                    event!(Level::TRACE, "had to forward");
                    // SAFETY: We just checked for null, and forward must be valid if it exists
                    this = unsafe { &*new_this };
                }
            }
            let old_ptr = self.ptr;
            self.ptr = this.into();
            // this can be relaxed because only the count matters
            // and we already did an acquire when we got the pointer
            this.header.count.fetch_add(1, Relaxed);
            drop_ref(old_ptr);
            true
        }
    }

    fn finish_push(&mut self, index: isize, o_ptr: Option<NonNull<ArcLogInner<T, A>>>, item: T) -> Result<usize, T> {
        if let Some(new_ptr) = o_ptr {
            event!(Level::TRACE, "post alloc_one");
            let old_ptr = self.ptr;
            unsafe { new_ptr.as_ref().header.count.fetch_add(1, Relaxed) };
            self.ptr = new_ptr;
            event!(Level::TRACE, "pre-drop ref");
            drop_ref(old_ptr);
            event!(Level::TRACE, "post drop ref")
        }
        event!(Level::TRACE, "finished push");
        if index == -1 { Err(item) } else {
            let _ = ManuallyDrop::new(item);
            Ok(index as usize)
        }        
    }

    #[instrument(skip(self, item))]
    pub fn push_spin(&mut self, item: T) -> usize {
        let (index, o_ptr ) = self.get_inner().alloc_items(&item, 1, isize::MAX);
        match self.finish_push(index, o_ptr, item){
            Ok(i) => { i }
            Err(_) => unreachable!()
        }
    }

    #[instrument(skip(self, item))]
    pub fn push_or_return(&mut self, item: T) -> Result<usize, T> {
        let (index, o_ptr ) = self.get_inner().alloc_items_one_shot(&item, 1, isize::MAX);
        self.finish_push(index, o_ptr, item)
    }
    #[instrument(skip(self, item))]
    pub fn push_spin_by_index(&mut self, item: T, index: usize) -> Result<usize, T> {
        let (index, o_ptr ) = self.get_inner().alloc_items(&item, 1, index as isize);
        self.finish_push(index, o_ptr, item)
            
    }
    pub fn push_or_return_by_index(&mut self, item: T, index: usize) -> Result<usize, T> {
        let (index, o_ptr ) = self.get_inner().alloc_items_one_shot(&item, 1, index as isize);
        self.finish_push(index, o_ptr, item)        
    }

}

// capacity and len should not change once we have a non-null forward pointer
struct ArcLogInnerHeader<T, A: AllocRef + Freeze> {
    count: AtomicUsize,
    // AllocRef does not support grow in place (it currently does not)
    // so this could just be a usize as it will not actually change
    cap: AtomicUsize,
    // len can never be greater than cap
    len: AtomicIsize,
    // forward will be a null-ptr if there is no forward node

    // if alloc wasn't copy, we could move it when we created a forward
    // and the forward and alloc could overlap bytes... though if we moved
    // alloc we'd have to lock the forward node on this dealloc, or make
    // sure alloc was atomic (say a pointer). If alloc was a pointer, we could
    // potentially use a mask to identify between a forward or alloc ptr.
    // But given that alloc is most likely zero-size, keeping them forward and alloc separate
    // seems like the most pragmatic path
    forward: AtomicPtr<ArcLogInner<T, A>>,
    alloc: ManuallyDrop<A>,
}

// This is a repr(C) because we need to makes sure that data is at the end.
// The zero size array is used just to give a pointer to the len-sized data
// that is allocated
#[repr(C)]
struct ArcLogInner<T, A: AllocRef + Freeze> {
    header: ArcLogInnerHeader<T, A>,
    data: [T; 0],
}

impl<T, A: AllocRef + Freeze> ArcLogInner<T, A> {
    
    fn with_capacity(capacity: usize, alloc: A) -> NonNull<Self> {
        let new_alloc = alloc
            .alloc(Self::get_layout(capacity))
            .expect("Error allocating")
            .as_mut_ptr();
        // SAFETY: The alloc was just made with T layout so this cast is safe
        let ptr = unsafe { &mut *(new_alloc as *mut Self) };
        ptr.header.forward.store(ptr::null_mut(), Relaxed);
        ptr.header.count.store(1, Relaxed);
        ptr.header.cap.store(
            if mem::size_of::<T>() == 0 {
                isize::MAX as usize
            } else {
                capacity
            },
            Relaxed,
        );
        ptr.header.alloc = ManuallyDrop::new(alloc);
        ptr.header.len.store(0, Release);
        ptr.into()
    }

    #[instrument(skip(this))]
    fn get_next_address(mut this: &Self) -> &Self {
        loop {
            // this can be relaxed because we're only going to look at the forward
            let new_this = this.header.forward.load(Relaxed);
            if new_this.is_null() {
                event!(Level::TRACE, "at end of forward change");
                break;
            } else {
                event!(Level::TRACE, "had to forward");
                // SAFETY: We check for null, so this must point to valid reference
                this = unsafe { &(*new_this) };
            }
        }
        this
    }

    #[instrument(skip(self))]
    fn alloc_items_one_shot(
        &self,
        data_ptr: *const T,
        count: usize,
        ref_index: isize,
    ) -> (isize, Option<NonNull<Self>>) {
        event!(Level::TRACE, "enter alloc items one shot");
        debug_assert!(count > 0);
        let size_of_t = mem::size_of::<T>();
        if size_of_t == 0 {
            event!(Level::TRACE, "is zero sized");
            let len = self.header.len.load(Acquire);
            if len.is_negative() || len > ref_index as isize { return (-1, None); }
            else {
                // TODO: Find out what nightly Vec with alloc does here
                let not_len = !len;
                let new_len = len.checked_add(count as isize).expect("Too many entries");
                match self
                    .header
                    .len
                    .compare_exchange(len, not_len, Relaxed, Relaxed)
                {
                    Ok(_old_claim_val) => {
                        self.header.len.store(new_len, Release);
                        return (len, None);
                    }
                    Err(_old_claim_val) => { return (-1, None); }
                }
            }
        } else {
            let mut this = self;
            event!(Level::TRACE, "is not zero sized");
            this = Self::get_next_address(this);
            event!(Level::TRACE, "got new address");
            // we reached the end of the forwarding so we have have to set an acquire barrier so that
            // everything is in sync
            let len = this.header.len.load(Acquire);
            if len.is_negative() || len > ref_index {
                return if this as *const _ == self as *const _ { (-1, None) } else { (-1, Some(this.into())) };    
            }
            let negative_len = !len;
            // this should still respect boundary set by len
            let new_len = len.checked_add(count as isize).expect("Too many entries");
            let cap = this.header.cap.load(Relaxed);
            if new_len as usize > cap {
                event!(Level::TRACE, "had to reallocate");
                // This guarantees exponential growth. The doubling cannot overflow
                // because `cap <= isize::MAX` and the type of `cap` is `usize`.
                let n_cap = cmp::max(cap * 2, new_len as usize);

                let elem_size = size_of_t;
                let min_non_zero_cap = if elem_size == 1 {
                    8
                } else if elem_size <= 1024 {
                    4
                } else {
                    1
                };
                let n_cap = cmp::max(min_non_zero_cap, n_cap);
                event!(Level::TRACE, "new allocation has this size {:?}", n_cap);
                let req_layout = Self::get_layout(n_cap);
                event!(Level::TRACE, "got new layout");
                let req_size = req_layout.size();
                match this
                        .header
                        .len
                        .compare_exchange(len, negative_len, Relaxed, Relaxed)
                    {
                        Ok(_old_claim_val) => {
                            event!(Level::TRACE, "got claim to add entries");

                            // while we waited for the lock someone may have added a forwarding address which would means
                            // we might be trying to write to the wrong place
                            let new_forward = this.header.forward.load(Relaxed);
                            if !new_forward.is_null() {
                                event!(
                                    Level::TRACE,
                                    "forward was updated while grabbing claim, fail"
                                );
                                // we aren't at the most up to date location, so reset the lock and restart from
                                // a more up to date location
                                this.header.len.store(len, Relaxed);
                                return (-1, Some(unsafe {NonNull::new_unchecked(this as *const _ as *mut _)}));
                            }
                            let ptr =  self.header.alloc.alloc(req_layout).unwrap_or_else(|_| handle_alloc_error(req_layout));
                            event!(Level::TRACE, "got allocation");
                            let new_mut_ref = unsafe {
                                let new_alloc_len = ptr.as_ref().len();
                                event!(
                                            Level::TRACE,
                                            "Old ptr: {:?} new_ptr: {:?} with len: {:?}",
                                            this as *const _,
                                            ptr,
                                            new_alloc_len
                                        );
                                assert_eq!(new_alloc_len, req_size);

                                let new_mut_ptr = ptr.as_mut_ptr() as *mut Self;

                                ptr::copy_nonoverlapping(this, new_mut_ptr, 1);
                                ptr::copy_nonoverlapping(
                                            this.data.as_ptr(),
                                            (*new_mut_ptr).data.as_mut_ptr(),
                                            len as usize,
                                        );
                                event!(
                                            Level::TRACE,
                                            "start writing new data to new allocation"
                                        );
                                ptr::copy_nonoverlapping(
                                            data_ptr,
                                            (*new_mut_ptr).data.as_mut_ptr().offset(len),
                                            count,
                                        );
                                &mut *new_mut_ptr
                            };
                            event!(Level::TRACE, "updating automics");
                            new_mut_ref.header.count.store(1, Relaxed);
                            new_mut_ref.header.cap.store(n_cap, Relaxed);
                            new_mut_ref.header.len.store(new_len, Relaxed);

                            // the data has to be ready once we update the forward ptr,
                            // so this must be a release
                            this.header.forward.store(new_mut_ref, Release);
                            // this should also be release, otherwise it could be moved before the forward
                            // and the forward must be seen by the next write
                            this.header.len.store(len, Release);
                            return (len, Some(new_mut_ref.into()));    
                        },
                        Err(_old_claim_val) => {
                            event!(Level::TRACE, "couldn't get claim");
                            return if this as *const _ == self as *const _ { (-1, None) } else { (-1, Some(this.into())) };
                        }
                    }
                } else {
                    match this
                        .header
                        .len
                        .compare_exchange(len, negative_len, Relaxed, Relaxed)
                    {
                        Ok(_old_claim_val) => {
                            // while we waited for the lock someone may have added a forwarding address which would means
                            // we might be trying to write to the wrong place
                            let new_forward = this.header.forward.load(Relaxed);
                            if !new_forward.is_null() {
                                // we aren't at the most up to date location, so reset the lock and restart from
                                // a more up to date location
                                this.header.len.store(len, Relaxed);

                                this.header.len.store(len, Relaxed);
                                return (-1, Some(unsafe {NonNull::new_unchecked(this as *const _ as *mut _)}));
                            }
                            // we can just add our data an and update the len
                            unsafe {
                                ptr::copy_nonoverlapping(
                                    data_ptr,
                                    (this.data.as_ptr() as *mut T).offset(len as isize),
                                    count,
                                );
                            }
                            this.header.len.store(new_len, Release);
                            if this as *const Self == self as *const Self {
                                return (len, None);
                            } else {
                                return (len, Some(this.into()));
                            }
                            
                        }
                        Err(_old_claim_val) => {
                            return if this as *const _ == self as *const _ { (-1, None) } else { (-1, Some(this.into())) };
                        }
                    
                } 
            }
        }

    }


    #[instrument(skip(self))]
    fn alloc_items(
        &self,
        data_ptr: *const T,
        count: usize,
        ref_index: isize,
    ) -> (isize, Option<NonNull<Self>>) {
        event!(Level::TRACE, "enter alloc items");
        debug_assert!(count > 0);
        let size_of_t = mem::size_of::<T>();
        if size_of_t == 0 {
            event!(Level::TRACE, "is zero sized");
            // if ths size of T is zero, there will never be a need to forward so we can do a much tighter loop
            loop {
                let mut len = self.header.len.load(Acquire);
                while len.is_negative() {
                    // maybe we should sleep?
                    len = self.header.len.load(Acquire);
                }
                
                if len > ref_index {
                    return (-1, None);
                } 
                // this should still respect boundary set by len
                let negative_len = !len;
                let new_len = len.checked_add(count as isize).expect("Too many entries");
                match self
                    .header
                    .len
                    .compare_exchange_weak(len, negative_len, Relaxed, Relaxed)
                {
                    Ok(_old_claim_val) => {
                        self.header.len.store(new_len, Release);
                        return (len, None);
                    }
                    Err(_old_claim_val) => {}
                }
            }
        } else {
            let mut this = self;

            event!(Level::TRACE, "is not zero sized");
            loop {
                this = Self::get_next_address(this);
                event!(Level::TRACE, "got new address");
                // we reached the end of the forwarding so we have have to set an acquire barrier so that
                // everything is in sync
                let mut len = this.header.len.load(Acquire);
                while len.is_negative() {
                    // maybe we should sleep?
                    len = this.header.len.load(Acquire);
                }
                if len > ref_index {
                    return if this as *const _ == self as *const _ { (-1, None) } else { (-1, Some(this.into())) };
                }
                let negative_len = !len;

                // this should still respect boundary set by len
                let new_len = len.checked_add(count as isize).expect("Too many entries");

                let cap = this.header.cap.load(Relaxed);

                if new_len as usize > cap {
                    event!(Level::TRACE, "had to reallocate");
                    // This guarantees exponential growth. The doubling cannot overflow
                    // because `cap <= isize::MAX` and the type of `cap` is `usize`.
                    let n_cap = cmp::max(cap * 2, new_len as usize);

                    let elem_size = size_of_t;
                    let min_non_zero_cap = if elem_size == 1 {
                        8
                    } else if elem_size <= 1024 {
                        4
                    } else {
                        1
                    };
                    let n_cap = cmp::max(min_non_zero_cap, n_cap);
                    event!(Level::TRACE, "new allocation has this size {:?}", n_cap);
                    let req_layout = Self::get_layout(n_cap);
                    event!(Level::TRACE, "got new layout");
                    let req_size = req_layout.size();
                    match this
                        .header
                        .len
                        .compare_exchange_weak(len, negative_len, Relaxed, Relaxed)
                    {
                        Ok(_old_claim_val) => {
                            event!(Level::TRACE, "got claim to add entries");

                            // while we waited for the lock someone may have added a forwarding address which would means
                            // we might be trying to write to the wrong place
                            let new_forward = this.header.forward.load(Relaxed);
                            if !new_forward.is_null() {
                                event!(
                                    Level::TRACE,
                                    "forward was updated while grabbing claim, restart loop"
                                );
                                // we aren't at the most up to date location, so reset the lock and restart from
                                // a more up to date location
                                this.header.len.store(len, Relaxed);

                                this = unsafe { &*new_forward };
                                event!(
                                    Level::TRACE,
                                    "forward has been updated since lock, so spinning again"
                                );
                                continue;
                            }
                            let ptr =  self.header.alloc.alloc(req_layout).unwrap_or_else(|_| handle_alloc_error(req_layout));
                            event!(Level::TRACE, "got allocation");
                            let new_mut_ref = unsafe {
                                        let new_alloc_len = ptr.as_ref().len();
                                        event!(
                                            Level::TRACE,
                                            "Old ptr: {:?} new_ptr: {:?} with len: {:?}",
                                            this as *const _,
                                            ptr,
                                            new_alloc_len
                                        );
                                        assert_eq!(new_alloc_len, req_size);

                                        let new_mut_ptr = ptr.as_mut_ptr() as *mut Self;

                                        ptr::copy_nonoverlapping(this, new_mut_ptr, 1);
                                        ptr::copy_nonoverlapping(
                                            this.data.as_ptr(),
                                            (*new_mut_ptr).data.as_mut_ptr(),
                                            len as usize,
                                        );
                                        event!(
                                            Level::TRACE,
                                            "start writing new data to new allocation"
                                        );
                                        ptr::copy_nonoverlapping(
                                            data_ptr,
                                            (*new_mut_ptr).data.as_mut_ptr().offset(len),
                                            count,
                                        );
                                        &mut *new_mut_ptr
                                    };
                                    event!(Level::TRACE, "updating automics");
                                    new_mut_ref.header.count.store(1, Relaxed);
                                    new_mut_ref.header.cap.store(n_cap, Relaxed);
                                    new_mut_ref.header.len.store(new_len, Relaxed);

                                    // the data has to be ready once we update the forward ptr,
                                    // so this must be a release
                                    this.header.forward.store(new_mut_ref, Release);
                                    // this should also be release, otherwise it could be moved before the forward
                                    // and the forward must be seen by the next write
                            this.header.len.store(len, Release);
                            return (len, Some(new_mut_ref.into()));
                                
                        },
                        Err(_old_claim_val) => {
                            event!(Level::TRACE, "couldn't get claim");
                        }
                    }
                } else {
                    match this
                        .header
                        .len
                        .compare_exchange_weak(len, negative_len, Relaxed, Relaxed)
                    {
                        Ok(_old_claim_val) => {
                            // while we waited for the lock someone may have added a forwarding address which would means
                            // we might be trying to write to the wrong place
                            let new_forward = this.header.forward.load(Relaxed);
                            if !new_forward.is_null() {
                                // we aren't at the most up to date location, so reset the lock and restart from
                                // a more up to date location
                                this.header.len.store(len, Relaxed);

                                this = unsafe { &*new_forward };
                                continue;
                            }
                            // we can just add our data an and update the len
                            unsafe {
                                ptr::copy_nonoverlapping(
                                    data_ptr,
                                    (this.data.as_ptr() as *mut T).offset(len as isize),
                                    count,
                                );
                            }
                            this.header.len.store(new_len, Release);
                            if this as *const Self == self as *const Self {
                                return (len, None);
                            } else {
                                return (len, Some(this.into()));
                            }
                            
                        }
                        Err(_old_claim_val) => {}
                    }
                }
            }
        }
    }

    fn get_layout(data_cap: usize) -> Layout {
        unsafe {
            let align = mem::align_of::<Self>();
            let size = mem::size_of::<Self>();
            let layout = Layout::from_size_align_unchecked(size, align);
            let layout_data = match Layout::array::<T>(data_cap) {
                Ok(layout) => layout,
                _ => panic!("Bad array layout"),
            };
            let layout = match layout.extend(layout_data) {
                Ok((layout, _)) => layout,
                _ => panic!("Bad layout"),
            };
            event!(
                Level::TRACE,
                "layout dims, data_cap: {:?}, size: {:?}, align: {:?}",
                data_cap,
                layout.size(),
                layout.align()
            );
            layout //.pad_to_align() not sure if this is needed?
                   // if it is, might have to be accounted for on ptr copy
        }
    }
}


