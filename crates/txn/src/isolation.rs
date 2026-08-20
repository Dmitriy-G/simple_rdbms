/// The isolation level a transaction runs under, controlling which
/// concurrency anomalies (dirty reads, non-repeatable reads, phantoms) it
/// is allowed to observe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IsolationLevel {
    /// May see uncommitted writes from other transactions. Not enforced by
    /// locking on reads.
    ReadUncommitted,
    /// Never sees uncommitted writes, but a value re-read later in the same
    /// transaction may have changed underneath it.
    ReadCommitted,
    /// Once a row is read, re-reading it within the same transaction
    /// yields the same value, but new rows matching a repeated predicate
    /// scan may appear (phantoms).
    RepeatableRead,
    /// The transaction sees a single consistent snapshot of the database as
    /// of its start, achieved via MVCC rather than locking reads.
    SnapshotIsolation,
    /// Transactions behave as if executed one at a time in some serial
    /// order.
    Serializable,
}
