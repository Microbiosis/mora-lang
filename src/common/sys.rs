//! Zero-dependency OS system call bindings (replacing external `libc`).
//!
//! Provides the exact POSIX C ABI symbols used by Mora on Unix platforms:
//! - Dynamic shared library loading (`dlopen`, `dlsym`, `dlclose`, `RTLD_NOW`, `RTLD_LOCAL`)
//! - Process group isolation and termination (`setpgid`, `killpg`, `SIGKILL`)

#[cfg(unix)]
pub mod unix {
    use std::ffi::{c_char, c_int, c_void};

    /// Resolve all undefined symbols before `dlopen` returns.
    pub const RTLD_NOW: c_int = 2;

    /// Symbols in this library are not made available to subsequently loaded libraries.
    pub const RTLD_LOCAL: c_int = 0;

    /// POSIX `SIGKILL` (9) signal number across Linux, macOS, and BSDs.
    pub const SIGKILL: c_int = 9;

    // SAFETY: Standard POSIX / System V C ABI symbols exported by the platform C runtime
    // (`libc` / `libdl` on Linux, `libSystem` on macOS).
    #[cfg_attr(target_os = "linux", link(name = "dl"))]
    unsafe extern "C" {
        pub(crate) fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
        pub(crate) fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        pub(crate) fn dlclose(handle: *mut c_void) -> c_int;
        pub(crate) fn setpgid(pid: c_int, pgid: c_int) -> c_int;
        pub(crate) fn killpg(pgrp: c_int, sig: c_int) -> c_int;
    }
}
