use crate::{DataLock, DataLockFactory, DataReadLock};

impl<T> DataReadLock for std::sync::RwLock<T> {
    type Target = T;
    type ReadGuard<'read> = std::sync::RwLockReadGuard<'read, T> where T: 'read;
    fn read(&self) -> Self::ReadGuard<'_> {
        self.read().unwrap()
    }
}

impl<T> DataLock for std::sync::RwLock<T> {
    type WriteGuard<'write> = std::sync::RwLockWriteGuard<'write, T> where T: 'write;

    fn write(&self) -> Self::WriteGuard<'_> {
        self.write().unwrap()
    }
}

impl<T> DataLockFactory<T> for std::sync::RwLock<T> {
    type Lock = Self;

    fn new(init: T) -> Self::Lock {
        Self::new(init)
    }
}
