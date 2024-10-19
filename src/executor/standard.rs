extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
use core::{ptr, task::Waker};
use parking_lot::Mutex;
use std::{
    cell::UnsafeCell,
    hint::{black_box, spin_loop},
    marker::PhantomData,
    mem,
    ptr::null_mut,
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
};

use crate::lock::DataWriteLock;

use super::{BoxedFn, ExecManager, ExecRegister};

const LOCKED_BIT: usize = 1;

struct WakerList {
    head: AtomicPtr<WakerNode>,
}

impl WakerList {
    const fn new() -> Self {
        Self {
            head: AtomicPtr::new(null_mut()),
        }
    }

    fn add_waker(&self, w: Waker) {
        let node = Box::into_raw(Box::new(WakerNode {
            waker: w,
            next_ptr: AtomicUsize::new(LOCKED_BIT),
        }));

        let prev_head = self.head.swap(node, Ordering::Relaxed);
        let _ = unsafe { (*node).next_ptr.swap(prev_head as usize, Ordering::Release) };
    }

    fn drain(&self) -> WakerListDrain {
        let curr = self.head.swap(null_mut(), Ordering::Relaxed);
        WakerListDrain { curr }
    }
}

impl Drop for WakerList {
    fn drop(&mut self) {
        self.drain();
    }
}

struct WakerListDrain {
    curr: *mut WakerNode,
}

impl Iterator for WakerListDrain {
    type Item = Waker;

    fn next(&mut self) -> Option<Self::Item> {
        if self.curr == null_mut() {
            return None;
        }

        let curr_box = unsafe { Box::from_raw(self.curr) };

        let mut next: usize;
        loop {
            next = curr_box.next_ptr.load(Ordering::Acquire);
            if next != LOCKED_BIT {
                break;
            }
            spin_loop();
        }

        let waker = Some(curr_box.waker);
        self.curr = next as *mut WakerNode;

        waker
    }
}

impl Drop for WakerListDrain {
    fn drop(&mut self) {
        for waker in self {
            drop(waker);
        }
    }
}

struct WakerNode {
    waker: Waker,
    next_ptr: AtomicUsize,
}

struct Data<T> {
    callbacks_in: Vec<Box<dyn Send + FnMut(&T) -> bool>>,
    callbacks_out: Vec<Box<dyn Send + FnMut(&T) -> bool>>,
    wakers_in: Vec<Waker>,
    wakers_out: Vec<Waker>,
}

impl<T> Data<T> {
    #[inline(always)]
    pub const fn new() -> Self {
        Self {
            callbacks_in: Vec::new(),
            callbacks_out: Vec::new(),
            wakers_in: Vec::new(),
            wakers_out: Vec::new(),
        }
    }
}

/// Standard executor that supports registration of functions and wakers.
pub struct StdExec<T> {
    registration_mtx: Mutex<()>,
    wakers: WakerList,
    _types: PhantomData<T>,
}

unsafe impl<T> Sync for StdExec<T> where T: Send {}

/// TODO Optimize later
impl<T> StdExec<T> {
    /// Create a new Executor.
    #[inline(always)]
    pub const fn new() -> Self {
        Self {
            registration_mtx: Mutex::new(()),
            wakers: WakerList::new(),
            _types: PhantomData,
        }
    }

    #[inline(always)]
    unsafe fn execute_unsafe<'a, L: DataWriteLock<Target = T>, F: FnOnce() -> L::WriteGuard<'a>>(
        &'a self,
        commit: F,
    ) where
        L: 'a,
    {
        let _ = commit();
        for waker in self.wakers.drain() {
            waker.wake();
        }

        // let mut i = 0;
        // let mut len = callbacks_out.len();
        // let ptr_slice = callbacks_out.as_ptr().cast_mut();
        // while i < len {
        //     let func = ptr_slice.add(i);
        //     if (*func)(&value) {
        //         i += 1;
        //     } else {
        //         len -= 1;
        //         ptr::swap(func, ptr_slice.add(len));
        //     }
        // }

        // callbacks_out.truncate(len);
    }
}

impl<T, L> ExecManager<L> for StdExec<T>
where
    L: DataWriteLock<Target = T>,
{
    fn execute<'a, C: FnOnce() -> L::WriteGuard<'a>>(&'a self, commit: C)
    where
        L: 'a,
    {
        unsafe {
            self.execute_unsafe::<L, C>(commit);
        }
    }
}

impl<T, F> ExecRegister<Box<F>> for StdExec<T>
where
    F: 'static + Send + FnMut(&T) -> bool,
{
    #[inline]
    fn register<C: FnOnce() -> Option<Box<F>>>(&self, condition: C) -> bool {
        let guard = self.registration_mtx.lock();
        let res = match condition() {
            Some(f) => true,
            None => false,
        };
        drop(guard);
        res
    }
}

impl<T> ExecRegister<BoxedFn<T>> for StdExec<T> {
    #[inline]
    fn register<C: FnOnce() -> Option<BoxedFn<T>>>(&self, condition: C) -> bool {
        let guard = self.registration_mtx.lock();
        let res = match condition() {
            Some(f) => true,
            None => false,
        };
        drop(guard);
        res
    }
}

impl<'a, T> ExecRegister<&'a Waker> for StdExec<T> {
    #[inline]
    fn register<C: FnOnce() -> Option<&'a Waker>>(&self, condition: C) -> bool {
        let guard = self.registration_mtx.lock();
        let res = match condition() {
            Some(w) => {
                self.wakers.add_waker(w.clone());
                true
            }
            None => false,
        };
        drop(guard);
        res
    }
}
