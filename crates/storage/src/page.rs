use common::{FrameId, PageId};

/// The fixed size, in bytes, of every page in the database file and the
/// write-ahead log. Matches `common::DbConfig::DEFAULT_PAGE_SIZE`; a
/// database file's actual page size is recorded once at creation time and
/// is not meant to vary per-page.
pub const PAGE_SIZE: usize = 4096;

/// A single fixed-size page: the unit of I/O between the disk manager and
/// the buffer pool, and the unit of layout for slotted-page heap storage
/// and B+tree nodes. Holds raw bytes; interpreting them (as a heap page, an
/// index node, etc.) is the job of the module that owns that layout.
#[derive(Clone)]
pub struct Page {
    id: PageId,
    data: [u8; PAGE_SIZE],
}

impl Page {
    /// Creates a zeroed page for the given id, as when extending the file
    /// with a brand-new page.
    pub fn new(id: PageId) -> Self {
        Self { id, data: [0u8; PAGE_SIZE] }
    }

    /// The id of this page within the database file.
    pub fn id(&self) -> PageId {
        self.id
    }

    /// Read-only access to the page's raw bytes.
    pub fn data(&self) -> &[u8; PAGE_SIZE] {
        &self.data
    }

    /// Mutable access to the page's raw bytes, for in-place layout updates.
    pub fn data_mut(&mut self) -> &mut [u8; PAGE_SIZE] {
        &mut self.data
    }
}

/// An RAII handle to a page pinned in the buffer pool. Borrowing a page
/// through a `PageGuard` (rather than a bare `&Page`) is what lets the
/// buffer pool track pin counts: acquiring a guard increments the frame's
/// pin count, and dropping it decrements that count, making the frame
/// eligible for eviction again once nothing holds a guard to it.
///
/// `PageGuard` is the *only* way to release a pin: it has no public
/// constructor (its fields are `pub(crate)`) and `BufferPool` exposes no
/// manual unpin method, so the sole way to release a pin is to drop the
/// guard that holds it. A caller cannot double-release a pin, because
/// there is no second operation left to call:
///
/// ```compile_fail
/// # use storage::buffer::BufferPool;
/// # use storage::disk::DiskManager;
/// # use storage::replacer::LruKReplacer;
/// # let dir = tempfile::tempdir().unwrap();
/// # let disk = DiskManager::open(dir.path().join("t.db"), storage::page::PAGE_SIZE).unwrap();
/// # let pool = BufferPool::new(disk, 4, Box::new(LruKReplacer::new(4, 2)));
/// let (page_id, guard) = pool.new_page().unwrap();
/// drop(guard);
/// pool.unpin_page(page_id, false).unwrap(); // no such method on `BufferPool`
/// ```
pub struct PageGuard<'pool> {
    pub(crate) page_id: PageId,
    /// The frame in the pool's frame table currently holding this page.
    pub(crate) frame_id: FrameId,
    pub(crate) pool: &'pool crate::buffer::BufferPool,
}

impl<'pool> PageGuard<'pool> {
    /// The id of the page this guard pins.
    pub fn page_id(&self) -> PageId {
        self.page_id
    }
}

// `page()`, `page_mut()`, `mark_dirty()`, and `Drop` live in `crate::buffer`
// alongside `BufferPool`, since they need access to its private frame
// table, pin counts, and dirty flags.
