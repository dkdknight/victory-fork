//! Every barrier call of ours goes through [`CountedBarrier`]: a run of
//! commands kept for one indirect execution (`crate::batch`) is recorded
//! first.

/// `ResourceBarrier`, after any kept run of the list.
#[cfg(windows)]
pub(crate) trait CountedBarrier {
    /// # Safety
    /// As `ResourceBarrier`.
    unsafe fn counted_barrier(&self, barriers: &[windows::Win32::Graphics::Direct3D12::D3D12_RESOURCE_BARRIER]);
}

#[cfg(windows)]
impl CountedBarrier for windows::Win32::Graphics::Direct3D12::ID3D12GraphicsCommandList {
    unsafe fn counted_barrier(&self, barriers: &[windows::Win32::Graphics::Direct3D12::D3D12_RESOURCE_BARRIER]) {
        crate::batch::flush(self);
        // SAFETY: the caller's.
        unsafe { self.ResourceBarrier(barriers) };
    }
}
