pub mod std;

use core::{
    future::Future,
    ops::{Deref, DerefMut},
};

use crate::SymResult;

const CLOSED_BIT: usize = 1;
const VERSION_BUMP: usize = 2;
const VERSION_MASK: usize = !CLOSED_BIT;

pub trait SensorCore {
    type Target;

    /// A read guard with shared, immutable access to `type@Self::Target`.
    type ReadGuard<'read>: Deref<Target = Self::Target>
    where
        Self: 'read;

    /// A write guard with exclusive, mutable access to `type@Self::Target`.
    type WriteGuard<'write>: DerefMut<Target = Self::Target>
    where
        Self: 'write;

    fn register_writer(&self);
    fn deregister_writer(&self);

    fn version(&self) -> usize;

    fn bump_version(&self);

    fn try_read(&self) -> Option<Self::ReadGuard<'_>>;
    fn try_write(&self) -> Option<Self::WriteGuard<'_>>;
}

pub trait SensorCoreAsync: SensorCore {
    fn read(&self) -> impl Future<Output = Self::ReadGuard<'_>>;
    fn write(&self) -> impl Future<Output = Self::WriteGuard<'_>>;

    /// Wait until a change from reference version is detected and returns `Ok` if a change was detected or `Err` if
    /// the sensor has been closed (regardless of whether an update occurred). In both cases the current version is returned.
    fn wait_changed(&self, reference_version: usize) -> impl Future<Output = Result<usize, usize>>;

    #[inline]
    fn modify<M: FnOnce(&mut Self::Target) -> bool>(
        &self,
        modifier: M,
    ) -> impl Future<Output = bool> {
        async move {
            let mut guard = self.write().await;
            let modified = modifier(&mut guard);
            if modified {
                self.bump_version();
            }
            drop(guard);
            modified
        }
    }

    #[inline]
    fn wait_for_and_map<'a, O, C: FnMut(Self::ReadGuard<'a>) -> SymResult<O>>(
        &'a self,
        mut condition: C,
        mut reference_version: Option<usize>,
    ) -> impl Future<Output = (SymResult<O>, usize)> {
        async move {
            loop {
                if let Some(version) = reference_version {
                    match self.wait_changed(version).await {
                        Ok(latest_version) => reference_version = Some(latest_version),
                        Err(latest_version) => {
                            let guard = self.read().await;
                            let mapped = match condition(guard) {
                                Ok(v) => v,
                                Err(v) => v,
                            };
                            return (Err(mapped), latest_version);
                        }
                    }
                } else {
                    reference_version = Some(self.version());
                }

                let guard = self.read().await;
                let res = condition(guard);
                if res.is_ok() {
                    return (res, unsafe { reference_version.unwrap_unchecked() });
                }
            }
        }
    }
}

pub trait SensorCoreSync: SensorCore {
    fn read_blocking(&self) -> Self::ReadGuard<'_>;
    fn write_blocking(&self) -> Self::WriteGuard<'_>;

    /// Wait until change from reference version is detected and returns `Ok` if a change was detected or `Err` if
    /// the sensor has been closed (regardless of whether an update occurred). In both cases the current version is returned.
    fn wait_changed_blocking(&self, reference_version: usize) -> Result<usize, usize>;

    #[inline]
    fn modify_blocking<M: FnOnce(&mut Self::Target) -> bool>(&self, modifier: M) -> bool {
        let mut guard = self.write_blocking();
        let modified = modifier(&mut guard);
        if modified {
            self.bump_version();
        }
        drop(guard);
        modified
    }

    #[inline]
    fn wait_for_blocking<C: FnMut(&Self::Target) -> bool>(
        &self,
        mut condition: C,
        mut reference_version: Option<usize>,
    ) -> (SymResult<Self::ReadGuard<'_>>, usize) {
        loop {
            if let Some(version) = reference_version {
                match self.wait_changed_blocking(version) {
                    Ok(latest_version) => reference_version = Some(latest_version),
                    Err(latest_version) => return (Err(self.read_blocking()), latest_version),
                }
            }

            let guard = self.read_blocking();
            if condition(&guard) {
                return (Ok(guard), unsafe { reference_version.unwrap_unchecked() });
            }
        }
    }
}
