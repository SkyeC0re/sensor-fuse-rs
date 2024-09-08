use std::sync::Arc;

use crate::{
    executor::standard::StdExec, DataReadLock, DataWriteLock, ReadGuardSpecifier, RevisedData,
    SensorWriter,
};

use super::{
    AbstractArcSensorObserver, AbstractArcSensorWriter, AbstractSensorObserver,
    AbstractSensorWriter,
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

pub type RwSensorData<T> = RevisedData<(std::sync::RwLock<T>, ())>;
pub type RwSensor<'a, T> = AbstractSensorObserver<'a, T, std::sync::RwLock<T>, ()>;
pub type RwSensorWriter<T> = AbstractSensorWriter<T, std::sync::RwLock<T>, ()>;

impl<T> RwSensorWriter<T> {
    #[inline(always)]
    pub const fn new(init: T) -> Self {
        SensorWriter::new_from_shared(RevisedData::new((std::sync::RwLock::new(init), ())))
    }
}
impl<T> From<T> for RwSensorWriter<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

pub type MutexSensorData<T> = RevisedData<(std::sync::Mutex<T>, ())>;
pub type MutexSensor<'a, T> = AbstractSensorObserver<'a, T, std::sync::Mutex<T>, ()>;
pub type MutexSensorWriter<T> = AbstractSensorWriter<T, std::sync::Mutex<T>, ()>;

impl<T> MutexSensorWriter<T> {
    #[inline(always)]
    pub const fn new(init: T) -> Self {
        SensorWriter::new_from_shared(RevisedData::new((std::sync::Mutex::new(init), ())))
    }
}

impl<T> From<T> for MutexSensorWriter<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}
pub type RwSensorDataExec<T> = RevisedData<(std::sync::RwLock<T>, StdExec<T>)>;
pub type RwSensorExec<'a, T> = AbstractSensorObserver<'a, T, std::sync::RwLock<T>, StdExec<T>>;
pub type RwSensorWriterExec<T> = AbstractSensorWriter<T, std::sync::RwLock<T>, StdExec<T>>;
impl<T> RwSensorWriterExec<T> {
    #[inline(always)]
    pub const fn new(init: T) -> Self {
        SensorWriter::new_from_shared(RevisedData::new((
            std::sync::RwLock::new(init),
            StdExec::new(),
        )))
    }
}

impl<T> From<T> for RwSensorWriterExec<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

pub type MutexSensorDataExec<T> = RevisedData<(std::sync::Mutex<T>, StdExec<T>)>;
pub type MutexSensorExec<'a, T> =
    AbstractSensorObserver<'a, T, std::sync::Mutex<T>, StdExec<T>>;
pub type MutexSensorWriterExec<T> = AbstractSensorWriter<T, std::sync::Mutex<T>, StdExec<T>>;
impl<T> MutexSensorWriterExec<T> {
    #[inline(always)]
    pub const fn new(init: T) -> Self {
        SensorWriter::new_from_shared(RevisedData::new((
            std::sync::Mutex::new(init),
            StdExec::new(),
        )))
    }
}

impl<T> From<T> for MutexSensorWriterExec<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

pub type ArcRwSensorData<T> = Arc<RevisedData<(std::sync::RwLock<T>, ())>>;
pub type ArcRwSensor<T> = AbstractArcSensorObserver<T, std::sync::RwLock<T>, ()>;
pub type ArcRwSensorWriter<T> = AbstractArcSensorWriter<T, std::sync::RwLock<T>, ()>;
impl<T> ArcRwSensorWriter<T> {
    #[inline(always)]
    pub fn new(init: T) -> Self {
        SensorWriter::new_from_shared(Arc::new(RevisedData::new((
            std::sync::RwLock::new(init),
            (),
        ))))
    }
}

impl<T> From<T> for ArcRwSensorWriter<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

pub type ArcMutexSensorData<T> = Arc<RevisedData<(std::sync::Mutex<T>, ())>>;
pub type ArcMutexSensor<T> = AbstractArcSensorObserver<T, std::sync::Mutex<T>, ()>;
pub type ArcMutexSensorWriter<T> = AbstractArcSensorWriter<T, std::sync::Mutex<T>, ()>;
impl<T> ArcMutexSensorWriter<T> {
    #[inline(always)]
    pub fn new(init: T) -> Self {
        SensorWriter::new_from_shared(Arc::new(RevisedData::new((
            std::sync::Mutex::new(init),
            (),
        ))))
    }
}

impl<T> From<T> for ArcMutexSensorWriter<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

pub type ArcRwSensorDataExec<T> = Arc<RevisedData<(std::sync::RwLock<T>, StdExec<T>)>>;
pub type ArcRwSensorExec<T> = AbstractArcSensorObserver<T, std::sync::RwLock<T>, StdExec<T>>;
pub type ArcRwSensorWriterExec<T> =
    AbstractArcSensorWriter<T, std::sync::RwLock<T>, StdExec<T>>;
impl<T> ArcRwSensorWriterExec<T> {
    #[inline(always)]
    pub fn new(init: T) -> Self {
        SensorWriter::new_from_shared(Arc::new(RevisedData::new((
            std::sync::RwLock::new(init),
            StdExec::new(),
        ))))
    }
}

impl<T> From<T> for ArcRwSensorWriterExec<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

pub type ArcMutexSensorDataExec<T> = Arc<RevisedData<(std::sync::Mutex<T>, StdExec<T>)>>;
pub type ArcMutexSensorExec<T> = AbstractArcSensorObserver<T, std::sync::Mutex<T>, StdExec<T>>;
pub type ArcMutexSensorWriterExec<T> =
    AbstractArcSensorWriter<T, std::sync::Mutex<T>, StdExec<T>>;
impl<T> ArcMutexSensorWriterExec<T> {
    #[inline(always)]
    pub fn new(init: T) -> Self {
        SensorWriter::new_from_shared(Arc::new(RevisedData::new((
            std::sync::Mutex::new(init),
            StdExec::new(),
        ))))
    }
}

impl<T> From<T> for ArcMutexSensorWriterExec<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}
