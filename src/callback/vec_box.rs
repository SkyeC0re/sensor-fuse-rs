extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
use core::{ptr, task::Waker};

use super::{ExecManagerMut, ExecRegisterMut};

pub struct VecBoxManager<T> {
    callbacks: Vec<Box<dyn Send + FnMut(&T) -> bool>>,
    wakers: Vec<Waker>,
}

impl<T> Default for VecBoxManager<T> {
    fn default() -> Self {
        Self {
            callbacks: Default::default(),
            wakers: Default::default(),
        }
    }
}

impl<T> VecBoxManager<T> {
    pub const fn new() -> Self {
        Self {
            callbacks: Vec::new(),
            wakers: Vec::new(),
        }
    }
}

impl<T> ExecManagerMut<T> for VecBoxManager<T> {
    fn execute(&mut self, value: &T) {
        for waker in self.wakers.drain(..) {
            waker.wake();
        }

        let mut i = 0;
        let mut len = self.callbacks.len();
        let ptr_slice = self.callbacks.as_ptr().cast_mut();
        unsafe {
            while i < len {
                let func = ptr_slice.add(i);
                if (*func)(value) {
                    i += 1;
                } else {
                    len -= 1;
                    ptr::swap(func, ptr_slice.add(len));
                }
            }
        }

        self.callbacks.truncate(len);
    }
}

impl<T, F: 'static + Send + FnMut(&T) -> bool> ExecRegisterMut<F> for VecBoxManager<T> {
    fn register(&mut self, f: F) {
        self.callbacks.push(Box::new(f));
    }
}

impl<T> ExecRegisterMut<&Waker> for VecBoxManager<T> {
    fn register(&mut self, w: &Waker) {
        self.wakers.push(w.clone());
    }
}
