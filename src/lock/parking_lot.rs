use std::{cell::UnsafeCell, sync::Arc};

use parking_lot;

use crate::{
    callback::vec_box::VecBoxManager, DataReadLock, DataWriteLock, ExecData, ExecLock,
    ReadGuardSpecifier, RevisedData, SensorWriter, SensorWriterExec,
};

use super::{
    AbstractArcSensorObserver, AbstractArcSensorObserverExec, AbstractArcSensorWriter,
    AbstractArcSensorWriterExec, AbstractSensorObserver, AbstractSensorObserverExec,
    AbstractSensorWriter, AbstractSensorWriterExec,
};

pub type RwSensorData<T> = RevisedData<parking_lot::RwLock<T>>;
pub type RwSensor<'a, T> = AbstractSensorObserver<'a, parking_lot::RwLock<T>>;
pub type RwSensorWriter<T> = AbstractSensorWriter<parking_lot::RwLock<T>>;

impl<T> RwSensorWriter<T> {
    #[inline(always)]
    pub const fn new(init: T) -> Self {
        SensorWriter(RevisedData::new(parking_lot::RwLock::new(init)))
    }
}
impl<T> From<T> for RwSensorWriter<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

pub type RwSensorDataExec<T> =
    RevisedData<ExecLock<parking_lot::RwLock<ExecData<T, VecBoxManager<T>>>, T, VecBoxManager<T>>>;
pub type RwSensorExec<'a, T> = AbstractSensorObserverExec<
    'a,
    parking_lot::RwLock<ExecData<T, VecBoxManager<T>>>,
    T,
    VecBoxManager<T>,
>;
pub type RwSensorWriterExec<T> = AbstractSensorWriterExec<
    parking_lot::RwLock<ExecData<T, VecBoxManager<T>>>,
    T,
    VecBoxManager<T>,
>;
impl<T> RwSensorWriterExec<T> {
    #[inline(always)]
    pub const fn new(init: T) -> Self {
        SensorWriterExec(RevisedData::new(ExecLock::new(parking_lot::RwLock::new(
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

pub type MutexSensorData<T> = RevisedData<parking_lot::RwLock<T>>;
pub type MutexSensor<'a, T> = AbstractSensorObserver<'a, parking_lot::RwLock<T>>;
pub type MutexSensorWriter<T> = AbstractSensorWriter<parking_lot::Mutex<T>>;

impl<T> MutexSensorWriter<T> {
    pub fn new(init: T) -> Self {
        SensorWriter(RevisedData::new(parking_lot::Mutex::new(init)))
    }
}

impl<T> From<T> for MutexSensorWriter<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

pub type MutexSensorDataExec<T> =
    RevisedData<ExecLock<parking_lot::Mutex<ExecData<T, VecBoxManager<T>>>, T, VecBoxManager<T>>>;
pub type MutexSensorExec<'a, T> = AbstractSensorObserverExec<
    'a,
    parking_lot::Mutex<ExecData<T, VecBoxManager<T>>>,
    T,
    VecBoxManager<T>,
>;

pub type MutexSensorWriterExec<T> = AbstractSensorWriterExec<
    parking_lot::Mutex<ExecData<T, VecBoxManager<T>>>,
    T,
    VecBoxManager<T>,
>;
impl<T> MutexSensorWriterExec<T> {
    #[inline(always)]
    pub const fn new(init: T) -> Self {
        SensorWriterExec(RevisedData::new(ExecLock::new(parking_lot::Mutex::new(
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

pub type ArcRwSensor<T> = AbstractArcSensorObserver<parking_lot::RwLock<T>>;
pub type ArcRwSensorWriter<T> = AbstractArcSensorWriter<parking_lot::RwLock<T>>;

impl<T> ArcRwSensorWriter<T> {
    pub fn new(init: T) -> Self {
        SensorWriter(Arc::new(RevisedData::new(parking_lot::RwLock::new(init))))
    }
}
impl<T> From<T> for ArcRwSensorWriter<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

pub type ArcRwSensorExec<T> = AbstractArcSensorObserverExec<
    parking_lot::RwLock<ExecData<T, VecBoxManager<T>>>,
    T,
    VecBoxManager<T>,
>;
pub type ArcRwSensorWriterExec<T> = AbstractArcSensorWriterExec<
    parking_lot::RwLock<ExecData<T, VecBoxManager<T>>>,
    T,
    VecBoxManager<T>,
>;

impl<T> ArcRwSensorWriterExec<T> {
    #[inline(always)]
    pub fn new(init: T) -> Self {
        SensorWriterExec(Arc::new(RevisedData::new(ExecLock::new(
            parking_lot::RwLock::new(ExecData {
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

pub type ArcMutexSensor<T> = AbstractArcSensorObserver<parking_lot::Mutex<T>>;
pub type ArcMutexSensorWriter<T> = AbstractArcSensorWriter<parking_lot::Mutex<T>>;

impl<T> ArcMutexSensorWriter<T> {
    pub fn new(init: T) -> Self {
        SensorWriter(Arc::new(RevisedData::new(parking_lot::Mutex::new(init))))
    }
}

impl<T> From<T> for ArcMutexSensorWriter<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

pub type ArcMutexSensorExec<T> = AbstractArcSensorObserverExec<
    parking_lot::Mutex<ExecData<T, VecBoxManager<T>>>,
    T,
    VecBoxManager<T>,
>;
pub type ArcMutexSensorWriterExec<T> = AbstractArcSensorWriterExec<
    parking_lot::Mutex<ExecData<T, VecBoxManager<T>>>,
    T,
    VecBoxManager<T>,
>;
impl<T> ArcMutexSensorWriterExec<T> {
    #[inline(always)]
    pub fn new(init: T) -> Self {
        SensorWriterExec(Arc::new(RevisedData::new(ExecLock::new(
            parking_lot::Mutex::new(ExecData {
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

    #[inline(always)]
    fn write(&self) -> Self::WriteGuard<'_> {
        self.write()
    }

    #[inline(always)]
    fn try_write(&self) -> Option<Self::WriteGuard<'_>> {
        self.try_write()
    }

    #[inline(always)]
    fn atomic_downgrade(
        write_guard: Self::WriteGuard<'_>,
    ) -> impl std::ops::Deref<Target = Self::Target> {
        parking_lot::RwLockWriteGuard::downgrade(write_guard)
    }
}

impl<T> DataWriteLock for parking_lot::Mutex<T> {
    type WriteGuard<'write> = parking_lot::MutexGuard<'write, T> where T: 'write;
    #[inline(always)]
    fn write(&self) -> Self::WriteGuard<'_> {
        self.lock()
    }

    #[inline(always)]
    fn try_write(&self) -> Option<Self::WriteGuard<'_>> {
        self.try_lock()
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
