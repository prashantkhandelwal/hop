# Hop

<img src="assets/hop.svg" alt="Hop preview window and spacebar logo" width="96" height="96">

A Rust quick-preview app for Windows and Linux. Preview images, UTF-8 text/source
files, folders, and file metadata without opening the file's normal application.

## Build

Install a current stable Rust toolchain (Rust 1.93 or later recommended).

```text
cargo build --release
```

The release binary is `target\release\hop.exe` on Windows and `target/release/hop`
on Linux. Start the Windows watcher with `.\target\release\hop.exe --watch`,
or double-click the Windows release executable. Release builds do not create
a console window; commands launched from a terminal can still print there.

Windows requires the MSVC C++ build tools. On Linux, a graphical X11 or Wayland
session and OpenGL/EGL drivers are required. On Debian/Ubuntu, build prerequisites
typically include `build-essential`, `pkg-config`, `libx11-dev`,
`libxi-dev`, `libxcursor-dev`, `libxrandr-dev`, `libxkbcommon-dev`,
`libwayland-dev`, and `libegl1-mesa-dev`.

## Windows Explorer: Space to preview

```powershell
cargo run --release -- --watch
```

Running Hop without arguments on Windows also starts the watcher and adds an
**Hop preview-window/spacebar** icon to the Windows notification area (possibly in the hidden-icons
overflow). Leave it running, select a file in an Explorer file-list pane,
and press **Space**.
The preview stays **always on top** and opens **without taking focus from Explorer**.
Press **Space** or **Escape** in Explorer's file list or in the preview to close it.
Clicking outside the preview window also closes it, including clicking a file
in Explorer. Clicks inside the preview, its title bar, or playback controls
keep it open. Dragging a video slider outside the window does not dismiss it
unless you start a new click outside.
Right-click the tray icon for:

- **Settings**: configure default video volume (0-100%) and controls auto-hide
  delay (0.5-30 seconds), and optionally enable **Start Hop when I sign in to
  Windows**. Save applies playback changes to previews opened afterward; reopen
  an existing preview to use them. Startup changes are registered when you Save.
- **About**: show Hop's version and a short description.
- **Exit**: remove the tray icon, stop the watcher, and close the preview,
  Settings, and About windows opened by that watcher.

The icon is restored if Windows Explorer restarts. Watcher errors are reported
through tray notifications, with a message box if notification delivery fails.
When launched from a terminal, **Ctrl+C** is also available to stop the watcher;
use tray **Exit** to close its managed windows as well.

Settings and About are also available as `hop --settings` and `hop --about`.
Linux remains CLI-launched, without a tray watcher. Settings are stored as JSON
in `%APPDATA%\hop\settings.json` on Windows and
`$XDG_CONFIG_HOME/hop/settings.json` (or `~/.config/hop/settings.json`) on Linux.
Missing settings use the existing defaults of 70% volume and a two-second
auto-hide delay. Invalid settings produce an explicit error; open Settings to
reset them, then Save. Saving replaces the file atomically.

Windows startup is opt-in and applies only to your Windows account. The checkbox
reads the current `Hop` entry under
`HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run`, rather than
duplicating that state in the JSON settings. Saving it checked registers the
current executable with `--watch`, so Hop starts in the tray at your next sign-in.
Saving it unchecked removes that entry but does not stop an already-running
watcher. Cancel leaves startup unchanged. **Restore defaults** unchecks startup;
Save is still required. Keep the executable at its current location, or reopen
Settings from its new location and Save to update the startup command.
Registration errors remain visible in Settings; startup-read errors offer Retry
or **Disable startup on Save**. Linux startup integration is not included.

- Keep using **Left**, **Right**, **Up**, or **Down** in Explorer to select files.
  The same preview
  window updates to the newly selected file, following Explorer's sort order
  and list/grid layout. Holding an arrow key repeats navigation.
- Use Explorer's keyboard selection to browse while keeping the preview open;
  clicking Explorer now dismisses it. If you click the preview to focus it,
  unmodified arrow keys there always move Explorer's selection, even after
  clicking preview content, Play/Pause, or a playback slider. The background
  Explorer selection and the preview stay synchronized without moving keyboard
  focus away from the preview. Holding an arrow key continues updating both.
  Navigation uses Explorer's displayed item order and live grid dimensions.
  The file grid is located inside the originating folder view, independently of
  which nested Explorer control last had keyboard focus. Missing automation
  support is reported as an explicit navigation error.
  The selected item is also focused and scrolled into view without activating
  Explorer. In Details/Content views, Up/Down move between files; Left/Right
  do not change the selected file.
  Playback sliders remain usable with the mouse. Standalone previews retain
  normal slider keyboard behavior because they are not linked to Explorer.
- Space/Escape closing from Explorer applies only to the originating file list,
  not search/address/rename fields or other applications.
- Closing or hiding the source Explorer tab stops navigation and shows a
  notification in the preview; the last preview remains open.
- Entering a folder or navigating back within the same Explorer tab keeps the
  preview connected to that tab and follows its new selection. Hop allows a
  one-second transition while Explorer makes its replacement view visible;
  a view that remains hidden stops navigation.
- If Explorer rebuilds its folder view, Hop reconnects to the same visible
  source tab instead of permanently disabling navigation. Persistent failures
  remain visible and are retried with a short delay; warnings clear on recovery.
  Pending arrow presses are discarded after a failure to avoid unexpected
  movement later. Hop never reconnects to another tab or retries a selection
  change whose outcome is uncertain.
- Explorer selection reads, arrow navigation, reconnection, and shell-interface
  cleanup run on a dedicated STA worker with its own message pump, outside
  window callbacks. The preview exchanges only commands, paths, and status
  messages with it; synchronous redraws and tray/menu messages cannot make
  Explorer COM calls from the preview's GUI thread.
  Folder-layout queries use a separate MTA worker. UI Automation objects stay
  on that thread throughout their lifetime.
- Only unmodified Space in an Explorer file-list control triggers a preview.
  Search, address, and rename fields are excluded.
- The first selected item is previewed when multiple items are selected.
- Only one preview is opened by each watcher at a time. Run only one watcher.
- The watcher observes keys; it does not suppress Explorer's own Space behavior.
- Explorer tabs are matched against the focused shell view. Desktop icons,
  file-open dialogs, other file managers, and virtual shell items are not supported.
- Failures are printed to the watcher's terminal; file-loading errors also appear
  in the preview window.
- No administrator permissions, shell modifications, or global hotkey
  registration are needed. Windows sign-in startup is optional.
- Outside-click detection observes mouse presses without blocking or consuming
  them, so the destination application still receives its click. Merely moving
  the mouse, scrolling, or keeping keyboard focus in Explorer does not dismiss
  the preview.
  The Windows mouse hook runs on a dedicated message-pump thread, so software
  video rendering cannot hold up global mouse movement.

## Direct preview: Windows and Linux

```text
cargo run --release -- --preview "path to a file"
```

The installed binary accepts `hop <path>` or `hop --preview <path>`.
Use `hop -- <path>` if a filename starts with `-`. Paths are passed directly,
not interpreted by a shell. Close with Space, Escape, or the window close button.
Standalone previews also stay on top, but take focus when opened.
On Windows, standalone previews use the same outside-click dismissal.
On Linux, previews close when they lose focus after initial activation (which
also includes switching applications with the keyboard); Wayland does not
permit arbitrary global mouse monitoring.
On Linux, always-on-top behavior depends on the window manager/compositor.
Explorer arrow navigation is available only for previews opened by the Windows
watcher; standalone previews and the Linux CLI do not control a file manager.

**Linux integration in this version is CLI-only.** There is no automatic global
Space listener on Linux. A file manager custom action can invoke `hop` with
its selected local path; file-manager extensions and Wayland desktop shortcuts
are intentionally outside this version's scope.

## Preview support and limits

| Type | Behavior |
| --- | --- |
| Images | PNG, JPEG, BMP, ICO, TIFF/TIF (first page), WebP, PNM, TGA, QOI |
| Animated GIF | Native Rust animation with frame timing and looping |
| Video with audio | In-pane autoplay and looping using libmpv; MP4, MKV, WebM, MOV, AVI, WMV, MPEG, M4V, TS/MTS/M2TS, OGV, FLV, 3GP |
| Text/source | UTF-8 text, up to 256 KiB, selectable read-only content |
| Folders | Centered side-by-side group with a folder icon and name on the left and scrollable contents on the right; up to 200 entries sorted within the displayed subset |
| Other files | Centered file-type icon with the file name underneath, followed by the human-readable file size |

The preview pane has a light grey background. The preview content uses the
entire client area without a header, file details,
dimensions, or keyboard hints. Images are centered and scaled to fit while
preserving their aspect ratio. The native title bar and window controls remain.
Text/source previews have 16-point padding on all sides, including while scrolling.
While content loads, an animated loading spinner appears in the center of the pane.
Loading errors and navigation failures are still displayed when needed;
truncation notices are written to the terminal rather than occupying the pane.

Images are limited to 64 MiB on disk, dimensions of 16,384 pixels per axis, and a
256 MiB decoder allocation budget. Still-image textures are reduced to at most
4,096 pixels per axis. GIFs are bounded to 500 frames, 128 MiB of retained
frame data, and 2,048 pixels per axis; exceeding a limit shows an explicit
error rather than silently dropping frames. GIF delays below 20 ms are clamped.
Images are decoded and converted to texture-ready pixels on a worker thread,
which wakes the UI as soon as a preview result is ready. Texture upload remains
on the UI thread. Unsupported files show only a centered
file-type icon, file name, and size (B, KiB, MiB, etc.), without an unsupported-format
message or extra metadata. Names below folder and file-type icons wrap to at most
three lines; hover over the name to see it in full. Windows uses the registered
shell file-type icon; Linux and unavailable Windows associations use a generic
document icon with an extension
badge. Icon lookup failures are logged to the terminal.

PDFs, office documents, archives, executables, standalone audio, and other known
binary formats use this fallback even when the file happens to contain ASCII.
Unknown binary/non-UTF-8 files and oversized images also use it. Plain-text
files with unrecognized extensions can still be previewed as text.
Missing/unreadable files and decoding errors in supported formats continue to
show an explicit error rather than being disguised as an unsupported type.

## Video runtime

Video requires **libmpv client API 2** with the software render API (a current
mpv build is recommended). Installing just `mpv.exe` is not sufficient.
Image and GIF previews do not need libmpv.

- **Windows (x64):** obtain `libmpv-2.dll` from a trusted libmpv development
  build, such as the `mpv-dev-x86_64` archives in
  [mpv-winbuild-cmake releases](https://github.com/shinchiro/mpv-winbuild-cmake/releases).
  Place it beside the executable: `target\release\libmpv-2.dll` for a release
  build, or `target\debug\libmpv-2.dll` for `cargo run`.
- **Linux:** install the distribution's libmpv runtime package, for example
  `sudo apt install libmpv2` on current Debian/Ubuntu, or the `mpv` package
  on Arch Linux. The loader looks for `libmpv.so.2` or `libmpv.so`.
- Alternatively, set **`HOP_MPV_LIBRARY`** to the full path of a trusted
  library. This overrides automatic discovery and errors visibly if invalid.
  Only load libraries from trusted sources; they execute native code.

Videos autoplay with audio at the configured volume (70% by default) and loop
until you close the preview or select another file. Navigating away destroys
the player and stops audio.
Video stays in the same always-on-top pane; it does not open an
external player. Space/Escape and Explorer navigation retain their existing
behavior. A compact, translucent control overlay appears **only for video**,
centered along the bottom of the preview pane with a 12-point inset, without
reducing the video area. It appears
when a video opens or you move the mouse, and hides after the configured
inactivity delay (two seconds by default). Clicking, using the keyboard, or
scrolling reveals it again.
Controls stay visible while a mouse button is held, so seeking and volume
drags are not interrupted.

- **Play / Pause** is an icon button centered in the control row: a play
  triangle when paused and two pause bars while playing. Hover for its action
  label. Clicking toggles playback without closing the preview.
- **Timeline** shows elapsed/total time. Click to seek, or drag and release
  to seek to a precise position. Unknown-duration/non-seekable media disables
  the timeline until seeking is available.
- **Volume** ranges from 0% (silent) to 100%, starting at the saved default
  for each file.

Controls follow the real player state. Seeking while paused keeps playback
paused. Playback/seek/volume errors are shown without hiding the current frame.
Images and GIFs retain their full-pane layout without playback controls.

Codec support depends on the installed libmpv/FFmpeg build. Frames are rendered
in software into an aspect-preserving surface capped at 1,920 x 1,080, then
displayed in Hop's existing renderer. This works without native child-window
embedding on Windows, X11, and Wayland, but is more CPU-intensive than
hardware-accelerated playback. A missing/incompatible library, failed codec,
or audio output error appears in the pane and terminal. Native mpv config
files and user scripts are disabled.

libmpv is loaded dynamically, not linked into Hop. If distributing a runtime
alongside Hop, comply with that build's license and its bundled codecs.

Hop reads files but never executes them or renders embedded scripts. Previews
are snapshots, not live views. Network/cloud-backed paths may take longer to
load or cause the operating system to download their contents. Errors decoding
an otherwise supported image are shown explicitly rather than hidden.

## Development

The application logo combines a preview window, an H monogram, and a spacebar
key. It is embedded in the Windows executable and tray and used by the preview,
Settings, and About windows. No separate icon files are needed beside Hop.
The editable source is [assets/hop.svg](assets/hop.svg); [assets/hop.png](assets/hop.png)
is its transparent 512-pixel export. After updating the SVG, export it to that PNG
at 512 x 512, then run `.\assets\generate-ico.ps1` on Windows to regenerate the
multi-resolution [assets/hop.ico](assets/hop.ico) before building.

```text
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
```

Tests cover argument handling, bounded text and directory previews, UTF-8
boundaries, binary detection, image decoding, missing files, and image size
limits. Explorer keyboard/focus behavior and Linux display-server behavior
also need manual testing on the corresponding desktop.
Navigation tests cover arrow filtering, source-handle validation, stale load
results, error recovery, and preview reloads.
Media tests cover PNG/BMP/TIFF decoding, GIF frame timing/colors/looping and
memory limits, texture updates, and bounded video render sizes.

A separate integration test requires a local video **with an audio track**
and an installed libmpv runtime. In PowerShell:

```powershell
$env:HOP_MPV_LIBRARY = 'G:\path\to\libmpv-2.dll'
$env:HOP_TEST_VIDEO = 'G:\path\to\sample.mp4'
cargo test media::tests::decodes_real_video_and_releases_player -- --ignored
cargo test ui::tests::navigating_away_releases_active_video_and_audio -- --ignored
```

These check changing decoded frames, active audio output, play/pause, seeking,
volume, releasing/reopening the player, and stopping playback on navigation.
Normal `cargo test` skips
these environment-dependent tests.

On an interactive Windows desktop, this separate Explorer regression test
opens its own temporary folder window, checks same-tab folder transitions,
selection tracking, stale-navigation cleanup, and hidden-view handling, then
closes that window:

```text
cargo test entering_folder_tracks_the_same_tab_and_releases_stale_navigation -- --ignored
```
