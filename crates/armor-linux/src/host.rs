//! Calls from guest code into the host's own libraries (ADR-0041).
//!
//! A guest stub stores a call's arguments in sixteen 8-byte slots in guest
//! memory (integers and pointers as they are, a `double` by its bits, a
//! `float` by its bits in the low half) and makes syscall
//! [`HOST_CALL`] with the function's id in `rdi` and the slots' address in
//! `rsi`. armor finds the function in the host library (`dlopen`,
//! `dlsym`, once), calls it with the types of its signature on the guest
//! thread's own host thread, and writes the result into slot 0. An id
//! armor does not know, or a function the host does not have, gives
//! `ENOSYS`, as the syscall does under any other kernel or emulator: the
//! stub then falls back to the guest's own code. Guest addresses are host
//! addresses (ADR-0018), so pointers pass unchanged.

use std::collections::HashMap;
use std::ffi::{CStr, c_void};
use std::sync::{Mutex, OnceLock};

use armor_mem::{AddressSpace, GuestMemory, Prot};

/// The syscall number of a host call: outside Linux's x86_64 numbers (and
/// below the x32 range at 0x4000_0000).
pub const HOST_CALL: u64 = 0x41_0000;

/// The number of 8-byte argument slots.
pub const SLOTS: usize = 16;

/// The C types of a host function's parameters and result, as the
/// generated table names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signature {
    /// `double f(double, int)`.
    F64I32ToF64,
}

/// A host function a guest stub may call.
#[derive(Debug, Clone, Copy)]
pub struct HostFunction {
    pub id: u64,
    pub library: &'static CStr,
    pub symbol: &'static CStr,
    pub signature: Signature,
}

/// The host functions guest stubs may call, by id. Id 1 is the test of the
/// mechanism (`tests/guest/c/host_call.c`).
pub const FUNCTIONS: &[HostFunction] = &[HostFunction {
    id: 1,
    library: c"libm.so.6",
    symbol: c"ldexp",
    signature: Signature::F64I32ToF64,
}];

mod gl_table;
mod glx;

/// The id of the probe the OpenGL stubs make when they load: 0 when armor
/// runs the guest with the host's OpenGL (`--host-gl`), else `ENOSYS`.
const GL_PROBE: &str = "armorHostGL";

/// A function's id: FNV-1a (64 bits) of its name, as `cargo xtask gl-gen`
/// computes it for the stubs.
pub fn function_id(name: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in name.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The host address of `symbol` in host library `library` (loaded once,
/// never unloaded), or `None`.
pub(crate) fn host_symbol(library: &CStr, symbol: &CStr) -> Option<usize> {
    static LIBRARIES: OnceLock<Mutex<HashMap<Vec<u8>, usize>>> = OnceLock::new();
    let mut libraries = LIBRARIES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("host libraries lock");
    let handle = *libraries
        .entry(library.to_bytes().to_vec())
        // SAFETY: a NUL-terminated name; the library stays loaded.
        .or_insert_with(|| unsafe { libc::dlopen(library.as_ptr(), libc::RTLD_NOW) } as usize);
    if handle == 0 {
        return None;
    }
    // SAFETY: a valid handle and a NUL-terminated name.
    let address = unsafe { libc::dlsym(handle as *mut c_void, symbol.as_ptr()) } as usize;
    (address != 0).then_some(address)
}

/// The host address of a function by id (0: missing), found once.
fn resolved() -> &'static Mutex<HashMap<u64, usize>> {
    static RESOLVED: OnceLock<Mutex<HashMap<u64, usize>>> = OnceLock::new();
    RESOLVED.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The host address of `function`, or `None` if its library or symbol is
/// missing on this host.
fn address(function: &HostFunction) -> Option<usize> {
    let mut resolved = resolved().lock().expect("host functions lock");
    let address = *resolved
        .entry(function.id)
        .or_insert_with(|| host_symbol(function.library, function.symbol).unwrap_or(0));
    (address != 0).then_some(address)
}

/// The host address of OpenGL function `name`: exported by the host's
/// `libGL.so.1`, else from `glXGetProcAddressARB` (extensions).
fn gl_address(id: u64, name: &CStr) -> Option<usize> {
    let mut resolved = resolved().lock().expect("host functions lock");
    let address = *resolved.entry(id).or_insert_with(|| {
        host_symbol(c"libGL.so.1", name)
            .or_else(|| {
                let get = host_symbol(c"libGL.so.1", c"glXGetProcAddressARB")?;
                // SAFETY: `void (*glXGetProcAddressARB(const GLubyte *))(void)`.
                let get: unsafe extern "C" fn(*const u8) -> usize =
                    unsafe { std::mem::transmute(get) };
                // SAFETY: a NUL-terminated name.
                let address = unsafe { get(name.as_ptr().cast()) };
                (address != 0).then_some(address)
            })
            .unwrap_or(0)
    });
    (address != 0).then_some(address)
}

/// Strings the host returned, copied into guest memory once each.
#[derive(Default)]
pub(crate) struct GuestStrings {
    copied: Mutex<HashMap<Vec<u8>, u64>>,
}

impl GuestStrings {
    /// The guest address of a copy of host string `text` (0 for null).
    pub(crate) fn copy(&self, mem: &AddressSpace, text: *const std::ffi::c_char) -> u64 {
        if text.is_null() {
            return 0;
        }
        // SAFETY: the host function returned a NUL-terminated string.
        let bytes = unsafe { CStr::from_ptr(text) }.to_bytes_with_nul().to_vec();
        let mut copied = self.copied.lock().expect("guest strings lock");
        if let Some(address) = copied.get(&bytes) {
            return *address;
        }
        let Ok(address) = mem.map_anywhere(bytes.len() as u64, Prot::READ | Prot::WRITE) else {
            return 0;
        };
        if mem.write(address, &bytes).is_err() {
            return 0;
        }
        copied.insert(bytes, address);
        address
    }
}

fn strings() -> &'static GuestStrings {
    static STRINGS: OnceLock<GuestStrings> = OnceLock::new();
    STRINGS.get_or_init(GuestStrings::default)
}

/// Calls `address`, a host function of `signature`, with the arguments in
/// `slots`, and stores its result in slot 0.
///
/// # Safety
/// `address` must be a host function of exactly `signature`, and the
/// pointers among the arguments must be valid for it.
unsafe fn call_host(address: usize, signature: Signature, slots: &mut [u64; SLOTS]) {
    match signature {
        Signature::F64I32ToF64 => {
            // SAFETY: the caller guarantees the signature.
            let f: extern "C" fn(f64, i32) -> f64 =
                unsafe { std::mem::transmute(address as *const c_void) };
            slots[0] = f(f64::from_bits(slots[0]), slots[1] as i32).to_bits();
        }
    }
}

/// Syscall [`HOST_CALL`]: function `id`, its slots at guest address
/// `slots_address`; OpenGL and GLX only with `host_gl` (`--host-gl`).
pub(crate) fn host_call(
    mem: &AddressSpace,
    host_gl: bool,
    id: u64,
    slots_address: u64,
) -> crate::syscall::SysResult {
    use crate::syscall::err;
    let mut bytes = [0u8; SLOTS * 8];
    let read = |bytes: &mut [u8; SLOTS * 8]| {
        mem.read(slots_address, bytes)
            .map_err(|_| err(libc::EFAULT))
    };
    let mut slots = [0u64; SLOTS];
    let load = |bytes: &[u8; SLOTS * 8], slots: &mut [u64; SLOTS]| {
        for (slot, chunk) in slots.iter_mut().zip(bytes.as_chunks::<8>().0) {
            *slot = u64::from_le_bytes(*chunk);
        }
    };
    if let Some(function) = FUNCTIONS.iter().find(|f| f.id == id) {
        let address = address(function).ok_or(err(libc::ENOSYS))?;
        read(&mut bytes)?;
        load(&bytes, &mut slots);
        // SAFETY: the table gives each function its exact C signature.
        unsafe { call_host(address, function.signature, &mut slots) };
    } else if !host_gl || host_symbol(c"libGL.so.1", c"glGetString").is_none() {
        return Err(err(libc::ENOSYS));
    } else if id == function_id(GL_PROBE) {
        return Ok(0);
    } else if let Some(name) = glx::name_of(id) {
        read(&mut bytes)?;
        load(&bytes, &mut slots);
        glx::call(name, &mut slots, mem, strings()).ok_or(err(libc::ENOSYS))?;
    } else {
        let index = gl_table::GL_FUNCTIONS
            .binary_search_by_key(&id, |(id, _, _)| *id)
            .map_err(|_| err(libc::ENOSYS))?;
        let (_, name, signature) = gl_table::GL_FUNCTIONS[index];
        let address = gl_address(id, name).ok_or(err(libc::ENOSYS))?;
        read(&mut bytes)?;
        load(&bytes, &mut slots);
        // SAFETY: the generated table gives each function its signature.
        unsafe { gl_table::call(signature, address, &mut slots) };
        if name == c"glGetString" || name == c"glGetStringi" {
            slots[0] = strings().copy(mem, slots[0] as *const std::ffi::c_char);
        }
    }
    mem.write(slots_address, &slots[0].to_le_bytes())
        .map_err(|_| err(libc::EFAULT))?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_test_function_is_called_on_the_host() {
        let function = FUNCTIONS[0];
        let address = address(&function).expect("libm's ldexp");
        let mut slots = [0u64; SLOTS];
        slots[0] = 0.75f64.to_bits();
        slots[1] = (-3i64) as u64;
        // SAFETY: ldexp is `double ldexp(double, int)`.
        unsafe { call_host(address, function.signature, &mut slots) };
        assert_eq!(f64::from_bits(slots[0]), 0.75 / 8.0);
    }
}
