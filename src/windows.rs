use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
    CoUninitialize, IServiceProvider,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationGridPattern, TreeScope_Subtree, UIA_GridPatternId,
    UIA_IsGridPatternAvailablePropertyId,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VIRTUAL_KEY, VK_CONTROL, VK_ESCAPE, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
    VK_SPACE,
};
use windows::Win32::UI::Shell::{
    FVM_CONTENT, FVM_DETAILS, FVM_LIST, IFolderView, IShellBrowser, IShellFolderViewDual,
    IShellView, IShellWindows, IWebBrowserApp, SID_STopLevelBrowser, SVGIO_ALLVIEW,
    SVSI_DESELECTOTHERS, SVSI_ENSUREVISIBLE, SVSI_FOCUSED, SVSI_NOTAKEFOCUS, SVSI_SELECT,
    ShellWindows,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GA_ROOT, GA_ROOTOWNER, GUITHREADINFO, GetAncestor,
    GetClassNameW, GetForegroundWindow, GetGUIThreadInfo, GetMessageW, GetWindowThreadProcessId,
    IsChild, IsWindow, IsWindowVisible, MSG, MSLLHOOKSTRUCT, PM_REMOVE, PeekMessageW,
    PostThreadMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_MOUSE_LL,
    WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_QUIT, WM_RBUTTONDOWN, WM_XBUTTONDOWN, WindowFromPoint,
};
use windows::core::Interface;

thread_local! {
    static CLICK_TARGET: Cell<isize> = const { Cell::new(0) };
    static OUTSIDE_CLICKED: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
}

pub struct OutsideClickMonitor {
    clicked: Arc<AtomicBool>,
    thread_id: u32,
    worker: Option<thread::JoinHandle<()>>,
}

impl OutsideClickMonitor {
    pub fn new(window: &impl raw_window_handle::HasWindowHandle) -> Result<Self, String> {
        let handle = window
            .window_handle()
            .map_err(|error| format!("Cannot inspect preview window: {error}"))?;
        let raw_window_handle::RawWindowHandle::Win32(handle) = handle.as_raw() else {
            return Err("Outside-click dismissal requires a native Windows window.".into());
        };
        Self::start(handle.hwnd.get())
    }

    fn start(target: isize) -> Result<Self, String> {
        let clicked = Arc::new(AtomicBool::new(false));
        let worker_clicked = Arc::clone(&clicked);
        let (ready, startup) = mpsc::sync_channel(1);
        // WH_MOUSE_LL blocks global input until its installing thread dispatches the hook.
        // Keep that thread independent of video rendering and all other GUI work.
        let worker = thread::Builder::new()
            .name("outside-clicks".into())
            .spawn(move || {
                let mut message = MSG::default();
                let _ = unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) };
                let hook = match unsafe {
                    SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), None, 0)
                } {
                    Ok(hook) => hook,
                    Err(error) => {
                        let _ = ready.send(Err(format!("Cannot monitor outside clicks: {error}")));
                        return;
                    }
                };
                CLICK_TARGET.set(target);
                OUTSIDE_CLICKED.with_borrow_mut(|state| *state = Some(worker_clicked));
                if ready.send(Ok(unsafe { GetCurrentThreadId() })).is_ok() {
                    loop {
                        match unsafe { GetMessageW(&mut message, None, 0, 0) }.0 {
                            -1 => {
                                eprintln!(
                                    "Hop: Outside-click message pump failed: {}",
                                    windows::core::Error::from_thread()
                                );
                                break;
                            }
                            0 => break,
                            _ => unsafe {
                                let _ = TranslateMessage(&message);
                                DispatchMessageW(&message);
                            },
                        }
                    }
                }
                CLICK_TARGET.set(0);
                OUTSIDE_CLICKED.with_borrow_mut(|state| *state = None);
                if let Err(error) = unsafe { UnhookWindowsHookEx(hook) } {
                    eprintln!("Hop: Cannot remove outside-click monitor: {error}");
                }
            })
            .map_err(|error| format!("Cannot start outside-click worker: {error}"))?;
        match startup.recv() {
            Ok(Ok(thread_id)) => Ok(Self {
                clicked,
                thread_id,
                worker: Some(worker),
            }),
            result => {
                if worker.join().is_err() {
                    return Err("Outside-click worker panicked during startup.".into());
                }
                Err(match result {
                    Ok(Err(error)) => error,
                    Err(error) => format!("Outside-click worker stopped during startup: {error}"),
                    Ok(Ok(_)) => unreachable!(),
                })
            }
        }
    }

    pub fn take_close_request(&self) -> bool {
        self.clicked.swap(false, Ordering::Relaxed)
    }
}

impl Drop for OutsideClickMonitor {
    fn drop(&mut self) {
        if let Err(error) =
            unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) }
        {
            eprintln!("Hop: Cannot stop outside-click worker: {error}");
            return;
        }
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            eprintln!("Hop: Outside-click worker panicked.");
        }
    }
}

fn is_mouse_press(message: usize) -> bool {
    [
        WM_LBUTTONDOWN,
        WM_RBUTTONDOWN,
        WM_MBUTTONDOWN,
        WM_XBUTTONDOWN,
    ]
    .into_iter()
    .any(|button| message == button as usize)
}

unsafe extern "system" fn mouse_hook(code: i32, message: WPARAM, data: LPARAM) -> LRESULT {
    if code == 0 && is_mouse_press(message.0) && data.0 != 0 {
        let target = CLICK_TARGET.get();
        if target != 0 {
            let event = unsafe { &*(data.0 as *const MSLLHOOKSTRUCT) };
            let hit = unsafe { WindowFromPoint(event.pt) };
            let root = unsafe { GetAncestor(hit, GA_ROOT) };
            let owner = unsafe { GetAncestor(hit, GA_ROOTOWNER) };
            if ![hit, root, owner]
                .iter()
                .any(|window| window.0 as isize == target)
            {
                OUTSIDE_CLICKED.with_borrow(|state| {
                    if let Some(clicked) = state {
                        clicked.store(true, Ordering::Relaxed);
                    }
                });
            }
        }
    }
    // Observe only: the outside application must still receive the original click.
    unsafe { CallNextHookEx(None, code, message, data) }
}

pub(crate) struct ComApartment;

impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

fn key_down(key: VIRTUAL_KEY) -> bool {
    unsafe { GetAsyncKeyState(i32::from(key.0)) < 0 }
}

fn class_name(window: HWND) -> String {
    let mut buffer = [0u16; 256];
    let length = unsafe { GetClassNameW(window, &mut buffer) };
    String::from_utf16_lossy(&buffer[..length as usize])
}

fn explorer_focus() -> windows::core::Result<Option<(HWND, HWND)>> {
    let foreground = unsafe { GetForegroundWindow() };
    if !matches!(
        class_name(foreground).as_str(),
        "CabinetWClass" | "ExploreWClass"
    ) {
        return Ok(None);
    }
    let thread = unsafe { GetWindowThreadProcessId(foreground, None) };
    let mut info = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    unsafe { GetGUIThreadInfo(thread, &mut info)? };
    // Address/search bars and inline rename fields must retain normal typing.
    if !info.hwndCaret.0.is_null()
        || !matches!(
            class_name(info.hwndFocus).as_str(),
            "DirectUIHWND" | "SysListView32"
        )
    {
        return Ok(None);
    }
    Ok(Some((foreground, info.hwndFocus)))
}

fn selected_view(
    shell: &IShellWindows,
    foreground: HWND,
    focus: HWND,
) -> windows::core::Result<Option<(IShellFolderViewDual, IFolderView)>> {
    unsafe {
        for index in 0..shell.Count()? {
            let browser: IWebBrowserApp = shell.Item(&VARIANT::from(index))?.cast()?;
            if browser.HWND()?.0 != foreground.0 as isize {
                continue;
            }
            // Explorer tabs can share a top-level HWND; match the actual focused view.
            let provider: IServiceProvider = browser.cast()?;
            let shell_browser: IShellBrowser = provider.QueryService(&SID_STopLevelBrowser)?;
            let view = shell_browser.QueryActiveShellView()?;
            let view_window = view.GetWindow()?;
            if focus != view_window && !IsChild(view_window, focus).as_bool() {
                continue;
            }
            return Ok(Some((browser.Document()?.cast()?, view.cast()?)));
        }
    }
    Ok(None)
}

fn selected_path(document: &IShellFolderViewDual) -> windows::core::Result<Option<PathBuf>> {
    unsafe {
        let selected = document.SelectedItems()?;
        if selected.Count()? == 0 {
            return Ok(None);
        }
        let path = selected.Item(&VARIANT::from(0))?.Path()?;
        Ok(Some(PathBuf::from(OsString::from_wide(&path))))
    }
}

const SOURCE_ENV: &str = "HOP_EXPLORER_SOURCE";

#[derive(Clone, Copy)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy)]
struct LayoutRequest {
    id: u64,
    window: usize,
    mode: u32,
}

struct LayoutResponse {
    id: u64,
    result: Result<i32, (windows::core::HRESULT, String)>,
}

struct LayoutWorker {
    requests: Sender<LayoutRequest>,
    responses: Receiver<LayoutResponse>,
}

impl LayoutWorker {
    fn new() -> Result<Self, String> {
        let (requests, receiver) = mpsc::channel::<LayoutRequest>();
        let (sender, responses) = mpsc::channel();
        thread::Builder::new()
            .name("explorer-layout".into())
            .spawn(move || {
                let initialize = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok();
                if let Err(error) = initialize {
                    Self::initialization_failed(receiver, sender, error);
                    return;
                }
                let _apartment = ComApartment;
                let automation: IUIAutomation =
                    match unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_ALL) } {
                        Ok(automation) => automation,
                        Err(error) => {
                            Self::initialization_failed(receiver, sender, error);
                            return;
                        }
                    };
                while let Ok(request) = receiver.recv() {
                    let result = Self::stride(&automation, request)
                        .map_err(|error| (error.code(), error.to_string()));
                    if sender
                        .send(LayoutResponse {
                            id: request.id,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
                // All UIA interfaces and errors must be released on this MTA before COM teardown.
            })
            .map_err(|error| format!("Cannot start Explorer layout worker: {error}"))?;
        Ok(Self {
            requests,
            responses,
        })
    }

    fn initialization_failed(
        requests: Receiver<LayoutRequest>,
        responses: Sender<LayoutResponse>,
        error: windows::core::Error,
    ) {
        let failure = (
            error.code(),
            format!("Cannot initialize Explorer layout: {error}"),
        );
        drop(error);
        if let Ok(request) = requests.recv() {
            let _ = responses.send(LayoutResponse {
                id: request.id,
                result: Err(failure),
            });
        }
    }

    fn stride(automation: &IUIAutomation, request: LayoutRequest) -> windows::core::Result<i32> {
        let grid = ExplorerConnection::grid_pattern(
            automation,
            HWND(request.window as *mut std::ffi::c_void),
        )?;
        let stride = unsafe {
            if request.mode == FVM_LIST.0 as u32 {
                grid.CurrentRowCount()?
            } else {
                grid.CurrentColumnCount()?
            }
        };
        if stride <= 0 {
            return Err(windows::core::Error::new(
                windows::Win32::Foundation::E_FAIL,
                "Explorer returned an invalid grid layout.",
            ));
        }
        Ok(stride)
    }

    fn request(&self, request: LayoutRequest) -> windows::core::Result<()> {
        self.requests.send(request).map_err(|_| Self::stopped())
    }

    fn poll(&self, id: u64) -> windows::core::Result<Option<i32>> {
        loop {
            match self.responses.try_recv() {
                Ok(response) if response.id == id => {
                    return response
                        .result
                        .map(Some)
                        .map_err(|(code, message)| windows::core::Error::new(code, message));
                }
                Ok(_) => continue,
                Err(TryRecvError::Empty) => return Ok(None),
                Err(TryRecvError::Disconnected) => return Err(Self::stopped()),
            }
        }
    }

    fn stopped() -> windows::core::Error {
        windows::core::Error::new(
            windows::Win32::Foundation::E_FAIL,
            "The Explorer layout worker stopped unexpectedly.",
        )
    }
}

enum NavigationTarget {
    Pending,
    Unchanged,
    Select(i32),
}

#[derive(Debug, PartialEq)]
enum ConnectionPoll {
    Pending,
    Selection(Option<PathBuf>),
}

type ExplorerResult = Result<Option<PathBuf>, String>;

enum ExplorerUpdate {
    State {
        result: ExplorerResult,
        can_retry: bool,
    },
    Close,
}

pub struct ExplorerNavigator {
    requests: Sender<Direction>,
    updates: Receiver<ExplorerUpdate>,
    result: ExplorerResult,
    can_retry: bool,
    close: bool,
}

impl ExplorerNavigator {
    pub fn from_environment() -> Result<Option<Self>, String> {
        let Some((foreground, focus)) = explorer_source()? else {
            return Ok(None);
        };
        let handles = (foreground.0 as usize, focus.0 as usize);
        let (requests, receiver) = mpsc::channel();
        let (sender, updates) = mpsc::channel();
        let (ready, startup) = mpsc::sync_channel(1);
        thread::Builder::new()
            .name("explorer-shell".into())
            .spawn(move || {
                let connection = ExplorerConnection::connect(
                    HWND(handles.0 as *mut std::ffi::c_void),
                    HWND(handles.1 as *mut std::ffi::c_void),
                );
                match connection {
                    Ok(connection) => {
                        if ready.send(Ok(())).is_ok() {
                            Self::run(connection, receiver, sender);
                        }
                    }
                    Err(error) => {
                        let _ = ready.send(Err(error));
                    }
                }
            })
            .map_err(|error| format!("Cannot start Explorer shell worker: {error}"))?;
        startup
            .recv()
            .map_err(|error| format!("Explorer shell worker stopped during startup: {error}"))??;
        Ok(Some(Self {
            requests,
            updates,
            result: Ok(None),
            can_retry: true,
            close: false,
        }))
    }

    fn run(
        mut connection: ExplorerConnection,
        requests: Receiver<Direction>,
        updates: Sender<ExplorerUpdate>,
    ) {
        let mut previous = None;
        loop {
            let mut message = MSG::default();
            unsafe {
                while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
            // Run shell RPC only after dispatch returns, never inside a window callback.
            let result = (|| {
                loop {
                    match requests.try_recv() {
                        Ok(direction) => connection.navigate(direction)?,
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => return Ok(None),
                    }
                }
                if connection.close_requested()? && updates.send(ExplorerUpdate::Close).is_err() {
                    return Ok(None);
                }
                connection.poll().map(Some)
            })();
            let state = match result {
                Ok(None) => return,
                Ok(Some(ConnectionPoll::Pending)) => None,
                Ok(Some(ConnectionPoll::Selection(path))) => Some((Ok(path), true)),
                Err(error) => {
                    let can_retry = connection.pause_after_error(&error);
                    while requests.try_recv().is_ok() {}
                    Some((Err(error), can_retry))
                }
            };
            if let Some(state) = state {
                let can_retry = state.1;
                if previous.as_ref() != Some(&state) {
                    previous = Some(state.clone());
                    if updates
                        .send(ExplorerUpdate::State {
                            result: state.0,
                            can_retry,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
                if !can_retry {
                    return;
                }
            }
            thread::sleep(Duration::from_millis(8));
        }
    }

    fn receive(&mut self) {
        loop {
            match self.updates.try_recv() {
                Ok(ExplorerUpdate::State { result, can_retry }) => {
                    self.result = result;
                    self.can_retry = can_retry;
                }
                Ok(ExplorerUpdate::Close) => self.close = true,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if self.can_retry {
                        self.result = Err("The Explorer shell worker stopped unexpectedly.".into());
                        self.can_retry = false;
                    }
                    break;
                }
            }
        }
    }

    pub fn close_requested(&mut self) -> bool {
        self.receive();
        std::mem::take(&mut self.close)
    }

    pub fn navigate(&mut self, direction: Direction) -> Result<(), String> {
        self.receive();
        if let Err(error) = &self.result {
            return Err(error.clone());
        }
        self.requests
            .send(direction)
            .map_err(|error| format!("Cannot request Explorer navigation: {error}"))
    }

    pub fn poll(&mut self) -> ExplorerResult {
        self.receive();
        self.result.clone()
    }

    pub fn can_retry(&self) -> bool {
        self.can_retry
    }
}

fn explorer_source() -> Result<Option<(HWND, HWND)>, String> {
    std::env::var_os(SOURCE_ENV)
        .map(|source| {
            parse_source(
                source
                    .to_str()
                    .ok_or("Invalid Explorer navigation source.")?,
            )
        })
        .transpose()
}

struct ExplorerConnection {
    document: IShellFolderViewDual,
    view: IFolderView,
    layout_worker: LayoutWorker,
    layout_request: Option<LayoutRequest>,
    next_request: u64,
    directions: VecDeque<Direction>,
    foreground: HWND,
    source_view: HWND,
    next_poll: Instant,
    retry_error: Option<(Instant, String)>,
    close_keys: CloseKeys,
    _apartment: ComApartment,
}

impl ExplorerConnection {
    #[cfg(test)]
    fn from_environment() -> Result<Option<Self>, String> {
        let Some((foreground, focus)) = explorer_source()? else {
            return Ok(None);
        };
        Self::connect(foreground, focus).map(Some)
    }

    fn connect(foreground: HWND, focus: HWND) -> Result<Self, String> {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .map_err(|error| format!("Cannot initialize Explorer navigation: {error}"))?;
        let apartment = ComApartment;
        let shell: IShellWindows = unsafe { CoCreateInstance(&ShellWindows, None, CLSCTX_ALL) }
            .map_err(|error| format!("Cannot connect to Explorer: {error}"))?;
        let (document, view) = selected_view(&shell, foreground, focus)
            .map_err(|error| format!("Cannot find Explorer view: {error}"))?
            .ok_or("The originating Explorer view is no longer available.")?;
        let source_view = unsafe { view.cast::<IShellView>().and_then(|view| view.GetWindow()) }
            .map_err(|error| format!("Cannot identify the originating Explorer view: {error}"))?;
        Ok(Self {
            document,
            view,
            layout_worker: LayoutWorker::new()?,
            layout_request: None,
            next_request: 0,
            directions: VecDeque::new(),
            foreground,
            source_view,
            next_poll: Instant::now(),
            retry_error: None,
            close_keys: CloseKeys::default(),
            _apartment: apartment,
        })
    }

    pub fn close_requested(&mut self) -> Result<bool, String> {
        let pressed = self
            .close_keys
            .update(key_down(VK_SPACE), key_down(VK_ESCAPE));
        if !pressed
            || [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN]
                .into_iter()
                .any(key_down)
        {
            return Ok(false);
        }
        let source = explorer_focus()
            .map_err(|error| format!("Cannot inspect Explorer keyboard focus: {error}"))?;
        Ok(source.is_some_and(|(foreground, focus)| {
            foreground == self.foreground
                && (focus == self.source_view
                    || unsafe { IsChild(self.source_view, focus).as_bool() })
        }))
    }

    fn validate_source(&self) -> Result<(), String> {
        if unsafe {
            IsWindow(Some(self.foreground)).as_bool()
                && IsWindow(Some(self.source_view)).as_bool()
                && IsChild(self.foreground, self.source_view).as_bool()
                && IsWindowVisible(self.source_view).as_bool()
        } {
            Ok(())
        } else {
            Err(
                "Explorer navigation stopped: the source window or tab was closed or hidden."
                    .into(),
            )
        }
    }

    pub fn navigate(&mut self, direction: Direction) -> Result<(), String> {
        self.ready_to_retry()?;
        self.validate_source()?;
        self.directions.push_back(direction);
        self.next_poll = Instant::now();
        Ok(())
    }

    fn advance_navigation(&mut self) -> windows::core::Result<()> {
        while let Some(direction) = self.directions.front().copied() {
            match self.with_reconnect(|navigator| navigator.navigation_target(direction))? {
                NavigationTarget::Pending => break,
                NavigationTarget::Unchanged => {}
                NavigationTarget::Select(target) => {
                    let flags = SVSI_SELECT.0
                        | SVSI_DESELECTOTHERS.0
                        | SVSI_FOCUSED.0
                        | SVSI_ENSUREVISIBLE.0
                        | SVSI_NOTAKEFOCUS.0;
                    // Do not retry a failed selection: Explorer may already have applied it.
                    unsafe { self.view.SelectItem(target, flags as u32)? };
                }
            }
            self.directions.pop_front();
            self.layout_request = None;
            self.next_poll = Instant::now();
        }
        Ok(())
    }

    fn navigation_target(
        &mut self,
        direction: Direction,
    ) -> windows::core::Result<NavigationTarget> {
        unsafe {
            let count = self.view.ItemCount(SVGIO_ALLVIEW)?;
            if count == 0 {
                return Ok(NavigationTarget::Unchanged);
            }
            let current = self.view.GetFocusedItem()?;
            if current < 0 || current >= count {
                return Err(windows::core::Error::new(
                    windows::Win32::Foundation::E_FAIL,
                    "Explorer has no focused file to navigate from.",
                ));
            }
            let mode = self.view.GetCurrentViewMode()?;
            let target = if let Some(step) = Self::navigation_step(mode, direction) {
                (current + step).clamp(0, count - 1)
            } else {
                let window = self.view.cast::<IShellView>()?.GetWindow()?.0 as usize;
                if let Some(request) = self.layout_request
                    && request.mode == mode
                    && request.window == window
                {
                    let Some(stride) = self.layout_worker.poll(request.id)? else {
                        return Ok(NavigationTarget::Pending);
                    };
                    Self::grid_neighbor(count, current, direction, stride)
                } else {
                    let request = LayoutRequest {
                        id: self.next_request,
                        window,
                        mode,
                    };
                    self.next_request = self.next_request.wrapping_add(1);
                    self.layout_worker.request(request)?;
                    self.layout_request = Some(request);
                    return Ok(NavigationTarget::Pending);
                }
            };
            Ok(if target == current {
                NavigationTarget::Unchanged
            } else {
                NavigationTarget::Select(target)
            })
        }
    }

    fn grid_pattern(
        automation: &IUIAutomation,
        window: HWND,
    ) -> windows::core::Result<IUIAutomationGridPattern> {
        unsafe {
            let root = automation
                .ElementFromHandle(window)
                .map_err(|error| Self::layout_error("Cannot access the folder view", error))?;
            let condition = automation
                .CreatePropertyCondition(UIA_IsGridPatternAvailablePropertyId, &VARIANT::from(true))
                .map_err(|error| {
                    Self::layout_error("Cannot query the file-grid capability", error)
                })?;
            let grid = root
                .FindFirst(TreeScope_Subtree, &condition)
                .map_err(|error| Self::layout_error("Cannot find the folder's file grid", error))?;
            grid.GetCurrentPatternAs(UIA_GridPatternId)
                .map_err(|error| Self::layout_error("Cannot read the file-grid pattern", error))
        }
    }

    fn layout_error(operation: &str, error: windows::core::Error) -> windows::core::Error {
        if error.code().is_ok() {
            windows::core::Error::new(
                windows::Win32::Foundation::E_NOINTERFACE,
                format!("{operation}: Explorer returned no UI Automation element or pattern."),
            )
        } else {
            windows::core::Error::new(error.code(), format!("{operation}: {error}"))
        }
    }

    fn navigation_step(mode: u32, direction: Direction) -> Option<i32> {
        let vertical = mode == FVM_LIST.0 as u32
            || mode == FVM_DETAILS.0 as u32
            || mode == FVM_CONTENT.0 as u32;
        match direction {
            Direction::Up if vertical => Some(-1),
            Direction::Down if vertical => Some(1),
            Direction::Left if !vertical => Some(-1),
            Direction::Right if !vertical => Some(1),
            Direction::Left | Direction::Right if mode != FVM_LIST.0 as u32 => Some(0),
            _ => None,
        }
    }

    fn grid_neighbor(count: i32, current: i32, direction: Direction, stride: i32) -> i32 {
        match direction {
            Direction::Left | Direction::Up if current >= stride => current - stride,
            Direction::Right | Direction::Down if current / stride < (count - 1) / stride => {
                current.saturating_add(stride).min(count - 1)
            }
            _ => current,
        }
    }

    fn with_reconnect<T>(
        &mut self,
        mut operation: impl FnMut(&mut Self) -> windows::core::Result<T>,
    ) -> windows::core::Result<T> {
        match operation(self) {
            Err(error) if Self::is_disconnected(error.code()) => {
                drop(error);
                let shell = unsafe { CoCreateInstance(&ShellWindows, None, CLSCTX_ALL) }?;
                let (document, view) = selected_view(&shell, self.foreground, self.source_view)?
                    .ok_or_else(|| windows::core::Error::new(
                        windows::Win32::Foundation::E_FAIL,
                        "The originating Explorer view is no longer available for reconnection.",
                    ))?;
                self.document = document;
                self.view = view;
                self.layout_request = None;
                let result = operation(self);
                if result.is_ok() {
                    eprintln!("Hop: Reconnected to the originating Explorer folder view.");
                }
                result
            }
            result => result,
        }
    }

    fn is_disconnected(code: windows::core::HRESULT) -> bool {
        use windows::Win32::Foundation::{
            CO_E_OBJNOTCONNECTED, RPC_E_DISCONNECTED, RPC_E_SERVER_DIED, RPC_E_SERVER_DIED_DNE,
        };
        use windows::Win32::UI::Accessibility::UIA_E_ELEMENTNOTAVAILABLE;
        const RPC_S_SERVER_UNAVAILABLE: u32 = 1722;
        matches!(
            code,
            CO_E_OBJNOTCONNECTED | RPC_E_DISCONNECTED | RPC_E_SERVER_DIED | RPC_E_SERVER_DIED_DNE
        ) || code == windows::core::HRESULT::from_win32(RPC_S_SERVER_UNAVAILABLE)
            || code.0 as u32 == UIA_E_ELEMENTNOTAVAILABLE
    }

    fn ready_to_retry(&mut self) -> Result<(), String> {
        if let Some((retry_at, error)) = &self.retry_error
            && Instant::now() < *retry_at
        {
            return Err(error.clone());
        }
        self.retry_error = None;
        Ok(())
    }

    pub fn pause_after_error(&mut self, error: &str) -> bool {
        if self.validate_source().is_err() {
            return false;
        }
        if self.retry_error.is_none() {
            let retry_at = Instant::now() + Duration::from_millis(250);
            self.retry_error = Some((retry_at, error.to_owned()));
            self.next_poll = retry_at;
            self.directions.clear();
            self.layout_request = None;
        }
        true
    }

    fn poll(&mut self) -> Result<ConnectionPoll, String> {
        self.ready_to_retry()?;
        self.validate_source()?;
        self.advance_navigation()
            .map_err(|error| format!("Cannot navigate Explorer: {error}"))?;
        if Instant::now() < self.next_poll {
            return Ok(ConnectionPoll::Pending);
        }
        self.next_poll = Instant::now() + Duration::from_millis(32);
        let path = self
            .with_reconnect(|navigator| selected_path(&navigator.document))
            .map_err(|error| format!("Cannot read Explorer selection: {error}"))?;
        if path.as_ref().is_some_and(|path| !path.is_absolute()) {
            return Err("The selected Explorer item is not a local filesystem path.".into());
        }
        Ok(ConnectionPoll::Selection(path))
    }
}

#[derive(Default)]
struct CloseKeys {
    armed: bool,
    previous: [bool; 2],
}

impl CloseKeys {
    fn update(&mut self, space: bool, escape: bool) -> bool {
        let pressed = self.armed && ((space && !self.previous[0]) || (escape && !self.previous[1]));
        if !space && !escape {
            self.armed = true;
        }
        self.previous = [space, escape];
        pressed
    }
}

fn parse_source(source: &str) -> Result<(HWND, HWND), String> {
    let (foreground, focus) = source
        .split_once(':')
        .ok_or("Invalid Explorer navigation source.")?;
    let parse = |value: &str| {
        value
            .parse::<usize>()
            .ok()
            .filter(|handle| *handle != 0)
            .map(|handle| HWND(handle as *mut std::ffi::c_void))
            .ok_or_else(|| "Invalid Explorer navigation window handle.".to_owned())
    };
    Ok((parse(foreground)?, parse(focus)?))
}

fn launch_preview(shell: &IShellWindows) -> Result<Option<Child>, String> {
    let Some((foreground, focus)) = explorer_focus().map_err(|error| error.to_string())? else {
        return Ok(None);
    };
    let Some((document, _)) =
        selected_view(shell, foreground, focus).map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let Some(path) = selected_path(&document).map_err(|error| error.to_string())? else {
        return Ok(None);
    };
    if unsafe { GetForegroundWindow() } != foreground {
        return Ok(None);
    }
    if !path.is_absolute() {
        return Err("The selected Explorer item is not a local filesystem path.".into());
    }
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    Command::new(executable)
        .arg("--preview")
        .arg(path)
        .env(
            SOURCE_ENV,
            format!("{}:{}", foreground.0 as usize, focus.0 as usize),
        )
        .spawn()
        .map(Some)
        .map_err(|error| format!("Cannot launch preview: {error}"))
}

pub fn watch() -> Result<(), String> {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
        .ok()
        .map_err(|error| format!("Cannot initialize Windows COM: {error}"))?;
    let _apartment = ComApartment;
    let shell: IShellWindows = unsafe { CoCreateInstance(&ShellWindows, None, CLSCTX_ALL) }
        .map_err(|error| format!("Cannot connect to Explorer: {error}"))?;
    let mut tray = crate::tray::Tray::new()?;
    println!("Hop is watching Windows Explorer. Select a file and press Space.");
    println!(
        "Press Space or Escape in Explorer or the preview to close it. Ctrl+C here stops Hop."
    );
    println!("The preview stays on top. Use arrow keys in Explorer to select files.");
    let mut previous_space = key_down(VK_SPACE);
    let mut children = WatchChildren::default();
    loop {
        let mut message = MSG::default();
        unsafe {
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        let preview_was_open = children.preview.is_some();
        for (role, child) in [
            ("preview", &mut children.preview),
            ("Settings", &mut children.settings),
            ("About", &mut children.about),
        ] {
            if let Some(process) = child
                && let Some(status) = process
                    .try_wait()
                    .map_err(|error| format!("Cannot check Hop child process: {error}"))?
            {
                if !status.success() {
                    tray.report_error(&format!(
                        "The Hop {role} window (PID {}) exited unexpectedly: {status}",
                        process.id()
                    ));
                }
                *child = None;
            }
        }
        let events: Vec<_> = tray.events().collect();
        for event in events {
            use crate::tray::Event;
            let result = match event {
                Event::Exit => return children.stop(),
                Event::Settings => open_dialog(&mut children.settings, "--settings"),
                Event::About => open_dialog(&mut children.about, "--about"),
                Event::Recreate => tray.add_icon(),
                Event::Error(error) => Err(error),
            };
            if let Err(error) = result {
                tray.report_error(&error);
            }
        }
        let space = key_down(VK_SPACE);
        let modified = [VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RWIN]
            .into_iter()
            .any(key_down);
        if space && !previous_space && !modified && !preview_was_open {
            match launch_preview(&shell) {
                Ok(child) => children.preview = child,
                Err(error) => tray.report_error(&error),
            }
        }
        previous_space = space;
        thread::sleep(Duration::from_millis(12));
    }
}

#[derive(Default)]
struct WatchChildren {
    preview: Option<Child>,
    settings: Option<Child>,
    about: Option<Child>,
}

impl WatchChildren {
    fn stop(&mut self) -> Result<(), String> {
        let mut errors = Vec::new();
        for child in [&mut self.preview, &mut self.settings, &mut self.about] {
            if let Some(process) = child {
                let result = (|| {
                    if process.try_wait()?.is_none() {
                        process.kill()?;
                        process.wait()?;
                    }
                    Ok::<(), std::io::Error>(())
                })();
                match result {
                    Ok(()) => *child = None,
                    Err(error) => errors.push(format!(
                        "Cannot close Hop window (PID {}): {error}",
                        process.id()
                    )),
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("\n"))
        }
    }
}

impl Drop for WatchChildren {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            eprintln!("Hop: {error}");
            crate::tray::show_error(&error);
        }
    }
}

fn open_dialog(child: &mut Option<Child>, argument: &str) -> Result<(), String> {
    if let Some(process) = child {
        return activate_process_window(process.id());
    }
    *child = Some(
        Command::new(std::env::current_exe().map_err(|error| error.to_string())?)
            .arg(argument)
            .env_remove(SOURCE_ENV)
            .spawn()
            .map_err(|error| format!("Cannot open {argument}: {error}"))?,
    );
    Ok(())
}

fn activate_process_window(process: u32) -> Result<(), String> {
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, SW_RESTORE, SetForegroundWindow, ShowWindow,
    };
    struct Search {
        process: u32,
        window: HWND,
    }
    unsafe extern "system" fn find(window: HWND, parameter: LPARAM) -> windows::core::BOOL {
        unsafe {
            let search = &mut *(parameter.0 as *mut Search);
            let mut process = 0;
            GetWindowThreadProcessId(window, Some(&mut process));
            if process == search.process
                && search.window.is_invalid()
                && IsWindowVisible(window).as_bool()
            {
                search.window = window;
            }
            windows::core::BOOL(1)
        }
    }
    let mut search = Search {
        process,
        window: HWND::default(),
    };
    unsafe {
        EnumWindows(Some(find), LPARAM((&mut search as *mut Search) as isize))
            .map_err(|error| format!("Cannot find the existing Hop window: {error}"))?;
        if !search.window.is_invalid() {
            let _ = ShowWindow(search.window, SW_RESTORE);
            if !SetForegroundWindow(search.window).as_bool() {
                return Err("Windows could not focus the existing Hop window.".into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::Shell::FVM_ICON;
    use windows::Win32::UI::WindowsAndMessaging::WM_KEYDOWN;

    fn navigation_proxy() -> (
        ExplorerNavigator,
        Receiver<Direction>,
        Sender<ExplorerUpdate>,
    ) {
        let (requests, receiver) = mpsc::channel();
        let (sender, updates) = mpsc::channel();
        (
            ExplorerNavigator {
                requests,
                updates,
                result: Ok(None),
                can_retry: true,
                close: false,
            },
            receiver,
            sender,
        )
    }

    #[test]
    fn navigation_proxy_preserves_errors_until_worker_reports_recovery() {
        let (mut navigator, requests, updates) = navigation_proxy();
        updates
            .send(ExplorerUpdate::State {
                result: Err("Explorer is refreshing.".into()),
                can_retry: true,
            })
            .unwrap();
        for _ in 0..3 {
            assert_eq!(navigator.poll().unwrap_err(), "Explorer is refreshing.");
            assert!(navigator.can_retry());
            assert!(navigator.navigate(Direction::Down).is_err());
            assert!(matches!(requests.try_recv(), Err(TryRecvError::Empty)));
        }
        let path = PathBuf::from(r"C:\fixture\02.txt");
        updates
            .send(ExplorerUpdate::State {
                result: Ok(Some(path.clone())),
                can_retry: true,
            })
            .unwrap();
        navigator.navigate(Direction::Right).unwrap();
        assert!(matches!(requests.try_recv(), Ok(Direction::Right)));
        assert_eq!(navigator.poll().unwrap(), Some(path));
        updates.send(ExplorerUpdate::Close).unwrap();
        assert!(navigator.close_requested());
        assert!(!navigator.close_requested());
        updates
            .send(ExplorerUpdate::State {
                result: Err("The source tab closed.".into()),
                can_retry: false,
            })
            .unwrap();
        drop(updates);
        assert_eq!(navigator.poll().unwrap_err(), "The source tab closed.");
        assert!(!navigator.can_retry());
    }

    #[test]
    fn navigation_proxy_reports_unexpected_worker_shutdown() {
        let (mut navigator, _requests, updates) = navigation_proxy();
        drop(updates);
        assert!(
            navigator
                .poll()
                .unwrap_err()
                .contains("worker stopped unexpectedly")
        );
        assert!(!navigator.can_retry());
        assert!(navigator.navigate(Direction::Down).is_err());
    }

    struct SyncProbe {
        navigator: ExplorerNavigator,
        baseline: Option<IShellFolderViewDual>,
        baseline_error: Option<windows::core::HRESULT>,
        flags: u32,
        navigation: Option<Result<(), String>>,
        selection: Option<ExplorerResult>,
    }

    unsafe extern "system" fn sync_probe_proc(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        use windows::Win32::UI::WindowsAndMessaging::*;
        unsafe {
            if message == WM_NCCREATE {
                let create = &*(lparam.0 as *const CREATESTRUCTW);
                SetWindowLongPtrW(window, GWLP_USERDATA, create.lpCreateParams as isize);
            }
            if message == WM_APP + 42 {
                let probe = &mut *(GetWindowLongPtrW(window, GWLP_USERDATA) as *mut SyncProbe);
                probe.flags = InSendMessageEx(None);
                if let Some(document) = &probe.baseline {
                    probe.baseline_error = selected_path(document).err().map(|error| error.code());
                }
                probe.navigation = Some(probe.navigator.navigate(Direction::Down));
                probe.selection = Some(probe.navigator.poll());
                return LRESULT(1);
            }
            DefWindowProcW(window, message, wparam, lparam)
        }
    }

    fn synchronous_probe(
        navigator: ExplorerNavigator,
        baseline: Option<IShellFolderViewDual>,
    ) -> SyncProbe {
        use windows::Win32::Foundation::HINSTANCE;
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::UI::WindowsAndMessaging::*;
        use windows::core::{HSTRING, w};
        struct ProbeWindow {
            window: HWND,
            class: HSTRING,
            instance: HINSTANCE,
        }
        impl Drop for ProbeWindow {
            fn drop(&mut self) {
                unsafe {
                    if let Err(error) = DestroyWindow(self.window) {
                        eprintln!("Cannot destroy test callback window: {error}");
                    }
                    if let Err(error) = UnregisterClassW(&self.class, Some(self.instance)) {
                        eprintln!("Cannot unregister test callback window: {error}");
                    }
                }
            }
        }
        let mut probe = Box::new(SyncProbe {
            navigator,
            baseline,
            baseline_error: None,
            flags: 0,
            navigation: None,
            selection: None,
        });
        {
            let instance: HINSTANCE = unsafe { GetModuleHandleW(None) }.unwrap().into();
            let class = HSTRING::from(format!("Hop.Test.Sync.{:p}", &*probe));
            let registration = WNDCLASSW {
                hInstance: instance,
                lpszClassName: windows::core::PCWSTR(class.as_ptr()),
                lpfnWndProc: Some(sync_probe_proc),
                ..Default::default()
            };
            assert_ne!(unsafe { RegisterClassW(&registration) }, 0);
            let window = unsafe {
                CreateWindowExW(
                    WS_EX_TOOLWINDOW,
                    &class,
                    w!("Hop synchronous callback test"),
                    WS_OVERLAPPED,
                    0,
                    0,
                    0,
                    0,
                    None,
                    None,
                    Some(instance),
                    Some((&mut *probe as *mut SyncProbe).cast()),
                )
            }
            .unwrap();
            let window = ProbeWindow {
                window,
                class,
                instance,
            };
            let handle = window.window.0 as usize;
            let sender = thread::spawn(move || unsafe {
                SendMessageTimeoutW(
                    HWND(handle as *mut std::ffi::c_void),
                    WM_APP + 42,
                    WPARAM(0),
                    LPARAM(0),
                    SMTO_ABORTIFHUNG | SMTO_BLOCK,
                    3000,
                    None,
                )
                .0
            });
            let deadline = Instant::now() + Duration::from_secs(5);
            while !sender.is_finished() && Instant::now() < deadline {
                let mut message = MSG::default();
                unsafe {
                    while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                        let _ = TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                }
                thread::sleep(Duration::from_millis(1));
            }
            assert!(sender.is_finished(), "Synchronous callback did not finish.");
            assert_ne!(
                sender.join().unwrap(),
                0,
                "Sending the synchronous callback timed out."
            );
        }
        *probe
    }

    #[test]
    fn navigation_proxy_is_nonblocking_in_a_real_synchronous_window_callback() {
        use windows::Win32::UI::WindowsAndMessaging::ISMEX_SEND;
        fn assert_send<T: Send>() {}
        assert_send::<ExplorerNavigator>();
        let (navigator, requests, _updates) = navigation_proxy();
        let probe = synchronous_probe(navigator, None);
        assert_ne!(probe.flags & ISMEX_SEND, 0);
        assert!(probe.navigation.unwrap().is_ok());
        assert!(probe.selection.unwrap().is_ok());
        assert!(matches!(requests.try_recv(), Ok(Direction::Down)));
    }

    #[test]
    #[ignore = "requires HOP_EXPLORER_SOURCE pointing to a test-owned Details view with multiple files"]
    fn connected_explorer_avoids_input_synchronous_com_rejection() {
        use windows::Win32::Foundation::RPC_E_CANTCALLOUT_ININPUTSYNCCALL;
        use windows::Win32::UI::WindowsAndMessaging::ISMEX_SEND;
        let connection = ExplorerConnection::from_environment().unwrap().unwrap();
        assert_eq!(
            unsafe { connection.view.GetCurrentViewMode() }.unwrap(),
            FVM_DETAILS.0 as u32
        );
        let current = unsafe { connection.view.GetFocusedItem() }.unwrap();
        let count = unsafe { connection.view.ItemCount(SVGIO_ALLVIEW) }.unwrap();
        assert!(current >= 0 && current + 1 < count);
        let navigator = ExplorerNavigator::from_environment().unwrap().unwrap();
        let mut probe = synchronous_probe(navigator, Some(connection.document.clone()));
        assert_ne!(probe.flags & ISMEX_SEND, 0);
        assert_eq!(
            probe.baseline_error,
            Some(RPC_E_CANTCALLOUT_ININPUTSYNCCALL)
        );
        assert!(probe.navigation.take().unwrap().is_ok());
        assert!(probe.selection.take().unwrap().is_ok());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let path = probe.navigator.poll().unwrap();
            if unsafe { connection.view.GetFocusedItem() }.unwrap() == current + 1
                && path == selected_path(&connection.document).unwrap()
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Deferred Explorer navigation did not complete."
            );
            thread::sleep(Duration::from_millis(8));
        }
    }

    #[test]
    fn only_disconnected_connections_trigger_rebinding() {
        use windows::Win32::Foundation::{E_ACCESSDENIED, E_FAIL, E_INVALIDARG, S_FALSE, S_OK};
        for code in [
            0x800401fd_u32,
            0x80010108,
            0x80010007,
            0x80010012,
            0x800706ba,
            0x80040201,
        ] {
            assert!(ExplorerConnection::is_disconnected(windows::core::HRESULT(
                code as i32
            )));
        }
        for code in [E_ACCESSDENIED, E_FAIL, E_INVALIDARG, S_FALSE, S_OK] {
            assert!(!ExplorerConnection::is_disconnected(code));
        }
    }

    #[test]
    #[ignore = "requires HOP_EXPLORER_SOURCE pointing to a test-owned visible Explorer view"]
    fn connected_explorer_rebinds_read_operations_once_to_the_same_view() {
        use windows::Win32::Foundation::{CO_E_OBJNOTCONNECTED, E_ACCESSDENIED};
        let mut navigator = ExplorerConnection::from_environment().unwrap().unwrap();
        let source = navigator.source_view;
        let expected = selected_path(&navigator.document).unwrap();
        let mut calls = 0;
        let path = navigator
            .with_reconnect(|navigator| {
                calls += 1;
                if calls == 1 {
                    return Err(windows::core::Error::from_hresult(CO_E_OBJNOTCONNECTED));
                }
                selected_path(&navigator.document)
            })
            .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(path, expected);
        assert_eq!(navigator.source_view, source);
        let mut calls = 0;
        let error = navigator
            .with_reconnect::<()>(|_| {
                calls += 1;
                Err(windows::core::Error::from_hresult(CO_E_OBJNOTCONNECTED))
            })
            .unwrap_err();
        assert_eq!(calls, 2);
        assert_eq!(error.code(), CO_E_OBJNOTCONNECTED);
        let mut calls = 0;
        let error = navigator
            .with_reconnect::<()>(|_| {
                calls += 1;
                Err(windows::core::Error::from_hresult(E_ACCESSDENIED))
            })
            .unwrap_err();
        assert_eq!(calls, 1);
        assert_eq!(error.code(), E_ACCESSDENIED);
    }

    #[test]
    #[ignore = "requires HOP_EXPLORER_SOURCE pointing to a test-owned visible Explorer view"]
    fn connected_explorer_pauses_without_extending_backoff_or_replaying_failed_input() {
        let mut navigator = ExplorerConnection::from_environment().unwrap().unwrap();
        let expected = selected_path(&navigator.document).unwrap();
        navigator.navigate(Direction::Down).unwrap();
        assert_eq!(navigator.directions.len(), 1);
        let failure = "Explorer is temporarily disconnected.";
        assert!(navigator.pause_after_error(failure));
        assert!(navigator.directions.is_empty());
        assert!(navigator.layout_request.is_none());
        let retry_at = navigator.retry_error.as_ref().unwrap().0;
        for _ in 0..4 {
            assert_eq!(navigator.navigate(Direction::Down).unwrap_err(), failure);
            assert_eq!(navigator.poll().unwrap_err(), failure);
            assert!(navigator.pause_after_error(failure));
            assert_eq!(navigator.retry_error.as_ref().unwrap().0, retry_at);
            assert!(navigator.directions.is_empty());
        }
        navigator.retry_error.as_mut().unwrap().0 = Instant::now();
        navigator.next_poll = Instant::now();
        assert_eq!(
            navigator.poll().unwrap(),
            ConnectionPoll::Selection(expected)
        );
        assert!(navigator.retry_error.is_none());
    }

    #[test]
    fn layout_responses_ignore_superseded_requests_and_preserve_errors() {
        use windows::Win32::Foundation::E_ACCESSDENIED;
        let (requests, _receiver) = mpsc::channel();
        let (sender, responses) = mpsc::channel();
        let worker = LayoutWorker {
            requests,
            responses,
        };
        sender
            .send(LayoutResponse {
                id: 1,
                result: Ok(3),
            })
            .unwrap();
        assert!(worker.poll(2).unwrap().is_none());
        sender
            .send(LayoutResponse {
                id: 2,
                result: Err((E_ACCESSDENIED, "Cannot access the test file grid.".into())),
            })
            .unwrap();
        let error = worker.poll(2).unwrap_err();
        assert_eq!(error.code(), E_ACCESSDENIED);
        assert!(
            error
                .to_string()
                .contains("Cannot access the test file grid")
        );
        sender
            .send(LayoutResponse {
                id: 3,
                result: Ok(5),
            })
            .unwrap();
        assert_eq!(worker.poll(3).unwrap(), Some(5));
        drop(sender);
        assert!(
            worker
                .poll(4)
                .unwrap_err()
                .to_string()
                .contains("stopped unexpectedly")
        );
    }

    #[test]
    fn layout_worker_releases_com_before_plain_error_data_is_consumed() {
        fn assert_send<T: Send>() {}
        assert_send::<LayoutRequest>();
        assert_send::<LayoutResponse>();
        for id in 0..4 {
            let LayoutWorker {
                requests,
                responses,
            } = LayoutWorker::new().unwrap();
            requests
                .send(LayoutRequest {
                    id,
                    window: usize::MAX,
                    mode: FVM_ICON.0 as u32,
                })
                .unwrap();
            let response = responses.recv_timeout(Duration::from_secs(10)).unwrap();
            drop(requests);
            assert!(matches!(
                responses.recv_timeout(Duration::from_secs(10)),
                Err(mpsc::RecvTimeoutError::Disconnected)
            ));
            let (code, message) = response.result.unwrap_err();
            assert_eq!(response.id, id);
            assert!(code.is_err());
            assert!(
                message.contains("Cannot access the folder view"),
                "{message}"
            );
        }
    }

    #[test]
    fn missing_automation_objects_report_an_error_instead_of_success() {
        use windows::Win32::Foundation::{E_NOINTERFACE, S_FALSE, S_OK};
        for code in [S_OK, S_FALSE] {
            let error = ExplorerConnection::layout_error(
                "Cannot find the folder's file grid",
                windows::core::Error::from_hresult(code),
            );
            assert_eq!(error.code(), E_NOINTERFACE);
            let message = error.to_string();
            assert!(message.contains("Cannot find the folder's file grid"));
            assert!(message.contains("no UI Automation element or pattern"));
            assert!(!message.contains("completed successfully"));
        }
    }

    #[test]
    fn automation_failures_preserve_the_original_code_and_operation() {
        use windows::Win32::Foundation::E_ACCESSDENIED;
        let error = ExplorerConnection::layout_error(
            "Cannot access the folder view",
            windows::core::Error::from_hresult(E_ACCESSDENIED),
        );
        assert_eq!(error.code(), E_ACCESSDENIED);
        assert!(error.to_string().contains("Cannot access the folder view"));
    }

    #[test]
    fn navigation_follows_the_view_order_and_layout() {
        use Direction::*;
        for mode in [FVM_DETAILS, FVM_CONTENT, FVM_LIST] {
            assert_eq!(
                ExplorerConnection::navigation_step(mode.0 as u32, Up),
                Some(-1)
            );
            assert_eq!(
                ExplorerConnection::navigation_step(mode.0 as u32, Down),
                Some(1)
            );
        }
        for direction in [Left, Right] {
            assert_eq!(
                ExplorerConnection::navigation_step(FVM_DETAILS.0 as u32, direction),
                Some(0)
            );
            assert_eq!(
                ExplorerConnection::navigation_step(FVM_LIST.0 as u32, direction),
                None
            );
        }
        assert_eq!(
            ExplorerConnection::navigation_step(FVM_ICON.0 as u32, Left),
            Some(-1)
        );
        assert_eq!(
            ExplorerConnection::navigation_step(FVM_ICON.0 as u32, Right),
            Some(1)
        );
        for direction in [Up, Down] {
            assert_eq!(
                ExplorerConnection::navigation_step(FVM_ICON.0 as u32, direction),
                None
            );
        }
    }

    #[test]
    fn grid_navigation_handles_partial_rows_columns_and_edges() {
        use Direction::*;
        for (current, direction, expected) in [
            (1, Up, 1),
            (1, Down, 4),
            (4, Up, 1),
            (4, Down, 4),
            (2, Down, 4),
            (0, Up, 0),
        ] {
            assert_eq!(
                ExplorerConnection::grid_neighbor(5, current, direction, 3),
                expected
            );
        }
        assert_eq!(ExplorerConnection::grid_neighbor(5, 0, Right, 2), 2);
        assert_eq!(ExplorerConnection::grid_neighbor(5, 2, Left, 2), 0);
        assert_eq!(ExplorerConnection::grid_neighbor(5, 3, Right, 2), 4);
        assert_eq!(ExplorerConnection::grid_neighbor(5, 4, Right, 2), 4);
        assert_eq!(ExplorerConnection::grid_neighbor(1, 0, Down, 15), 0);
    }

    #[test]
    fn outside_click_monitor_runs_and_stops_independently_of_the_caller() {
        let mut monitor = OutsideClickMonitor::start(1).unwrap();
        let worker = monitor.worker.take().unwrap();
        assert_ne!(monitor.thread_id, unsafe { GetCurrentThreadId() });
        assert_eq!(CLICK_TARGET.get(), 0);
        assert!(OUTSIDE_CLICKED.with_borrow(|state| state.is_none()));
        assert!(!worker.is_finished());

        assert!(!monitor.take_close_request());
        let clicked = Arc::clone(&monitor.clicked);
        thread::spawn(move || clicked.store(true, Ordering::Relaxed))
            .join()
            .unwrap();
        assert!(monitor.take_close_request());
        assert!(!monitor.take_close_request());

        drop(monitor);
        worker.join().unwrap();
    }

    #[test]
    fn outside_click_monitors_have_independent_lifetimes() {
        let first = OutsideClickMonitor::start(1).unwrap();
        let second = OutsideClickMonitor::start(2).unwrap();
        assert_ne!(first.thread_id, second.thread_id);
        drop(first);
        assert!(!second.worker.as_ref().unwrap().is_finished());
        assert!(!second.take_close_request());
        drop(second);
    }

    #[test]
    fn outside_click_monitor_only_considers_button_down_events() {
        for message in [
            WM_LBUTTONDOWN,
            WM_RBUTTONDOWN,
            WM_MBUTTONDOWN,
            WM_XBUTTONDOWN,
        ] {
            assert!(is_mouse_press(message as usize));
        }
        for message in [0x0200, 0x0202, 0x0205, 0x0208, 0x020a, WM_KEYDOWN] {
            assert!(!is_mouse_press(message as usize));
        }
    }

    #[test]
    fn explorer_close_keys_require_release_and_ignore_holds() {
        let mut keys = CloseKeys::default();
        assert!(!keys.update(true, false));
        assert!(!keys.update(true, false));
        assert!(!keys.update(false, false));
        assert!(keys.update(true, false));
        assert!(!keys.update(true, false));
        assert!(!keys.update(false, false));
        assert!(keys.update(false, true));
        assert!(!keys.update(false, true));
    }

    #[test]
    fn validates_explorer_source_handles() {
        let (foreground, focus) = parse_source("123:456").unwrap();
        assert_eq!(foreground.0 as usize, 123);
        assert_eq!(focus.0 as usize, 456);
        for source in ["", "123", "0:1", "1:0", "-1:2", "1:two", "1:2:3"] {
            assert!(parse_source(source).is_err(), "{source}");
        }
    }
}
