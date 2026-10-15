//! GLX for the guest's OpenGL stubs (ADR-0041): armor's own connection to
//! the X server, used with the X ids the guest's own `libX11` created
//! (windows, visuals: the server's, the same for every client).
//!
//! The guest stub (`guest/libgl/glx.c`) never passes its `Display*`: armor
//! uses its own. A visual travels as its id (the stub turns ids into the
//! guest's own `XVisualInfo` with the guest's `XGetVisualInfo`); a
//! `GLXFBConfig` or `GLXContext` is an opaque value the guest gives back
//! unchanged; an array of configs is written into a buffer the stub
//! allocated.

use std::ffi::{c_char, c_int, c_ulong, c_void};
use std::sync::OnceLock;

use super::{GuestStrings, SLOTS, function_id, host_symbol};
use armor_mem::{AddressSpace, GuestMemory};

/// The part of Xlib's `XVisualInfo` armor reads (LP64, the same on x86-64
/// and AArch64): `visual` pointer, `visualid`, `screen`, `depth`, ...
#[repr(C)]
struct XVisualInfo {
    visual: *mut c_void,
    visualid: c_ulong,
    screen: c_int,
    depth: c_int,
    class: c_int,
    red_mask: c_ulong,
    green_mask: c_ulong,
    blue_mask: c_ulong,
    colormap_size: c_int,
    bits_per_rgb: c_int,
}

const VISUAL_ID_MASK: i64 = 0x1;
const VISUAL_SCREEN_MASK: i64 = 0x2;

/// armor's host display and the Xlib functions it needs.
struct Host {
    display: *mut c_void,
    get_visual_info:
        unsafe extern "C" fn(*mut c_void, i64, *mut XVisualInfo, *mut c_int) -> *mut XVisualInfo,
    free: unsafe extern "C" fn(*mut c_void) -> c_int,
}

// SAFETY: Xlib is initialised for threads (`XInitThreads`) before the
// display is opened; the pointer is only passed to Xlib and GLX.
unsafe impl Send for Host {}
// SAFETY: as above.
unsafe impl Sync for Host {}

fn host() -> Option<&'static Host> {
    static HOST: OnceLock<Option<Host>> = OnceLock::new();
    HOST.get_or_init(|| {
        let init = host_symbol(c"libX11.so.6", c"XInitThreads")?;
        let open = host_symbol(c"libX11.so.6", c"XOpenDisplay")?;
        let get_visual_info = host_symbol(c"libX11.so.6", c"XGetVisualInfo")?;
        let free = host_symbol(c"libX11.so.6", c"XFree")?;
        // SAFETY: the Xlib signatures, from its headers.
        unsafe {
            let init: unsafe extern "C" fn() -> c_int = std::mem::transmute(init);
            let open: unsafe extern "C" fn(*const c_char) -> *mut c_void =
                std::mem::transmute(open);
            init();
            let display = open(std::ptr::null());
            (!display.is_null()).then(|| Host {
                display,
                get_visual_info: std::mem::transmute::<
                    usize,
                    unsafe extern "C" fn(
                        *mut c_void,
                        i64,
                        *mut XVisualInfo,
                        *mut c_int,
                    ) -> *mut XVisualInfo,
                >(get_visual_info),
                free: std::mem::transmute::<usize, unsafe extern "C" fn(*mut c_void) -> c_int>(
                    free,
                ),
            })
        }
    })
    .as_ref()
}

/// The host's `XVisualInfo` of visual `id` on `screen` (freed by the
/// caller with `free`), or null.
fn visual(host: &Host, id: u64, screen: u64) -> *mut XVisualInfo {
    let mut template = XVisualInfo {
        visual: std::ptr::null_mut(),
        visualid: id as c_ulong,
        screen: screen as c_int,
        depth: 0,
        class: 0,
        red_mask: 0,
        green_mask: 0,
        blue_mask: 0,
        colormap_size: 0,
        bits_per_rgb: 0,
    };
    let mut count = 0;
    // SAFETY: a valid display and template.
    unsafe {
        (host.get_visual_info)(
            host.display,
            VISUAL_ID_MASK | VISUAL_SCREEN_MASK,
            &mut template,
            &mut count,
        )
    }
}

/// The GLX functions armor handles, by name.
const NAMES: &[&str] = &[
    "glXQueryExtension",
    "glXQueryVersion",
    "glXQueryExtensionsString",
    "glXGetClientString",
    "glXQueryServerString",
    "glXChooseVisual",
    "glXGetConfig",
    "glXCreateContext",
    "glXDestroyContext",
    "glXMakeCurrent",
    "glXMakeContextCurrent",
    "glXSwapBuffers",
    "glXIsDirect",
    "glXQueryDrawable",
    "glXChooseFBConfig",
    "glXGetFBConfigs",
    "glXGetFBConfigAttrib",
    "glXGetVisualFromFBConfig",
    "glXCreateNewContext",
    "glXCreateContextAttribsARB",
    "glXSwapIntervalEXT",
    "glXSwapIntervalMESA",
    "glXGetSwapIntervalMESA",
    "glXSwapIntervalSGI",
    "glXWaitGL",
    "glXWaitX",
    "glXCreateWindow",
    "glXDestroyWindow",
    "glXGetCurrentContext",
    "glXGetCurrentDrawable",
    "glXQueryContext",
];

/// The name of GLX function `id`, if armor handles it.
pub(super) fn name_of(id: u64) -> Option<&'static str> {
    static IDS: OnceLock<Vec<(u64, &'static str)>> = OnceLock::new();
    IDS.get_or_init(|| NAMES.iter().map(|n| (function_id(n), *n)).collect())
        .iter()
        .find(|(i, _)| *i == id)
        .map(|(_, n)| *n)
}

/// Calls GLX function `name` with the stub's slots; the result goes into
/// slot 0. `None` when the host has no display or no such function.
pub(super) fn call(
    name: &str,
    s: &mut [u64; SLOTS],
    mem: &AddressSpace,
    strings: &GuestStrings,
) -> Option<()> {
    let host = host()?;
    let dpy = host.display;
    let symbol = std::ffi::CString::new(name).ok()?;
    let f = host_symbol(c"libGL.so.1", &symbol)?;
    macro_rules! glx {
        ($($t:ty),* => $r:ty) => {{
            // The GLX signature of `name`, from glx.xml and glxext.h (the
            // transmute is inside the caller's `unsafe` block).
            let f: unsafe extern "C" fn(*mut c_void $(, $t)*) -> $r = std::mem::transmute(f);
            f
        }};
    }
    // SAFETY: the functions' GLX signatures; pointers the guest passed are
    // guest addresses, which are host addresses (ADR-0018).
    unsafe {
        match name {
            "glXQueryExtension" | "glXQueryVersion" => {
                let f = glx!(u64, u64 => c_int);
                s[0] = f(dpy, s[0], s[1]) as u64;
            }
            "glXQueryExtensionsString" => {
                let f = glx!(c_int => *const c_char);
                s[0] = strings.copy(mem, f(dpy, s[0] as c_int));
            }
            "glXGetClientString" | "glXQueryServerString" => {
                let text = if name == "glXGetClientString" {
                    glx!(c_int => *const c_char)(dpy, s[0] as c_int)
                } else {
                    glx!(c_int, c_int => *const c_char)(dpy, s[0] as c_int, s[1] as c_int)
                };
                s[0] = strings.copy(mem, text);
            }
            "glXChooseVisual" => {
                let f = glx!(c_int, u64 => *mut XVisualInfo);
                let vi = f(dpy, s[0] as c_int, s[1]);
                s[0] = if vi.is_null() {
                    0
                } else {
                    let id = (*vi).visualid as u64;
                    (host.free)(vi.cast());
                    id
                };
            }
            "glXGetConfig" | "glXCreateContext" => {
                // s[0], s[1]: the visual's id and screen.
                let vi = visual(host, s[0], s[1]);
                if vi.is_null() {
                    s[0] = if name == "glXGetConfig" { 7 } else { 0 }; // GLX_BAD_VISUAL
                    return Some(());
                }
                if name == "glXGetConfig" {
                    let f = glx!(*mut XVisualInfo, c_int, u64 => c_int);
                    s[0] = f(dpy, vi, s[2] as c_int, s[3]) as u64;
                } else {
                    let f = glx!(*mut XVisualInfo, u64, c_int => u64);
                    s[0] = f(dpy, vi, s[2], s[3] as c_int);
                }
                (host.free)(vi.cast());
            }
            "glXDestroyContext" => glx!(u64 => ())(dpy, s[0]),
            "glXMakeCurrent" => s[0] = glx!(u64, u64 => c_int)(dpy, s[0], s[1]) as u64,
            "glXMakeContextCurrent" => {
                s[0] = glx!(u64, u64, u64 => c_int)(dpy, s[0], s[1], s[2]) as u64;
            }
            "glXSwapBuffers" => glx!(u64 => ())(dpy, s[0]),
            "glXIsDirect" => s[0] = glx!(u64 => c_int)(dpy, s[0]) as u64,
            "glXQueryDrawable" => glx!(u64, c_int, u64 => ())(dpy, s[0], s[1] as c_int, s[2]),
            "glXChooseFBConfig" | "glXGetFBConfigs" => {
                // s[0] screen, s[1] attributes (ChooseFBConfig), s[2] the
                // count's address, s[3] the guest's buffer, s[4] its
                // capacity in configs.
                let mut count: c_int = 0;
                let configs = if name == "glXChooseFBConfig" {
                    glx!(c_int, u64, *mut c_int => *mut u64)(dpy, s[0] as c_int, s[1], &mut count)
                } else {
                    glx!(c_int, *mut c_int => *mut u64)(dpy, s[0] as c_int, &mut count)
                };
                let n = if configs.is_null() {
                    0
                } else {
                    (count.max(0) as u64).min(s[4])
                };
                for i in 0..n {
                    let value = *configs.add(i as usize);
                    mem.write(s[3] + 8 * i, &value.to_le_bytes()).ok()?;
                }
                if !configs.is_null() {
                    (host.free)(configs.cast());
                }
                mem.write(s[2], &(n as c_int).to_le_bytes()).ok()?;
                s[0] = n;
            }
            "glXGetFBConfigAttrib" => {
                s[0] = glx!(u64, c_int, u64 => c_int)(dpy, s[0], s[1] as c_int, s[2]) as u64;
            }
            "glXGetVisualFromFBConfig" => {
                let vi = glx!(u64 => *mut XVisualInfo)(dpy, s[0]);
                s[0] = if vi.is_null() {
                    0
                } else {
                    let id = (*vi).visualid as u64;
                    (host.free)(vi.cast());
                    id
                };
            }
            "glXCreateNewContext" => {
                s[0] = glx!(u64, c_int, u64, c_int => u64)(
                    dpy,
                    s[0],
                    s[1] as c_int,
                    s[2],
                    s[3] as c_int,
                );
            }
            "glXCreateContextAttribsARB" => {
                s[0] = glx!(u64, u64, c_int, u64 => u64)(dpy, s[0], s[1], s[2] as c_int, s[3]);
            }
            "glXSwapIntervalEXT" => glx!(u64, c_int => ())(dpy, s[0], s[1] as c_int),
            "glXSwapIntervalMESA" | "glXSwapIntervalSGI" => {
                // No display parameter.
                let f: unsafe extern "C" fn(c_int) -> c_int = std::mem::transmute(f);
                s[0] = f(s[0] as c_int) as u64;
            }
            "glXGetSwapIntervalMESA" | "glXWaitGL" | "glXWaitX" => {
                let f: unsafe extern "C" fn() -> c_int = std::mem::transmute(f);
                s[0] = f() as u64;
            }
            "glXCreateWindow" => {
                s[0] = glx!(u64, u64, u64 => u64)(dpy, s[0], s[1], s[2]);
            }
            "glXDestroyWindow" => glx!(u64 => ())(dpy, s[0]),
            "glXGetCurrentContext" | "glXGetCurrentDrawable" => {
                let f: unsafe extern "C" fn() -> u64 = std::mem::transmute(f);
                s[0] = f();
            }
            "glXQueryContext" => {
                s[0] = glx!(u64, c_int, u64 => c_int)(dpy, s[0], s[1] as c_int, s[2]) as u64;
            }
            _ => return None,
        }
    }
    Some(())
}
