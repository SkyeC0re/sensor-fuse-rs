use std::sync::Arc;

use crate::{
    executor::standard::StdExec, DataReadLock, DataWriteLock, RawSensorData, ReadGuardSpecifier,
    SensorObserver, SensorWriter,
};

impl<T> ReadGuardSpecifier for std::sync::RwLock<T> {
    type Target = T;
    type ReadGuard<'read> = std::sync::RwLockReadGuard<'read, T> where T: 'read;
}

impl<T> DataReadLock for std::sync::RwLock<T> {
    #[inline(always)]
    fn read(&self) -> Self::ReadGuard<'_> {
        self.read().unwrap()
    }

    #[inline(always)]
    fn try_read(&self) -> Option<Self::ReadGuard<'_>> {
        self.try_read().ok()
    }
}

impl<T> DataWriteLock for std::sync::RwLock<T> {
    type WriteGuard<'write> = std::sync::RwLockWriteGuard<'write, T> where T: 'write;
    type DowngradedGuard<'read> = Self::WriteGuard<'read>
    where
        Self: 'read;

    #[inline(always)]
    fn write(&self) -> Self::WriteGuard<'_> {
        self.write().unwrap()
    }

    #[inline(always)]
    fn try_write(&self) -> Option<Self::WriteGuard<'_>> {
        self.try_write().ok()
    }

    #[inline(always)]
    fn atomic_downgrade(write_guard: Self::WriteGuard<'_>) -> Self::DowngradedGuard<'_> {
        write_guard
    }
}

impl<T> DataWriteLock for std::sync::Mutex<T> {
    type WriteGuard<'write> = std::sync::MutexGuard<'write, T> where T: 'write;
    type DowngradedGuard<'read> = Self::WriteGuard<'read>
    where
        Self: 'read;
    #[inline(always)]
    fn write(&self) -> Self::WriteGuard<'_> {
        self.lock().unwrap()
    }

    #[inline(always)]
    fn try_write(&self) -> Option<Self::WriteGuard<'_>> {
        self.try_lock().ok()
    }

    #[inline(always)]
    fn atomic_downgrade(write_guard: Self::WriteGuard<'_>) -> Self::DowngradedGuard<'_> {
        write_guard
    }
}

impl<T> ReadGuardSpecifier for std::sync::Mutex<T> {
    type Target = T;
    type ReadGuard<'read> = std::sync::MutexGuard<'read, T> where T: 'read;
}

impl<T> DataReadLock for std::sync::Mutex<T> {
    #[inline(always)]
    fn read(&self) -> Self::ReadGuard<'_> {
        self.lock().unwrap()
    }

    #[inline(always)]
    fn try_read(&self) -> Option<Self::ReadGuard<'_>> {
        self.try_lock().ok()
    }
}

pub type RwSensorData<T> = RawSensorData<std::sync::RwLock<T>, ()>;
pub type RwSensor<'a, T> = SensorObserver<T, &'a RwSensorData<T>>;
pub type RwSensorWriter<T> = SensorWriter<T, RwSensorData<T>>;

impl<T> RwSensorWriter<T> {
    #[inline(always)]
    pub const fn new(init: T) -> Self {
        SensorWriter::from_parts(std::sync::RwLock::new(init), ())
    }
}
impl<T> From<T> for RwSensorWriter<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

pub type MutexSensorData<T> = RawSensorData<std::sync::Mutex<T>, ()>;
pub type MutexSensor<'a, T> = SensorObserver<T, &'a MutexSensorData<T>>;
pub type MutexSensorWriter<T> = SensorWriter<T, MutexSensorData<T>>;

impl<T> MutexSensorWriter<T> {
    #[inline(always)]
    pub const fn new(init: T) -> Self {
        SensorWriter::from_parts(std::sync::Mutex::new(init), ())
    }
}

impl<T> From<T> for MutexSensorWriter<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

pub type RwSensorDataExec<T> = RawSensorData<std::sync::RwLock<T>, StdExec<T>>;
pub type RwSensorExec<'a, T> = SensorObserver<T, &'a RwSensorDataExec<T>>;
pub type RwSensorWriterExec<T> = SensorWriter<T, RwSensorDataExec<T>>;
impl<T> RwSensorWriterExec<T> {
    #[inline(always)]
    pub const fn new(init: T) -> Self {
        SensorWriter::from_parts(std::sync::RwLock::new(init), StdExec::new())
    }
}

impl<T> From<T> for RwSensorWriterExec<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

pub type MutexSensorDataExec<T> = RawSensorData<std::sync::Mutex<T>, StdExec<T>>;
pub type MutexSensorExec<'a, T> = SensorObserver<T, &'a MutexSensorDataExec<T>>;
pub type MutexSensorWriterExec<T> = SensorWriter<T, MutexSensorDataExec<T>>;
impl<T> MutexSensorWriterExec<T> {
    #[inline(always)]
    pub const fn new(init: T) -> Self {
        SensorWriter::from_parts(std::sync::Mutex::new(init), StdExec::new())
    }
}

impl<T> From<T> for MutexSensorWriterExec<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

pub type ArcRwSensorData<T> = Arc<RawSensorData<std::sync::RwLock<T>, ()>>;
pub type ArcRwSensor<T> = SensorObserver<T, ArcRwSensorData<T>>;
pub type ArcRwSensorWriter<T> = SensorWriter<T, ArcRwSensorData<T>>;
impl<T> ArcRwSensorWriter<T> {
    #[inline(always)]
    pub fn new(init: T) -> Self {
        SensorWriter::from_parts_arc(std::sync::RwLock::new(init), ())
    }
}

impl<T> From<T> for ArcRwSensorWriter<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

pub type ArcMutexSensorData<T> = Arc<RawSensorData<std::sync::Mutex<T>, ()>>;
pub type ArcMutexSensor<T> = SensorObserver<T, ArcMutexSensorData<T>>;
pub type ArcMutexSensorWriter<T> = SensorWriter<T, ArcMutexSensorData<T>>;
impl<T> ArcMutexSensorWriter<T> {
    #[inline(always)]
    pub fn new(init: T) -> Self {
        SensorWriter::from_parts_arc(std::sync::Mutex::new(init), ())
    }
}

impl<T> From<T> for ArcMutexSensorWriter<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

pub type ArcRwSensorDataExec<T> = Arc<RawSensorData<std::sync::RwLock<T>, StdExec<T>>>;
pub type ArcRwSensorExec<T> = SensorObserver<T, ArcRwSensorDataExec<T>>;
pub type ArcRwSensorWriterExec<T> = SensorWriter<T, ArcRwSensorDataExec<T>>;
impl<T> ArcRwSensorWriterExec<T> {
    #[inline(always)]
    pub fn new(init: T) -> Self {
        SensorWriter::from_parts_arc(std::sync::RwLock::new(init), StdExec::new())
    }
}

impl<T> From<T> for ArcRwSensorWriterExec<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

pub type ArcMutexSensorDataExec<T> = Arc<RawSensorData<std::sync::Mutex<T>, StdExec<T>>>;
pub type ArcMutexSensorExec<T> = SensorObserver<T, ArcMutexSensorDataExec<T>>;
pub type ArcMutexSensorWriterExec<T> = SensorWriter<T, ArcMutexSensorDataExec<T>>;
impl<T> ArcMutexSensorWriterExec<T> {
    #[inline(always)]
    pub fn new(init: T) -> Self {
        SensorWriter::from_parts_arc(std::sync::Mutex::new(init), StdExec::new())
    }
}

impl<T> From<T> for ArcMutexSensorWriterExec<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}
