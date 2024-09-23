use parking_lot;

use crate::{
    DataReadLock, DataWriteLock, RawSensorData, ReadGuardSpecifier, SensorObserver, SensorWriter,
};

impl<T> ReadGuardSpecifier for parking_lot::RwLock<T> {
    type Target = T;
    type ReadGuard<'read> = parking_lot::RwLockReadGuard<'read, T> where T: 'read;
}

impl<T> DataReadLock for parking_lot::RwLock<T> {
    #[inline(always)]
    fn read(&self) -> Self::ReadGuard<'_> {
        self.read()
    }

    #[inline(always)]
    fn try_read(&self) -> Option<Self::ReadGuard<'_>> {
        self.try_read()
    }
}

impl<T> DataWriteLock for parking_lot::RwLock<T> {
    type WriteGuard<'write> = parking_lot::RwLockWriteGuard<'write, T> where T: 'write;
    type DowngradedGuard<'read> = Self::ReadGuard<'read>
    where
        Self: 'read;

    #[inline(always)]
    fn write(&self) -> Self::WriteGuard<'_> {
        self.write()
    }

    #[inline(always)]
    fn try_write(&self) -> Option<Self::WriteGuard<'_>> {
        self.try_write()
    }

    #[inline(always)]
    fn atomic_downgrade(write_guard: Self::WriteGuard<'_>) -> Self::DowngradedGuard<'_> {
        parking_lot::RwLockWriteGuard::downgrade(write_guard)
    }
}

impl<T> DataWriteLock for parking_lot::Mutex<T> {
    type WriteGuard<'write> = parking_lot::MutexGuard<'write, T> where T: 'write;
    type DowngradedGuard<'read> = Self::WriteGuard<'read>
    where
        Self: 'read;
    #[inline(always)]
    fn write(&self) -> Self::WriteGuard<'_> {
        self.lock()
    }

    #[inline(always)]
    fn try_write(&self) -> Option<Self::WriteGuard<'_>> {
        self.try_lock()
    }

    #[inline(always)]
    fn atomic_downgrade(write_guard: Self::WriteGuard<'_>) -> Self::DowngradedGuard<'_> {
        write_guard
    }
}

impl<T> ReadGuardSpecifier for parking_lot::Mutex<T> {
    type Target = T;
    type ReadGuard<'read> = parking_lot::MutexGuard<'read, T> where T: 'read;
}

impl<T> DataReadLock for parking_lot::Mutex<T> {
    #[inline(always)]
    fn read(&self) -> Self::ReadGuard<'_> {
        self.lock()
    }

    #[inline(always)]
    fn try_read(&self) -> Option<Self::ReadGuard<'_>> {
        self.try_lock()
    }
}

pub type RwSensorData<T> = RawSensorData<parking_lot::RwLock<T>, ()>;
pub type RwSensor<'a, T> = SensorObserver<T, &'a RwSensorData<T>>;
pub type RwSensorWriter<T> = SensorWriter<T, RwSensorData<T>>;

impl<T> RwSensorWriter<T> {
    #[inline(always)]
    pub const fn new(init: T) -> Self {
        SensorWriter::from_parts(parking_lot::RwLock::new(init), ())
    }
}
impl<T> From<T> for RwSensorWriter<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

pub type MutexSensorData<T> = RawSensorData<parking_lot::Mutex<T>, ()>;
pub type MutexSensor<'a, T> = SensorObserver<T, &'a MutexSensorData<T>>;
pub type MutexSensorWriter<T> = SensorWriter<T, MutexSensorData<T>>;

impl<T> MutexSensorWriter<T> {
    #[inline(always)]
    pub const fn new(init: T) -> Self {
        SensorWriter::from_parts(parking_lot::Mutex::new(init), ())
    }
}

impl<T> From<T> for MutexSensorWriter<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

#[cfg(feature = "alloc")]
pub use alloc_req::*;
#[cfg(feature = "alloc")]
mod alloc_req {
    extern crate alloc;
    use alloc::sync::Arc;

    use crate::{executor::standard::StdExec, RawSensorData, SensorObserver, SensorWriter};

    pub type RwSensorDataExec<T> = RawSensorData<parking_lot::RwLock<T>, StdExec<T>>;
    pub type RwSensorExec<'a, T> = SensorObserver<T, &'a RwSensorDataExec<T>>;
    pub type RwSensorWriterExec<T> = SensorWriter<T, RwSensorDataExec<T>>;
    impl<T> RwSensorWriterExec<T> {
        #[inline(always)]
        pub const fn new(init: T) -> Self {
            SensorWriter::from_parts(parking_lot::RwLock::new(init), StdExec::new())
        }
    }

    impl<T> From<T> for RwSensorWriterExec<T> {
        #[inline(always)]
        fn from(value: T) -> Self {
            Self::new(value)
        }
    }

    pub type MutexSensorDataExec<T> = RawSensorData<parking_lot::Mutex<T>, StdExec<T>>;
    pub type MutexSensorExec<'a, T> = SensorObserver<T, &'a MutexSensorDataExec<T>>;
    pub type MutexSensorWriterExec<T> = SensorWriter<T, MutexSensorDataExec<T>>;
    impl<T> MutexSensorWriterExec<T> {
        #[inline(always)]
        pub const fn new(init: T) -> Self {
            SensorWriter::from_parts(parking_lot::Mutex::new(init), StdExec::new())
        }
    }

    impl<T> From<T> for MutexSensorWriterExec<T> {
        #[inline(always)]
        fn from(value: T) -> Self {
            Self::new(value)
        }
    }

    pub type ArcRwSensorData<T> = Arc<RawSensorData<parking_lot::RwLock<T>, ()>>;
    pub type ArcRwSensor<T> = SensorObserver<T, ArcRwSensorData<T>>;
    pub type ArcRwSensorWriter<T> = SensorWriter<T, ArcRwSensorData<T>>;
    impl<T> ArcRwSensorWriter<T> {
        #[inline(always)]
        pub fn new(init: T) -> Self {
            SensorWriter::from_parts_arc(parking_lot::RwLock::new(init), ())
        }
    }

    impl<T> From<T> for ArcRwSensorWriter<T> {
        #[inline(always)]
        fn from(value: T) -> Self {
            Self::new(value)
        }
    }

    pub type ArcMutexSensorData<T> = Arc<RawSensorData<parking_lot::Mutex<T>, ()>>;
    pub type ArcMutexSensor<T> = SensorObserver<T, ArcMutexSensorData<T>>;
    pub type ArcMutexSensorWriter<T> = SensorWriter<T, ArcMutexSensorData<T>>;
    impl<T> ArcMutexSensorWriter<T> {
        #[inline(always)]
        pub fn new(init: T) -> Self {
            SensorWriter::from_parts_arc(parking_lot::Mutex::new(init), ())
        }
    }

    impl<T> From<T> for ArcMutexSensorWriter<T> {
        #[inline(always)]
        fn from(value: T) -> Self {
            Self::new(value)
        }
    }

    pub type ArcRwSensorDataExec<T> = Arc<RawSensorData<parking_lot::RwLock<T>, StdExec<T>>>;
    pub type ArcRwSensorExec<T> = SensorObserver<T, ArcRwSensorDataExec<T>>;
    pub type ArcRwSensorWriterExec<T> = SensorWriter<T, ArcRwSensorDataExec<T>>;
    impl<T> ArcRwSensorWriterExec<T> {
        #[inline(always)]
        pub fn new(init: T) -> Self {
            SensorWriter::from_parts_arc(parking_lot::RwLock::new(init), StdExec::new())
        }
    }

    impl<T> From<T> for ArcRwSensorWriterExec<T> {
        #[inline(always)]
        fn from(value: T) -> Self {
            Self::new(value)
        }
    }

    pub type ArcMutexSensorDataExec<T> = Arc<RawSensorData<parking_lot::Mutex<T>, StdExec<T>>>;
    pub type ArcMutexSensorExec<T> = SensorObserver<T, ArcMutexSensorDataExec<T>>;
    pub type ArcMutexSensorWriterExec<T> = SensorWriter<T, ArcMutexSensorDataExec<T>>;
    impl<T> ArcMutexSensorWriterExec<T> {
        #[inline(always)]
        pub fn new(init: T) -> Self {
            SensorWriter::from_parts_arc(parking_lot::Mutex::new(init), StdExec::new())
        }
    }

    impl<T> From<T> for ArcMutexSensorWriterExec<T> {
        #[inline(always)]
        fn from(value: T) -> Self {
            Self::new(value)
        }
    }
}
