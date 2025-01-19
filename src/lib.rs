//! # Sensor-Fuse
//! An un-opinionated no-std compatible observer/callback framework for the opinionated user.
//!
//! Designed as generalization to [Tokio's watch channel](https://docs.rs/tokio/latest/tokio/sync/watch/index.html), Sensor-Fuse aims
//! to provide a modular framework for implementing observer or observer + callback patterns. From the perspective of this crate sensor data can
//! be defined as the following:
//! - The raw data.
//! - A (possibly atomic) version counter.
//! - A change detection strategy.
//! - A locking strategy that allows atomic access to the raw data.
//! - An optional heap pointer that wraps all of the above.
//!
//! Given this definition, Tokio's watch channel only allows for the generalization of the raw data type, however
//! Sensor-Fuse additionally allows for the generalization of the locking strategy and optional heap pointer as well.
//! In fact, Tokio's watch channel within this framework is roughly equivalent to selecting `std::sync::RwLock` as the locking strategy,
//! with an `Arc` pointer wrapping everything.
//!
//! ```
//! extern crate sensor_fuse;
//! // TODO example.
//! ```
//!

#![warn(bad_style)]
#![warn(missing_docs)]
#![warn(trivial_casts)]
#![warn(trivial_numeric_casts)]
#![warn(unused)]
#![warn(unused_extern_crates)]
#![warn(unused_import_braces)]
#![warn(unused_qualifications)]
#![warn(unused_results)]
// This project explicitly does not enforce any bounds an any futures, but attempts to use conditional `Send`
// approximation wherever possible. This allows the user to, for example, pass `!Send` data to
// futures that are otherwise `Send`, and the future will naturally become `!Send`, without requiring the user
// to call a separate function that differs only in whether or not it accepts `Send` data.
#![allow(async_fn_in_trait)]

#[cfg(feature = "alloc")]
extern crate alloc;

pub mod prelude;
pub mod sensor_core;

#[cfg(feature = "alloc")]
use alloc::sync::Arc;
use core::{
    cell::UnsafeCell,
    future::Future,
    ops::{Deref, DerefMut},
    task::Poll,
};
use core::{mem::MaybeUninit, pin::Pin};
use derived_deref::{Deref, DerefMut};
use futures::FutureExt;
use sensor_core::{closed_bit_set, SensorCore, SensorCoreAsync, CLOSED_BIT, VERSION_BUMP};

#[derive(Deref, DerefMut, Clone, Copy)]
#[repr(transparent)]
pub struct OwnedData<T>(pub T);

impl<T> From<T> for OwnedData<T> {
    #[inline(always)]
    fn from(value: T) -> Self {
        Self(value)
    }
}

pub type SymResult<T> = Result<T, T>;

/*** Sensor State ***/

/// Trait for sharing a (most likely heap pointer) wrapped `struct@RawSensorData` with a locking strategy.
pub trait ShareStrategy<'a> {
    /// The data type that the sensor stores.
    type Core: SensorCore;
    /// The wrapping container for the sensor state.
    type Shared: Deref<Target = Self::Core>;

    /// Share the sensor state for the largest possible lifetime.
    fn share_data(self) -> Self::Shared;

    /// Create an immutable borrow to the sensor state, elided by the lifetime of
    /// wrapping container.
    fn share_elided_ref(self) -> &'a Self::Core;
}

impl<'a, C: SensorCore> ShareStrategy<'a> for &'a C {
    type Core = C;
    type Shared = Self;

    #[inline(always)]
    fn share_data(self) -> Self {
        self
    }

    #[inline(always)]
    fn share_elided_ref(self) -> Self {
        self
    }
}

#[cfg(feature = "alloc")]
impl<'a, C: SensorCore> ShareStrategy<'a> for &'a Arc<C> {
    type Core = C;
    type Shared = Arc<C>;

    #[inline(always)]
    fn share_data(self) -> Arc<C> {
        self.clone()
    }

    #[inline(always)]
    fn share_elided_ref(self) -> &'a C {
        self
    }
}

/*** Sensor Writing ***/

/// The generalized sensor writer.
#[repr(transparent)]
pub struct SensorWriter<C, S>(S)
where
    C: SensorCore,
    // Ideally we would collapse these two requirements by embedding the functionality
    // and relying directly on `fn@shared_elided_ref` from the `trait@ShareStrategy` trait,
    // but the compiler cannot adequitly derive the lifetime requirements and feasibility in certain situations when
    // async functions are called for an async sensor writer. As such, the share strategy requirement is used for
    // creating observers with specific lifetimes, whilst the deref requirement is used for accessing the sensor core
    // for any arbitrary lifetime.
    for<'a> &'a S: ShareStrategy<'a, Core = C>;

impl<C, S> SensorWriter<C, S>
where
    C: SensorCore,
    for<'a> &'a S: ShareStrategy<'a, Core = C>,
{
    #[inline]
    pub(crate) fn from_core(shared_core: S) -> Self {
        Self(shared_core)
    }
}

impl<C, S> Drop for SensorWriter<C, S>
where
    C: SensorCore,
    for<'a> &'a S: ShareStrategy<'a, Core = C>,
{
    #[inline]
    fn drop(&mut self) {
        unsafe { self.0.share_elided_ref().deregister_writer() };
    }
}

impl<C, S> Clone for SensorWriter<C, S>
where
    C: SensorCore,
    S: Deref<Target = C> + Clone,
    for<'a> &'a S: ShareStrategy<'a, Core = C>,
{
    #[inline]
    fn clone(&self) -> Self {
        unsafe { self.0.share_elided_ref().register_writer() };
        Self::from_core(self.0.clone())
    }
}

impl<C, S> SensorWriter<C, S>
where
    C: SensorCore + From<C::Target>,
    S: From<C>,
    for<'a> &'a S: ShareStrategy<'a, Core = C>,
{
    /// Produces a sensor writer from an initial value.
    #[inline]
    pub fn from_value(value: C::Target) -> Self {
        Self(S::from(C::from(value)))
    }
}

impl<C, S> SensorWriter<C, S>
where
    C: SensorCore,
    for<'a> &'a S: ShareStrategy<'a, Core = C>,
{
    /// Mark the current sensor value as unseen to all observers, notify them and execute all registered callbacks.
    #[inline(always)]
    pub fn mark_all_unseen(&self) {
        self.0.share_elided_ref().mark_unseen();
    }

    /// Spawn an observer by immutably borrowing the sensor writer's data. By definition this observer's scope will be limited
    /// by the scope of the writer.
    #[inline(always)]
    pub fn spawn_referenced_observer(&self) -> SensorObserver<C, &'_ C> {
        let core = self.0.share_elided_ref();
        SensorObserver {
            version: core.version(),
            core,
        }
    }

    /// Spawn an observer by leveraging the sensor writer's sharing strategy. May allow the observer
    /// to outlive the sensor writer for an appropriate sharing strategy (such as if the writer wraps its data in an `Arc`).
    #[inline(always)]
    pub fn spawn_observer(&self) -> SensorObserver<C, <&'_ S as ShareStrategy>::Shared> {
        let shared_core = self.0.share_data();
        SensorObserver {
            version: shared_core.version(),
            core: shared_core,
        }
    }

    /// Attempts to instantaneously aqcuire a write lock to the underlying core's data.
    #[inline(always)]
    pub fn try_write(
        &self,
    ) -> Option<<<&S as ShareStrategy<'_>>::Core as SensorCore>::WriteGuard<'_>> {
        unsafe { self.0.share_elided_ref().try_write() }
    }

    /// Attempts to instantaneously aqcuire a read lock to the underlying core's data.
    #[inline(always)]
    pub fn try_read(
        &self,
    ) -> Option<<<&S as ShareStrategy<'_>>::Core as SensorCore>::ReadGuard<'_>> {
        self.0.share_elided_ref().try_read()
    }
}

impl<C, S> SensorWriter<C, S>
where
    C: SensorCoreAsync,
    for<'a> &'a S: ShareStrategy<'a, Core = C>,
{
    /// Acquire a read lock on the underlying data.
    #[inline(always)]
    pub async fn read(&self) -> <<&S as ShareStrategy<'_>>::Core as SensorCore>::ReadGuard<'_> {
        self.0.share_elided_ref().read().await
    }

    /// Acquire a write lock on the underlying data.
    #[inline(always)]
    pub async fn write(&self) -> <<&S as ShareStrategy<'_>>::Core as SensorCore>::WriteGuard<'_> {
        self.0.share_elided_ref().write().await
    }

    /// Update the sensor value, notify observers and execute all registered callbacks.
    #[inline(always)]
    pub async fn update<'a>(&'a self, sample: C::Target) {
        let _ = self
            .modify_with(|v| {
                *v = sample;
                true
            })
            .await;
    }

    /// Modify the sensor value in place, notify observers and execute all registered callbacks.
    #[inline(always)]
    pub async fn modify_with(&self, f: impl FnOnce(&mut C::Target) -> bool) -> bool {
        unsafe { self.0.share_elided_ref().modify(f).await.1 }
    }
}

/*** Sensor Observation ***/

/// General observer functionality.
pub trait SensorObserve {
    type Target;
    type ReadGuard<'read>: Deref<Target = Self::Target>
    where
        Self: 'read;
    type Checkpoint: VersionFunctionality + Clone + Unpin;

    /// Saves the current version checkpoint of the sensor, and allows the observer to revert to this
    /// checkpoint in the future using `fn@restore_checkpoint`.
    fn save_checkpoint(&self) -> Self::Checkpoint;

    /// Restores the current version checkpoint of the observer. Note that this does **not** restore the
    /// actual data of the sensor from when the checkpoint was saved, only what version the observer perceives
    /// as the most recent data version of the sensor.
    fn restore_checkpoint(&mut self, checkpoint: &Self::Checkpoint);

    /// Mark the current sensor data as seen.
    fn mark_seen(&mut self);

    /// Mark the current sensor data as unseen.
    fn mark_unseen(&mut self);

    /// Returns true if the sensor data has been marked as unseen.
    fn has_changed(&self) -> bool;

    /// Returns true if all upstream writers has been dropped and no more updates can occur.
    fn is_closed(&self) -> bool;

    /// Fuse this observer with another using the given fusion function into a cacheless observer.
    #[inline(always)]
    fn fuse<B, T, F>(self, other: B, f: F) -> FusedSensorObserver<Self, B, T, F>
    where
        Self: Sized,
        B: SensorObserve,
        F: FnMut(&<Self as SensorObserve>::Target, &<B as SensorObserve>::Target) -> T,
    {
        FusedSensorObserver::fuse_with(self, other, f)
    }

    /// Map this observer into another using the given mapping function into a cacheless observer.
    #[inline(always)]
    fn map<T, F>(self, f: F) -> MappedSensorObserver<Self, T, F>
    where
        Self: Sized,
        F: FnMut(&<Self as SensorObserve>::Target) -> T,
    {
        MappedSensorObserver::map_with(self, f)
    }
}

/// Async observer functionality.
pub trait SensorObserveAsync: SensorObserve {
    async fn read(&self) -> Self::ReadGuard<'_>;

    /// Asynchronously wait until the sensor value is updated. This call will **not** update the observer's version,
    /// as such an additional call to `pull` or `pull_updated` is required.
    async fn wait_until_changed(&self) -> SymResult<Self::Checkpoint>;

    /// Asynchronously wait for a particular condition to become true. This method checks the current sensor value even if the current value is marked
    /// as seen by the observer.
    async fn wait_for<M: FnMut(&Self::Target) -> bool>(
        &mut self,
        condition_map: M,
    ) -> SymResult<Self::ReadGuard<'_>>;

    /// Asynchronously wait for a particular condition to become true. This method waits for the sensor value to be marked unseen before staring checks.
    async fn wait_for_next<M: FnMut(&Self::Target) -> bool>(
        &mut self,
        condition_map: M,
    ) -> SymResult<Self::ReadGuard<'_>>;
}

#[repr(transparent)]
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct Version(usize);

impl Version {
    #[inline(always)]
    pub const fn closed_bit_set(self) -> bool {
        closed_bit_set(self.0)
    }

    #[inline(always)]
    pub const fn set_closed_bit(&mut self) {
        self.0 |= CLOSED_BIT
    }

    #[inline(always)]
    pub const fn increment(&mut self) {
        self.0 = self.0.wrapping_add(VERSION_BUMP);
    }

    #[inline(always)]
    pub const fn decrement(&mut self) {
        self.0 = self.0.wrapping_sub(VERSION_BUMP);
    }
}

pub trait VersionFunctionality {
    /// Returns true if the sensor is closed and no further updates can occur.
    fn closed(&self) -> bool;
}

impl VersionFunctionality for Version {
    #[inline(always)]
    fn closed(&self) -> bool {
        self.closed_bit_set()
    }
}

/// The generalized sensor observer.
pub struct SensorObserver<C: SensorCore, R: Deref<Target = C>> {
    core: R,
    version: Version,
}

impl<C: SensorCore, R: Deref<Target = C>> SensorObserve for SensorObserver<C, R> {
    type Target = C::Target;
    type ReadGuard<'read>
        = C::ReadGuard<'read>
    where
        Self: 'read;

    type Checkpoint = Version;

    #[inline(always)]
    fn save_checkpoint(&self) -> Self::Checkpoint {
        self.version
    }

    fn restore_checkpoint(&mut self, checkpoint: &Self::Checkpoint) {
        self.version = *checkpoint;
    }

    #[inline(always)]
    fn mark_seen(&mut self) {
        self.version = self.core.version();
    }

    #[inline(always)]
    fn mark_unseen(&mut self) {
        self.version = self.core.version();
        self.version.decrement();
    }

    #[inline(always)]
    fn has_changed(&self) -> bool {
        self.core.version() != self.version
    }

    #[inline(always)]
    fn is_closed(&self) -> bool {
        self.core.version().closed_bit_set()
    }
}

impl<C: SensorCoreAsync, R: Deref<Target = C>> SensorObserveAsync for SensorObserver<C, R> {
    #[inline(always)]
    async fn read(&self) -> Self::ReadGuard<'_> {
        self.core.read().await
    }

    #[inline]
    async fn wait_until_changed(&self) -> SymResult<Version> {
        let version = self.core.wait_changed(self.version).await;
        match version.closed_bit_set() {
            true => Err(version),
            false => Ok(version),
        }
    }

    #[inline]
    async fn wait_for<F: FnMut(&Self::Target) -> bool>(
        &mut self,
        condition: F,
    ) -> SymResult<Self::ReadGuard<'_>> {
        let mut reference_version = self.core.version();
        reference_version.decrement();
        let (guard, latest_version) = self.core.wait_for(condition, reference_version).await;
        self.version = latest_version;
        match latest_version.closed_bit_set() {
            true => Err(guard),
            false => Ok(guard),
        }
    }

    #[inline]
    async fn wait_for_next<F: FnMut(&Self::Target) -> bool>(
        &mut self,
        condition: F,
    ) -> SymResult<Self::ReadGuard<'_>> {
        let (guard, latest_version) = self.core.wait_for(condition, self.version).await;
        self.version = latest_version;
        match latest_version.closed_bit_set() {
            true => Err(guard),
            false => Ok(guard),
        }
    }
}

/*** Mapped and Fused Observers ***/

/// A mapped sensor observer.
pub struct MappedSensorObserver<A: SensorObserve, T, F: FnMut(&A::Target) -> T> {
    /// The original pre-mapped observer.
    pub inner: A,
    map: UnsafeCell<F>,
}

impl<A, T, F> MappedSensorObserver<A, T, F>
where
    A: SensorObserve,
    F: FnMut(&A::Target) -> T,
{
    /// Create a new mapped observer given another observer and an appropriate mapping function.
    #[inline(always)]
    pub const fn map_with(a: A, f: F) -> Self {
        Self {
            inner: a,
            map: UnsafeCell::new(f),
        }
    }
}

impl<A, T, F> SensorObserve for MappedSensorObserver<A, T, F>
where
    A: SensorObserve,
    F: FnMut(&A::Target) -> T,
{
    type Target = T;
    type ReadGuard<'read>
        = OwnedData<T>
    where
        Self: 'read;
    type Checkpoint = A::Checkpoint;

    #[inline(always)]
    fn save_checkpoint(&self) -> Self::Checkpoint {
        self.inner.save_checkpoint()
    }

    #[inline(always)]
    fn restore_checkpoint(&mut self, checkpoint: &Self::Checkpoint) {
        self.inner.restore_checkpoint(checkpoint);
    }

    #[inline(always)]
    fn mark_seen(&mut self) {
        self.inner.mark_seen();
    }

    #[inline(always)]
    fn mark_unseen(&mut self) {
        self.inner.mark_unseen();
    }

    #[inline(always)]
    fn has_changed(&self) -> bool {
        self.inner.has_changed()
    }

    #[inline(always)]
    fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }
}

impl<A, T, F> SensorObserveAsync for MappedSensorObserver<A, T, F>
where
    A: SensorObserveAsync,
    F: FnMut(&A::Target) -> T,
{
    fn read(&self) -> impl Future<Output = Self::ReadGuard<'_>> {
        FutureExt::map(self.inner.read(), |inner_value| {
            // Safety: `MappedSensorObserver` is not `Sync`, disallowing race conditions over the mapping function.
            OwnedData(unsafe { (*self.map.get())(&inner_value) })
        })
    }

    fn wait_until_changed(&self) -> impl Future<Output = SymResult<Self::Checkpoint>> {
        self.inner.wait_until_changed()
    }

    async fn wait_for<C: FnMut(&T) -> bool>(
        &mut self,
        mut condition: C,
    ) -> SymResult<Self::ReadGuard<'_>> {
        let MappedSensorObserver { inner, map } = self;
        let mut mapped = MaybeUninit::uninit();
        match inner
            .wait_for(|guard| condition(mapped.write(OwnedData((map.get_mut())(&guard)))))
            .await
        {
            Ok(_) => Ok(unsafe { mapped.assume_init() }), // Safety: `mapped` is populated on success.
            Err(guard) => Err(OwnedData((map.get_mut())(&guard))),
        }
    }

    async fn wait_for_next<C: FnMut(&T) -> bool>(
        &mut self,
        mut condition: C,
    ) -> SymResult<Self::ReadGuard<'_>> {
        let MappedSensorObserver { inner, map } = self;
        let mut mapped = MaybeUninit::uninit();
        match inner
            .wait_for_next(|guard| condition(mapped.write(OwnedData((map.get_mut())(&guard)))))
            .await
        {
            Ok(_) => Ok(unsafe { mapped.assume_init() }), // Safety: `mapped` is populated on success.
            Err(guard) => Err(OwnedData((map.get_mut())(&guard))),
        }
    }
}

/// A fused sensor observer.
pub struct FusedSensorObserver<
    A: SensorObserve,
    B: SensorObserve,
    T,
    F: FnMut(&A::Target, &B::Target) -> T,
> {
    /// The first original pre-mapped observer.
    pub a: A,
    /// The second original pre-mapped observer.
    pub b: B,
    fuse: UnsafeCell<F>,
}

impl<A, B, T, F> FusedSensorObserver<A, B, T, F>
where
    A: SensorObserve,
    B: SensorObserve,
    F: FnMut(&A::Target, &B::Target) -> T,
{
    /// Create a new cacheless fused observer given two other independent observers and an appropriate fusing function.
    #[inline(always)]
    pub fn fuse_with(a: A, b: B, f: F) -> Self {
        Self {
            a,
            b,
            fuse: UnsafeCell::new(f),
        }
    }
}

#[derive(Clone)]
pub struct FusedVersion<A: VersionFunctionality, B: VersionFunctionality> {
    a: A,
    b: B,
}

impl<A: VersionFunctionality, B: VersionFunctionality> VersionFunctionality for FusedVersion<A, B> {
    #[inline]
    fn closed(&self) -> bool {
        self.a.closed() && self.b.closed()
    }
}

impl<A, B, T, F> SensorObserve for FusedSensorObserver<A, B, T, F>
where
    A: SensorObserve,
    B: SensorObserve,
    F: FnMut(&A::Target, &B::Target) -> T,
{
    type Target = T;
    type ReadGuard<'read>
        = OwnedData<T>
    where
        Self: 'read;
    type Checkpoint = FusedVersion<A::Checkpoint, B::Checkpoint>;

    #[inline]
    fn save_checkpoint(&self) -> Self::Checkpoint {
        FusedVersion {
            a: self.a.save_checkpoint(),
            b: self.b.save_checkpoint(),
        }
    }

    #[inline]
    fn restore_checkpoint(&mut self, checkpoint: &Self::Checkpoint) {
        self.a.restore_checkpoint(&checkpoint.a);
        self.b.restore_checkpoint(&checkpoint.b);
    }

    #[inline(always)]
    fn mark_seen(&mut self) {
        self.a.mark_seen();
        self.b.mark_seen();
    }

    #[inline(always)]
    fn mark_unseen(&mut self) {
        self.a.mark_unseen();
        self.b.mark_unseen();
    }

    #[inline(always)]
    fn has_changed(&self) -> bool {
        self.a.has_changed() || self.b.has_changed()
    }

    #[inline(always)]
    fn is_closed(&self) -> bool {
        self.a.is_closed() && self.b.is_closed()
    }
}

struct FusedWaitChangedFut<
    A: Future<Output = SymResult<CA>>,
    CA: VersionFunctionality + Clone + Unpin,
    B: Future<Output = SymResult<CB>>,
    CB: VersionFunctionality + Clone + Unpin,
> {
    a: Option<A>,
    // Checkpoint for a.
    ca: CA,
    // Checkpoint for b.
    b: B,
    cb: CB,
}

// impl<
// A: Future<Output = SymResult<CA>>,
// CA: VersionFunctionality + Clone + Unpin,
// B: Future<Output = SymResult<CB>>,
// CB: VersionFunctionality + Clone + Unpin,
// F: Fn() -> bool> Drop for FusedWaitChangedFut<A, CA, B, CB, F> {
//     fn drop(&mut self) {

//     }
// }
impl<
        A: Future<Output = SymResult<CA>>,
        CA: VersionFunctionality + Clone + Unpin,
        B: Future<Output = SymResult<CB>>,
        CB: VersionFunctionality + Clone + Unpin,
    > Future for FusedWaitChangedFut<A, CA, B, CB>
{
    type Output = SymResult<FusedVersion<CA, CB>>;

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        let Self { a, ca, b, cb } = unsafe { self.get_unchecked_mut() };

        if let Some(Poll::Ready(checkpoint_a_res)) = a
            .as_mut()
            .map(|a| unsafe { Pin::new_unchecked(a).poll(cx) })
        {
            match checkpoint_a_res {
                Ok(checkpoint) => {
                    return Poll::Ready(Ok(FusedVersion {
                        a: checkpoint,
                        b: cb.clone(),
                    }));
                }
                Err(checkpoint) => {
                    *ca = checkpoint;
                    *a = None;
                }
            }
        }

        // From here it is known that `A` is closed, therefore `B`'s result can be propogated without alteration.
        unsafe { Pin::new_unchecked(b) }
            .poll(cx)
            .map(|checkpoint_b_res| {
                let version = match &checkpoint_b_res {
                    Ok(checkpoint) | Err(checkpoint) => FusedVersion {
                        a: ca.clone(),
                        b: checkpoint.clone(),
                    },
                };

                match checkpoint_b_res {
                    Ok(_) => Ok(version),
                    Err(_) => Err(version),
                }
            })
    }
}

impl<A, B, T, F> FusedSensorObserver<A, B, T, F>
where
    A: SensorObserve + SensorObserveAsync,
    B: SensorObserve + SensorObserveAsync,
    F: FnMut(&A::Target, &B::Target) -> T,
{
    async fn wait_for_inner<C: FnMut(&T) -> bool>(
        &mut self,
        mut condition: C,
        check_current: bool,
    ) -> SymResult<<FusedSensorObserver<A, B, T, F> as SensorObserve>::ReadGuard<'_>> {
        let checkpoint = self.save_checkpoint();
        let mut data = DropFn::new((self, checkpoint), |(s, cp)| s.restore_checkpoint(cp));

        let (s, restore_checkpoint) = &mut *data;
        if !check_current {
            let _ = s.wait_until_changed().await;
        }
        loop {
            let guard_a = s.a.read().await;
            let checkpoint_a = s.a.save_checkpoint();

            let guard_b = s.b.read().await;
            let checkpoint_b = s.b.save_checkpoint();

            let fused_checkpoint = FusedVersion {
                a: checkpoint_a,
                b: checkpoint_b,
            };

            let fused = OwnedData(s.fuse.get_mut()(&guard_a, &guard_b));

            if condition(&fused) {
                *restore_checkpoint = fused_checkpoint;
                return Ok(fused);
            }

            drop(guard_b);
            drop(guard_a);

            s.restore_checkpoint(&fused_checkpoint);
            let _ = s.wait_until_changed().await;
        }
    }
}
impl<A, B, T, F> SensorObserveAsync for FusedSensorObserver<A, B, T, F>
where
    A: SensorObserve + SensorObserveAsync,
    B: SensorObserve + SensorObserveAsync,
    F: FnMut(&A::Target, &B::Target) -> T,
{
    fn read(&self) -> impl Future<Output = Self::ReadGuard<'_>> {
        async move {
            let a = self.a.read().await;
            let b = self.b.read().await;
            OwnedData(unsafe { (*self.fuse.get())(&*a, &*b) })
        }
    }

    fn wait_until_changed(&self) -> impl Future<Output = SymResult<Self::Checkpoint>> {
        let Self { a, b, .. } = self;
        FusedWaitChangedFut {
            a: Some(a.wait_until_changed()),
            ca: a.save_checkpoint(),
            b: self.b.wait_until_changed(),
            cb: b.save_checkpoint(),
        }
    }

    async fn wait_for<C: FnMut(&T) -> bool>(
        &mut self,
        condition: C,
    ) -> SymResult<Self::ReadGuard<'_>> {
        self.wait_for_inner(condition, true).await
    }

    #[inline(always)]
    async fn wait_for_next<C: FnMut(&T) -> bool>(
        &mut self,
        condition: C,
    ) -> SymResult<Self::ReadGuard<'_>> {
        self.wait_for_inner(condition, false).await
    }
}

struct DropFn<T, F: FnOnce(&mut T)> {
    v: T,
    f: MaybeUninit<F>,
}

impl<T, F: FnOnce(&mut T)> DropFn<T, F> {
    #[inline(always)]
    pub const fn new(value: T, drop_fn: F) -> Self {
        Self {
            v: value,
            f: MaybeUninit::new(drop_fn),
        }
    }
}

impl<T, F: FnOnce(&mut T)> Drop for DropFn<T, F> {
    #[inline]
    fn drop(&mut self) {
        let Self { v, f } = self;

        unsafe { (f.assume_init_read())(v) };
    }
}

impl<T, F: FnOnce(&mut T)> Deref for DropFn<T, F> {
    type Target = T;

    #[inline(always)]
    fn deref(&self) -> &Self::Target {
        &self.v
    }
}

impl<T, F: FnOnce(&mut T)> DerefMut for DropFn<T, F> {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.v
    }
}
