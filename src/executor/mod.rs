use crate::lock::DataWriteLock;

#[cfg(feature = "alloc")]
pub mod standard;

/// An execution manager that only requires immutable access.
pub trait ExecManager<L>
where
    L: DataWriteLock,
{
    /// Execute all executables registered on the executor manager by giving it access to an exclusive read guard. This will
    /// either be a write guard or a read guard that was atomically downgraded from a write guard. As such the execution
    /// manager is given the guarantee that as long as it holds the guard, no other `execute` call will be initiated.
    fn execute(&self, value: L::DowngradedGuard<'_>);
}

/// Signifies that an execution manager may register an executable using immutable access.
pub trait ExecRegister<L, F>: ExecManager<L>
where
    L: DataWriteLock,
{
    /// Register an executable unit on the callback manager's execution set. In most cases this will usually be a function.
    fn register(&self, f: F, lock: &L);
}

impl<L> ExecManager<L> for ()
where
    L: DataWriteLock,
{
    #[inline(always)]
    fn execute(&self, _: L::DowngradedGuard<'_>) {}
}
