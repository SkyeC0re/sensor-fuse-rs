use std::{
    cell::UnsafeCell, marker::PhantomPinned, mem::MaybeUninit, ptr::null_mut, sync::atomic::{AtomicPtr, AtomicU8, AtomicUsize}, task::Waker
};

use critical_section::Mutex;
const MAX_WAKE_CLUSTERING: usize = 8;

#[derive(Debug)]
#[repr(align(4))]
struct Node {
    waker: MaybeUninit<Waker>,
    next: AtomicPtr<Node>,
    prev: AtomicPtr<Node>,
    // ... WAKE_NEXT_BIT | COMPLETE_BIT
    state: AtomicU8,

    _p: PhantomPinned,
}

impl Node {
    const STATE_COMPLETE_BIT: u8 = 0b1;
    const STATE_WAKE_NEXT_BIT: u8 = 0b10;

    const SENTINEL: usize = 1;
}

pub struct Core {
    lock_state: AtomicUsize,
    version: AtomicUsize,
    main_queue_lock: Mutex<()>,
    rw_head: UnsafeCell<*mut Node>,
    pr_head: UnsafeCell<*mut Node>,

    rw_tail: AtomicPtr<Node>,
    pr_tail: AtomicPtr<Node>,


    writes_queued: AtomicUsize,
    writers: AtomicUsize,

    changed_queue_lock: Mutex<()>,
    wc_head: UnsafeCell<*mut Node>,
    wc_tail: AtomicPtr<Node>,
}


impl Core {
    fn notify_changed_queue(&self) {

    }
}
