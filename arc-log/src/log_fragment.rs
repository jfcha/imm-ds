use core::marker::PhantomData;
use core::sync::atomic::{AtomicUsize, AtomicIsize, AtomicBool, AtomicPtr};
use core::ptr::NonNull;
use alloc::alloc::AllocRef;
use alloc::vec::Vec;


pub struct LogFragment<T, A: AllocRef> {
    ptr: NonNull<InnerLogFragmentHeader<A>>,
    index: usize,
    pd: PhantomData<InnerLogFragment<T, A>> 
}

struct InnerLogFragmentHeader<A: AllocRef> {
    forward: AtomicPtr<InnerLogFragmentHeader<A>>,
    count: AtomicUsize,
    len: AtomicIsize,
    cap: AtomicUsize,
    vec_lock: AtomicBool,
    min_vec: Vec::<usize>,
    start_index: usize,
    alloc: A,
}

#[repr(C)]
struct InnerLogFragment<T, A: AllocRef> {
    header: InnerLogFragmentHeader<A>,
    data: [T]
}

impl<T, A: AllocRef> LogFragment<T, A> {
    pub fn new_in(alloc: A) -> Self {
        LogFragment {
            ptr: InnerLogFragmentHeader::with_capacity(0, alloc),
            index: 0,
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

    fn finish_push(
        &mut self,
        index: isize,
        o_ptr: Option<NonNull<ArcLogInner<T, A>>>,
        item: T,
    ) -> Result<usize, T> {
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
        if index == -1 {
            Err(item)
        } else {
            let _ = ManuallyDrop::new(item);
            Ok(index as usize)
        }
    }

    #[instrument(skip(self, item))]
    pub fn push_spin(&mut self, item: T) -> usize {
        let (index, o_ptr) = self.get_inner().alloc_items(&item, 1, isize::MAX);
        match self.finish_push(index, o_ptr, item) {
            Ok(i) => i,
            Err(_) => unreachable!(),
        }
    }

    #[instrument(skip(self, item))]
    pub fn push_or_return(&mut self, item: T) -> Result<usize, T> {
        let (index, o_ptr) = self.get_inner().alloc_items_one_shot(&item, 1, isize::MAX);
        self.finish_push(index, o_ptr, item)
    }
    #[instrument(skip(self, item))]
    pub fn push_spin_by_index(&mut self, item: T, index: usize) -> Result<usize, T> {
        let (index, o_ptr) = self.get_inner().alloc_items(&item, 1, index as isize);
        self.finish_push(index, o_ptr, item)
    }
    pub fn push_or_return_by_index(&mut self, item: T, index: usize) -> Result<usize, T> {
        let (index, o_ptr) = self
            .get_inner()
            .alloc_items_one_shot(&item, 1, index as isize);
        self.finish_push(index, o_ptr, item)
    }
}
