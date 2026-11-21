//! Zero-dependency cross-platform dynamic shared-library loader (`LoadLibraryA` / `dlopen`).
//!
//! Shared across Mora's hardware GPU drivers (`runtime::gpu::{cuda, vulkan, metal}`) and
//! native OS TLS backends (`runtime::tls::{schannel, openssl}`).

use std::ffi::{CStr, CString, c_void};

/// Zero-dependency cross-platform dynamic shared-library handle (`LoadLibraryA` / `dlopen`).
#[derive(Debug)]
pub(crate) struct DynLib {
    handle: *mut c_void,
}

// SAFETY: OS shared-library handles (`HMODULE` on Windows, `void*` from `dlopen` on Unix)
// are process-wide immutable loader tokens safe to share across threads for symbol lookup.
unsafe impl Send for DynLib {}
// SAFETY: Symbol lookups (`GetProcAddress` / `dlsym`) on an open process-wide library handle
// are thread-safe read operations.
unsafe impl Sync for DynLib {}

#[cfg(windows)]
mod os_loader {
    use std::ffi::c_void;

    // SAFETY: Standard Windows kernel32 ABI symbols present on every Windows installation.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub(super) fn LoadLibraryA(lp_lib_file_name: *const i8) -> *mut c_void;
        pub(super) fn GetProcAddress(h_module: *mut c_void, lp_proc_name: *const i8)
        -> *mut c_void;
        pub(super) fn FreeLibrary(h_lib_module: *mut c_void) -> i32;
    }
}

impl DynLib {
    /// Attempt to open the first loadable library from `candidates`.
    pub(crate) fn open_any(candidates: &[&str]) -> Option<Self> {
        for name in candidates {
            if let Some(lib) = Self::open(name) {
                return Some(lib);
            }
        }
        None
    }

    pub(crate) fn open(name: &str) -> Option<Self> {
        let c_name = CString::new(name).ok()?;
        #[cfg(windows)]
        {
            // SAFETY: `c_name` is a valid NUL-terminated ASCII/ANSI string for `LoadLibraryA`.
            let handle = unsafe { os_loader::LoadLibraryA(c_name.as_ptr()) };
            if handle.is_null() {
                None
            } else {
                Some(Self { handle })
            }
        }
        #[cfg(unix)]
        {
            let handle = {
                // SAFETY: `c_name` is a valid NUL-terminated C string passed to POSIX `dlopen`.
                unsafe {
                    crate::common::sys::unix::dlopen(
                        c_name.as_ptr(),
                        crate::common::sys::unix::RTLD_NOW | crate::common::sys::unix::RTLD_LOCAL,
                    )
                }
            };
            if handle.is_null() {
                None
            } else {
                Some(Self { handle })
            }
        }
    }

    /// Resolve a C symbol from this library and transmute it to function pointer type `T`.
    ///
    /// # Safety
    /// SAFETY: Caller must supply a valid NUL-terminated ASCII symbol name in `symbol_nul` and ensure
    /// `T` has the exact C ABI signature and size of a raw pointer (`size_of::<T>() == size_of::<*mut c_void>()`).
    pub(crate) unsafe fn get_symbol<T: Copy>(&self, symbol_nul: &[u8]) -> Option<T> {
        debug_assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<*mut c_void>());
        let cstr = CStr::from_bytes_with_nul(symbol_nul).ok()?;
        #[cfg(windows)]
        let ptr = {
            // SAFETY: `self.handle` is a live `HMODULE` and `cstr` is NUL-terminated.
            unsafe { os_loader::GetProcAddress(self.handle, cstr.as_ptr()) }
        };
        #[cfg(unix)]
        let ptr = {
            // SAFETY: `self.handle` is a live `dlopen` handle and `cstr` is NUL-terminated.
            unsafe { crate::common::sys::unix::dlsym(self.handle, cstr.as_ptr()) }
        };
        if ptr.is_null() {
            return None;
        }
        // SAFETY: Caller guarantees `T` is a C ABI function pointer matching the resolved symbol.
        Some(unsafe { std::mem::transmute_copy(&ptr) })
    }
}

impl Drop for DynLib {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            #[cfg(windows)]
            {
                // SAFETY: `self.handle` was returned by `LoadLibraryA` and is freed once on Drop.
                unsafe {
                    os_loader::FreeLibrary(self.handle);
                }
            }
            #[cfg(unix)]
            {
                // SAFETY: `self.handle` was returned by `dlopen` and is closed once on Drop.
                unsafe {
                    crate::common::sys::unix::dlclose(self.handle);
                }
            }
        }
    }
}
