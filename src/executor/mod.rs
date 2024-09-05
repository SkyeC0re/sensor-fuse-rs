#[cfg(feature = "alloc")]
pub mod standard;

use core::{cell::UnsafeCell, marker::PhantomData};

use crate::lock::DataWriteLock;

mod private {
    pub trait Sealed {}
}

/// Defines whether or not an execution manager requires mutable access to execute. If so
/// then the locking strategy for the raw sensor data will be used to ensure synchronized
/// access to the executor.
pub trait ExecutionStrategy<T>: private::Sealed {
    /// Run the execution manager with the specified value.
    fn execute(&self, value: &T);
}

/// Defines whether or not an execution manager requires mutable access to register a specific executable type.
pub trait RegistrationStrategy<F>: private::Sealed {
    /// Register an executable on the execution manager.
    fn register<L: DataWriteLock>(data: &(L, Self), f: F);
}

/// An execution manager that only requires immutable access.
pub trait ExecManager<T> {
    /// Execute all executables registered on the executor manager.
    fn execute(&self, value: &T);
}

/// Signifies that an execution manager may register an executable using immutable access.
pub trait ExecRegister<F> {
    /// Register an executable unit on the callback manager's execution set. In most cases this will usually be a function.
    fn register(&self, f: F);
}

impl<T> ExecManager<T> for () {
    #[inline(always)]
    fn execute(&self, _: &T) {}
}

/// An execution manager that requires mutable access.
pub trait ExecManagerMut<T> {
    /// Execute all executables registered on the executor manager.
    fn execute(&mut self, value: &T);
}

/// Signifies that an execution manager may register an executable using mutable access.
pub trait ExecRegisterMut<F> {
    /// Register an executable unit on the callback manager's execution set. In most cases this will usually be a function.
    fn register(&mut self, f: F);
}

/// Wrapper to select the immutable execution strategy. See `trait@ExecutionStrategy`.
#[repr(transparent)]
pub struct AsImmut<T, E: ExecManager<T>>(E, PhantomData<T>);

impl<T, E> AsImmut<T, E>
where
    E: ExecManager<T>,
{
    /// Select the immutable execution strategy for an execution manager.
    #[inline(always)]
    pub const fn new(exec_manager: E) -> Self {
        Self(exec_manager, PhantomData)
    }
}

impl<T, E> private::Sealed for AsImmut<T, E> where E: ExecManager<T> {}
impl<T, E> ExecutionStrategy<T> for AsImmut<T, E>
where
    E: ExecManager<T>,
{
    fn execute(&self, value: &T) {
        self.0.execute(value)
    }
}

impl<T, F, E> RegistrationStrategy<F> for AsImmut<T, E>
where
    E: ExecManager<T> + ExecRegister<F>,
{
    #[inline(always)]
    fn register<L: DataWriteLock>(data: &(L, Self), f: F) {
        data.1 .0.register(f);
    }
}

/// Wrapper to select the mutable execution strategy. See `trait@ExecutionStrategy`.
#[repr(transparent)]
pub struct AsMut<T, E: ExecManagerMut<T>>(UnsafeCell<E>, PhantomData<T>);

unsafe impl<T, E> Sync for AsMut<T, E> where E: ExecManagerMut<T> {}

impl<T, E> AsMut<T, E>
where
    E: ExecManagerMut<T>,
{
    /// Select the mutable execution strategy for an execution manager.
    #[inline(always)]
    pub const fn new(exec_manager: E) -> Self {
        Self(UnsafeCell::new(exec_manager), PhantomData)
    }
}

impl<T, E> private::Sealed for AsMut<T, E> where E: ExecManagerMut<T> {}
impl<T, E> ExecutionStrategy<T> for AsMut<T, E>
where
    E: ExecManagerMut<T>,
{
    fn execute(&self, value: &T) {
        unsafe {
            (*self.0.get()).execute(value);
        }
    }
}

impl<T, F, E> RegistrationStrategy<F> for AsMut<T, E>
where
    E: ExecManagerMut<T> + ExecRegisterMut<F>,
{
    #[inline]
    fn register<L: DataWriteLock>(data: &(L, Self), f: F) {
        // Piggyback off of the data's semaphore.
        let guard = L::atomic_downgrade(data.0.write());
        unsafe {
            (*data.1 .0.get()).register(f);
        }
        drop(guard);
    }
}
