//! macOS: process details from `libproc`, `$TMPDIR` for the agent socket.
//!
//! On purpose, keep it:
//! - soi's own files follow XDG here too (`crate::dirs`): `~/.config/soi`,
//!   `~/.local/state/soi`, not `~/Library/Application Support`.
//! - The agent socket lives in `$TMPDIR` (per user, mode 0700). Unix
//!   socket paths may be at most 104 bytes on macOS; `$TMPDIR/soi.sock` fits.

use std::ffi::{CStr, OsStr};
use std::mem::{MaybeUninit, size_of, size_of_val};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use gtk::prelude::*;

use super::{Platform, Snap};

pub struct MacOs;

/// One `proc_pidinfo` record of type `T`, or `None` if the process is gone
/// or not ours to inspect.
fn pid_info<T>(pid: u32, flavor: libc::c_int) -> Option<T> {
    let mut info = MaybeUninit::<T>::zeroed();
    let size = size_of::<T>() as libc::c_int;
    // SAFETY: the buffer is `size` bytes long and a zeroed `T` is valid for
    // the plain C structs used here; the kernel fills in at most `size` bytes.
    let filled = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            flavor,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    (filled == size).then(|| unsafe { info.assume_init() })
}

/// The terminal's foreground process group (its leader's PID), unless it's
/// the shell's own.
fn foreground_group(shell: u32) -> Option<u32> {
    let info: libc::proc_bsdinfo = pid_info(shell, libc::PROC_PIDTBSDINFO)?;
    let foreground = info.e_tpgid;
    (foreground != 0 && foreground != shell).then_some(foreground)
}

fn process_name(pid: u32) -> Option<String> {
    let mut name = [0u8; 2 * libc::MAXCOMLEN + 1];
    // SAFETY: the buffer is as long as we say; proc_name NUL-terminates.
    let len = unsafe {
        libc::proc_name(
            pid as libc::c_int,
            name.as_mut_ptr().cast(),
            name.len() as u32,
        )
    };
    (len > 0).then(|| String::from_utf8_lossy(&name[..len as usize]).into_owned())
}

/// Cocoa's `NSRect`: origin at the bottom left, y growing upwards.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
struct NsRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

type Id = *mut std::ffi::c_void;

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_msgSend();
    #[cfg(target_arch = "x86_64")]
    fn objc_msgSend_stret();
    fn sel_registerName(name: *const libc::c_char) -> Id;
}

unsafe extern "C" {
    /// GTK's `NSWindow` for a surface (`gdk/macos/gdkmacossurface.h`).
    fn gdk_macos_surface_get_native_window(surface: *mut std::ffi::c_void) -> Id;
}

fn selector(name: &CStr) -> Id {
    // SAFETY: a NUL-terminated selector name; the runtime keeps it.
    unsafe { sel_registerName(name.as_ptr()) }
}

/// `[object name]` for a method returning an object.
unsafe fn send_id(object: Id, name: &CStr) -> Id {
    // SAFETY: the caller makes sure `object` answers `name` with an object.
    let send: unsafe extern "C" fn(Id, Id) -> Id =
        unsafe { std::mem::transmute(objc_msgSend as unsafe extern "C" fn()) };
    unsafe { send(object, selector(name)) }
}

/// `[object name]` for a method returning an `NSRect`. Intel Macs return
/// structs this large through `objc_msgSend_stret`.
unsafe fn send_rect(object: Id, name: &CStr) -> NsRect {
    #[cfg(target_arch = "x86_64")]
    let send = objc_msgSend_stret;
    #[cfg(not(target_arch = "x86_64"))]
    let send = objc_msgSend;
    // SAFETY: the caller makes sure `object` answers `name` with an NSRect.
    let send: unsafe extern "C" fn(Id, Id) -> NsRect =
        unsafe { std::mem::transmute(send as unsafe extern "C" fn()) };
    unsafe { send(object, selector(name)) }
}

/// Where a window snapped to `snap` goes in `area`, the screen without the
/// menu bar and Dock.
fn snapped(area: NsRect, snap: Snap) -> NsRect {
    let (half_width, half_height) = (area.width / 2.0, area.height / 2.0);
    match snap {
        Snap::Left => NsRect {
            width: half_width,
            ..area
        },
        Snap::Right => NsRect {
            x: area.x + half_width,
            width: half_width,
            ..area
        },
        Snap::Top => NsRect {
            y: area.y + half_height,
            height: half_height,
            ..area
        },
        Snap::Bottom => NsRect {
            height: half_height,
            ..area
        },
    }
}

impl Platform for MacOs {
    fn foreground_program(&self, shell: u32) -> Option<String> {
        let foreground = foreground_group(shell)?;
        process_name(foreground)
    }

    fn foreground_programs(&self, shell: u32) -> Vec<String> {
        let Some(group) = foreground_group(shell) else {
            return Vec::new();
        };
        let mut pids = [0 as libc::pid_t; 256];
        // SAFETY: the buffer is as many bytes long as we say; the kernel
        // fills in at most that many.
        let count = unsafe {
            libc::proc_listpgrppids(
                group as libc::pid_t,
                pids.as_mut_ptr().cast(),
                size_of_val(&pids) as libc::c_int,
            )
        };
        let mut members: Vec<u32> = pids[..count.clamp(0, pids.len() as libc::c_int) as usize]
            .iter()
            .map(|&pid| pid as u32)
            .filter(|&pid| pid != 0 && pid != group)
            .collect();
        members.sort_unstable();
        std::iter::once(group)
            .chain(members)
            .filter_map(process_name)
            .collect()
    }

    fn process_cwd(&self, pid: u32) -> Option<PathBuf> {
        let info: libc::proc_vnodepathinfo = pid_info(pid, libc::PROC_PIDVNODEPATHINFO)?;
        // SAFETY: `vip_path` is one MAXPATHLEN buffer the kernel NUL-terminates.
        let path =
            unsafe { CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr().cast::<libc::c_char>()) };
        let path = path.to_bytes();
        (!path.is_empty()).then(|| PathBuf::from(OsStr::from_bytes(path)))
    }

    /// `$TMPDIR`: a per-user directory only its owner can read (unlike
    /// `/tmp`), as macOS has no `$XDG_RUNTIME_DIR`.
    fn runtime_dir(&self) -> Option<PathBuf> {
        Some(std::env::temp_dir())
    }

    /// Pango turns "Monospace" into Courier here.
    fn monospace_family(&self) -> &'static str {
        "Menlo"
    }

    /// devfs, with `/dev/fd` mounted inside it; there's no `/proc` or `/sys`.
    fn kernel_filesystems(&self) -> &'static [&'static str] {
        &["/dev"]
    }

    /// The user's data directory, Homebrew's (Apple Silicon, then Intel),
    /// then Ghostty's app bundle, which keeps its themes in
    /// `Resources/ghostty/themes`.
    fn data_dirs(&self) -> Vec<PathBuf> {
        let ghostty = "Applications/Ghostty.app/Contents/Resources";
        vec![
            crate::dirs::data_home(),
            PathBuf::from("/opt/homebrew/share"),
            PathBuf::from("/usr/local/share"),
            PathBuf::from("/").join(ghostty),
            crate::dirs::home_dir().join(ghostty),
        ]
    }

    fn cmd_shortcuts(&self) -> bool {
        true
    }

    fn login_shell(&self) -> bool {
        true
    }

    /// GTK's macOS backend (4.24) shows the window as tiles and updates
    /// only those inside the redrawn area; the others can keep an older
    /// buffer, so stale or double-size pieces of the window and the desktop
    /// show through, mostly in translucent windows. Redrawing everything
    /// each frame updates every tile.
    fn gtk_env(&self) -> &'static [(&'static str, &'static str)] {
        &[("GSK_DEBUG", "full-redraw")]
    }

    /// Sets the frame of GTK's `NSWindow`, as window tools like Rectangle
    /// do from outside.
    fn snap_window(&self, window: &gtk::Window, snap: Snap) -> bool {
        let Some(surface) = window.surface() else {
            return false;
        };
        // SAFETY: on macOS every GDK surface is a GdkMacosSurface; the
        // NSWindow lives as long as the surface, which we hold. `screen`
        // returns an NSScreen (or nil), `visibleFrame` an NSRect, and
        // `setFrame:display:` takes an NSRect and a BOOL.
        unsafe {
            let ns_window = gdk_macos_surface_get_native_window(surface.as_ptr().cast());
            if ns_window.is_null() {
                return false;
            }
            let screen = send_id(ns_window, c"screen");
            if screen.is_null() {
                return false;
            }
            let frame = snapped(send_rect(screen, c"visibleFrame"), snap);
            let set_frame: unsafe extern "C" fn(Id, Id, NsRect, i8) =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            set_frame(ns_window, selector(c"setFrame:display:"), frame, 1);
        }
        true
    }

    /// GTK draws no shadows around macOS windows; the system does.
    fn surface_is_window(&self) -> bool {
        true
    }

    /// Ghostty reads the XDG directory first, then its Application Support one.
    fn ghostty_config_dirs(&self) -> Vec<PathBuf> {
        vec![
            crate::dirs::config_home().join("ghostty"),
            crate::dirs::home_dir().join("Library/Application Support/com.mitchellh.ghostty"),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snaps_to_halves_of_the_visible_screen() {
        // Cocoa: y from the bottom, here above a 100 point Dock.
        let area = NsRect {
            x: 0.0,
            y: 100.0,
            width: 1000.0,
            height: 800.0,
        };
        let rect = |x, y, width, height| NsRect {
            x,
            y,
            width,
            height,
        };
        assert_eq!(snapped(area, Snap::Left), rect(0.0, 100.0, 500.0, 800.0));
        assert_eq!(snapped(area, Snap::Right), rect(500.0, 100.0, 500.0, 800.0));
        assert_eq!(snapped(area, Snap::Top), rect(0.0, 500.0, 1000.0, 400.0));
        assert_eq!(snapped(area, Snap::Bottom), rect(0.0, 100.0, 1000.0, 400.0));
    }
}
