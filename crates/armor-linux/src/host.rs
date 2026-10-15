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

use armor_mem::GuestMemory;

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

/// The host addresses of the functions found so far, by id (0: missing).
fn resolved() -> &'static Mutex<HashMap<u64, usize>> {
    static RESOLVED: OnceLock<Mutex<HashMap<u64, usize>>> = OnceLock::new();
    RESOLVED.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The host address of `function`, or `None` if its library or symbol is
/// missing on this host.
fn address(function: &HostFunction) -> Option<usize> {
    let mut resolved = resolved().lock().expect("host functions lock");
    let address = *resolved.entry(function.id).or_insert_with(|| {
        // SAFETY: dlopen and dlsym take NUL-terminated strings; a library
        // stays loaded for the life of the process (no dlclose).
        unsafe {
            let library = libc::dlopen(function.library.as_ptr(), libc::RTLD_NOW);
            if library.is_null() {
                return 0;
            }
            libc::dlsym(library, function.symbol.as_ptr()) as usize
        }
    });
    (address != 0).then_some(address)
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
/// `slots_address`.
pub(crate) fn host_call<M: GuestMemory + ?Sized>(
    mem: &M,
    id: u64,
    slots_address: u64,
) -> crate::syscall::SysResult {
    use crate::syscall::err;
    let Some(function) = FUNCTIONS.iter().find(|f| f.id == id) else {
        return Err(err(libc::ENOSYS));
    };
    let Some(address) = address(function) else {
        return Err(err(libc::ENOSYS));
    };
    let mut bytes = [0u8; SLOTS * 8];
    mem.read(slots_address, &mut bytes)
        .map_err(|_| err(libc::EFAULT))?;
    let mut slots = [0u64; SLOTS];
    for (slot, chunk) in slots.iter_mut().zip(bytes.as_chunks::<8>().0) {
        *slot = u64::from_le_bytes(*chunk);
    }
    // SAFETY: the table gives each function its exact C signature.
    unsafe { call_host(address, function.signature, &mut slots) };
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
