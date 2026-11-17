use anyhow::{Result, ensure};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    UI::{
        Input::KeyboardAndMouse::{VK_F4, VK_SPACE},
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::*,
    },
};

const FRAME_SUBCLASS: usize = 0x52534E46;

unsafe extern "system" fn frame_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    _data: usize,
) -> LRESULT {
    // SAFETY: Windows invokes this on the owning UI thread for the live HWND.
    // Unrelated messages retain GPUI's handler; no Rust state crosses the callback.
    unsafe {
        if message == WM_NCDESTROY {
            RemoveWindowSubclass(hwnd, Some(frame_proc), id);
        }
        if matches!(
            message,
            WM_NCHITTEST
                | WM_NCLBUTTONDOWN
                | WM_NCLBUTTONUP
                | WM_NCLBUTTONDBLCLK
                | WM_NCRBUTTONDOWN
                | WM_NCRBUTTONUP
                | WM_NCRBUTTONDBLCLK
        ) {
            // GPUI otherwise sends non-client clicks through client input handlers,
            // which can prevent Windows from starting its native resize/move loop.
            return DefWindowProcW(hwnd, message, wparam, lparam);
        }
        if message == WM_SYSKEYDOWN {
            // GPUI dispatches system keys without TranslateMessage. Open the native
            // menu explicitly so Alt+Space also works with a focused search input.
            if wparam == VK_SPACE as usize {
                return DefWindowProcW(hwnd, WM_SYSCOMMAND, SC_KEYMENU as usize, VK_SPACE as isize);
            }
            if wparam == VK_F4 as usize {
                return DefWindowProcW(hwnd, WM_SYSCOMMAND, SC_CLOSE as usize, 0);
            }
        }
        if message == WM_SYSCOMMAND && wparam & 0xfff0 == SC_KEYMENU as usize {
            return DefWindowProcW(hwnd, message, wparam, lparam);
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}

/// Let Windows own caption hit-testing, Snap, the system menu, dragging and resizing.
pub fn configure(window: &gpui::Window) -> Result<()> {
    let RawWindowHandle::Win32(handle) = HasWindowHandle::window_handle(window)
        .map_err(|e| anyhow::anyhow!("window handle: {e}"))?
        .as_raw()
    else {
        anyhow::bail!("expected a Windows window");
    };
    let hwnd = handle.hwnd.get() as *mut core::ffi::c_void;
    // SAFETY: GPUI owns this live HWND; this runs on its UI thread during creation.
    unsafe {
        ensure!(
            SetWindowSubclass(hwnd, Some(frame_proc), FRAME_SUBCLASS, 0) != 0,
            "install native window frame handler: {}",
            std::io::Error::last_os_error()
        );
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        SetWindowLongPtrW(
            hwnd,
            GWL_STYLE,
            (style | WS_CAPTION | WS_THICKFRAME | WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX)
                as isize,
        );
        ensure!(
            SetWindowPos(
                hwnd,
                core::ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED
            ) != 0,
            "refresh native window frame: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

pub fn activate_existing() {
    let title: Vec<u16> = "RustSearch".encode_utf16().chain(Some(0)).collect();
    // SAFETY: this targets the existing application window in the current interactive session.
    unsafe {
        let hwnd = FindWindowW(core::ptr::null(), title.as_ptr());
        if !hwnd.is_null() {
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            }
            SetForegroundWindow(hwnd);
        }
    }
}
