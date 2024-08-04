use std::task::Waker;

use derived_deref::{Deref, DerefMut};

use super::CallbackManager;

pub struct VecBoxManager<T> {
    callbacks: Vec<Box<dyn FnMut(&T) -> bool>>,
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

impl<T> CallbackManager<T> for VecBoxManager<T> {
    fn register<F: 'static + FnMut(&T) -> bool>(&mut self, f: F) {
        self.callbacks.push(Box::new(f));
    }

    fn register_waker(&mut self, w: &Waker) {
        self.wakers.push(w.clone());
    }

    fn callback(&mut self, value: &T) {
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
                    std::ptr::swap(func, ptr_slice.add(len));
                }
            }
        }

        self.callbacks.truncate(len);
    }
}

#[derive(Deref, DerefMut, Default)]
pub struct AsyncVecBoxManager<T>(Vec<Box<dyn FnMut(&T) -> bool>>);

impl<T> AsyncVecBoxManager<T> {
    pub const fn new() -> Self {
        Self(Vec::new())
    }
}
