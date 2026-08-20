use std::collections::{HashMap, VecDeque};

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

/// A frame's access history and evictability, as tracked by `LruKReplacer`.
struct FrameEntry {
    /// The frame's `k` most recent access timestamps, oldest first.
    history: VecDeque<u64>,
    evictable: bool,
}

/// An LRU-K replacer: evicts the frame whose K-th most recent access lies
/// furthest in the past, falling back to plain LRU (evict the least
/// recently used) for frames with fewer than K recorded accesses. This
/// approximates "least likely to be reused soon" better than plain LRU
/// under sequential-scan workloads, which tend to defeat plain LRU.
pub struct LruKReplacer {
    k: usize,
    #[allow(dead_code)]
    capacity: usize,
    frames: HashMap<FrameId, FrameEntry>,
    clock: u64,
}

impl LruKReplacer {
    /// Creates a replacer tracking up to `capacity` frames, ranking
    /// eviction candidates by their `k`-th most recent access.
    pub fn new(capacity: usize, k: usize) -> Self {
        Self { capacity, k, frames: HashMap::new(), clock: 0 }
    }
}

impl Replacer for LruKReplacer {
    fn record_access(&mut self, frame_id: FrameId) {
        self.clock += 1;
        let timestamp = self.clock;
        let entry = self
            .frames
            .entry(frame_id)
            .or_insert_with(|| FrameEntry { history: VecDeque::new(), evictable: false });
        entry.history.push_back(timestamp);
        if entry.history.len() > self.k {
            entry.history.pop_front();
        }
    }

    fn set_evictable(&mut self, frame_id: FrameId, evictable: bool) {
        if let Some(entry) = self.frames.get_mut(&frame_id) {
            entry.evictable = evictable;
        } else if evictable {
            self.frames.insert(frame_id, FrameEntry { history: VecDeque::new(), evictable: true });
        }
    }

    fn evict(&mut self) -> Option<FrameId> {
        // Rank candidates as (has_less_than_k_accesses, distance): frames
        // with fewer than `k` accesses (infinite backward k-distance, so
        // ranked purely by how stale their most recent access is) always
        // beat frames with a full history (ranked by distance since their
        // k-th most recent access). Larger is a better eviction candidate.
        let mut best: Option<(FrameId, bool, u64)> = None;
        for (&frame_id, entry) in self.frames.iter() {
            if !entry.evictable {
                continue;
            }
            let (is_inf, metric) = if entry.history.len() < self.k {
                let most_recent = entry.history.back().copied().unwrap_or(0);
                (true, u64::MAX - most_recent)
            } else {
                let kth_most_recent = entry.history.front().copied().unwrap_or(0);
                (false, self.clock - kth_most_recent)
            };
            let is_better = match best {
                None => true,
                Some((_, best_inf, best_metric)) => (is_inf, metric) > (best_inf, best_metric),
            };
            if is_better {
                best = Some((frame_id, is_inf, metric));
            }
        }

        let victim = best.map(|(frame_id, _, _)| frame_id);
        if let Some(frame_id) = victim {
            self.frames.remove(&frame_id);
        }
        victim
    }

    fn remove(&mut self, frame_id: FrameId) {
        self.frames.remove(&frame_id);
    }

    fn size(&self) -> usize {
        self.frames.values().filter(|entry| entry.evictable).count()
    }
}
