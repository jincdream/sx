use std::sync::atomic::{AtomicU8, Ordering};

/// Atomic counter for allocating sandbox IP indices (2–254).
#[allow(dead_code)]
static NEXT_INDEX: AtomicU8 = AtomicU8::new(2);

/// Allocate a unique sandbox index for IP assignment.
#[allow(dead_code)]
pub fn allocate_index() -> anyhow::Result<u8> {
    let idx = NEXT_INDEX.fetch_add(1, Ordering::SeqCst);
    if idx > 254 {
        anyhow::bail!("No more IP slots available (max 253 sandboxes)");
    }
    Ok(idx)
}

/// Release a sandbox index (currently a no-op; indices are not recycled).
#[allow(dead_code)]
pub fn release_index(_idx: u8) {
    // In a production system, you'd recycle indices. For simplicity, we don't.
}

#[allow(unused_imports)]
pub use crate::os::sys::{ensure_bridge, setup_sandbox_net, teardown_sandbox_net};
