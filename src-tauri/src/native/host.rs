//! Where the native renderer draws: on macOS, a view of its own placed beneath the
//! web page, so the page (with a transparent background) shows the HUD on top.

use raw_window_handle::{RawDisplayHandle, RawWindowHandle};

/// A view the renderer owns. Dropped (and removed from the window) on the main thread.
pub struct HostView {
    #[cfg(target_os = "macos")]
    view: objc2::rc::Retained<objc2_app_kit::NSView>,
}

// SAFETY: the view is only touched on the main thread (`create` and `remove` run there);
// other threads only hold the pointer, which wgpu's Metal layer keeps alive.
unsafe impl Send for HostView {}

#[cfg(target_os = "macos")]
impl HostView {
    /// Must run on the main thread.
    pub fn create(ns_window: *mut std::ffi::c_void) -> Result<HostView, String> {
        use objc2::MainThreadMarker;
        use objc2_app_kit::{NSAutoresizingMaskOptions, NSView, NSWindow, NSWindowOrderingMode};
        let mtm = MainThreadMarker::new().ok_or("The renderer's view must be made on the main thread.")?;
        // SAFETY: Tauri hands out the window's live NSWindow.
        let window: &NSWindow = unsafe { &*(ns_window as *const NSWindow) };
        let content = window.contentView().ok_or("The window has no content view.")?;
        let view = NSView::initWithFrame(mtm.alloc::<NSView>(), content.bounds());
        view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
        // Beneath everything else, i.e. beneath the web page.
        content.addSubview_positioned_relativeTo(&view, NSWindowOrderingMode::Below, None);
        Ok(HostView { view })
    }

    pub fn handles(&self) -> (RawDisplayHandle, RawWindowHandle) {
        use raw_window_handle::{AppKitDisplayHandle, AppKitWindowHandle};
        let ptr = std::ptr::NonNull::from(&*self.view).cast();
        (RawDisplayHandle::AppKit(AppKitDisplayHandle::new()), RawWindowHandle::AppKit(AppKitWindowHandle::new(ptr)))
    }

    /// Size in physical pixels.
    pub fn size(&self) -> (u32, u32) {
        let b = self.view.bounds();
        let scale = self.view.window().map_or(2.0, |w| w.backingScaleFactor());
        ((b.size.width * scale).round().max(1.0) as u32, (b.size.height * scale).round().max(1.0) as u32)
    }

    /// False while the window is hidden behind others or minimised.
    pub fn visible(&self) -> bool {
        use objc2_app_kit::NSWindowOcclusionState;
        self.view.window().is_some_and(|w| w.occlusionState().contains(NSWindowOcclusionState::Visible))
    }

    /// Must run on the main thread.
    pub fn remove(self) {
        self.view.removeFromSuperview();
    }
}

#[cfg(not(target_os = "macos"))]
impl HostView {
    pub fn create(_: *mut std::ffi::c_void) -> Result<HostView, String> {
        Err("The native renderer is only available on macOS so far.".into())
    }

    pub fn handles(&self) -> (RawDisplayHandle, RawWindowHandle) {
        unreachable!()
    }

    pub fn size(&self) -> (u32, u32) {
        (1, 1)
    }

    pub fn visible(&self) -> bool {
        true
    }

    pub fn remove(self) {}
}
