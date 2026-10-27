//! Native drag-and-drop: accept a link dragged from a browser onto Klippo's
//! window and download it. Windows allows one OLE drop target per
//! window; winit registers its own, so `install` replaces it (`RevokeDragDrop`
//! then `RegisterDragDrop`) with this one. It reads the dropped text
//! (`CF_UNICODETEXT`), which holds the URL when a link is dragged, and hands
//! any addresses to the GUI through a small queue.
//!
//! windows-sys has no COM interface types, so the `IDropTarget` object and the
//! one `IDataObject` method we call are built by hand as C vtables.

use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use windows_sys::core::{GUID, HRESULT};
use windows_sys::Win32::Foundation::{HGLOBAL, HWND, POINTL};
use windows_sys::Win32::System::Com::{FORMATETC, STGMEDIUM};
use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows_sys::Win32::System::Ole::{
    OleInitialize, RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop, CF_UNICODETEXT, DROPEFFECT, DROPEFFECT_COPY,
};

const S_OK: HRESULT = 0;
const E_NOINTERFACE: HRESULT = 0x8000_4002u32 as HRESULT;
const E_POINTER: HRESULT = 0x8000_4003u32 as HRESULT;

static DROPPED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Drains the links dropped onto the window since the last call.
pub fn take() -> Vec<String> {
    match DROPPED.lock() {
        Ok(mut q) => std::mem::take(&mut *q),
        Err(_) => Vec::new(),
    }
}

fn push(url: String) {
    if let Ok(mut q) = DROPPED.lock() {
        q.push(url);
    }
}

// --- COM vtables, built by hand (windows-sys has no interface types) ---

#[repr(C)]
struct IUnknownVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
}

#[repr(C)]
struct IDropTargetVtbl {
    base: IUnknownVtbl,
    drag_enter: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, POINTL, *mut DROPEFFECT) -> HRESULT,
    drag_over: unsafe extern "system" fn(*mut c_void, u32, POINTL, *mut DROPEFFECT) -> HRESULT,
    drag_leave: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    drop: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, POINTL, *mut DROPEFFECT) -> HRESULT,
}

/// We only ever call `GetData` (slot 3). The earlier slots keep the COM layout
/// so the call lands on the right function pointer; later slots are unused.
#[repr(C)]
struct IDataObjectVtbl {
    query_interface: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    get_data: unsafe extern "system" fn(*mut c_void, *const FORMATETC, *mut STGMEDIUM) -> HRESULT,
}

#[repr(C)]
struct DropTarget {
    vtbl: *const IDropTargetVtbl,
    refs: AtomicU32,
}

// IID_IDropTarget {00000122-0000-0000-C000-000000000046}
const IID_IDROPTARGET: GUID = GUID { data1: 0x0000_0122, data2: 0, data3: 0, data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46] };
// IID_IUnknown {00000000-0000-0000-C000-000000000046}
const IID_IUNKNOWN: GUID = GUID { data1: 0, data2: 0, data3: 0, data4: [0xC0, 0, 0, 0, 0, 0, 0, 0x46] };

fn guid_eq(a: *const GUID, b: &GUID) -> bool {
    if a.is_null() {
        return false;
    }
    let a = unsafe { &*a };
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}

unsafe extern "system" fn qi(this: *mut c_void, riid: *const GUID, ppv: *mut *mut c_void) -> HRESULT {
    if ppv.is_null() {
        return E_POINTER;
    }
    if guid_eq(riid, &IID_IUNKNOWN) || guid_eq(riid, &IID_IDROPTARGET) {
        *ppv = this;
        add_ref(this);
        S_OK
    } else {
        *ppv = std::ptr::null_mut();
        E_NOINTERFACE
    }
}

unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
    let obj = &*(this as *const DropTarget);
    obj.refs.fetch_add(1, Ordering::SeqCst) + 1
}

unsafe extern "system" fn release(this: *mut c_void) -> u32 {
    let obj = &*(this as *const DropTarget);
    let prev = obj.refs.fetch_sub(1, Ordering::SeqCst);
    if prev == 1 {
        drop(Box::from_raw(this as *mut DropTarget));
        0
    } else {
        prev - 1
    }
}

unsafe extern "system" fn drag_enter(_this: *mut c_void, _data: *mut c_void, _keys: u32, _pt: POINTL, effect: *mut DROPEFFECT) -> HRESULT {
    if !effect.is_null() {
        *effect = DROPEFFECT_COPY;
    }
    S_OK
}

unsafe extern "system" fn drag_over(_this: *mut c_void, _keys: u32, _pt: POINTL, effect: *mut DROPEFFECT) -> HRESULT {
    if !effect.is_null() {
        *effect = DROPEFFECT_COPY;
    }
    S_OK
}

unsafe extern "system" fn drag_leave(_this: *mut c_void) -> HRESULT {
    S_OK
}

unsafe extern "system" fn do_drop(_this: *mut c_void, data: *mut c_void, _keys: u32, _pt: POINTL, effect: *mut DROPEFFECT) -> HRESULT {
    if !effect.is_null() {
        *effect = DROPEFFECT_COPY;
    }
    if let Some(text) = read_unicode_text(data) {
        for line in text.split(['\r', '\n']).map(str::trim).filter(|l| !l.is_empty()) {
            if is_url(line) {
                push(line.to_string());
            }
        }
    }
    S_OK
}

/// Reads `CF_UNICODETEXT` from the dropped data object as a Rust string.
unsafe fn read_unicode_text(data: *mut c_void) -> Option<String> {
    if data.is_null() {
        return None;
    }
    let vtbl = *(data as *const *const IDataObjectVtbl);
    let fmt = FORMATETC {
        cfFormat: CF_UNICODETEXT,
        ptd: std::ptr::null_mut(),
        dwAspect: 1, // DVASPECT_CONTENT
        lindex: -1,
        tymed: 1, // TYMED_HGLOBAL
    };
    let mut stg: STGMEDIUM = std::mem::zeroed();
    if ((*vtbl).get_data)(data, &fmt, &mut stg) != S_OK {
        return None;
    }
    let hglobal = stg.u.hGlobal as HGLOBAL;
    let ptr = GlobalLock(hglobal) as *const u16;
    let mut text = None;
    if !ptr.is_null() {
        let max = GlobalSize(hglobal) / 2;
        let mut len = 0usize;
        while len < max && *ptr.add(len) != 0 {
            len += 1;
        }
        text = Some(String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len)));
        GlobalUnlock(hglobal);
    }
    ReleaseStgMedium(&mut stg);
    text
}

fn is_url(s: &str) -> bool {
    url::Url::parse(s).map(|u| matches!(u.scheme(), "http" | "https" | "ftp")).unwrap_or(false)
}

static VTBL: IDropTargetVtbl = IDropTargetVtbl {
    base: IUnknownVtbl { query_interface: qi, add_ref, release },
    drag_enter,
    drag_over,
    drag_leave,
    drop: do_drop,
};

/// Installs the drop target on the window. Call once, on the GUI thread.
pub fn install(hwnd: isize) {
    let h = hwnd as HWND;
    unsafe {
        // winit enables OLE when its drag-drop is on; initialize again is a no-op.
        OleInitialize(std::ptr::null());
        // Replace winit's file-only target with ours (links and files).
        RevokeDragDrop(h);
        let obj = Box::into_raw(Box::new(DropTarget { vtbl: &VTBL, refs: AtomicU32::new(1) }));
        let hr = RegisterDragDrop(h, obj as *mut c_void);
        crate::gui::system::test_log(&format!("dnd register {}", if hr == S_OK { "ok" } else { "failed" }));
        if hr != S_OK {
            release(obj as *mut c_void);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Memory::{GlobalAlloc, GMEM_FIXED};

    /// A minimal IDataObject that returns one `CF_UNICODETEXT` block, so the
    /// drop handler can be tested without a real OS drag source.
    #[repr(C)]
    struct FakeData {
        vtbl: *const IDataObjectVtbl,
        text: Vec<u16>,
    }

    unsafe extern "system" fn f_qi(_t: *mut c_void, _r: *const GUID, _p: *mut *mut c_void) -> HRESULT {
        E_NOINTERFACE
    }
    unsafe extern "system" fn f_add(_t: *mut c_void) -> u32 {
        1
    }
    unsafe extern "system" fn f_rel(_t: *mut c_void) -> u32 {
        1
    }
    unsafe extern "system" fn f_get(this: *mut c_void, fmt: *const FORMATETC, stg: *mut STGMEDIUM) -> HRESULT {
        if (*fmt).cfFormat != CF_UNICODETEXT {
            return 1;
        }
        let obj = &*(this as *const FakeData);
        let h = GlobalAlloc(GMEM_FIXED, obj.text.len() * 2);
        std::ptr::copy_nonoverlapping(obj.text.as_ptr(), h as *mut u16, obj.text.len());
        (*stg).tymed = 1;
        (*stg).u.hGlobal = h;
        (*stg).pUnkForRelease = std::ptr::null_mut();
        S_OK
    }

    static F_VTBL: IDataObjectVtbl = IDataObjectVtbl { query_interface: f_qi, add_ref: f_add, release: f_rel, get_data: f_get };

    #[test]
    fn a_dropped_link_is_read_and_queued() {
        let text: Vec<u16> = "https://host.test/movie.mp4\0".encode_utf16().collect();
        let mut obj = FakeData { vtbl: &F_VTBL, text };
        let _ = take(); // clear anything left over
        let mut effect: DROPEFFECT = 0;
        unsafe {
            do_drop(std::ptr::null_mut(), &mut obj as *mut FakeData as *mut c_void, 0, POINTL { x: 0, y: 0 }, &mut effect);
        }
        assert_eq!(effect, DROPEFFECT_COPY);
        assert_eq!(take(), vec!["https://host.test/movie.mp4".to_string()]);
    }

    #[test]
    fn only_http_like_urls_pass() {
        assert!(is_url("https://a/b.zip"));
        assert!(is_url("http://a/b"));
        assert!(is_url("ftp://a/b"));
        assert!(!is_url("just text"));
        assert!(!is_url("mailto:x@y.z"));
    }
}
