use alloc::alloc::{handle_alloc_error, Allocator, Global, Layout, LayoutError};
// use alloc::collections::TryReserveError::{self, *};

use core::cell::UnsafeCell;
use core::cmp;
use core::fmt;
use core::marker::PhantomData;
use core::mem::{self, ManuallyDrop, MaybeUninit};
use core::ops::Deref;
use core::ops::Index;
use core::ptr::Pointee;
use core::ptr::Thin;
use core::ptr::addr_of;
use core::ptr::addr_of_mut;
use core::ptr::{self, NonNull};
use core::slice;
use core::slice::SliceIndex;
use core::sync::atomic::{AtomicIsize, AtomicPtr, AtomicUsize, Ordering::*};
use tracing::{event, instrument, Level};

// pub unsafe auto trait Freeze {}
// impl<T: ?Sized> !Freeze for UnsafeCell<T> {}
// unsafe impl<T: ?Sized> Freeze for &T {}
// unsafe impl<T: ?Sized> Freeze for &mut T {}
// unsafe impl<T: ?Sized> Freeze for *const T {}
// unsafe impl<T: ?Sized> Freeze for *mut T {}
// unsafe impl<T: ?Sized> Freeze for PhantomData<T> {}

pub struct ArcLog<T, A: Allocator= Global> {
    ptr: NonNull<ArcLogInner<T, A>>,
    pd: PhantomData<ArcLogInner<T, A>>,
}

impl<T: Sync> ArcLog<T> {
    pub unsafe fn new() -> Self {
        ArcLog::new_in(Global)
    }
    pub unsafe fn with_capacity(capacity: usize) -> Self {
        ArcLog::with_capacity_in(capacity, Global)
    }
}

unsafe impl<T, A: Allocator> Send for ArcLog<T, A> {}
// nothing prevents ArcLog from being Sync, but we may want to reserve
// this for future optimization, like keeping a local len value
//unsafe impl<T, A: AllocRef + Freeze> Sync for ArcLog<T, A> {}
impl<T, A: Allocator> Unpin for ArcLog<T, A> {}

impl<T, A: Allocator> Drop for ArcLog<T, A> {
    fn drop(&mut self) {
        drop_ref(self.ptr);
    }
}

// there is a decision to make as to whether we should include Ts
// and have bound, or ignore them so you can always debug
impl<T: fmt::Debug, A: Allocator> fmt::Debug for ArcLog<T, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let header_r = unsafe { &(*self.ptr.as_ptr()).header};
        let len_raw = header_r.len.load(Acquire);
        let len = get_len(len_raw);
        let is_locked = is_locked(len_raw);
        let has_forward = has_forward(len_raw);
        f.debug_struct("ArcLog")
            .field("ptr", &self.ptr)
            .field("forward", &header_r.forward)
            .field("count", &header_r.count.load(Relaxed))
            .field("cap", &header_r.cap)
            .field("len_raw", &len_raw)
            .field("is_locked", &is_locked)
            .field("has_forward", &has_forward)
            .field("len", &len)
            .field("data", &&**self)
            .finish()
    }
}

impl<T, A: Allocator, I: SliceIndex<[T]>> Index<I> for ArcLog<T, A> {
    type Output = I::Output;
    #[inline]
    fn index(&self, index: I) -> &Self::Output {
        Index::index(&**self, index)
    }
}

impl<T, A: Allocator> Clone for ArcLog<T, A> {
    fn clone(&self) -> Self {
        unsafe {(*addr_of!((*self.ptr.as_ptr()).header.count)).fetch_add(1, Relaxed)};
        ArcLog {
            ptr: self.ptr,
            pd: PhantomData,
        }
    }
}

impl<T, A: Allocator> Deref for ArcLog<T, A> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        let mut len = unsafe { (*addr_of!((*self.ptr.as_ptr()).header.len)).load(Acquire)};
        // should decide if we should chase to the next here
        len = get_len(len);
       // unsafe { MaybeUninit::slice_assume_init_ref(slice::from_raw_parts(inner.data.get() as *const MaybeUninit<_>, len as usize)) }
       unsafe { MaybeUninit::slice_assume_init_ref(slice::from_raw_parts(addr_of!((*self.ptr.as_ptr()).data) as *const MaybeUninit<_>, len as usize)) }
    }
}

#[inline(never)]
//#[instrument]
fn drop_ref<T, A: Allocator>(ptr: NonNull<ArcLogInner<T, A>>) {
    event!(Level::TRACE, "enter drop_ref");
    // this is release because we need to capture all loads before we deallocate.
    // in theory we only need need to do this for the last subtraction, but we don't
    // have another clear place to do this, and this is what Arc does...
    let prev_val =  unsafe {(*ptr.as_ptr()).header.count.fetch_sub(1, Release)};
    if prev_val != 1 {
        event!(Level::TRACE, "more than one ref, no need to drop");
        return;
    }
    event!(Level::TRACE, "last ref so have to drop");
    // functionally, this does nothing, but we need a release barrier here so writes don't creep up from the destructors
    // ref_ptr.header.count.store(0, Acquire);
    // the previous check guarantees we have exclusive access
    unsafe {(*ptr.as_ptr()).header.count.load(Acquire)};
    let raw_len = unsafe {(*ptr.as_ptr()).header.len.load(Acquire)};
    let forward_ptr = unsafe { (*ptr.as_ptr()).header.forward};

    

    match forward_ptr {
        Some(f_ptr) => {
            // if there is a forwarding address, we can just deallocate because the forward is responsible for dropping the inners
            event!(Level::TRACE, "forward isn't null so we reenter drop_ref");
            // SAFETY: We just checked it was null and a forward pointer should always be valid
            drop_ref::<T,A>(f_ptr);
            event!(Level::TRACE, "finished reentry of drop_ref");
        },
        None => {
            let len_to_drop =  get_len(raw_len);
            event!(
                Level::TRACE,
                "forward pointer is null, with {:?} items to drop",
                len_to_drop
            );
            unsafe {
                ptr::drop_in_place(ptr::slice_from_raw_parts_mut(
                    addr_of_mut!((*ptr.as_ptr()).data) as *mut T,
                    len_to_drop,
                ));
            }
            event!(Level::TRACE, "drop items");
        }
    } 

    //let alloc_ref = unsafe { (*ptr.as_ptr()).header.alloc };
    let alloc_ref = unsafe { &(*ptr.as_ptr()).header.alloc };
    unsafe {
        event!(Level::TRACE, "calling dealloc");
        // SAFETY: We are the last reference so we need to deallocate
        alloc_ref.deallocate(
            NonNull::new_unchecked(ptr.as_ptr() as *mut u8),
            ArcLogInner::<T, A>::get_layout((*ptr.as_ptr()).header.cap),
        );
        event!(Level::TRACE, "dealloc completed");
    }
}


impl<T: Sync, A: Allocator + Clone> ArcLog<T, A> {
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
        let header = unsafe { &(*self.ptr.as_ptr()).header};
        let raw_len = header.len.load(Acquire);
        if !has_forward(raw_len) {
            false
        } else {
            // SAFETY: We just checked for null, and forward must be valid if it exists
            let mut p_this = unsafe { (header.forward).unwrap() };
            let mut r_this = unsafe { p_this.as_ref() };
            loop {
                // this has to be acquire, because we may access data after this forward
                let raw_len = r_this.header.len.load(Acquire);
                if has_forward(raw_len) {
                    event!(Level::TRACE, "had to forward");
                    p_this = unsafe { (r_this.header.forward).unwrap() };
                    r_this = unsafe { p_this.as_ref() };
                } else {
                    event!(Level::TRACE, "at end of forward change");
                    break;
                }
            }
            let old_ptr = self.ptr;
            self.ptr = p_this;
            // this can be relaxed because only the count matters
            // and we already did an acquire when we got the pointer
            r_this.header.count.fetch_add(1, Relaxed);
            drop_ref(old_ptr);
            true
        }
    }

    fn finish_push(
        &mut self,
        index: isize,
        o_ptr: Option<NonNull<ArcLogInner<T, A>>>,
        item: T,
    ) -> Result<usize, T> {
        if let Some(new_ptr) = o_ptr {
            event!(Level::TRACE, "post alloc_one");
            let old_ptr = self.ptr;
            unsafe {  (*new_ptr.as_ptr()).header.count.fetch_add(1, Relaxed) };
            self.ptr = new_ptr;
            event!(Level::TRACE, "pre-drop ref");
            drop_ref(old_ptr);
            event!(Level::TRACE, "post drop ref")
        }
        event!(Level::TRACE, "finished push");
        if index == -1 {
            Err(item)
        } else {
            let _ = ManuallyDrop::new(item);
            Ok(index as usize)
        }
    }

    /// returns the index of the item that was pushed
    #[instrument(skip(self, item))]
    pub fn push_spin(&mut self, item: T) -> usize {
        let (index, o_ptr) = ArcLogInner::alloc_items(self.ptr, &item, 1, isize::MAX);
        match self.finish_push(index, o_ptr, item) {
            Ok(i) => i,
            Err(_) => unreachable!(),
        }
    }

    #[instrument(skip(self, item))]
    pub fn push_or_return(&mut self, item: T) -> Result<usize, T> {
        let (index, o_ptr) = ArcLogInner::alloc_items_one_shot(self.ptr, &item, 1, isize::MAX as usize);
        self.finish_push(index, o_ptr, item)
    }
    #[instrument(skip(self, item))]
    pub fn push_spin_by_index(&mut self, item: T, index: usize) -> Result<usize, T> {
        let (index, o_ptr) = ArcLogInner::alloc_items(self.ptr, &item, 1, index as isize);
        self.finish_push(index, o_ptr, item)
    }
    pub fn push_or_return_by_index(&mut self, item: T, index: usize) -> Result<usize, T> {
        let (index, o_ptr) = ArcLogInner::alloc_items_one_shot(self.ptr, &item, 1, index as isize as usize);
        self.finish_push(index, o_ptr, item)
    }
}

// capacity and len should not change once we have a non-null forward pointer
struct ArcLogInnerHeader<T, A: Allocator> {
    count: AtomicUsize,
    // AllocRef does not support grow in place (it currently does not)
    // so this could just be a usize as it will not actually change
    cap: usize,
    // len can never be greater than cap
    // len will also use the top two bit to encode if writing and whether there
    // is a forwarding address
    len: AtomicUsize,
    // forward will be a null-ptr if there is no forward node

    // if alloc wasn't copy, we could move it when we created a forward
    // and the forward and alloc could overlap bytes... though if we moved
    // alloc we'd have to lock the forward node on this dealloc, or make
    // sure alloc was atomic (say a pointer). If alloc was a pointer, we could
    // potentially use a mask to identify between a forward or alloc ptr.
    // But given that alloc is most likely zero-size, keeping them forward and alloc separate
    // seems like the most pragmatic path

    // ideally this would be a thin pointer to ArcLogInner, but so far I cannot
    // find a way to express this. We assume we can cast back from InnerHeader to Inner
    forward: Option<NonNull<ArcLogInner<T, A>>>,
    alloc: A,
}

// This is a repr(C) because we need to makes sure that data is at the end.
// The zero size array is used just to give a starting point to the len-sized data
// that is appended to as part of the allocation. This also allows us to cast from a
// InnerHeader back to Inner
#[repr(C)]
struct ArcLogInner<T, A: Allocator> {
    header: ArcLogInnerHeader<T, A>,
    data: [MaybeUninit<T>;0],
}

impl<T, A: Allocator + Clone> ArcLogInner<T, A> {
    fn with_capacity(capacity: usize, alloc: A) -> NonNull<Self> {
        let new_alloc = alloc
            .allocate(Self::get_layout(capacity))
            .expect("Error allocating")
            .as_mut_ptr();
        // SAFETY: The alloc was just made with T layout so this cast is safe
        let ptr = unsafe { &mut *(new_alloc as *mut Self) };
        ptr.header.forward = None;
        ptr.header.count.store(1, Relaxed);
        ptr.header.cap = if mem::size_of::<T>() == 0 {
                isize::MAX as usize
            } else {
                capacity
            };
        ptr.header.alloc = alloc;
        ptr.header.len.store(0, Release);
        ptr.into()
    }

    #[instrument(skip(p_this))]
    fn get_next_address(mut p_this: NonNull<Self>) -> NonNull<Self> {
        let mut r_this =  unsafe{ p_this.as_ref()};
        loop {
            // this can be relaxed because we're only going to look at the forward
            let _len = r_this.header.len.load(Acquire);
            let new_this = unsafe { r_this.header.forward};
            match new_this {
                Some(fwrd) => {
                    event!(Level::TRACE, "had to forward");
                    // SAFETY: We check for null, so this must point to valid reference
                    p_this = fwrd;
                    r_this =  unsafe{ p_this.as_ref()};
                },
                None => {
                    event!(Level::TRACE, "at end of forward change");
                    break;
                }
            }
        }
        p_this
    }
    // #[inline(always)]
    // pub(crate) unsafe fn data(this: NonNull<Self>) -> (NonNull<T>, usize) {
    //     let size = this.as_ref().header.;
    //     let tptr = this.as_ptr();
    //     let daddr = core::ptr::addr_of_mut!((*tptr).data);
    //     let nn = NonNull::new_unchecked(daddr.cast::<T>());
    //     (nn, size)
    // }


    #[instrument(skip(p_self))]
    fn alloc_items_one_shot(
        mut p_self: NonNull<Self>,
        data_ptr: *const T,
        count: usize,
        ref_index: usize,
    ) -> (isize, Option<NonNull<Self>>) {
        event!(Level::TRACE, "enter alloc items one shot");
        debug_assert!(count > 0);
        let size_of_t = mem::size_of::<T>();
        
        if size_of_t == 0 {
            event!(Level::TRACE, "is zero sized");
            let r_self = unsafe {p_self.as_ref()};
            // TODO: in the zero size case, this can probably be relaxed
            let len = r_self.header.len.load(Acquire);
            if len > ref_index {
                return (-1, None);
            } else {
                // TODO: Find out what nightly Vec with alloc does here
                //let not_len = !len;
                let new_len = len.checked_add(count).expect("capacity overflow");
                match r_self
                    .header
                    .len
                    .compare_exchange(len, new_len, Relaxed, Relaxed)
                {
                    Ok(_old_claim_val) => {
                        //self.header.len.store(new_len, Release);
                        return (len as isize, None);
                    }
                    Err(_old_claim_val) => {
                        return (-1, None);
                    }
                }
            }
        } else {
            event!(Level::TRACE, "is not zero sized");
            let mut p_this = Self::get_next_address(p_self);
            let r_this = unsafe {p_this.as_ref()};
            event!(Level::TRACE, "got new address");
            // we reached the end of the forwarding so we have have to set an acquire barrier so that
            // everything is in sync
            let raw_len = r_this.header.len.load(Acquire);
            let len  = get_len(raw_len);
            if has_forward_or_lock(raw_len) || len > ref_index {
                return if p_this == p_self {
                    (-1, None)
                } else {
                    (-1, Some(p_this.into()))
                };
            }
            //let negative_len = !len;
            // this should still respect boundary set by len
            let new_len = len.checked_add(count).expect("capacity overflow");
            if has_forward_or_lock(new_len) {
                panic!("capacity overflow");
            } 
            let cap = r_this.header.cap;
            if new_len > cap {
                event!(Level::TRACE, "had to reallocate");
                // This guarantees exponential growth. The doubling cannot overflow
                // because `cap <= isize::MAX` and the type of `cap` is `usize`.
                let n_cap = cmp::max(cap * 2, new_len);

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
                let locked_len = lock_len(len);
                match r_this
                    .header
                    .len
                    .compare_exchange(len, locked_len, Relaxed, Relaxed)
                {
                    Ok(_old_claim_val) => {
                        event!(Level::TRACE, "got claim to add entries");
                        let ptr = r_this
                            .header
                            .alloc
                            .allocate(req_layout)
                            .unwrap_or_else(|_| handle_alloc_error(req_layout));
                        event!(Level::TRACE, "got allocation");
                        let new_mut_ref = unsafe {
                            let new_alloc_len = ptr.as_ref().len();
                            event!(
                                Level::TRACE,
                                "Old ptr: {:?} new_ptr: {:?} with len: {:?}",
                                p_this,
                                ptr,
                                new_alloc_len
                            );
                            assert_eq!(new_alloc_len, req_size);

                            let new_mut_ptr = ptr.as_mut_ptr() as *mut Self;

                            //ptr::copy_nonoverlapping(p_this.as_ptr(), new_mut_ptr, 1);
                            ptr::copy_nonoverlapping( //(ptr::addr_of_mut!((*new_mut_ptr).data) as *mut T).offset(len as isize)
                                (*p_self.as_ptr()).data.as_ptr(),
                                (*new_mut_ptr).data.as_mut_ptr(),
                                len,
                            );
                            event!(Level::TRACE, "start writing new data to new allocation");
                            ptr::copy_nonoverlapping(
                                data_ptr,
                                ((*new_mut_ptr).data.as_mut_ptr() as *mut T).offset(len as isize),
                                count,
                            );
                            &mut *new_mut_ptr
                        };
                        event!(Level::TRACE, "updating automics");
                        new_mut_ref.header.count.store(1, Relaxed);
                        new_mut_ref.header.cap = n_cap;
                        new_mut_ref.header.len.store(new_len, Relaxed);
                        new_mut_ref.header.alloc = unsafe { (*p_self.as_ptr()).header.alloc.clone() };
                        new_mut_ref.header.forward = None;
                        // the data has to be ready once we update the forward ptr,
                        // so this must be a release
                        let new_nn_ptr : NonNull<_> = new_mut_ref.into();
                        unsafe {(*p_self.as_ptr()).header.forward = Some(new_nn_ptr);}
                        let forward_len = add_forward_to_len(len);
                        // this should also be release, otherwise it could be moved before the forward
                        // and the forward must be seen by the next write
                        unsafe { (*p_self.as_ptr()).header.len.store(forward_len, Release) };
                        return (len as isize, Some(new_nn_ptr));
                    }
                    Err(_old_claim_val) => {
                        event!(Level::TRACE, "couldn't get claim");
                        return if p_this == p_self {
                            (-1, None)
                        } else {
                            (-1, Some(p_this))
                        };
                    }
                }
            } else {
                let locked_len = lock_len(len);
                match r_this
                    .header
                    .len
                    .compare_exchange(len, locked_len, Relaxed, Relaxed)
                {
                    Ok(_old_claim_val) => {                       
                        // we can just add our data an and update the len
                        unsafe {
                            ptr::copy_nonoverlapping(
                                data_ptr,
                                ((*p_this.as_ptr()).data.as_mut_ptr() as *mut T).offset(len as isize),
                                count,
                            );
                        }
                        r_this.header.len.store(new_len, Release);
                        if p_this == p_self {
                            return (len as isize, None);
                        } else {
                            return (len as isize, Some(p_this));
                        }
                    }
                    Err(_old_claim_val) => {
                        return if p_this == p_self {
                            (-1, None)
                        } else {
                            (-1, Some(p_this))
                        };
                    }
                }
            }
        }
    }

    #[instrument(skip(p_self))]
    fn alloc_items(
        p_self: NonNull<Self>,
        data_ptr: *const T,
        count: usize,
        ref_index: isize,
    ) -> (isize, Option<NonNull<Self>>) {
        event!(Level::TRACE, "enter alloc items");
        debug_assert!(count > 0);
        let size_of_t = mem::size_of::<T>();
        if size_of_t == 0 {
            event!(Level::TRACE, "is zero sized");
            let r_self = unsafe { p_self.as_ref() };
            // if ths size of T is zero, there will never be a need to forward so we can do a much tighter loop
            loop {
                let mut len = r_self.header.len.load(Acquire);
                if len > ref_index as usize {
                    return (-1, None);
                }
                // this should still respect boundary set by len
                let new_len = len.checked_add(count).expect("Too many entries");
                match r_self
                    .header
                    .len
                    .compare_exchange_weak(len, new_len, Relaxed, Relaxed)
                {
                    Ok(_old_claim_val) => {
                        r_self.header.len.store(new_len, Release);
                        return (len as isize, None);
                    }
                    Err(_old_claim_val) => {}
                }
            }
        } else {
            let mut p_this = p_self;
            let mut raw_len;
            event!(Level::TRACE, "is not zero sized");
            'outer: loop {
                raw_len = unsafe {(*p_this.as_ptr()).header.len.load(Acquire)};
                event!(Level::TRACE, "raw_len is {}", raw_len);
                if has_forward(raw_len){
                    event!(Level::TRACE, "has forward");
                    p_this = unsafe { (*p_this.as_ptr()).header.forward.unwrap_unchecked() };
                    continue;
                } else if is_locked(raw_len) {
                    event!(Level::TRACE, "locked, waiting for unlock");
                    // might need to sleep
                    continue;                    
                } else {
                    event!{Level::TRACE, "entering inner loop"};
                    loop {
                        event!{Level::TRACE, "inner loop"};
                        match  unsafe{(*p_this.as_ptr()).header.len.compare_exchange(raw_len, lock_len(raw_len), Acquire, Acquire)} {
                            Ok(_) => {
                                break 'outer
                            },
                            Err(new_value) => {
                                event!(Level::TRACE, "len changed, new value is {}", new_value);
                                if has_forward(new_value){
                                    p_this = unsafe {  (*p_this.as_ptr()).header.forward.unwrap_unchecked() };
                                    continue 'outer;
                                } else if is_locked(new_value) {
                                    continue 'outer;
                                } else {
                                    raw_len = new_value;
                                    continue;
                                }
                            }
                        }
                    }
                }
            }
            event!(Level::TRACE, "got new lock");
                // we reached the end of the forwarding so we have have to set an acquire barrier so that
                // everything is in sync
            let len  = raw_len;
                if len as isize > ref_index {
                    return if p_this == p_self {
                        (-1, None)
                    } else {
                        (-1, Some(p_this))
                    };
                }
                // this should still respect boundary set by len
                let new_len = len.checked_add(count).expect("Too many entries");
                let cap =  unsafe{(*p_this.as_ptr()).header.cap};

                if new_len > cap {
                    event!(Level::TRACE, "had to reallocate");
                    // This guarantees exponential growth. The doubling cannot overflow
                    // because `cap <= isize::MAX` and the type of `cap` is `usize`.
                    let n_cap = cmp::max(cap * 2, new_len);

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
                    let r_self = unsafe {p_self.as_ref()};
                    let ptr = r_self
                                .header
                                .alloc
                                .allocate(req_layout)
                                .unwrap_or_else(|_| handle_alloc_error(req_layout));
                    event!(Level::TRACE, "got allocation");
                    let new_mut_ref = unsafe {
                                let new_alloc_len = ptr.as_ref().len();
                                event!(
                                    Level::TRACE,
                                    "Old ptr: {:?} new_ptr: {:?} with len: {:?}",
                                    p_this,
                                    ptr,
                                    new_alloc_len
                                );
                                assert_eq!(new_alloc_len, req_size);

                                let new_mut_ptr = ptr.as_mut_ptr() as *mut Self;

                                //ptr::copy_nonoverlapping(this, new_mut_ptr, 1);
                                ptr::copy_nonoverlapping(
                                    ptr::addr_of!((*p_this.as_ptr()).data) as *const T,
                                    ptr::addr_of_mut!((*new_mut_ptr).data) as *mut T,
                                    len as usize,
                                );
                                event!(Level::TRACE, "start writing new data to new allocation");
                                ptr::copy_nonoverlapping(
                                    data_ptr,
                                    (ptr::addr_of_mut!((*new_mut_ptr).data) as *mut T).offset(len as isize),
                                    count,
                                );
                                &mut *new_mut_ptr
                            };
                            event!(Level::TRACE, "updating automics");
                            new_mut_ref.header.count.store(1, Relaxed);
                            new_mut_ref.header.cap = n_cap;
                            new_mut_ref.header.len.store(new_len, Relaxed);
                            new_mut_ref.header.alloc = r_self.header.alloc.clone();
                            new_mut_ref.header.forward = None;
                            let new_mut_ref: NonNull<_> = unsafe { NonNull::new_unchecked(ptr.as_mut_ptr() as *mut Self)}; //new_mut_ref.into();
                            // the data has to be ready once we update the forward ptr,
                            // so this must be a release
                           
                            unsafe { (*p_this.as_ptr()).header.forward = Some(new_mut_ref); }
                            let r_this2 = unsafe {p_this.as_ref()};
                            // this should also be release, otherwise it could be moved before the forward
                            // and the forward must be seen by the next write
                            r_this2.header.len.store( add_forward_to_len(len), Release);
                            return (len as isize, Some(new_mut_ref));                
                } else {
                    
                            // we can just add our data an and update the len
                            unsafe {
                                // ptr::copy_nonoverlapping(
                                //     data_ptr,
                                //     (ptr::addr_of_mut!(this.data) as *mut T).offset(len as isize),
                                //     count,
                                //);

                                //https://github.com/rust-lang/unsafe-code-guidelines/issues/256
                                ptr::copy_nonoverlapping(
                                    data_ptr,
                                    (ptr::addr_of_mut!((*p_this.as_ptr()).data) as *mut T).offset(len as isize),
                                    count,
                                );
                            }
                            unsafe{(*p_this.as_ptr()).header.len.store(new_len, Release)};
                            if p_this == p_self {
                                return (len as isize, None);
                            } else {
                                return (len as isize, Some(p_this));
                            }
                       
            }
        }
    }
}

impl<T, A: Allocator> ArcLogInner<T, A> {

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

const fn has_forward(val: usize) -> bool {
    (val | (usize::MAX >> 1)) == usize::MAX
}

const fn is_locked(val: usize) -> bool {
    ((val << 1) | (usize::MAX >> 1)) == usize::MAX
}

const fn get_len(val: usize) -> usize {
     val & (usize::MAX >> 2)
}

const fn lock_len(val: usize) -> usize {
    val | (!(usize::MAX >> 1) >> 1)
}

const fn add_forward_to_len(val: usize) -> usize {
    val | (!(usize::MAX >> 1))
}

const fn has_forward_or_lock(val: usize) -> bool {
    val & (usize::MAX >> 2) == val
}

