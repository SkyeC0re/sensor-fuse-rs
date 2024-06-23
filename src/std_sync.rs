use crate::DataLock;

impl<T> DataLock for std::sync::RwLock<T> {
    type Target = T;

    type ReadGuard<'read> = std::sync::RwLockReadGuard<'read, T> where T: 'read;

    type WriteGuard<'write> = std::sync::RwLockWriteGuard<'write, T> where T: 'write;

    fn new(init: Self::Target) -> Self {
        Self::new(init)
    }

    fn read(&self) -> Self::ReadGuard<'_> {
        self.read().unwrap()
    }

    fn write(&self) -> Self::WriteGuard<'_> {
        self.write().unwrap()
    }
}
