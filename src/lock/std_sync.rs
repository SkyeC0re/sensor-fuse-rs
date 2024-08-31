use std::{cell::UnsafeCell, sync::Arc};

use crate::{
    callback::vec_box::VecBoxManager, DataReadLock, DataWriteLock, ExecData, ExecLock,
    ReadGuardSpecifier, RevisedData, SensorWriter, SensorWriterExec,
};

use super::{
    AbstractArcSensorObserver, AbstractArcSensorObserverExec, AbstractArcSensorWriter,
    AbstractArcSensorWriterExec, AbstractSensorObserver, AbstractSensorObserverExec,
    AbstractSensorWriter, AbstractSensorWriterExec,
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

    #[inline(always)]
    fn write(&self) -> Self::WriteGuard<'_> {
        self.write().unwrap()
    }

    #[inline(always)]
    fn try_write(&self) -> Option<Self::WriteGuard<'_>> {
        self.try_write().ok()
    }

    #[inline(always)]
    fn atomic_downgrade(
        write_guard: Self::WriteGuard<'_>,
    ) -> impl std::ops::Deref<Target = Self::Target> {
        write_guard
    }
}

impl<T> DataWriteLock for std::sync::Mutex<T> {
    type WriteGuard<'write> = std::sync::MutexGuard<'write, T> where T: 'write;
    #[inline(always)]
    fn write(&self) -> Self::WriteGuard<'_> {
        self.lock().unwrap()
    }

    #[inline(always)]
    fn try_write(&self) -> Option<Self::WriteGuard<'_>> {
        self.try_lock().ok()
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

pub type RwSensorData<T> = RevisedData<std::sync::RwLock<T>>;
pub type RwSensor<'a, T> = AbstractSensorObserver<'a, std::sync::RwLock<T>>;
pub type RwSensorWriter<T> = AbstractSensorWriter<std::sync::RwLock<T>>;

impl<T> RwSensorWriter<T> {
    #[inline(always)]
    pub const fn new(init: T) -> Self {
        SensorWriter(RevisedData::new(std::sync::RwLock::new(init)))
    }
}
impl<T> From<T> for RwSensorWriter<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

pub type RwSensorDataExec<T> =
    RevisedData<ExecLock<std::sync::RwLock<ExecData<T, VecBoxManager<T>>>, T, VecBoxManager<T>>>;
pub type RwSensorExec<'a, T> = AbstractSensorObserverExec<
    'a,
    std::sync::RwLock<ExecData<T, VecBoxManager<T>>>,
    T,
    VecBoxManager<T>,
>;
pub type RwSensorWriterExec<T> =
    AbstractSensorWriterExec<std::sync::RwLock<ExecData<T, VecBoxManager<T>>>, T, VecBoxManager<T>>;
impl<T> RwSensorWriterExec<T> {
    #[inline(always)]
    pub const fn new(init: T) -> Self {
        SensorWriterExec(RevisedData::new(ExecLock::new(std::sync::RwLock::new(
            ExecData {
                exec_manager: UnsafeCell::new(VecBoxManager::new()),
                data: init,
            },
        ))))
    }
}

impl<T> From<T> for RwSensorWriterExec<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

pub type MutexSensorData<T> = RevisedData<std::sync::RwLock<T>>;
pub type MutexSensor<'a, T> = AbstractSensorObserver<'a, std::sync::RwLock<T>>;
pub type MutexSensorWriter<T> = AbstractSensorWriter<std::sync::Mutex<T>>;

impl<T> MutexSensorWriter<T> {
    pub fn new(init: T) -> Self {
        SensorWriter(RevisedData::new(std::sync::Mutex::new(init)))
    }
}

impl<T> From<T> for MutexSensorWriter<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

pub type MutexSensorDataExec<T> =
    RevisedData<ExecLock<std::sync::Mutex<ExecData<T, VecBoxManager<T>>>, T, VecBoxManager<T>>>;
pub type MutexSensorExec<'a, T> = AbstractSensorObserverExec<
    'a,
    std::sync::Mutex<ExecData<T, VecBoxManager<T>>>,
    T,
    VecBoxManager<T>,
>;

pub type MutexSensorWriterExec<T> =
    AbstractSensorWriterExec<std::sync::Mutex<ExecData<T, VecBoxManager<T>>>, T, VecBoxManager<T>>;
impl<T> MutexSensorWriterExec<T> {
    #[inline(always)]
    pub const fn new(init: T) -> Self {
        SensorWriterExec(RevisedData::new(ExecLock::new(std::sync::Mutex::new(
            ExecData {
                exec_manager: UnsafeCell::new(VecBoxManager::new()),
                data: init,
            },
        ))))
    }
}

impl<T> From<T> for MutexSensorWriterExec<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

pub type ArcRwSensor<T> = AbstractArcSensorObserver<std::sync::RwLock<T>>;
pub type ArcRwSensorWriter<T> = AbstractArcSensorWriter<std::sync::RwLock<T>>;

impl<T> ArcRwSensorWriter<T> {
    pub fn new(init: T) -> Self {
        SensorWriter(Arc::new(RevisedData::new(std::sync::RwLock::new(init))))
    }
}
impl<T> From<T> for ArcRwSensorWriter<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

pub type ArcRwSensorExec<T> = AbstractArcSensorObserverExec<
    std::sync::RwLock<ExecData<T, VecBoxManager<T>>>,
    T,
    VecBoxManager<T>,
>;
pub type ArcRwSensorWriterExec<T> = AbstractArcSensorWriterExec<
    std::sync::RwLock<ExecData<T, VecBoxManager<T>>>,
    T,
    VecBoxManager<T>,
>;

impl<T> ArcRwSensorWriterExec<T> {
    #[inline(always)]
    pub fn new(init: T) -> Self {
        SensorWriterExec(Arc::new(RevisedData::new(ExecLock::new(
            std::sync::RwLock::new(ExecData {
                exec_manager: UnsafeCell::new(VecBoxManager::new()),
                data: init,
            }),
        ))))
    }
}
impl<T> From<T> for ArcRwSensorWriterExec<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

pub type ArcMutexSensor<T> = AbstractArcSensorObserver<std::sync::Mutex<T>>;
pub type ArcMutexSensorWriter<T> = AbstractArcSensorWriter<std::sync::Mutex<T>>;

impl<T> ArcMutexSensorWriter<T> {
    pub fn new(init: T) -> Self {
        SensorWriter(Arc::new(RevisedData::new(std::sync::Mutex::new(init))))
    }
}

impl<T> From<T> for ArcMutexSensorWriter<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

pub type ArcMutexSensorExec<T> = AbstractArcSensorObserverExec<
    std::sync::Mutex<ExecData<T, VecBoxManager<T>>>,
    T,
    VecBoxManager<T>,
>;
pub type ArcMutexSensorWriterExec<T> = AbstractArcSensorWriterExec<
    std::sync::Mutex<ExecData<T, VecBoxManager<T>>>,
    T,
    VecBoxManager<T>,
>;
impl<T> ArcMutexSensorWriterExec<T> {
    #[inline(always)]
    pub fn new(init: T) -> Self {
        SensorWriterExec(Arc::new(RevisedData::new(ExecLock::new(
            std::sync::Mutex::new(ExecData {
                exec_manager: UnsafeCell::new(VecBoxManager::new()),
                data: init,
            }),
        ))))
    }
}

impl<T> From<T> for ArcMutexSensorWriterExec<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}
