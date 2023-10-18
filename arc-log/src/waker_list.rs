use core::sync::atomic::AtomicUsize;
use core::task::Waker;
use core::mem::MaybeUninit;
use alloc::boxed::Box;
use alloc::sync::Arc;

pub struct InnerWakers {
    next_inner: Option<Box<InnerWakers>>,
    wakers: [MaybeUninit<Waker>]
}

pub struct WakerHeader<const N: usize> {
    len: AtomicUsize,
    next_inner: Option<Box<InnerWakers>>,
    wakers: [MaybeUninit<Waker>; N]
}

struct WakerSupplier<const N: usize> {
    waker_list: Arc<WakerHeader<N>>
}

struct WakerWaker<const N: usize> {
    waker_list: Arc<WakerHeader<N>>
}

impl <const N: usize> WakerWaker<N> {
    pub fn new() -> Self {
        Self {
            waker_list: Arc::new(WakerHeader {
                len: AtomicUsize::new(0),
                next_inner: None,
                wakers: MaybeUninit::uninit_array::<N>()
            })
        }
    }
}




