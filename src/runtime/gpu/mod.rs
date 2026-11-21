//! v0.110.0: Native Hardware GPU Backend Discovery & Dynamic Driver Loader
//! (`CUDA Driver API` + PTX JIT, `Vulkan C API` + SPIR-V Compute, `Metal Framework` + MSL).
//!
//! Architecture:
//! - Zero compile-time SDK or Cargo dependencies: dynamically opens OS-provided GPU drivers
//!   (`nvcuda.dll` / `libcuda.so.1`, `vulkan-1.dll` / `libvulkan.so.1` / `libMoltenVK.dylib`,
//!   `/System/Library/Frameworks/Metal.framework/Metal`) at runtime via `LoadLibraryA` / `dlopen`.
//! - When real hardware GPUs are detected, `crate::runtime::compute_dag::backend_for_device`
//!   dispatches compute graphs directly onto hardware VRAM & native GPU kernels.
//! - When no GPU driver is present (e.g. headless CPU CI runners) or `MORA_FORCE_CPU_EMULATION=1`
//!   is set, cleanly falls back to `EmulatedDeviceBackend` without crashing.

pub(crate) mod cuda;
pub(crate) mod metal;
pub(crate) mod vulkan;

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_void};
use std::sync::{Arc, OnceLock};

use crate::common::Device;
use crate::runtime::compute_dag::DeviceBackend;
use crate::runtime::simd_kernels;

/// Rich runtime hardware telemetry for a host CPU or detected physical GPU device.
#[derive(Debug, Clone)]
pub(crate) struct GpuDeviceInfo {
    pub device: Device,
    pub driver: String,
    pub name: String,
    pub total_vram_bytes: u64,
    pub compute_capability: String,
    pub multiprocessors: u32,
    pub hardware_accelerated: bool,
    pub driver_version: String,
}

impl GpuDeviceInfo {
    pub(crate) fn cpu_info() -> Self {
        Self {
            device: Device::Cpu,
            driver: "cpu".to_string(),
            name: format!("Host CPU ({})", simd_kernels::simd_isa_name()),
            total_vram_bytes: 0,
            compute_capability: simd_kernels::simd_isa_name().to_string(),
            multiprocessors: simd_kernels::effective_worker_threads() as u32,
            hardware_accelerated: true,
            driver_version: "native".to_string(),
        }
    }

    pub(crate) fn emulated_info(device: &Device) -> Self {
        let driver = match device {
            Device::Cpu => "cpu",
            Device::Cuda(_) => "cuda(host-emulated)",
            Device::Metal(_) => "metal(host-emulated)",
            Device::Vulkan(_) => "vulkan(host-emulated)",
        };
        Self {
            device: device.clone(),
            driver: driver.to_string(),
            name: format!("Host-Emulated {device}"),
            total_vram_bytes: 0,
            compute_capability: simd_kernels::simd_isa_name().to_string(),
            multiprocessors: simd_kernels::effective_worker_threads() as u32,
            hardware_accelerated: false,
            driver_version: "emulated".to_string(),
        }
    }

    pub(crate) fn to_string_map(&self) -> HashMap<String, String> {
        let mut m = HashMap::new();
        m.insert("device".to_string(), self.device.to_string());
        m.insert("driver".to_string(), self.driver.clone());
        m.insert("gpu_name".to_string(), self.name.clone());
        m.insert("vram_bytes".to_string(), self.total_vram_bytes.to_string());
        m.insert(
            "compute_capability".to_string(),
            self.compute_capability.clone(),
        );
        m.insert(
            "multiprocessors".to_string(),
            self.multiprocessors.to_string(),
        );
        m.insert(
            "hardware_accelerated".to_string(),
            self.hardware_accelerated.to_string(),
        );
        m.insert("driver_version".to_string(), self.driver_version.clone());
        m
    }
}

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

pub(crate) struct HardwareBackendEntry {
    pub info: GpuDeviceInfo,
    pub backend: Arc<dyn DeviceBackend>,
}

struct HardwareBackendRegistry {
    entries: HashMap<Device, HardwareBackendEntry>,
    ordered_infos: Vec<GpuDeviceInfo>,
}

impl HardwareBackendRegistry {
    fn discover() -> Self {
        let mut entries: HashMap<Device, HardwareBackendEntry> = HashMap::new();
        let mut ordered_infos = vec![GpuDeviceInfo::cpu_info()];

        for entry in cuda::probe_cuda_backends() {
            ordered_infos.push(entry.info.clone());
            entries.insert(entry.info.device.clone(), entry);
        }
        for entry in metal::probe_metal_backends() {
            ordered_infos.push(entry.info.clone());
            entries.insert(entry.info.device.clone(), entry);
        }
        for entry in vulkan::probe_vulkan_backends() {
            ordered_infos.push(entry.info.clone());
            entries.insert(entry.info.device.clone(), entry);
        }

        Self {
            entries,
            ordered_infos,
        }
    }
}

static HARDWARE_REGISTRY: OnceLock<HardwareBackendRegistry> = OnceLock::new();

fn registry() -> &'static HardwareBackendRegistry {
    HARDWARE_REGISTRY.get_or_init(HardwareBackendRegistry::discover)
}

/// Return true if the user explicitly requested host-emulated CPU fallback via env var.
pub(crate) fn emulation_forced() -> bool {
    std::env::var("MORA_FORCE_CPU_EMULATION")
        .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Resolve a live hardware GPU backend for `device`, or `None` when absent / emulation forced.
pub(crate) fn resolve_hardware_backend(device: &Device) -> Option<Arc<dyn DeviceBackend>> {
    if emulation_forced() || matches!(device, Device::Cpu) {
        return None;
    }
    registry()
        .entries
        .get(device)
        .map(|e| Arc::clone(&e.backend))
}

/// Inspect hardware telemetry for `device` (falls back to honest host-emulated metadata when absent).
pub(crate) fn hardware_device_info(device: &Device) -> GpuDeviceInfo {
    if matches!(device, Device::Cpu) {
        return GpuDeviceInfo::cpu_info();
    }
    if !emulation_forced()
        && let Some(entry) = registry().entries.get(device)
    {
        return entry.info.clone();
    }
    GpuDeviceInfo::emulated_info(device)
}

/// List all detected execution devices (always includes `"cpu"`, plus live hardware GPUs).
pub(crate) fn list_hardware_devices() -> Vec<GpuDeviceInfo> {
    if emulation_forced() {
        return vec![GpuDeviceInfo::cpu_info()];
    }
    registry().ordered_infos.clone()
}
