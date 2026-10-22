use std::sync::mpsc::{self, Receiver, Sender};
use windows::Win32::Foundation::{
    ERROR_INVALID_HANDLE, ERROR_INVALID_PARAMETER, ERROR_SUCCESS, GetLastError, HINSTANCE, HWND,
    LPARAM, LRESULT, POINT, SetLastError, WPARAM,
};
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, GetConsoleWindow};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_ERROR, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NIM_SETVERSION, NIN_SELECT, NINF_KEY, NOTIFYICON_VERSION_4, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

const CALLBACK_MESSAGE: u32 = WM_APP + 1;
const SETTINGS_ID: usize = 1;
const ABOUT_ID: usize = 2;
const EXIT_ID: usize = 3;

#[derive(Debug, PartialEq)]
pub enum Event {
    Settings,
    About,
    Exit,
    Recreate,
    Error(String),
}

struct State {
    sender: Sender<Event>,
    taskbar_created: u32,
}

struct Icon(HICON);

impl Drop for Icon {
    fn drop(&mut self) {
        if let Err(error) = unsafe { DestroyIcon(self.0) } {
            eprintln!("Hop: Cannot release tray icon: {error}");
        }
    }
}

fn load_icon(instance: HINSTANCE) -> Result<Icon, String> {
    // winresource embeds the application icon as resource 1. Do not use
    // LR_SHARED: the returned icon is owned and released by Icon.
    unsafe {
        LoadImageW(
            Some(instance),
            PCWSTR(std::ptr::without_provenance(1)),
            IMAGE_ICON,
            32,
            32,
            LR_DEFAULTCOLOR,
        )
    }
    .map(|handle| Icon(HICON(handle.0)))
    .map_err(|error| format!("Cannot load Hop tray icon: {error}"))
}

pub struct Tray {
    window: HWND,
    icon: Icon,
    instance: HINSTANCE,
    class_registered: bool,
    added: bool,
    state: Box<State>,
    events: Receiver<Event>,
}

impl Tray {
    pub fn new() -> Result<Self, String> {
        let (sender, events) = mpsc::channel();
        let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
        if taskbar_created == 0 {
            return Err(format!(
                "Cannot register Explorer restart notification: {}",
                windows::core::Error::from_thread()
            ));
        }
        let instance: HINSTANCE = unsafe { GetModuleHandleW(None) }
            .map_err(|error| format!("Cannot locate Hop module: {error}"))?
            .into();
        let icon = load_icon(instance)?;
        let mut tray = Self {
            window: HWND::default(),
            icon,
            instance,
            class_registered: false,
            added: false,
            state: Box::new(State {
                sender,
                taskbar_created,
            }),
            events,
        };
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: w!("Hop.Tray"),
            ..Default::default()
        };
        if unsafe { RegisterClassW(&class) } == 0 {
            return Err(format!(
                "Cannot register tray window: {}",
                windows::core::Error::from_thread()
            ));
        }
        tray.class_registered = true;
        tray.window = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                w!("Hop.Tray"),
                w!("Hop - tray"),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance),
                Some((&*tray.state as *const State).cast()),
            )
        }
        .map_err(|error| format!("Cannot create tray window: {error}"))?;
        tray.add_icon()?;
        Ok(tray)
    }

    fn data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.window,
            uID: 1,
            ..Default::default()
        }
    }

    pub fn add_icon(&mut self) -> Result<(), String> {
        let mut data = self.data();
        data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        data.uCallbackMessage = CALLBACK_MESSAGE;
        data.hIcon = self.icon.0;
        copy_wide(&mut data.szTip, "Hop - quick file previews");
        if !unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
            return Err("Windows could not add Hop to the notification area.".into());
        }
        self.added = true;
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        if !unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) }.as_bool() {
            return Err("Windows could not configure Hop's tray notifications.".into());
        }
        Ok(())
    }

    pub fn events(&self) -> impl Iterator<Item = Event> + '_ {
        self.events.try_iter()
    }

    pub fn report_error(&self, error: &str) {
        eprintln!("Hop: {error}");
        let mut data = self.data();
        data.uFlags = NIF_INFO;
        data.dwInfoFlags = NIIF_ERROR;
        copy_wide(&mut data.szInfoTitle, "Hop");
        copy_wide(&mut data.szInfo, error);
        if !unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) }.as_bool() {
            show_error(error);
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        if self.added && !unsafe { Shell_NotifyIconW(NIM_DELETE, &self.data()) }.as_bool() {
            eprintln!("Hop: Windows could not remove the tray icon.");
        }
        if !self.window.is_invalid()
            && let Err(error) = unsafe { DestroyWindow(self.window) }
        {
            eprintln!("Hop: Cannot destroy tray window: {error}");
        }
        if self.class_registered
            && let Err(error) = unsafe { UnregisterClassW(w!("Hop.Tray"), Some(self.instance)) }
        {
            eprintln!("Hop: Cannot unregister tray window: {error}");
        }
    }
}

fn copy_wide<const N: usize>(target: &mut [u16; N], text: &str) {
    target.fill(0);
    let mut used = 0;
    for character in text.chars() {
        let mut buffer = [0u16; 2];
        let encoded = character.encode_utf16(&mut buffer);
        if used + encoded.len() >= N {
            break;
        }
        target[used..used + encoded.len()].copy_from_slice(encoded);
        used += encoded.len();
    }
}

fn command_event(command: usize) -> Option<Event> {
    match command {
        SETTINGS_ID => Some(Event::Settings),
        ABOUT_ID => Some(Event::About),
        EXIT_ID => Some(Event::Exit),
        _ => None,
    }
}

struct Menu(HMENU);

impl Drop for Menu {
    fn drop(&mut self) {
        if let Err(error) = unsafe { DestroyMenu(self.0) } {
            eprintln!("Hop: Cannot release tray menu: {error}");
        }
    }
}

fn popup_menu(window: HWND) -> Result<Option<Event>, String> {
    unsafe {
        let menu =
            Menu(CreatePopupMenu().map_err(|error| format!("Cannot create tray menu: {error}"))?);
        for (id, label) in [(SETTINGS_ID, w!("Settings")), (ABOUT_ID, w!("About"))] {
            AppendMenuW(menu.0, MF_STRING, id, label)
                .map_err(|error| format!("Cannot add tray menu item: {error}"))?;
        }
        AppendMenuW(menu.0, MF_SEPARATOR, 0, None)
            .map_err(|error| format!("Cannot add tray menu separator: {error}"))?;
        AppendMenuW(menu.0, MF_STRING, EXIT_ID, w!("Exit"))
            .map_err(|error| format!("Cannot add Exit menu item: {error}"))?;
        let mut point = POINT::default();
        GetCursorPos(&mut point).map_err(|error| format!("Cannot position tray menu: {error}"))?;
        if !SetForegroundWindow(window).as_bool() {
            return Err("Cannot focus Hop's tray menu.".into());
        }
        let selected = TrackPopupMenu(
            menu.0,
            TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
            point.x,
            point.y,
            None,
            window,
            None,
        )
        .0 as usize;
        PostMessageW(Some(window), WM_NULL, WPARAM(0), LPARAM(0))
            .map_err(|error| format!("Cannot dismiss tray menu: {error}"))?;
        Ok(command_event(selected))
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if message == WM_NCCREATE {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            SetLastError(ERROR_SUCCESS);
            SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize);
            if GetLastError() != ERROR_SUCCESS {
                return LRESULT(0);
            }
            return DefWindowProcW(window, message, wparam, lparam);
        }
        let state = (GetWindowLongPtrW(window, GWLP_USERDATA) as *const State).as_ref();
        if let Some(state) = state {
            let event = if message == CALLBACK_MESSAGE {
                let notification = lparam.0 as u32 & 0xffff;
                if [
                    WM_CONTEXTMENU,
                    WM_RBUTTONUP,
                    NIN_SELECT,
                    NIN_SELECT | NINF_KEY,
                ]
                .contains(&notification)
                {
                    match popup_menu(window) {
                        Ok(event) => event,
                        Err(error) => Some(Event::Error(error)),
                    }
                } else {
                    None
                }
            } else if message == WM_COMMAND {
                command_event(wparam.0 & 0xffff)
            } else if message == WM_CLOSE {
                Some(Event::Exit)
            } else if message == state.taskbar_created {
                Some(Event::Recreate)
            } else {
                None
            };
            if let Some(event) = event
                && state.sender.send(event).is_err()
            {
                eprintln!("Hop: The tray watcher stopped receiving commands.");
            }
            if message == CALLBACK_MESSAGE
                || message == WM_COMMAND
                || message == WM_CLOSE
                || message == state.taskbar_created
            {
                return LRESULT(0);
            }
        }
        if message == WM_NCDESTROY {
            SetWindowLongPtrW(window, GWLP_USERDATA, 0);
        }
        DefWindowProcW(window, message, wparam, lparam)
    }
}

pub fn attach_parent_console() -> Result<bool, String> {
    if !unsafe { GetConsoleWindow() }.is_invalid() {
        return Ok(true);
    }
    match unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } {
        Ok(()) => Ok(true),
        Err(error)
            if [ERROR_INVALID_HANDLE, ERROR_INVALID_PARAMETER]
                .iter()
                .any(|code| error.code() == windows::core::HRESULT::from_win32(code.0)) =>
        {
            Ok(false)
        }
        Err(error) => Err(format!("Cannot attach to the launching terminal: {error}")),
    }
}

pub fn show_error(error: &str) {
    let text: Vec<u16> = error.encode_utf16().chain(Some(0)).collect();
    unsafe {
        MessageBoxW(None, PCWSTR(text.as_ptr()), w!("Hop"), MB_OK | MB_ICONERROR);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_icon_loads_from_the_embedded_application_resource() {
        let instance = unsafe { GetModuleHandleW(None) }.unwrap().into();
        let icon = load_icon(instance).unwrap();
        assert!(!icon.0.is_invalid());
        let copied = Icon(unsafe { CopyIcon(icon.0) }.unwrap());
        assert!(!copied.0.is_invalid());
    }

    #[test]
    fn tray_menu_commands_have_distinct_actions() {
        assert_eq!(command_event(SETTINGS_ID), Some(Event::Settings));
        assert_eq!(command_event(ABOUT_ID), Some(Event::About));
        assert_eq!(command_event(EXIT_ID), Some(Event::Exit));
        assert_eq!(command_event(0), None);
        assert_eq!(command_event(99), None);
    }

    #[test]
    fn tray_strings_are_terminated_without_splitting_surrogate_pairs() {
        let mut text = [42u16; 5];
        copy_wide(&mut text, "Hop");
        assert_eq!(&text, &[72, 111, 112, 0, 0]);
        copy_wide(&mut text, "ab\u{1f600}c");
        assert_eq!(&text, &[97, 98, 0xd83d, 0xde00, 0]);
        let mut short = [42u16; 4];
        copy_wide(&mut short, "ab\u{1f600}");
        assert_eq!(&short, &[97, 98, 0, 0]);
    }
}
