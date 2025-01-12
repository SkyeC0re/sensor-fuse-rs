//! Sensor core functionality and standard sensor core implementations.
//!

pub mod alloc;
pub mod no_alloc;

use core::{
    future::Future,
    ops::{Deref, DerefMut},
};

use crate::{ObservationStatus, SymResult, Version, STATUS_SUCCESS_BIT};

// All credit to [Tokio's Watch Channel](https://docs.rs/tokio/latest/tokio/sync/watch/index.html). If it's not broken don't fix it.
pub(crate) const CLOSED_BIT: usize = 1;
pub(crate) const VERSION_BUMP: usize = 2;
pub(crate) const VERSION_MASK: usize = !CLOSED_BIT;

#[inline(always)]
pub(crate) const fn closed_bit_set(version: usize) -> bool {
    version & CLOSED_BIT != 0
}

/// Should be initialized with a writer count of one.
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

    /// Increment the internal writer count of the sensor.
    ///
    /// # Safety
    ///
    /// Implementors are free to optimize for the fact that once the last writer is dropped, this function
    /// will not be called again.
    unsafe fn register_writer(&self);
    /// Increment the internal writer count of the sensor.
    ///
    /// # Safety
    ///
    /// Implementors are free to optimize for the fact this will be exactly once for each writer when the writer is dropped and
    /// only after a call to `fn@register_writer` (except for the initial writer which should already be accounted for at initialization).
    unsafe fn deregister_writer(&self);

    fn version(&self) -> Version;

    /// Marks the current value as unseen. This should also wake any observers waiting for sensor updates.
    fn mark_unseen(&self);

    fn try_read(&self) -> Option<Self::ReadGuard<'_>>;

    /// Attempt to acquire a write guard without waiting.
    ///
    /// # Safety
    ///
    /// This function is allowed to exhibit undefinded behaviour if called when there are no registered writers.
    unsafe fn try_write(&self) -> Option<Self::WriteGuard<'_>>;
}

/// Asyncronous sensor core functionality.
pub trait SensorCoreAsync: SensorCore {
    /// Asyncronously acquires a read lock to the underlying data.
    fn read(&self) -> impl Future<Output = Self::ReadGuard<'_>>;
    /// Asyncronously acquires a write lock to the underlying data.
    fn write(&self) -> impl Future<Output = Self::WriteGuard<'_>>;

    /// Wait until a change from reference version is detected and returns the most recent version and the observation status.
    fn wait_changed(&self, reference_version: Version) -> impl Future<Output = Version>;

    /// Modify the current sensor data and optionally update the version of the sensor.
    ///
    /// # Safety
    ///
    /// This function is allowed to exhibit undefinded behaviour if called when there are no registered writers.
    #[inline]
    async unsafe fn modify<M: FnOnce(&mut Self::Target) -> bool>(
        &self,
        modifier: M,
    ) -> (Self::WriteGuard<'_>, bool) {
        let mut guard = self.write().await;
        let modified = modifier(&mut guard);
        if modified {
            self.mark_unseen();
        }
        (guard, modified)
    }

    /// Asynchronously wait for a particular condition to become true. This method initially waits for the sensor's current version to deviate from
    /// the reference version before starting to check the sensor value with the condition function.
    #[inline]
    async fn wait_for<C: FnMut(&Self::Target) -> bool>(
        &self,
        mut condition: C,
        mut reference_version: Version,
    ) -> (Self::ReadGuard<'_>, Version) {
        loop {
            let latest_version = self.wait_changed(reference_version).await;

            let guard = self.read().await;
            if latest_version.closed_bit_set() {
                return (guard, latest_version);
            }

            if condition(&guard) {
                // Ensure the latest version associated with the guard is returned.
                let version = self.version();
                return (guard, version);
            }

            reference_version = latest_version;
        }
    }
}
