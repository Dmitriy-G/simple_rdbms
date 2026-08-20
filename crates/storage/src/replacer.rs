use common::FrameId;

/// Chooses which buffer pool frame to evict when a new page must be brought
/// into memory and no frame is free. Implementations decide *which*
/// evictable frame to reclaim; the buffer pool itself is responsible for
/// only ever offering up frames whose pin count is zero.
pub trait Replacer {
    /// Records that `frame_id` was just accessed, for recency/frequency
    /// bookkeeping.
    fn record_access(&mut self, frame_id: FrameId);

    /// Marks `frame_id` as evictable (pin count dropped to zero) or not
    /// evictable (pin count rose above zero).
    fn set_evictable(&mut self, frame_id: FrameId, evictable: bool);

    /// Picks a victim frame to evict according to the replacement policy,
    /// and stops tracking it. Returns `None` if no frame is evictable.
    fn evict(&mut self) -> Option<FrameId>;

    /// Removes a frame from tracking entirely, e.g. because its page was
    /// deallocated.
    fn remove(&mut self, frame_id: FrameId);

    /// The number of frames currently marked evictable.
    fn size(&self) -> usize;
}

/// An LRU-K replacer: evicts the frame whose K-th most recent access lies
/// furthest in the past, falling back to plain LRU (evict the least
/// recently used) for frames with fewer than K recorded accesses. This
/// approximates "least likely to be reused soon" better than plain LRU
/// under sequential-scan workloads, which tend to defeat plain LRU.
pub struct LruKReplacer {
    #[allow(dead_code)]
    k: usize,
    #[allow(dead_code)]
    capacity: usize,
}

impl LruKReplacer {
    /// Creates a replacer tracking up to `capacity` frames, ranking
    /// eviction candidates by their `k`-th most recent access.
    pub fn new(capacity: usize, k: usize) -> Self {
        Self { capacity, k }
    }
}

impl Replacer for LruKReplacer {
    fn record_access(&mut self, frame_id: FrameId) {
        let _ = frame_id;
        todo!("push an access timestamp to the frame's history, capped at k entries")
    }

    fn set_evictable(&mut self, frame_id: FrameId, evictable: bool) {
        let _ = (frame_id, evictable);
        todo!("flip the frame's evictable flag and adjust the evictable count")
    }

    fn evict(&mut self) -> Option<FrameId> {
        todo!("scan evictable frames for the max backward k-distance and evict it")
    }

    fn remove(&mut self, frame_id: FrameId) {
        let _ = frame_id;
        todo!("drop the frame's access history entirely")
    }

    fn size(&self) -> usize {
        todo!("return the count of frames currently marked evictable")
    }
}
