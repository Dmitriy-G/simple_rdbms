use std::collections::{HashMap, VecDeque};

use common::FrameId;

pub trait Replacer: Send {
    fn record_access(&mut self, frame_id: FrameId);

    fn set_evictable(&mut self, frame_id: FrameId, evictable: bool);

    fn evict(&mut self) -> Option<FrameId>;

    fn remove(&mut self, frame_id: FrameId);

    fn size(&self) -> usize;
}

struct FrameEntry {
    history: VecDeque<u64>,
    evictable: bool,
}

pub struct LruKReplacer {
    k: usize,
    #[allow(dead_code)]
    capacity: usize,
    frames: HashMap<FrameId, FrameEntry>,
    clock: u64,
}

impl LruKReplacer {
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
