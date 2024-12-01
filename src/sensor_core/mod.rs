pub mod alloc;

use core::{
    future::Future,
    ops::{Deref, DerefMut},
};

use crate::ObservationStatus;

// All credit to [Tokio's Watch Channel](https://docs.rs/tokio/latest/tokio/sync/watch/index.html). If it's not broken don't fix it.
pub(crate) const CLOSED_BIT: usize = 1;
pub(crate) const VERSION_BUMP: usize = 2;
pub(crate) const VERSION_MASK: usize = !CLOSED_BIT;

#[inline(always)]
pub(crate) const fn closed_bit_set(version: usize) -> bool {
    version & CLOSED_BIT > 0
}

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

    /// Marks the current value as unseen. This should also wake any observers waiting for sensor updates.
    fn mark_unseen(&self);

    fn try_read(&self) -> Option<Self::ReadGuard<'_>>;
    fn try_write(&self) -> Option<Self::WriteGuard<'_>>;
}

/// Asyncronous sensor core functionality.
pub trait SensorCoreAsync: SensorCore {
    /// Asyncronously acquires a read lock to the underlying data.
    fn read(&self) -> impl Future<Output = Self::ReadGuard<'_>>;
    /// Asyncronously acquires a write lock to the underlying data.
    fn write(&self) -> impl Future<Output = Self::WriteGuard<'_>>;

    /// Wait until a change from reference version is detected and returns the most recent version and the observation status.
    fn wait_changed(
        &self,
        reference_version: usize,
    ) -> impl Future<Output = (usize, ObservationStatus)>;

    /// Modify the current sensor data and optionally update the version of the sensor.
    #[inline]
    async fn modify<M: FnOnce(&mut Self::Target) -> bool>(
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
        mut reference_version: usize,
    ) -> (usize, (Self::ReadGuard<'_>, ObservationStatus)) {
        loop {
            let (latest_version, status) = self.wait_changed(reference_version).await;
            if status.closed() {
                let guard = self.read().await;
                let success = condition(&guard);
                return (
                    latest_version,
                    (
                        guard,
                        ObservationStatus::new()
                            .set_closed()
                            .modify_success(success),
                    ),
                );
            } else {
                reference_version = latest_version;
            }

            let guard = self.read().await;
            let success = condition(&guard);
            if success {
                // Ensure the latest version associated with the guard is returned.
                let version = self.version();
                return (
                    version,
                    (
                        guard,
                        ObservationStatus::new()
                            .set_success()
                            .modify_closed(version & CLOSED_BIT > 0),
                    ),
                );
            }
        }
    }
}

pub trait SensorCoreSync: SensorCore {
    fn read_blocking(&self) -> Self::ReadGuard<'_>;
    fn write_blocking(&self) -> Self::WriteGuard<'_>;

    /// Wait until change from reference version is detected and returns `Ok` if a change was detected or `Err` if
    /// the sensor has been closed (regardless of whether an update occurred). In both cases the current version is returned.
    fn wait_changed_blocking(&self, reference_version: usize) -> (usize, ObservationStatus);

    #[inline]
    fn modify_blocking<M: FnOnce(&mut Self::Target) -> bool>(&self, modifier: M) -> bool {
        let mut guard = self.write_blocking();
        let modified = modifier(&mut guard);
        if modified {
            self.mark_unseen();
        }
        drop(guard);
        modified
    }

    #[inline]
    fn wait_for_and_map_blocking<O, C: FnMut(&Self::Target) -> bool>(
        &self,
        mut condition: C,
        mut reference_version: Option<usize>,
    ) -> (usize, (Self::ReadGuard<'_>, ObservationStatus)) {
        loop {
            if let Some(version) = reference_version {
                let (latest_version, status) = self.wait_changed_blocking(version);
                if status.closed() {
                    let guard = self.read_blocking();
                    let success = condition(&guard);
                    return (
                        latest_version,
                        (
                            guard,
                            ObservationStatus::new()
                                .set_closed()
                                .modify_success(success),
                        ),
                    );
                } else {
                    reference_version = Some(latest_version);
                }
            } else {
                reference_version = Some(self.version());
            }

            let guard = self.read_blocking();
            let success = condition(&guard);
            if success {
                // Ensure the latest version associated with the guard is returned.
                let version = self.version();
                return (
                    version,
                    (
                        guard,
                        ObservationStatus::new()
                            .set_success()
                            .modify_closed(version & CLOSED_BIT > 0),
                    ),
                );
            }
        }
    }
}
