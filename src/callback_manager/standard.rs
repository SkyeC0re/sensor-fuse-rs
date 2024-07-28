use derived_deref::{Deref, DerefMut};

use super::CallbackManager;

#[derive(Deref, DerefMut, Default)]
pub struct VecBoxManager<T>(Vec<Box<dyn FnMut(&T) -> bool>>);

impl<T> VecBoxManager<T> {
    pub const fn new() -> Self {
        Self(Vec::new())
    }
}

impl<T> CallbackManager<T> for VecBoxManager<T> {
    fn register<F: 'static + FnMut(&T) -> bool>(&mut self, f: F) {
        self.push(Box::new(f));
    }

    fn callback(&mut self, value: &T) {
        let mut i = 0;
        let mut len = self.len();
        let ptr_slice = self.as_ptr().cast_mut();
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

        self.truncate(len);
    }
}
