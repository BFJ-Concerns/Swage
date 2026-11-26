//! The memory a program's buffers hold outside the engine's heap.
//!
//! V8 keeps a program's memory in two stores. Objects, strings and closures
//! live in the isolate's heap, which the engine bounds itself. The bytes of
//! an `ArrayBuffer` — and so of every typed array and `DataView` over one —
//! live outside it, in a backing store the engine asks its embedder to
//! allocate, and the heap limit never counts them: left alone, one
//! `new Uint8Array(2 ** 32)` takes four gibibytes from the gateway under any
//! heap limit.
//!
//! Every program's isolate is therefore built with the allocator made here,
//! which grants a backing store only while the bytes the program's buffers
//! hold stay within a ceiling. The ceiling is the program's
//! [`IsolateLimits::heap_bytes`](crate::IsolateLimits::heap_bytes): the one
//! number a caller states bounds each store on its own, so a program holds at
//! most that much heap and at most that much again in buffers. The two are
//! not pooled, because the engine can grow its heap after a buffer was
//! granted against the room the heap had left.
//!
//! # A refusal, and when it ends the program
//!
//! A buffer the program has dropped keeps its bytes until the engine collects
//! it, so the first refusal of a request proves nothing: the engine answers
//! it by collecting garbage and asking again, and a program that makes and
//! drops buffers within its ceiling never notices. When collecting has not
//! helped, the engine makes a last-resort collection and, before it, calls
//! the isolate's near-heap-limit callback — the same one it calls for a full
//! heap. That callback is where a [`Program`](crate::Program) is terminated,
//! and a refusal still standing in the budget is how it tells the two stores
//! apart and records
//! [`Termination::BufferLimit`](crate::Termination::BufferLimit) rather than
//! `HeapLimit`.
//!
//! The engine then raises its own `RangeError` for the request. A program
//! may see it in a `catch` block, and is ended at its next function call or
//! loop iteration: a refusal is not something a program recovers from.
//!
//! The ceiling holds by this allocator alone. Two things rest on the engine
//! Swage links, V8 15.0 as the `v8` crate builds it, and nothing in the
//! engine's interface promises either; the crate's tests for a buffer past
//! the limit are what say so when an upgrade changes one.
//!
//! - Ending the program rests on the engine calling that callback for a
//!   refused buffer.
//! - Refusing at all is safe only where the engine answers with an
//!   exception. It does in every constructor, and aborts the process where
//!   it moves a small typed array's bytes out of its heap on the first use
//!   of `.buffer`. This build keeps no typed array's bytes in its heap, so
//!   even the smallest are requested, and refused, in the constructor.
//!
//! # What the budget does not see
//!
//! - A resizable buffer (`new ArrayBuffer(n, { maxByteLength })`) is reserved
//!   and committed through the process's page allocator, never through this
//!   one, so no per-program count can cover it. The cage refuses the
//!   `maxByteLength` option, and a program's buffers are all fixed-length.
//! - A buffer the host creates through the engine's API is not a request the
//!   engine retries: it aborts the process when one is refused. Nothing
//!   crosses a program's door but JSON strings, and it stays that way.
//!
//! The engine frees backing stores from its collector's threads while the
//! program allocates on its own, so the count is atomic and takes no lock.

use std::alloc::{self, Layout};
use std::ffi::c_void;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use deno_core::v8;

/// The alignment of every backing store: `malloc`'s on the 64-bit targets
/// Swage builds for, which is what the engine's own allocator hands out.
const ALIGNMENT: usize = 16;

/// How many bytes a program's buffers hold, against the most they may.
pub(crate) struct BufferBudget {
    ceiling: usize,
    held: AtomicUsize,
    /// Set by a refusal and cleared by the next grant: whether the engine's
    /// latest request is one it was refused.
    refused: AtomicBool,
}

impl BufferBudget {
    /// A budget of `ceiling` bytes, or of the largest single allocation the
    /// platform can describe where `ceiling` is past it, so that every length
    /// the budget grants has a layout.
    pub(crate) const fn new(ceiling: usize) -> Self {
        let largest = isize::MAX as usize - (ALIGNMENT - 1);
        Self {
            ceiling: if ceiling < largest { ceiling } else { largest },
            held: AtomicUsize::new(0),
            refused: AtomicBool::new(false),
        }
    }

    /// Count `len` more bytes as held if they fit under the ceiling.
    fn take(&self, len: usize) -> bool {
        let fits = self
            .held
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |held| {
                held.checked_add(len).filter(|&total| total <= self.ceiling)
            })
            .is_ok();
        self.refused.store(!fits, Ordering::Release);
        fits
    }

    fn give_back(&self, len: usize) {
        self.held.fetch_sub(len, Ordering::AcqRel);
    }

    /// Undo a grant nothing could back: the bytes are not held, and the
    /// request stands refused like any other.
    fn revoke(&self, len: usize) {
        self.give_back(len);
        self.refused.store(true, Ordering::Release);
    }

    /// Whether the engine's latest request was refused: true from a refusal
    /// until a request is granted, so true throughout the engine's
    /// collect-and-ask-again over a request that never fits.
    pub(crate) fn refusal_stands(&self) -> bool {
        self.refused.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn held(&self) -> usize {
        self.held.load(Ordering::Acquire)
    }
}

/// The allocator a program's isolate is created with. It owns one count of
/// `budget`, released when the engine drops the allocator, which is after
/// the isolate and the last of its backing stores are gone.
pub(crate) fn allocator(budget: Arc<BufferBudget>) -> v8::UniqueRef<v8::Allocator> {
    static VTABLE: v8::RustAllocatorVtable<BufferBudget> = v8::RustAllocatorVtable {
        allocate: allocate_zeroed,
        allocate_uninitialized,
        free,
        drop: release,
    };
    // SAFETY: the handle is a live `BufferBudget` this allocator owns a count
    // of until `release`, and every function in `VTABLE` takes one.
    unsafe { v8::new_rust_allocator(Arc::into_raw(budget), &VTABLE) }
}

unsafe extern "C" fn allocate_zeroed(budget: &BufferBudget, len: usize) -> *mut c_void {
    // SAFETY: `layout` never describes zero bytes.
    granted(budget, len, |layout| unsafe { alloc::alloc_zeroed(layout) })
}

unsafe extern "C" fn allocate_uninitialized(budget: &BufferBudget, len: usize) -> *mut c_void {
    // SAFETY: `layout` never describes zero bytes.
    granted(budget, len, |layout| unsafe { alloc::alloc(layout) })
}

/// Allocate `len` bytes if the budget grants them. Null tells the engine to
/// collect garbage and ask again, and to raise a `RangeError` in the program
/// once that has not helped.
fn granted(
    budget: &BufferBudget,
    len: usize,
    allocate: impl FnOnce(Layout) -> *mut u8,
) -> *mut c_void {
    if !budget.take(len) {
        return ptr::null_mut();
    }
    let store = allocate(layout(len));
    if store.is_null() {
        // The process is out of memory, not the program over its ceiling;
        // the engine is told the same thing either way.
        budget.revoke(len);
    }
    store.cast()
}

unsafe extern "C" fn free(budget: &BufferBudget, store: *mut c_void, len: usize) {
    if store.is_null() {
        return;
    }
    // SAFETY: the engine frees a store with the length it asked for, so this
    // is the layout `granted` allocated it with.
    unsafe { alloc::dealloc(store.cast(), layout(len)) };
    budget.give_back(len);
}

unsafe extern "C" fn release(budget: *const BufferBudget) {
    // SAFETY: `budget` is the pointer `allocator` made with `Arc::into_raw`,
    // and the engine drops its allocator once.
    drop(unsafe { Arc::from_raw(budget) });
}

/// The layout of a `len`-byte backing store. The engine asks for no empty
/// store — an empty buffer has none — but a layout of one byte keeps a
/// request for one from being undefined.
fn layout(len: usize) -> Layout {
    Layout::from_size_align(len.max(1), ALIGNMENT)
        .expect("a length the budget granted is within what a layout can describe")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_granted_up_to_the_ceiling_and_not_past_it() {
        let budget = BufferBudget::new(100);
        assert!(budget.take(60));
        assert!(budget.take(40));
        assert!(!budget.take(1));
        assert_eq!(budget.held(), 100);
    }

    #[test]
    fn a_refusal_stands_until_a_request_is_granted() {
        let budget = BufferBudget::new(100);
        assert!(budget.take(80));
        assert!(!budget.refusal_stands());
        assert!(!budget.take(50));
        assert!(budget.refusal_stands());

        // The engine collected the first buffer and asked again.
        budget.give_back(80);
        assert!(budget.refusal_stands());
        assert!(budget.take(50));
        assert!(!budget.refusal_stands());
    }

    #[test]
    fn a_revoked_grant_is_not_held_and_stands_refused() {
        let budget = BufferBudget::new(100);
        assert!(budget.take(60));
        budget.revoke(60);
        assert_eq!(budget.held(), 0);
        assert!(budget.refusal_stands());
    }

    #[test]
    fn a_ceiling_past_the_address_space_grants_nothing_a_layout_cannot_describe() {
        let budget = BufferBudget::new(usize::MAX);
        assert!(!budget.take(isize::MAX as usize));
        assert!(budget.take(1024));
        assert!(!budget.take(usize::MAX));
        assert_eq!(budget.held(), 1024);
    }
}
