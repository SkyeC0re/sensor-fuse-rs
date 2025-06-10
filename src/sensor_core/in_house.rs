use std::{
    marker::PhantomPinned,
    mem::MaybeUninit,
    ptr::null_mut,
    sync::atomic::{AtomicPtr, AtomicU8},
    task::Waker,
};

#[derive(Debug)]
#[repr(align(4))]
struct Node {
    waker: MaybeUninit<Waker>,
    next: *mut Node,
    prev: *mut Node,
    // ... | WAIT_DOWNGRADE_BIT | WAKE_NEXT_BIT | COMPLETE_BIT | DROP_BIT | CANCEL_BIT | IS_WRITER_BIT
    state: AtomicU8,

    _p: PhantomPinned,
}

impl Node {
    const STATE_IS_WRITER_BIT: u8 = 0b1;
    const STATE_CANCEL_BIT: u8 = 0b10;
    const STATE_DROP_BIT: u8 = 0b100;
    const STATE_COMPLETE_BIT: u8 = 0b1000;
    const STATE_WAKE_NEXT_BIT: u8 = 0b10000;
    const STATE_WAIT_DOWNGRADE_BIT: u8 = 0b100000;

    const SENTINEL: usize = 1;

    /// Reset the node's values for a new future that is to be added to a queue.
    ///
    /// # Safety
    ///
    /// Behaviour is undefined if node is not in a completed (i.e. exclusive) state.
    #[inline(always)]
    unsafe fn assume_exclusive_reset(node: *mut Self, waker: Waker, is_writer: bool) {
        let node = &mut *node;
        let _ = node.waker.write(waker);
        *node.next.get_mut() = null_mut();
        *node.state.get_mut() = is_writer as u8;
    }
}
