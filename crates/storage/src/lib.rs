//! The storage engine: fixed-size page I/O, buffer pool management, the
//! on-disk heap file format, index structures, and write-ahead logging.
//!
//! Unlike the rest of the workspace, `storage` does not `forbid(unsafe_code)`
//! wholesale, because the buffer pool and page layout will eventually need
//! raw pointer access to page bytes for performance. Any `unsafe` block that
//! is added must go through `unsafe fn` boundaries explicitly, enforced by
//! `unsafe_op_in_unsafe_fn`.

#![deny(unsafe_op_in_unsafe_fn)]

pub mod btree;
pub mod buffer;
pub mod disk;
mod error;
pub mod heap;
pub mod page;
pub mod replacer;
pub mod wal;

pub use error::StorageError;
