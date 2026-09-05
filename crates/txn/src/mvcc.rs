use common::TxnId;

#[derive(Debug, Clone)]
pub struct VersionEntry {
    pub creator_txn_id: TxnId,
    pub begin_ts: Option<u64>,
    pub end_ts: Option<u64>,
    pub tuple_bytes: Vec<u8>,
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
        self.versions.iter().find(|entry| match entry.begin_ts {
            Some(begin_ts) if begin_ts <= read_ts => match entry.end_ts {
                Some(end_ts) => end_ts > read_ts,
                None => true,
            },
            _ => false,
        })
    }
}
