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
use std::sync::{Arc, OnceLock};

use crate::common::Device;
pub(crate) use crate::common::dynlib::DynLib;
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
