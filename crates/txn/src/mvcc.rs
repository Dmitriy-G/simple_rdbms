use common::TxnId;

#[derive(Debug, Clone)]
pub struct VersionEntry {
    pub creator_txn_id: TxnId,
    pub begin_ts: Option<u64>,
    pub end_ts: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct VersionChain {
    versions: Vec<VersionEntry>,
}

impl VersionChain {
    pub fn new() -> Self {
        Self { versions: Vec::new() }
    }

    pub fn push(&mut self, entry: VersionEntry) {
        self.versions.insert(0, entry);
    }

    pub fn visible_version(&self, read_ts: u64) -> Option<&VersionEntry> {
        self.versions.iter().find(|entry| Self::is_globally_visible(entry, read_ts))
    }

    pub fn visible_version_for(&self, reader_txn_id: TxnId, read_ts: u64) -> Option<&VersionEntry> {
        self.versions.iter().find(|entry| {
            entry.creator_txn_id == reader_txn_id || Self::is_globally_visible(entry, read_ts)
        })
    }

    fn is_globally_visible(entry: &VersionEntry, read_ts: u64) -> bool {
        match entry.begin_ts {
            Some(begin_ts) if begin_ts <= read_ts => match entry.end_ts {
                Some(end_ts) => end_ts > read_ts,
                None => true,
            },
            _ => false,
        }
    }

    pub fn mark_committed(&mut self, txn_id: TxnId, commit_ts: u64) {
        for entry in &mut self.versions {
            if entry.creator_txn_id == txn_id {
                entry.begin_ts = Some(commit_ts);
            }
        }
    }

    pub fn remove_creator(&mut self, txn_id: TxnId) {
        self.versions.retain(|entry| entry.creator_txn_id != txn_id);
    }

    pub fn is_empty(&self) -> bool {
        self.versions.is_empty()
    }

    pub fn is_prunable(&self, watermark: u64) -> bool {
        match self.versions.first() {
            None => true,
            Some(newest) => match (newest.begin_ts, newest.end_ts) {
                (Some(begin_ts), None) => begin_ts <= watermark,
                _ => false,
            },
        }
    }
}
