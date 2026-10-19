use std::mem::{size_of, zeroed};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use winapi::{
    shared::{
        minwindef::{DWORD, UINT},
        ntdef::HRESULT,
        windef::{HBITMAP, HDC, HWND, LPRECT, RECT},
    },
    um::{
        errhandlingapi::GetLastError,
        wingdi::{
            BitBlt,
            CreateCompatibleBitmap,
            CreateCompatibleDC,
            CreateDCW,
            DeleteDC,
            DeleteObject,
            GetDIBits,
            PatBlt,
            SelectObject,
            BITMAPINFO,
            BITMAPINFOHEADER,
            BI_RGB,
            BLACKNESS,
            CAPTUREBLT,
            DIB_RGB_COLORS, //CAPTUREBLT,
            HGDI_ERROR,
            RGBQUAD,
            SRCCOPY,
        },
        winuser::{GetSystemMetrics, SM_REMOTESESSION},
    },
};

const PW_RENDERFULLCONTENT: UINT = 2;
const GW_HWNDNEXT: UINT = 2;
const DWMWA_CLOAKED: DWORD = 14;

#[link(name = "user32")]
extern "system" {
    fn GetTopWindow(hwnd: HWND) -> HWND;
    fn GetWindow(hwnd: HWND, cmd: UINT) -> HWND;
    fn IsWindowVisible(hwnd: HWND) -> i32;
    fn IsIconic(hwnd: HWND) -> i32;
    fn GetWindowRect(hwnd: HWND, rect: LPRECT) -> i32;
    fn PrintWindow(hwnd: HWND, hdc: HDC, flags: UINT) -> i32;
    fn GetClassNameW(hwnd: HWND, name: *mut u16, max_count: i32) -> i32;
}

#[link(name = "dwmapi")]
extern "system" {
    fn DwmGetWindowAttribute(
        hwnd: HWND,
        attribute: DWORD,
        value: *mut std::ffi::c_void,
        size: DWORD,
    ) -> HRESULT;
}

const PIXEL_WIDTH: i32 = 4;

pub struct CapturerGDI {
    screen_dc: HDC,
    dc: HDC,
    bmp: HBITMAP,
    width: i32,
    height: i32,
    origin_x: i32,
    origin_y: i32,
    // CAPTUREBLT reads layered windows, but the remote display driver rejects
    // it and BitBlt then fails the whole frame. RDP sessions start without it.
    capture_blt: AtomicBool,
    logged_window_capture: AtomicBool,
}

impl CapturerGDI {
    pub fn new(
        name: &[u16],
        origin: (i32, i32),
        width: i32,
        height: i32,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        /* or Enumerate monitors with EnumDisplayMonitors,
        https://stackoverflow.com/questions/34987695/how-can-i-get-an-hmonitor-handle-from-a-display-device-name
            #[no_mangle]
            pub extern "C" fn callback(m: HMONITOR, dc: HDC, rect: LPRECT, lp: LPARAM) -> BOOL {}
        */
        /*
        shared::windef::HMONITOR,
        winuser::{GetMonitorInfoW, GetSystemMetrics, MONITORINFOEXW},
        let mut mi: MONITORINFOEXW = std::mem::MaybeUninit::uninit().assume_init();
        mi.cbSize = size_of::<MONITORINFOEXW>() as _;
        if GetMonitorInfoW(m, &mut mi as *mut MONITORINFOEXW as _) == 0 {
            return Err(format!("Failed to get monitor information of: {:?}", m).into());
        }
        */
        unsafe {
            if name.is_empty() {
                return Err("Empty display name".into());
            }
            let screen_dc = CreateDCW(&name[0], 0 as _, 0 as _, 0 as _);
            if screen_dc.is_null() {
                return Err("Failed to create dc from monitor name".into());
            }

            // Create a Windows Bitmap, and copy the bits into it
            let dc = CreateCompatibleDC(screen_dc);
            if dc.is_null() {
                DeleteDC(screen_dc);
                return Err("Can't get a Windows display".into());
            }

            let bmp = CreateCompatibleBitmap(screen_dc, width, height);
            if bmp.is_null() {
                DeleteDC(screen_dc);
                DeleteDC(dc);
                return Err("Can't create a Windows buffer".into());
            }

            let res = SelectObject(dc, bmp as _);
            if res.is_null() || res == HGDI_ERROR {
                DeleteDC(screen_dc);
                DeleteDC(dc);
                DeleteObject(bmp as _);
                return Err("Can't select Windows buffer".into());
            }
            let remote_session = GetSystemMetrics(SM_REMOTESESSION) != 0;
            Ok(Self {
                screen_dc,
                dc,
                bmp,
                width,
                height,
                origin_x: origin.0,
                origin_y: origin.1,
                capture_blt: AtomicBool::new(!remote_session),
                logged_window_capture: AtomicBool::new(false),
            })
        }
    }

    pub fn frame(&self, data: &mut Vec<u8>) -> Result<(), Box<dyn std::error::Error>> {
        unsafe {
            let use_capture_blt = self.capture_blt.load(Ordering::Relaxed);
            // CAPTUREBLT enables layered windows but also makes the cursor blink.
            let rop = if use_capture_blt {
                SRCCOPY | CAPTUREBLT
            } else {
                SRCCOPY
            };
            let mut res = BitBlt(
                self.dc,
                0,
                0,
                self.width,
                self.height,
                self.screen_dc,
                0,
                0,
                rop,
            );
            if res == 0 && use_capture_blt {
                let err = GetLastError();
                self.capture_blt.store(false, Ordering::Relaxed);
                hbb_common::log::warn!(
                    "BitBlt CAPTUREBLT failed (os error {err}), retrying without it"
                );
                res = BitBlt(
                    self.dc,
                    0,
                    0,
                    self.width,
                    self.height,
                    self.screen_dc,
                    0,
                    0,
                    SRCCOPY,
                );
            }
            if res == 0 {
                let err = GetLastError();
                // The Microsoft Remote Display Adapter (RdpIdd) rejects BitBlt of
                // the screen DC with ERROR_INVALID_HANDLE (6). PrintWindow of the
                // visible top-level windows still returns the desktop.
                let painted = self.capture_visible_windows();
                if painted == 0 {
                    return Err(format!(
                        "Failed to copy screen to Windows buffer: os error {err}; window capture produced no image"
                    )
                    .into());
                }
                if !self.logged_window_capture.swap(true, Ordering::Relaxed) {
                    hbb_common::log::info!(
                        "Screen BitBlt failed (os error {err}); captured {painted} visible windows"
                    );
                }
            }

            let stride = self.width * PIXEL_WIDTH;
            let size: usize = (stride * self.height) as usize;
            let mut data1: Vec<u8> = Vec::with_capacity(size);
            data1.set_len(size);
            data.resize(size, 0);

            let mut bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as _,
                    biWidth: self.width as _,
                    biHeight: self.height as _,
                    biPlanes: 1,
                    biBitCount: (8 * PIXEL_WIDTH) as _,
                    biCompression: BI_RGB,
                    biSizeImage: (self.width * self.height * PIXEL_WIDTH) as _,
                    biXPelsPerMeter: 0,
                    biYPelsPerMeter: 0,
                    biClrUsed: 0,
                    biClrImportant: 0,
                },
                bmiColors: [RGBQUAD {
                    rgbBlue: 0,
                    rgbGreen: 0,
                    rgbRed: 0,
                    rgbReserved: 0,
                }],
            };

            // copy bits into Vec
            let res = GetDIBits(
                self.dc,
                self.bmp,
                0,
                self.height as _,
                &mut data[0] as *mut u8 as _,
                &mut bmi as _,
                DIB_RGB_COLORS,
            );
            if res == 0 {
                return Err("GetDIBits failed".into());
            }
            crate::common::ARGBMirror(
                data.as_ptr(),
                stride,
                data1.as_mut_ptr(),
                stride,
                self.width,
                self.height,
            );
            crate::common::ARGBRotate(
                data1.as_ptr(),
                stride,
                data.as_mut_ptr(),
                stride,
                self.width,
                self.height,
                crate::RotationMode::kRotate180,
            );
            Ok(())
        }
    }

    /// Paint visible top-level windows into the screen bitmap, back to front.
    /// Cloaked shell surfaces (the RDP input-experience window) are skipped
    /// because they cover the desktop with a solid color.
    fn capture_visible_windows(&self) -> i32 {
        unsafe {
            let mut windows = Vec::new();
            let mut hwnd = GetTopWindow(ptr::null_mut());
            let mut guard = 0;
            while !hwnd.is_null() && guard < 500 {
                windows.push(hwnd);
                hwnd = GetWindow(hwnd, GW_HWNDNEXT);
                guard += 1;
            }
            windows.reverse();
            // Uncovered pixels must not be uninitialized bitmap memory.
            PatBlt(self.dc, 0, 0, self.width, self.height, BLACKNESS);
            let mut painted = 0i32;
            for hwnd in windows {
                if IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 {
                    continue;
                }
                let mut cloaked: u32 = 0;
                let hr = DwmGetWindowAttribute(
                    hwnd,
                    DWMWA_CLOAKED,
                    &mut cloaked as *mut u32 as *mut std::ffi::c_void,
                    size_of::<u32>() as DWORD,
                );
                if hr == 0 && cloaked != 0 {
                    continue;
                }
                if hr != 0 && Self::class_name(hwnd) == "Windows.UI.Core.CoreWindow" {
                    continue;
                }
                let mut rect: RECT = zeroed();
                if GetWindowRect(hwnd, &mut rect) == 0 {
                    continue;
                }
                let ww = rect.right - rect.left;
                let hh = rect.bottom - rect.top;
                if ww < 8 || hh < 8 || ww > 8000 || hh > 8000 {
                    continue;
                }
                let dx = rect.left - self.origin_x;
                let dy = rect.top - self.origin_y;
                if dx >= self.width || dy >= self.height || dx + ww <= 0 || dy + hh <= 0 {
                    continue;
                }
                let wmem = CreateCompatibleDC(self.screen_dc);
                if wmem.is_null() {
                    continue;
                }
                let wbmp = CreateCompatibleBitmap(self.screen_dc, ww, hh);
                if wbmp.is_null() {
                    DeleteDC(wmem);
                    continue;
                }
                let wold = SelectObject(wmem, wbmp as _);
                let ok = PrintWindow(hwnd, wmem, PW_RENDERFULLCONTENT);
                if ok != 0
                    && BitBlt(self.dc, dx, dy, ww, hh, wmem, 0, 0, SRCCOPY) != 0
                {
                    painted += 1;
                }
                SelectObject(wmem, wold);
                DeleteObject(wbmp as _);
                DeleteDC(wmem);
            }
            painted
        }
    }

    fn class_name(hwnd: HWND) -> String {
        unsafe {
            let mut buf = [0u16; 64];
            let n = GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
            if n <= 0 {
                return String::new();
            }
            String::from_utf16_lossy(&buf[..n as usize])
        }
    }
}

impl Drop for CapturerGDI {
    fn drop(&mut self) {
        unsafe {
            DeleteDC(self.screen_dc);
            DeleteDC(self.dc);
            DeleteObject(self.bmp as _);
        }
    }
}
