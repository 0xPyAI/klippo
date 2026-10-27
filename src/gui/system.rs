//! Windows shell integration: sounds, Explorer, autostart, virus scan.

use std::path::Path;
use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// CREATE_NO_WINDOW: keep helper consoles (reg.exe, rasdial.exe) invisible.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn hidden(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// True when Windows apps use the dark theme (Settings > Personalization >
/// Colors). None when it cannot be read.
#[cfg(windows)]
pub fn windows_apps_dark() -> Option<bool> {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let key = wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let name = wide("AppsUseLightTheme");
    let mut value: u32 = 1;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: the strings are NUL-terminated and `value` has room for a DWORD.
    let status = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), (&mut value as *mut u32).cast(), &mut size)
    };
    (status == 0).then_some(value == 0)
}

#[cfg(not(windows))]
pub fn windows_apps_dark() -> Option<bool> {
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundEvent {
    DownloadComplete,
    DownloadFailed,
    QueueComplete,
    QueueStarted,
}

/// Test switch: when `KLIPPO_TEST_LOG` names a file, actions that reach
/// outside Klippo (sounds, opening files, hanging up, dialing, taking the
/// keyboard from another program) are written to that file instead of done.
/// Returns true when it logged.
pub fn test_log(line: &str) -> bool {
    let Some(path) = std::env::var_os("KLIPPO_TEST_LOG").filter(|p| !p.is_empty()) else {
        return false;
    };
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{}", line);
    }
    true
}

/// Opens a downloaded file with its program.
pub fn open_file(path: &Path) {
    let _ = open_path(path);
}

/// Opens a file or folder with its program, with the error for the status line.
pub fn open_path(path: impl AsRef<std::ffi::OsStr>) -> std::io::Result<()> {
    if test_log(&format!("open {}", Path::new(path.as_ref()).display())) {
        return Ok(());
    }
    open::that(path)
}

/// Plays `custom_wav` if it is set and exists, otherwise a system sound.
pub fn play_sound(event: SoundEvent, custom_wav: &str) {
    if test_log(&format!("sound {:?} {}", event, custom_wav.trim())) {
        return;
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ALIAS, SND_ASYNC, SND_FILENAME, SND_NODEFAULT};
        let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
        let custom = custom_wav.trim();
        unsafe {
            if !custom.is_empty() && Path::new(custom).exists() {
                let w = wide(custom);
                PlaySoundW(w.as_ptr(), std::ptr::null_mut(), SND_FILENAME | SND_ASYNC | SND_NODEFAULT);
            } else {
                let alias = match event {
                    SoundEvent::DownloadComplete => "SystemAsterisk",
                    SoundEvent::DownloadFailed => "SystemHand",
                    SoundEvent::QueueComplete => "SystemExclamation",
                    SoundEvent::QueueStarted => "SystemNotification",
                };
                let w = wide(alias);
                PlaySoundW(w.as_ptr(), std::ptr::null_mut(), SND_ALIAS | SND_ASYNC | SND_NODEFAULT);
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (event, custom_wav);
    }
}

/// Opens Explorer with the file selected (falls back to opening the folder).
pub fn reveal_in_folder(path: &Path) {
    if test_log(&format!("reveal {}", path.display())) {
        return;
    }
    #[cfg(windows)]
    {
        if path.exists() {
            let arg = format!("/select,\"{}\"", path.display());
            if Command::new("explorer.exe").raw_arg(arg).spawn().is_ok() {
                return;
            }
        }
    }
    if let Some(parent) = path.parent() {
        let _ = open::that(parent);
    }
}

/// Shows the Windows "Open with" chooser for a file.
pub fn open_with(path: &Path) {
    if test_log(&format!("openwith {}", path.display())) {
        return;
    }
    #[cfg(windows)]
    {
        let _ = Command::new("rundll32.exe")
            .arg("shell32.dll,OpenAs_RunDLL")
            .arg(path)
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = open::that(path);
    }
}

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "Klippo";

/// True when Klippo is registered to start with Windows.
pub fn is_launch_on_startup() -> bool {
    hidden(Command::new("reg").args(["query", RUN_KEY, "/v", RUN_VALUE]))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn set_launch_on_startup(enabled: bool) -> std::io::Result<()> {
    let status = if enabled {
        let exe = std::env::current_exe()?;
        let value = format!("\"{}\" --startup", exe.display());
        hidden(Command::new("reg").args(["add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d", &value, "/f"])).output()?
    } else {
        if !is_launch_on_startup() {
            return Ok(());
        }
        hidden(Command::new("reg").args(["delete", RUN_KEY, "/v", RUN_VALUE, "/f"])).output()?
    };
    if status.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(String::from_utf8_lossy(&status.stderr).trim().to_string()))
    }
}

/// The proxy a PAC script picks for `url` (None = DIRECT), evaluated by
/// Windows (WinHTTP). Answers are kept for five minutes per host.
#[cfg(windows)]
pub fn pac_proxy_for(url: &str, pac_url: &str) -> Option<String> {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::Networking::WinHttp::{
        WinHttpCloseHandle, WinHttpGetProxyForUrl, WinHttpOpen, WINHTTP_ACCESS_TYPE_NAMED_PROXY, WINHTTP_ACCESS_TYPE_NO_PROXY,
        WINHTTP_AUTOPROXY_CONFIG_URL, WINHTTP_AUTOPROXY_OPTIONS, WINHTTP_PROXY_INFO,
    };
    type Cache = HashMap<(String, String), (Instant, Option<String>)>;
    static CACHE: Mutex<Option<Cache>> = Mutex::new(None);
    let host = url::Url::parse(url).ok().and_then(|u| u.host_str().map(|h| h.to_string())).unwrap_or_default();
    let key = (host, pac_url.to_string());
    if let Some((at, value)) = CACHE.lock().ok().and_then(|c| c.as_ref().and_then(|c| c.get(&key).cloned())) {
        if at.elapsed() < Duration::from_secs(300) {
            return value;
        }
    }
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (agent, pac, target) = (wide("Klippo"), wide(pac_url), wide(url));
    // SAFETY: every pointer passed is a NUL-terminated buffer that outlives the
    // call; the strings WinHTTP returns are freed with GlobalFree as documented.
    let value = unsafe {
        let session = WinHttpOpen(agent.as_ptr(), WINHTTP_ACCESS_TYPE_NO_PROXY, std::ptr::null(), std::ptr::null(), 0);
        if session.is_null() {
            return None;
        }
        let mut options: WINHTTP_AUTOPROXY_OPTIONS = std::mem::zeroed();
        options.dwFlags = WINHTTP_AUTOPROXY_CONFIG_URL;
        options.lpszAutoConfigUrl = pac.as_ptr();
        options.fAutoLogonIfChallenged = 1;
        let mut info: WINHTTP_PROXY_INFO = std::mem::zeroed();
        let ok = WinHttpGetProxyForUrl(session, target.as_ptr(), &mut options, &mut info);
        let mut found = None;
        if ok != 0 && info.dwAccessType == WINHTTP_ACCESS_TYPE_NAMED_PROXY && !info.lpszProxy.is_null() {
            let mut len = 0;
            while *info.lpszProxy.add(len) != 0 {
                len += 1;
            }
            let list = String::from_utf16_lossy(std::slice::from_raw_parts(info.lpszProxy, len));
            found = list.split([';', ' ']).map(str::trim).find(|p| !p.is_empty()).map(str::to_string);
        }
        if !info.lpszProxy.is_null() {
            GlobalFree(info.lpszProxy.cast());
        }
        if !info.lpszProxyBypass.is_null() {
            GlobalFree(info.lpszProxyBypass.cast());
        }
        WinHttpCloseHandle(session);
        found
    };
    if let Ok(mut cache) = CACHE.lock() {
        cache.get_or_insert_with(HashMap::new).insert(key, (Instant::now(), value.clone()));
    }
    value
}

#[cfg(not(windows))]
pub fn pac_proxy_for(_url: &str, _pac_url: &str) -> Option<String> {
    None
}

/// Dial-up / VPN entries of the Windows phone books (the same list rasdial
/// uses, also when APPDATA points somewhere else).
#[cfg(windows)]
pub fn dialup_entries() -> Vec<String> {
    use windows_sys::Win32::NetworkManagement::Rras::{RasEnumEntriesW, RASENTRYNAMEW};
    const ERROR_BUFFER_TOO_SMALL: u32 = 603;
    let size = std::mem::size_of::<RASENTRYNAMEW>() as u32;
    let blank = || {
        let mut e: RASENTRYNAMEW = unsafe { std::mem::zeroed() };
        e.dwSize = size;
        e
    };
    let mut buf = vec![blank()];
    let (mut bytes, mut count) = (size, 0u32);
    let mut rc = unsafe { RasEnumEntriesW(std::ptr::null(), std::ptr::null(), buf.as_mut_ptr(), &mut bytes, &mut count) };
    if rc == ERROR_BUFFER_TOO_SMALL {
        buf = (0..bytes.div_ceil(size)).map(|_| blank()).collect();
        bytes = size * buf.len() as u32;
        rc = unsafe { RasEnumEntriesW(std::ptr::null(), std::ptr::null(), buf.as_mut_ptr(), &mut bytes, &mut count) };
    }
    if rc != 0 {
        return Vec::new();
    }
    let mut entries: Vec<String> = buf[..count as usize]
        .iter()
        .map(|e| {
            let len = e.szEntryName.iter().position(|&c| c == 0).unwrap_or(e.szEntryName.len());
            String::from_utf16_lossy(&e.szEntryName[..len])
        })
        .collect();
    entries.sort();
    entries.dedup();
    entries
}

#[cfg(not(windows))]
pub fn dialup_entries() -> Vec<String> {
    Vec::new()
}

/// Connects the chosen dial-up / VPN entry when it is not connected, with the
/// redial attempts and wait from Options (in the background).
pub fn ensure_dialup(settings: &crate::gui::state::AppSettings) {
    let entry = settings.dialup_entry.trim().to_string();
    if entry.is_empty() {
        return;
    }
    if test_log(&format!("dial {} {} redial {} every {} s", entry, settings.dialup_user, settings.redial_attempts, settings.redial_wait_seconds)) {
        return;
    }
    let (user, pass) = (settings.dialup_user.clone(), settings.dialup_pass.clone());
    let attempts = settings.redial_attempts;
    let wait = settings.redial_wait_seconds.max(1) as u64;
    std::thread::spawn(move || {
        let connected = || {
            hidden(&mut Command::new("rasdial"))
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).lines().any(|l| l.trim().eq_ignore_ascii_case(&entry)))
                .unwrap_or(false)
        };
        let mut tried = 0;
        while !connected() {
            let mut cmd = Command::new("rasdial");
            cmd.arg(&entry);
            if !user.is_empty() {
                cmd.arg(&user).arg(&pass);
            }
            if hidden(&mut cmd).output().map(|o| o.status.success()).unwrap_or(false) {
                return;
            }
            tried += 1;
            if attempts != 0 && tried >= attempts {
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(wait));
        }
    });
}

/// Disconnects dial-up / VPN connections made with RAS.
pub fn hang_up_connections() {
    if test_log("hangup") {
        return;
    }
    let _ = hidden(Command::new("rasdial").arg("/disconnect")).spawn();
}

/// Runs the configured virus scanner; `%1` in `args` becomes the file path.
pub fn scan_file(scanner: &str, args: &str, file: &Path) -> std::io::Result<()> {
    let scanner = scanner.trim();
    if scanner.is_empty() {
        return Ok(());
    }
    let file_str = file.display().to_string();
    let args = if args.contains("%1") {
        args.replace("%1", &file_str)
    } else {
        format!("{} \"{}\"", args, file_str)
    };
    let mut cmd = Command::new(scanner);
    #[cfg(windows)]
    cmd.raw_arg(args);
    #[cfg(not(windows))]
    cmd.args(args.split_whitespace());
    hidden(&mut cmd).spawn().map(|_| ())
}

/// Attaches to the parent console so CLI output is visible from a GUI-subsystem exe.
pub fn attach_console() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Console::{AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE};
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        // Redirected output (pipe or file) already works; only attach when there is none.
        if handle.is_null() || handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

/// Sends the window to the notification area. It is minimized and loses its
/// taskbar button (tool-window style) instead of being hidden, because a
/// hidden window stops the GUI loop that drives the queue and scheduler.
pub fn hide_to_tray(hwnd: isize) {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, ShowWindow, GWL_EXSTYLE, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER,
            SW_HIDE, SW_SHOWMINNOACTIVE, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
        };
        let h = hwnd as windows_sys::Win32::Foundation::HWND;
        ShowWindow(h, SW_HIDE);
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
        SetWindowLongPtrW(h, GWL_EXSTYLE, (ex | WS_EX_TOOLWINDOW as isize) & !(WS_EX_APPWINDOW as isize));
        ShowWindow(h, SW_SHOWMINNOACTIVE);
        // Windows shows a minimized window without a taskbar button as a small
        // bar at the bottom left of the screen: move that bar off the screen.
        // Restoring uses the normal window position, which does not change.
        SetWindowPos(h, std::ptr::null_mut(), -32000, -32000, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
    }
    #[cfg(not(windows))]
    let _ = hwnd;
}

/// eframe shows the main window after its first frame, and the window library
/// then sets the window styles again (with a taskbar button). Puts a window
/// that is in the notification area, and still minimized, back there.
pub fn keep_in_tray(hwnd: isize) {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowLongPtrW, IsIconic, GWL_EXSTYLE, WS_EX_TOOLWINDOW};
        let h = hwnd as windows_sys::Win32::Foundation::HWND;
        if IsIconic(h) != 0 && (GetWindowLongPtrW(h, GWL_EXSTYLE) & WS_EX_TOOLWINDOW as isize) == 0 {
            hide_to_tray(hwnd);
        }
    }
    #[cfg(not(windows))]
    let _ = hwnd;
}

/// Makes the title bar of this program's windows named `title` light or dark.
/// Windows draws it dark when apps are dark, also for Klippo's light look.
pub fn set_title_bar_theme(title: &str, dark: bool) {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM};
        use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
        use windows_sys::Win32::System::Threading::GetCurrentProcessId;
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            EnumWindows, GetWindowTextW, GetWindowThreadProcessId, SetWindowPos, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
            SWP_NOZORDER,
        };
        struct Search {
            pid: u32,
            title: Vec<u16>,
            found: Vec<HWND>,
        }
        unsafe extern "system" fn each(h: HWND, data: LPARAM) -> BOOL {
            let search = &mut *(data as *mut Search);
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, &mut pid);
            if pid == search.pid {
                let mut buf = [0u16; 512];
                let n = GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32);
                if n > 0 && buf[..n as usize] == search.title[..] {
                    search.found.push(h);
                }
            }
            1
        }
        let mut search = Search { pid: GetCurrentProcessId(), title: title.encode_utf16().collect(), found: Vec::new() };
        EnumWindows(Some(each), &mut search as *mut Search as LPARAM);
        let value: BOOL = dark as BOOL;
        for h in search.found {
            DwmSetWindowAttribute(h, DWMWA_USE_IMMERSIVE_DARK_MODE as _, &value as *const BOOL as *const _, std::mem::size_of::<BOOL>() as u32);
            // Redraw the frame in the new color.
            SetWindowPos(h, std::ptr::null_mut(), 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
    #[cfg(not(windows))]
    let _ = (title, dark);
}

/// Brings a window back from the notification area with its taskbar button.
pub fn restore_from_tray(hwnd: isize) {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetWindowLongPtrW, SetForegroundWindow, SetWindowLongPtrW, ShowWindow, GWL_EXSTYLE, SW_HIDE, SW_RESTORE,
            WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
        };
        let h = hwnd as windows_sys::Win32::Foundation::HWND;
        ShowWindow(h, SW_HIDE);
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
        SetWindowLongPtrW(h, GWL_EXSTYLE, (ex & !(WS_EX_TOOLWINDOW as isize)) | WS_EX_APPWINDOW as isize);
        ShowWindow(h, SW_RESTORE);
        SetForegroundWindow(h);
    }
    #[cfg(not(windows))]
    let _ = hwnd;
}

/// Proxy from Windows Internet Options: (`ProxyServer`, `ProxyOverride`).
/// Cached for a minute because reading it starts reg.exe.
pub fn windows_proxy() -> Option<(String, String)> {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    type Cached = Option<(Instant, Option<(String, String)>)>;
    static CACHE: Mutex<Cached> = Mutex::new(None);

    let mut cache = CACHE.lock().unwrap();
    if let Some((at, ref value)) = *cache {
        if at.elapsed() < Duration::from_secs(60) {
            return value.clone();
        }
    }
    let value = read_windows_proxy();
    *cache = Some((Instant::now(), value.clone()));
    value
}

/// A notification-area icon used only to raise balloon toasts (download
/// complete or failed) while Klippo runs in the background. It stays
/// hidden (`NIS_HIDDEN`), so it never shows a second icon next to the tray icon;
/// Windows 10/11 still routes the balloon to the notification center.
#[cfg(windows)]
pub struct Notifier {
    hwnd: isize,
    added: bool,
}

#[cfg(windows)]
impl Notifier {
    /// A fixed id for this process's notification icon on the main window.
    const UID: u32 = 0x6B70;

    pub fn new(hwnd: isize) -> Self {
        Self { hwnd, added: false }
    }

    fn base(&self) -> windows_sys::Win32::UI::Shell::NOTIFYICONDATAW {
        use windows_sys::Win32::UI::Shell::NOTIFYICONDATAW;
        // SAFETY: NOTIFYICONDATAW is plain data; zero is a valid empty value.
        let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = self.hwnd as windows_sys::Win32::Foundation::HWND;
        nid.uID = Self::UID;
        nid
    }

    fn ensure_added(&mut self) {
        if self.added {
            return;
        }
        use windows_sys::Win32::UI::Shell::{Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_STATE, NIF_TIP, NIM_ADD, NIS_HIDDEN};
        use windows_sys::Win32::UI::WindowsAndMessaging::{LoadIconW, IDI_APPLICATION};
        let mut nid = self.base();
        nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_STATE;
        nid.uCallbackMessage = 0x8000 + 1; // WM_APP + 1; the hidden icon is never clicked.
        nid.hIcon = unsafe { LoadIconW(std::ptr::null_mut(), IDI_APPLICATION) };
        nid.dwState = NIS_HIDDEN;
        nid.dwStateMask = NIS_HIDDEN;
        let tip: Vec<u16> = "Klippo".encode_utf16().collect();
        nid.szTip[..tip.len()].copy_from_slice(&tip);
        // SAFETY: nid is fully initialized and lives across the call.
        self.added = unsafe { Shell_NotifyIconW(NIM_ADD, &nid) } != 0;
    }

    /// Shows a balloon with `title` and `body`; `failed` picks the error icon.
    pub fn notify(&mut self, title: &str, body: &str, failed: bool) {
        if test_log(&format!("notify {} | {}", title.trim(), body.trim())) {
            return;
        }
        self.ensure_added();
        if !self.added {
            return;
        }
        use windows_sys::Win32::UI::Shell::{Shell_NotifyIconW, NIF_INFO, NIF_STATE, NIIF_ERROR, NIIF_INFO, NIM_MODIFY, NIS_HIDDEN};
        let mut nid = self.base();
        nid.uFlags = NIF_INFO | NIF_STATE;
        nid.dwState = NIS_HIDDEN;
        nid.dwStateMask = NIS_HIDDEN;
        nid.dwInfoFlags = if failed { NIIF_ERROR } else { NIIF_INFO };
        let put = |dst: &mut [u16], text: &str| {
            let cap = dst.len() - 1;
            let w: Vec<u16> = text.encode_utf16().take(cap).collect();
            dst[..w.len()].copy_from_slice(&w);
            dst[w.len()] = 0;
        };
        put(&mut nid.szInfoTitle, title);
        put(&mut nid.szInfo, body);
        // SAFETY: nid is fully initialized and lives across the call.
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
    }
}

#[cfg(windows)]
impl Drop for Notifier {
    fn drop(&mut self) {
        if self.added {
            use windows_sys::Win32::UI::Shell::{Shell_NotifyIconW, NIM_DELETE};
            let nid = self.base();
            // SAFETY: nid identifies our icon by (hWnd, uID).
            unsafe { Shell_NotifyIconW(NIM_DELETE, &nid) };
        }
    }
}

fn read_windows_proxy() -> Option<(String, String)> {
    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings";
    let query = |name: &str| -> Option<String> {
        let out = hidden(Command::new("reg").args(["query", KEY, "/v", name])).output().ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let line = text.lines().find(|l| l.trim_start().starts_with(name))?;
        line.split_whitespace().skip(2).collect::<Vec<_>>().join(" ").into()
    };
    let enabled = query("ProxyEnable").is_some_and(|v| v.trim() == "0x1");
    if !enabled {
        return None;
    }
    let server = query("ProxyServer").filter(|s| !s.trim().is_empty())?;
    Some((server, query("ProxyOverride").unwrap_or_default()))
}

#[cfg(all(test, windows))]
mod notifier_tests {
    use super::Notifier;

    #[test]
    fn notify_is_diverted_to_the_test_log() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("actions.log");
        std::env::set_var("KLIPPO_TEST_LOG", &log);
        // hwnd 0 is never touched: notify returns at the test-log diversion.
        Notifier::new(0).notify("Download complete", "movie.mp4", false);
        std::env::remove_var("KLIPPO_TEST_LOG");
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(text.contains("notify Download complete | movie.mp4"), "log was: {:?}", text);
    }
}
