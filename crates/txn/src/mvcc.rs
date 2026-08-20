use common::TxnId;

/// One version of a tuple in an MVCC version chain: the tuple's bytes as of
/// some transaction's write, plus the visibility window (in timestamp
/// terms) during which that version is the one a reader should see.
#[derive(Debug, Clone)]
pub struct VersionEntry {
    /// The transaction that created this version.
    pub creator_txn_id: TxnId,
    /// The timestamp this version became visible (its creator's commit
    /// timestamp), or `None` if the creator has not yet committed.
    pub begin_ts: Option<u64>,
    /// The timestamp this version stopped being visible (the timestamp of
    /// whatever transaction superseded it), or `None` if it is still the
    /// current version.
    pub end_ts: Option<u64>,
    /// The tuple's encoded bytes as of this version.
    pub tuple_bytes: Vec<u8>,
}

/// The chain of historical versions for a single logical row, newest first.
/// A reader under snapshot isolation walks this chain to find the newest
/// version whose `begin_ts` is at or before its own snapshot timestamp.
#[derive(Debug, Clone, Default)]
pub struct VersionChain {
    versions: Vec<VersionEntry>,
}

impl VersionChain {
    /// Creates an empty version chain.
    pub fn new() -> Self {
        Self { versions: Vec::new() }
    }

    /// Prepends a newly created version to the chain.
    pub fn push(&mut self, entry: VersionEntry) {
        self.versions.insert(0, entry);
    }

    /// Finds the version visible to a reader with snapshot timestamp
    /// `read_ts`: the newest version whose `begin_ts` is at or before
    /// `read_ts` and whose `end_ts` is either absent or after it.
    pub fn visible_version(&self, read_ts: u64) -> Option<&VersionEntry> {
        let _ = read_ts;
        todo!("scan versions for the newest one visible as of read_ts")
    }
}
