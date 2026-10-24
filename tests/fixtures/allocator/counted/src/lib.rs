//! A crate whose test binary installs its own global allocator, as
//! bifrost-store does to count heap use. The allocator's methods are ordinary
//! functions, so QS gives them prologues; the runtime itself allocates.

pub struct CountingAllocator;

thread_local! {
    static BYTES: std::cell::Cell<i64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

unsafe impl std::alloc::GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        let pointer = unsafe { std::alloc::System.alloc(layout) };
        if !pointer.is_null() {
            BYTES.with(|bytes| bytes.set(bytes.get() + layout.size() as i64));
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: std::alloc::Layout) {
        BYTES.with(|bytes| bytes.set(bytes.get() - layout.size() as i64));
        unsafe { std::alloc::System.dealloc(pointer, layout) };
    }
}

pub fn live_bytes() -> i64 {
    BYTES.with(|bytes| bytes.get())
}

pub fn repeat(text: &str, times: usize) -> String {
    text.repeat(times)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeat_allocates_and_counts() {
        let before = live_bytes();
        let text = repeat("ab", 64);
        assert_eq!(text.len(), 128);
        assert!(live_bytes() >= before + 128);
    }
}
