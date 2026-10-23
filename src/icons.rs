//! Windows shell icons (the same ones Explorer shows) as RGBA pixels. `None` elsewhere or on any failure,
//! so callers can fall back to a drawn placeholder.

/// `by_extension`: icon for the file *type* (cheap, cacheable). Otherwise the file's own icon (exe, lnk).
#[cfg(windows)]
pub fn icon_rgba(path: &str, by_extension: bool) -> Option<(usize, usize, Vec<u8>)> {
    use std::ffi::c_void;
    use windows_sys::Win32::Graphics::Gdi::{CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS};
    use windows_sys::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_SYSICONINDEX, SHGFI_USEFILEATTRIBUTES};
    use windows_sys::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, ICONINFO};

    unsafe {
        let name = if by_extension {
            let ext = std::path::Path::new(path).extension()?.to_string_lossy().into_owned();
            format!("file.{ext}")
        } else { path.to_string() };
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let attrs = if by_extension { 0x80u32 } else { 0 };
        let use_attrs = if by_extension { SHGFI_USEFILEATTRIBUTES } else { 0 };
        let mut info: SHFILEINFOW = std::mem::zeroed();
        // First the big (up to 256 px) version from the system image list, like Explorer's "extra large icons".
        let mut hicon = std::ptr::null_mut();
        if SHGetFileInfoW(wide.as_ptr(), attrs, &mut info, std::mem::size_of::<SHFILEINFOW>() as u32, SHGFI_SYSICONINDEX | use_attrs) != 0 {
            hicon = big_icon(info.iIcon);
        }
        if hicon.is_null() {
            let mut small: SHFILEINFOW = std::mem::zeroed();
            if SHGetFileInfoW(wide.as_ptr(), attrs, &mut small, std::mem::size_of::<SHFILEINFOW>() as u32, SHGFI_ICON | use_attrs) == 0 || small.hIcon.is_null() {
                return None;
            }
            hicon = small.hIcon;
        }
        info.hIcon = hicon;

        let mut icon: ICONINFO = std::mem::zeroed();
        let mut result = None;
        if GetIconInfo(info.hIcon, &mut icon) != 0 {
            if !icon.hbmColor.is_null() {
                let mut bmp: BITMAP = std::mem::zeroed();
                GetObjectW(icon.hbmColor, std::mem::size_of::<BITMAP>() as i32, &mut bmp as *mut BITMAP as *mut c_void);
                let (w, h) = (bmp.bmWidth, bmp.bmHeight);
                if w > 0 && h > 0 && w <= 256 && h <= 256 {
                    let dc = CreateCompatibleDC(std::ptr::null_mut());
                    let read = |bitmap, buf: &mut Vec<u8>| -> bool {
                        let mut bi: BITMAPINFO = std::mem::zeroed();
                        bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
                        bi.bmiHeader.biWidth = w;
                        bi.bmiHeader.biHeight = -h;             // top-down rows
                        bi.bmiHeader.biPlanes = 1;
                        bi.bmiHeader.biBitCount = 32;
                        bi.bmiHeader.biCompression = BI_RGB;
                        GetDIBits(dc, bitmap, 0, h as u32, buf.as_mut_ptr() as *mut c_void, &mut bi, DIB_RGB_COLORS) != 0
                    };
                    let mut px = vec![0u8; (w * h * 4) as usize];
                    if read(icon.hbmColor, &mut px) {
                        if !px.chunks_exact(4).any(|p| p[3] != 0) && !icon.hbmMask.is_null() {
                            // Old-style icon without an alpha channel: opaque wherever the AND mask is 0.
                            let mut mask = vec![0u8; px.len()];
                            if read(icon.hbmMask, &mut mask) {
                                for (p, m) in px.chunks_exact_mut(4).zip(mask.chunks_exact(4)) { p[3] = if m[0] == 0 { 255 } else { 0 }; }
                            }
                        }
                        for p in px.chunks_exact_mut(4) { p.swap(0, 2); }   // BGRA -> RGBA
                        result = Some(halve_if_huge(w as usize, h as usize, px));
                    }
                    DeleteDC(dc);
                }
                DeleteObject(icon.hbmColor);
            }
            if !icon.hbmMask.is_null() { DeleteObject(icon.hbmMask); }
        }
        DestroyIcon(info.hIcon);
        result
    }
}

/// A 256 px icon is averaged down to 128 px: shown at about 40-70 px it then stays smooth instead of shimmering.
#[cfg(windows)]
fn halve_if_huge(w: usize, h: usize, px: Vec<u8>) -> (usize, usize, Vec<u8>) {
    if w < 192 || h < 192 { return (w, h, px); }
    let (nw, nh) = (w / 2, h / 2);
    let mut out = vec![0u8; nw * nh * 4];
    for y in 0..nh {
        for x in 0..nw {
            for c in 0..4 {
                let at = |dx: usize, dy: usize| px[((y * 2 + dy) * w + x * 2 + dx) * 4 + c] as u32;
                out[(y * nw + x) * 4 + c] = ((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1)) / 4) as u8;
            }
        }
    }
    (nw, nh, out)
}

/// Icon `index` of the system image list at 256 px (jumbo) or, failing that, 48 px, without the shortcut arrow.
/// IImageList is a COM interface; its methods are called through the table directly (GetIcon is entry 10).
#[cfg(windows)]
unsafe fn big_icon(index: i32) -> windows_sys::Win32::UI::WindowsAndMessaging::HICON {
    use std::ffi::c_void;
    use windows_sys::core::GUID;
    use windows_sys::Win32::UI::Shell::SHGetImageList;
    const IID_IIMAGELIST: GUID = GUID { data1: 0x46EB5926, data2: 0x582E, data3: 0x4017, data4: [0x9F, 0xDF, 0xE8, 0x99, 0x8D, 0xAA, 0x09, 0x50] };
    for size in [4, 2] {   // SHIL_JUMBO, SHIL_EXTRALARGE
        let mut list: *mut c_void = std::ptr::null_mut();
        if SHGetImageList(size, &IID_IIMAGELIST, &mut list) != 0 || list.is_null() { continue; }
        let table = *(list as *const *const usize);
        let get_icon: unsafe extern "system" fn(*mut c_void, i32, u32, *mut *mut c_void) -> i32 = std::mem::transmute(*table.add(10));
        let release: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(*table.add(2));
        let mut icon: *mut c_void = std::ptr::null_mut();
        let ok = get_icon(list, index, 1, &mut icon) == 0 && !icon.is_null();   // ILD_TRANSPARENT: no overlays
        release(list);
        if ok { return icon; }
    }
    std::ptr::null_mut()
}

#[cfg(not(windows))]
pub fn icon_rgba(_path: &str, _by_extension: bool) -> Option<(usize, usize, Vec<u8>)> { None }
