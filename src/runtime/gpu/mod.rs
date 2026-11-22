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
mod vulkan_spirv;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use crate::common::Device;
pub(crate) use crate::common::dynlib::DynLib;
use crate::runtime::compute_dag::{DeviceBackend, VramTelemetry};
use crate::runtime::simd_kernels;

/// Shared lock-free atomic counters tracking live GPU VRAM residency & transfers per device.
#[derive(Debug, Default)]
pub(crate) struct VramCounters {
    pub live_buffers: AtomicU64,
    pub live_bytes: AtomicU64,
    pub h2d_copies: AtomicU64,
    pub d2h_copies: AtomicU64,
    pub vram_hits: AtomicU64,
    pub native_f16_dispatches: AtomicU64,
    pub native_bf16_dispatches: AtomicU64,
}

impl VramCounters {
    pub(crate) fn on_alloc(&self, bytes: usize) {
        self.live_buffers.fetch_add(1, Ordering::Relaxed);
        self.live_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    pub(crate) fn on_free(&self, bytes: usize) {
        self.live_buffers.fetch_sub(1, Ordering::Relaxed);
        self.live_bytes.fetch_sub(bytes as u64, Ordering::Relaxed);
    }

    pub(crate) fn on_h2d(&self) {
        self.h2d_copies.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn on_d2h(&self) {
        self.d2h_copies.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn on_vram_hit(&self) {
        self.vram_hits.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn on_f16_dispatch(&self) {
        self.native_f16_dispatches.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn on_bf16_dispatch(&self) {
        self.native_bf16_dispatches.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn snapshot(&self) -> VramTelemetry {
        VramTelemetry {
            live_buffers: self.live_buffers.load(Ordering::Relaxed),
            live_bytes: self.live_bytes.load(Ordering::Relaxed),
            h2d_copies: self.h2d_copies.load(Ordering::Relaxed),
            d2h_copies: self.d2h_copies.load(Ordering::Relaxed),
            vram_hits: self.vram_hits.load(Ordering::Relaxed),
            native_f16_dispatches: self.native_f16_dispatches.load(Ordering::Relaxed),
            native_bf16_dispatches: self.native_bf16_dispatches.load(Ordering::Relaxed),
        }
    }
}

/// Merge live [`VramTelemetry`] metrics into a string telemetry map (`Tensor::device_info`, `dag.backend_info`).
pub(crate) fn insert_vram_telemetry_map(
    map: &mut HashMap<String, String>,
    vram_resident_supported: bool,
    telemetry: VramTelemetry,
) {
    map.insert(
        "vram_residency".to_string(),
        vram_resident_supported.to_string(),
    );
    map.insert(
        "vram_live_buffers".to_string(),
        telemetry.live_buffers.to_string(),
    );
    map.insert(
        "vram_live_bytes".to_string(),
        telemetry.live_bytes.to_string(),
    );
    map.insert(
        "vram_h2d_copies".to_string(),
        telemetry.h2d_copies.to_string(),
    );
    map.insert(
        "vram_d2h_copies".to_string(),
        telemetry.d2h_copies.to_string(),
    );
    map.insert("vram_hits".to_string(), telemetry.vram_hits.to_string());
    map.insert(
        "native_f16_dispatches".to_string(),
        telemetry.native_f16_dispatches.to_string(),
    );
    map.insert(
        "native_bf16_dispatches".to_string(),
        telemetry.native_bf16_dispatches.to_string(),
    );
}

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

/// Inspect full hardware + live VRAM residency telemetry map for `device`.
pub(crate) fn hardware_device_info_map(device: &Device) -> HashMap<String, String> {
    let info = hardware_device_info(device);
    let mut map = info.to_string_map();
    if let Some(backend) = resolve_hardware_backend(device) {
        insert_vram_telemetry_map(
            &mut map,
            backend.supports_vram_residency(),
            backend.vram_telemetry(),
        );
    } else {
        insert_vram_telemetry_map(&mut map, false, VramTelemetry::default());
    }
    map
}

/// List all detected execution devices (always includes `"cpu"`, plus live hardware GPUs).
pub(crate) fn list_hardware_devices() -> Vec<GpuDeviceInfo> {
    if emulation_forced() {
        return vec![GpuDeviceInfo::cpu_info()];
    }
    registry().ordered_infos.clone()
}
