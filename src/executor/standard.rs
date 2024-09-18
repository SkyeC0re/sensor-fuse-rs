extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
use core::{ptr, task::Waker};
use parking_lot::Mutex;
use std::cell::UnsafeCell;

use crate::lock::DataWriteLock;

use super::{BoxedFn, ExecManager, ExecRegister};

/// Standard executor that supports registration of functions and wakers.
pub struct StdExec<T> {
    registration_mtx: Mutex<()>,
    callbacks_in: UnsafeCell<Vec<Box<dyn Send + FnMut(&T) -> bool>>>,
    callbacks_out: UnsafeCell<Vec<Box<dyn Send + FnMut(&T) -> bool>>>,
    wakers_in: UnsafeCell<Vec<Waker>>,
    wakers_out: UnsafeCell<Vec<Waker>>,
}

unsafe impl<T> Sync for StdExec<T> where T: Send {}

/// TODO Optimize later
impl<T> StdExec<T> {
    /// Create a new Executor.
    #[inline]
    pub const fn new() -> Self {
        Self {
            registration_mtx: Mutex::new(()),
            callbacks_in: UnsafeCell::new(Vec::new()),
            callbacks_out: UnsafeCell::new(Vec::new()),
            wakers_in: UnsafeCell::new(Vec::new()),
            wakers_out: UnsafeCell::new(Vec::new()),
        }
    }

    #[inline]
    unsafe fn callback_and_truncate(&self, value: &T) {
        let callbacks = &mut *self.callbacks_out.get();
        let mut i = 0;
        let mut len = callbacks.len();
        let ptr_slice = callbacks.as_ptr().cast_mut();
        while i < len {
            let func = ptr_slice.add(i);
            if (*func)(value) {
                i += 1;
            } else {
                len -= 1;
                ptr::swap(func, ptr_slice.add(len));
            }
        }

        callbacks.truncate(len);
    }
}

impl<T, L> ExecManager<L> for StdExec<T>
where
    L: DataWriteLock<Target = T>,
{
    fn execute(&self, value: L::DowngradedGuard<'_>) {
        unsafe {
            // Call kept callbacks.
            self.callback_and_truncate(&value);
        }

        let guard = self.registration_mtx.lock();
        unsafe {
            ptr::swap(self.callbacks_in.get(), self.callbacks_out.get());
            ptr::swap(self.wakers_in.get(), self.wakers_out.get());
        }
        drop(guard);
        for waker in unsafe { (*self.wakers_out.get()).drain(..) } {
            waker.wake();
        }

        unsafe {
            self.callback_and_truncate(&value);
        }
    }
}

impl<T, L, F> ExecRegister<L, Box<F>> for StdExec<T>
where
    L: DataWriteLock<Target = T>,
    F: 'static + Send + FnMut(&T) -> bool,
{
    #[inline]
    fn register<C: FnOnce() -> Option<Box<F>>>(&self, condition: C, _: &L) -> bool {
        let guard = self.registration_mtx.lock();
        let res = match condition() {
            Some(f) => {
                unsafe {
                    (*self.callbacks_in.get()).push(f);
                }
                true
            }
            None => false,
        };
        drop(guard);
        res
    }
}

impl<T, L> ExecRegister<L, BoxedFn<T>> for StdExec<T>
where
    L: DataWriteLock<Target = T>,
{
    #[inline]
    fn register<C: FnOnce() -> Option<BoxedFn<T>>>(&self, condition: C, _: &L) -> bool {
        let guard = self.registration_mtx.lock();
        let res = match condition() {
            Some(f) => {
                unsafe {
                    (*self.callbacks_in.get()).push(f);
                }
                true
            }
            None => false,
        };
        drop(guard);
        res
    }
}

impl<'b, T, L> ExecRegister<L, &'b Waker> for StdExec<T>
where
    // Self: 'b,
    L: DataWriteLock<Target = T>,
{
    #[inline]
    fn register<C: FnOnce() -> Option<&'b Waker>>(&self, condition: C, _: &L) -> bool {
        let guard = self.registration_mtx.lock();
        let res = match condition() {
            Some(w) => {
                unsafe {
                    (*self.wakers_in.get()).push(w.clone());
                }
                true
            }
            None => false,
        };
        drop(guard);
        res
    }
}
