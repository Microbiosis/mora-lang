//! Wave 4 P2-2: Native Vulkan 1.0 C API (`vulkan-1.dll` / `libvulkan.so.1` / `libMoltenVK.dylib`)
//! Hardware Compute Backend with Pure-Rust SPIR-V 1.0 Shader Generation.
//!
//! Architecture (`AGENTS.md §0`):
//! - Zero compile-time Vulkan SDK / `shaderc` / Cargo dependencies: dynamically opens the OS
//!   Vulkan loader at runtime and enumerates physical GPUs, VRAM heaps, and compute queues.
//! - Emits a spec-compliant SPIR-V 1.0 `GLCompute` shader directly in pure Rust (`Vec<u32>`)
//!   implementing fused `Linear + Bias + Activation` (`None`/`Relu`/`Gelu`/`Silu`/`Tanh`/`Sigmoid`)
//!   and `MatMul` directly on Vulkan `VkDevice` storage buffers via `vkCmdDispatch`.
//! - Cleanly delegates non-SPIR-V-lowered fused ops to host SIMD while maintaining full hardware
//!   telemetry and deterministic `Drop` cleanup of all Vulkan handles.

use std::ffi::{CStr, c_char, c_void};
use std::ptr;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::common::{DType, Device};
use crate::error::{MoraError, MoraResult};
use crate::runtime::compute_dag::{DeviceBackend, FusedAct, FusedEwStep};
use crate::runtime::gpu::{DynLib, GpuDeviceInfo, HardwareBackendEntry};
use crate::runtime::simd_kernels;

// ── Vulkan 1.0 C ABI handles & constants (`vulkan_core.h`) ───────────────────

type VkResult = i32;
type VkFlags = u32;
type VkDeviceSize = u64;

type VkInstance = *mut c_void;
type VkPhysicalDevice = *mut c_void;
type VkDevice = *mut c_void;
type VkQueue = *mut c_void;
type VkCommandBuffer = *mut c_void;

// Non-dispatchable handles are always `u64` on 64-bit platforms (`VK_DEFINE_NON_DISPATCHABLE_HANDLE`).
type VkBuffer = u64;
type VkDeviceMemory = u64;
type VkShaderModule = u64;
type VkDescriptorSetLayout = u64;
type VkPipelineLayout = u64;
type VkPipelineCache = u64;
type VkPipeline = u64;
type VkDescriptorPool = u64;
type VkDescriptorSet = u64;
type VkCommandPool = u64;
type VkFence = u64;

const VK_SUCCESS: VkResult = 0;
const VK_API_VERSION_1_0: u32 = 1 << 22;

const VK_STRUCTURE_TYPE_APPLICATION_INFO: u32 = 0;
const VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO: u32 = 1;
const VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO: u32 = 2;
const VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO: u32 = 3;
const VK_STRUCTURE_TYPE_SUBMIT_INFO: u32 = 4;
const VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO: u32 = 5;
const VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO: u32 = 12;
const VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO: u32 = 16;
const VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO: u32 = 18;
const VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO: u32 = 29;
const VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO: u32 = 30;
const VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO: u32 = 32;
const VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO: u32 = 33;
const VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO: u32 = 34;
const VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET: u32 = 35;
const VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO: u32 = 39;
const VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO: u32 = 40;
const VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO: u32 = 42;

const VK_QUEUE_COMPUTE_BIT: VkFlags = 0x0000_0002;
const VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT: VkFlags = 0x0000_0002;
const VK_MEMORY_PROPERTY_HOST_COHERENT_BIT: VkFlags = 0x0000_0004;
const VK_MEMORY_HEAP_DEVICE_LOCAL_BIT: VkFlags = 0x0000_0001;
const VK_BUFFER_USAGE_STORAGE_BUFFER_BIT: VkFlags = 0x0000_0020;
const VK_SHARING_MODE_EXCLUSIVE: u32 = 0;
const VK_DESCRIPTOR_TYPE_STORAGE_BUFFER: u32 = 7;
const VK_SHADER_STAGE_COMPUTE_BIT: VkFlags = 0x0000_0020;
const VK_PIPELINE_BIND_POINT_COMPUTE: u32 = 1;
const VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT: VkFlags = 0x0000_0002;
const VK_COMMAND_BUFFER_LEVEL_PRIMARY: u32 = 0;
const VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT: VkFlags = 0x0000_0001;
const VK_WHOLE_SIZE: VkDeviceSize = u64::MAX;

#[repr(C)]
struct VkApplicationInfo {
    s_type: u32,
    p_next: *const c_void,
    p_application_name: *const c_char,
    application_version: u32,
    p_engine_name: *const c_char,
    engine_version: u32,
    api_version: u32,
}

#[repr(C)]
struct VkInstanceCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: VkFlags,
    p_application_info: *const VkApplicationInfo,
    enabled_layer_count: u32,
    pp_enabled_layer_names: *const *const c_char,
    enabled_extension_count: u32,
    pp_enabled_extension_names: *const *const c_char,
}

#[repr(C)]
struct VkPhysicalDevicePropertiesBuf {
    api_version: u32,
    driver_version: u32,
    vendor_id: u32,
    device_id: u32,
    device_type: u32,
    device_name: [c_char; 256],
    /// Generous padding covering `pipelineCacheUUID[16]`, `VkPhysicalDeviceLimits`, and
    /// `VkPhysicalDeviceSparseProperties` (actual tail < 600 bytes on all 64-bit platforms).
    _tail_pad: [u64; 128],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VkMemoryType {
    property_flags: VkFlags,
    heap_index: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VkMemoryHeap {
    size: VkDeviceSize,
    flags: VkFlags,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VkPhysicalDeviceMemoryProperties {
    memory_type_count: u32,
    memory_types: [VkMemoryType; 32],
    memory_heap_count: u32,
    memory_heaps: [VkMemoryHeap; 16],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VkExtent3D {
    width: u32,
    height: u32,
    depth: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VkQueueFamilyProperties {
    queue_flags: VkFlags,
    queue_count: u32,
    timestamp_valid_bits: u32,
    min_image_transfer_granularity: VkExtent3D,
}

#[repr(C)]
struct VkDeviceQueueCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: VkFlags,
    queue_family_index: u32,
    queue_count: u32,
    p_queue_priorities: *const f32,
}

#[repr(C)]
struct VkDeviceCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: VkFlags,
    queue_create_info_count: u32,
    p_queue_create_infos: *const VkDeviceQueueCreateInfo,
    enabled_layer_count: u32,
    pp_enabled_layer_names: *const *const c_char,
    enabled_extension_count: u32,
    pp_enabled_extension_names: *const *const c_char,
    p_enabled_features: *const c_void,
}

#[repr(C)]
struct VkBufferCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: VkFlags,
    size: VkDeviceSize,
    usage: VkFlags,
    sharing_mode: u32,
    queue_family_index_count: u32,
    p_queue_family_indices: *const u32,
}

#[repr(C)]
struct VkMemoryRequirements {
    size: VkDeviceSize,
    alignment: VkDeviceSize,
    memory_type_bits: u32,
}

#[repr(C)]
struct VkMemoryAllocateInfo {
    s_type: u32,
    p_next: *const c_void,
    allocation_size: VkDeviceSize,
    memory_type_index: u32,
}

#[repr(C)]
struct VkShaderModuleCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: VkFlags,
    code_size: usize,
    p_code: *const u32,
}

#[repr(C)]
struct VkDescriptorSetLayoutBinding {
    binding: u32,
    descriptor_type: u32,
    descriptor_count: u32,
    stage_flags: VkFlags,
    p_immutable_samplers: *const u64,
}

#[repr(C)]
struct VkDescriptorSetLayoutCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: VkFlags,
    binding_count: u32,
    p_bindings: *const VkDescriptorSetLayoutBinding,
}

#[repr(C)]
struct VkPipelineLayoutCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: VkFlags,
    set_layout_count: u32,
    p_set_layouts: *const VkDescriptorSetLayout,
    push_constant_range_count: u32,
    p_push_constant_ranges: *const c_void,
}

#[repr(C)]
struct VkPipelineShaderStageCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: VkFlags,
    stage: VkFlags,
    module: VkShaderModule,
    p_name: *const c_char,
    p_specialization_info: *const c_void,
}

#[repr(C)]
struct VkComputePipelineCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: VkFlags,
    stage: VkPipelineShaderStageCreateInfo,
    layout: VkPipelineLayout,
    base_pipeline_handle: VkPipeline,
    base_pipeline_index: i32,
}

#[repr(C)]
struct VkDescriptorPoolSize {
    descriptor_type: u32,
    descriptor_count: u32,
}

#[repr(C)]
struct VkDescriptorPoolCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: VkFlags,
    max_sets: u32,
    pool_size_count: u32,
    p_pool_sizes: *const VkDescriptorPoolSize,
}

#[repr(C)]
struct VkDescriptorSetAllocateInfo {
    s_type: u32,
    p_next: *const c_void,
    descriptor_pool: VkDescriptorPool,
    descriptor_set_count: u32,
    p_set_layouts: *const VkDescriptorSetLayout,
}

#[repr(C)]
struct VkDescriptorBufferInfo {
    buffer: VkBuffer,
    offset: VkDeviceSize,
    range: VkDeviceSize,
}

#[repr(C)]
struct VkWriteDescriptorSet {
    s_type: u32,
    p_next: *const c_void,
    dst_set: VkDescriptorSet,
    dst_binding: u32,
    dst_array_element: u32,
    descriptor_count: u32,
    descriptor_type: u32,
    p_image_info: *const c_void,
    p_buffer_info: *const VkDescriptorBufferInfo,
    p_texel_buffer_view: *const u64,
}

#[repr(C)]
struct VkCommandPoolCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: VkFlags,
    queue_family_index: u32,
}

#[repr(C)]
struct VkCommandBufferAllocateInfo {
    s_type: u32,
    p_next: *const c_void,
    command_pool: VkCommandPool,
    level: u32,
    command_buffer_count: u32,
}

#[repr(C)]
struct VkCommandBufferBeginInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: VkFlags,
    p_inheritance_info: *const c_void,
}

#[repr(C)]
struct VkSubmitInfo {
    s_type: u32,
    p_next: *const c_void,
    wait_semaphore_count: u32,
    p_wait_semaphores: *const u64,
    p_wait_dst_stage_mask: *const VkFlags,
    command_buffer_count: u32,
    p_command_buffers: *const VkCommandBuffer,
    signal_semaphore_count: u32,
    p_signal_semaphores: *const u64,
}

type FnVkCreateInstance = unsafe extern "system" fn(
    *const VkInstanceCreateInfo,
    *const c_void,
    *mut VkInstance,
) -> VkResult;
type FnVkDestroyInstance = unsafe extern "system" fn(VkInstance, *const c_void);
type FnVkEnumeratePhysicalDevices =
    unsafe extern "system" fn(VkInstance, *mut u32, *mut VkPhysicalDevice) -> VkResult;
type FnVkGetPhysicalDeviceProperties =
    unsafe extern "system" fn(VkPhysicalDevice, *mut VkPhysicalDevicePropertiesBuf);
type FnVkGetPhysicalDeviceMemoryProperties =
    unsafe extern "system" fn(VkPhysicalDevice, *mut VkPhysicalDeviceMemoryProperties);
type FnVkGetPhysicalDeviceQueueFamilyProperties =
    unsafe extern "system" fn(VkPhysicalDevice, *mut u32, *mut VkQueueFamilyProperties);
type FnVkCreateDevice = unsafe extern "system" fn(
    VkPhysicalDevice,
    *const VkDeviceCreateInfo,
    *const c_void,
    *mut VkDevice,
) -> VkResult;
type FnVkDestroyDevice = unsafe extern "system" fn(VkDevice, *const c_void);
type FnVkGetDeviceQueue = unsafe extern "system" fn(VkDevice, u32, u32, *mut VkQueue);
type FnVkCreateBuffer = unsafe extern "system" fn(
    VkDevice,
    *const VkBufferCreateInfo,
    *const c_void,
    *mut VkBuffer,
) -> VkResult;
type FnVkDestroyBuffer = unsafe extern "system" fn(VkDevice, VkBuffer, *const c_void);
type FnVkGetBufferMemoryRequirements =
    unsafe extern "system" fn(VkDevice, VkBuffer, *mut VkMemoryRequirements);
type FnVkAllocateMemory = unsafe extern "system" fn(
    VkDevice,
    *const VkMemoryAllocateInfo,
    *const c_void,
    *mut VkDeviceMemory,
) -> VkResult;
type FnVkFreeMemory = unsafe extern "system" fn(VkDevice, VkDeviceMemory, *const c_void);
type FnVkBindBufferMemory =
    unsafe extern "system" fn(VkDevice, VkBuffer, VkDeviceMemory, VkDeviceSize) -> VkResult;
type FnVkMapMemory = unsafe extern "system" fn(
    VkDevice,
    VkDeviceMemory,
    VkDeviceSize,
    VkDeviceSize,
    VkFlags,
    *mut *mut c_void,
) -> VkResult;
type FnVkUnmapMemory = unsafe extern "system" fn(VkDevice, VkDeviceMemory);
type FnVkCreateShaderModule = unsafe extern "system" fn(
    VkDevice,
    *const VkShaderModuleCreateInfo,
    *const c_void,
    *mut VkShaderModule,
) -> VkResult;
type FnVkDestroyShaderModule = unsafe extern "system" fn(VkDevice, VkShaderModule, *const c_void);
type FnVkCreateDescriptorSetLayout = unsafe extern "system" fn(
    VkDevice,
    *const VkDescriptorSetLayoutCreateInfo,
    *const c_void,
    *mut VkDescriptorSetLayout,
) -> VkResult;
type FnVkDestroyDescriptorSetLayout =
    unsafe extern "system" fn(VkDevice, VkDescriptorSetLayout, *const c_void);
type FnVkCreatePipelineLayout = unsafe extern "system" fn(
    VkDevice,
    *const VkPipelineLayoutCreateInfo,
    *const c_void,
    *mut VkPipelineLayout,
) -> VkResult;
type FnVkDestroyPipelineLayout =
    unsafe extern "system" fn(VkDevice, VkPipelineLayout, *const c_void);
type FnVkCreateComputePipelines = unsafe extern "system" fn(
    VkDevice,
    VkPipelineCache,
    u32,
    *const VkComputePipelineCreateInfo,
    *const c_void,
    *mut VkPipeline,
) -> VkResult;
type FnVkDestroyPipeline = unsafe extern "system" fn(VkDevice, VkPipeline, *const c_void);
type FnVkCreateDescriptorPool = unsafe extern "system" fn(
    VkDevice,
    *const VkDescriptorPoolCreateInfo,
    *const c_void,
    *mut VkDescriptorPool,
) -> VkResult;
type FnVkDestroyDescriptorPool =
    unsafe extern "system" fn(VkDevice, VkDescriptorPool, *const c_void);
type FnVkAllocateDescriptorSets = unsafe extern "system" fn(
    VkDevice,
    *const VkDescriptorSetAllocateInfo,
    *mut VkDescriptorSet,
) -> VkResult;
type FnVkUpdateDescriptorSets =
    unsafe extern "system" fn(VkDevice, u32, *const VkWriteDescriptorSet, u32, *const c_void);
type FnVkCreateCommandPool = unsafe extern "system" fn(
    VkDevice,
    *const VkCommandPoolCreateInfo,
    *const c_void,
    *mut VkCommandPool,
) -> VkResult;
type FnVkDestroyCommandPool = unsafe extern "system" fn(VkDevice, VkCommandPool, *const c_void);
type FnVkAllocateCommandBuffers = unsafe extern "system" fn(
    VkDevice,
    *const VkCommandBufferAllocateInfo,
    *mut VkCommandBuffer,
) -> VkResult;
type FnVkResetCommandBuffer = unsafe extern "system" fn(VkCommandBuffer, VkFlags) -> VkResult;
type FnVkBeginCommandBuffer =
    unsafe extern "system" fn(VkCommandBuffer, *const VkCommandBufferBeginInfo) -> VkResult;
type FnVkEndCommandBuffer = unsafe extern "system" fn(VkCommandBuffer) -> VkResult;
type FnVkCmdBindPipeline = unsafe extern "system" fn(VkCommandBuffer, u32, VkPipeline);
type FnVkCmdBindDescriptorSets = unsafe extern "system" fn(
    VkCommandBuffer,
    u32,
    VkPipelineLayout,
    u32,
    u32,
    *const VkDescriptorSet,
    u32,
    *const u32,
);
type FnVkCmdDispatch = unsafe extern "system" fn(VkCommandBuffer, u32, u32, u32);
type FnVkQueueSubmit =
    unsafe extern "system" fn(VkQueue, u32, *const VkSubmitInfo, VkFence) -> VkResult;
type FnVkQueueWaitIdle = unsafe extern "system" fn(VkQueue) -> VkResult;

struct VulkanApi {
    _lib: DynLib,
    create_instance: FnVkCreateInstance,
    destroy_instance: FnVkDestroyInstance,
    enumerate_physical_devices: FnVkEnumeratePhysicalDevices,
    get_physical_device_properties: FnVkGetPhysicalDeviceProperties,
    get_physical_device_memory_properties: FnVkGetPhysicalDeviceMemoryProperties,
    get_physical_device_queue_family_properties: FnVkGetPhysicalDeviceQueueFamilyProperties,
    create_device: FnVkCreateDevice,
    destroy_device: FnVkDestroyDevice,
    get_device_queue: FnVkGetDeviceQueue,
    create_buffer: FnVkCreateBuffer,
    destroy_buffer: FnVkDestroyBuffer,
    get_buffer_memory_requirements: FnVkGetBufferMemoryRequirements,
    allocate_memory: FnVkAllocateMemory,
    free_memory: FnVkFreeMemory,
    bind_buffer_memory: FnVkBindBufferMemory,
    map_memory: FnVkMapMemory,
    unmap_memory: FnVkUnmapMemory,
    create_shader_module: FnVkCreateShaderModule,
    destroy_shader_module: FnVkDestroyShaderModule,
    create_descriptor_set_layout: FnVkCreateDescriptorSetLayout,
    destroy_descriptor_set_layout: FnVkDestroyDescriptorSetLayout,
    create_pipeline_layout: FnVkCreatePipelineLayout,
    destroy_pipeline_layout: FnVkDestroyPipelineLayout,
    create_compute_pipelines: FnVkCreateComputePipelines,
    destroy_pipeline: FnVkDestroyPipeline,
    create_descriptor_pool: FnVkCreateDescriptorPool,
    destroy_descriptor_pool: FnVkDestroyDescriptorPool,
    allocate_descriptor_sets: FnVkAllocateDescriptorSets,
    update_descriptor_sets: FnVkUpdateDescriptorSets,
    create_command_pool: FnVkCreateCommandPool,
    destroy_command_pool: FnVkDestroyCommandPool,
    allocate_command_buffers: FnVkAllocateCommandBuffers,
    reset_command_buffer: FnVkResetCommandBuffer,
    begin_command_buffer: FnVkBeginCommandBuffer,
    end_command_buffer: FnVkEndCommandBuffer,
    cmd_bind_pipeline: FnVkCmdBindPipeline,
    cmd_bind_descriptor_sets: FnVkCmdBindDescriptorSets,
    cmd_dispatch: FnVkCmdDispatch,
    queue_submit: FnVkQueueSubmit,
    queue_wait_idle: FnVkQueueWaitIdle,
}

impl VulkanApi {
    fn load() -> Option<Arc<Self>> {
        let candidates: &[&str] = if cfg!(windows) {
            &["vulkan-1.dll"]
        } else if cfg!(target_os = "macos") {
            &["libvulkan.1.dylib", "libMoltenVK.dylib"]
        } else {
            &["libvulkan.so.1", "libvulkan.so"]
        };
        let lib = DynLib::open_any(candidates)?;
        // SAFETY: Every symbol name and signature matches Khronos `vulkan_core.h` 1.0 C ABI.
        unsafe {
            Some(Arc::new(Self {
                create_instance: lib.get_symbol(b"vkCreateInstance\0")?,
                destroy_instance: lib.get_symbol(b"vkDestroyInstance\0")?,
                enumerate_physical_devices: lib.get_symbol(b"vkEnumeratePhysicalDevices\0")?,
                get_physical_device_properties: lib
                    .get_symbol(b"vkGetPhysicalDeviceProperties\0")?,
                get_physical_device_memory_properties: lib
                    .get_symbol(b"vkGetPhysicalDeviceMemoryProperties\0")?,
                get_physical_device_queue_family_properties: lib
                    .get_symbol(b"vkGetPhysicalDeviceQueueFamilyProperties\0")?,
                create_device: lib.get_symbol(b"vkCreateDevice\0")?,
                destroy_device: lib.get_symbol(b"vkDestroyDevice\0")?,
                get_device_queue: lib.get_symbol(b"vkGetDeviceQueue\0")?,
                create_buffer: lib.get_symbol(b"vkCreateBuffer\0")?,
                destroy_buffer: lib.get_symbol(b"vkDestroyBuffer\0")?,
                get_buffer_memory_requirements: lib
                    .get_symbol(b"vkGetBufferMemoryRequirements\0")?,
                allocate_memory: lib.get_symbol(b"vkAllocateMemory\0")?,
                free_memory: lib.get_symbol(b"vkFreeMemory\0")?,
                bind_buffer_memory: lib.get_symbol(b"vkBindBufferMemory\0")?,
                map_memory: lib.get_symbol(b"vkMapMemory\0")?,
                unmap_memory: lib.get_symbol(b"vkUnmapMemory\0")?,
                create_shader_module: lib.get_symbol(b"vkCreateShaderModule\0")?,
                destroy_shader_module: lib.get_symbol(b"vkDestroyShaderModule\0")?,
                create_descriptor_set_layout: lib.get_symbol(b"vkCreateDescriptorSetLayout\0")?,
                destroy_descriptor_set_layout: lib.get_symbol(b"vkDestroyDescriptorSetLayout\0")?,
                create_pipeline_layout: lib.get_symbol(b"vkCreatePipelineLayout\0")?,
                destroy_pipeline_layout: lib.get_symbol(b"vkDestroyPipelineLayout\0")?,
                create_compute_pipelines: lib.get_symbol(b"vkCreateComputePipelines\0")?,
                destroy_pipeline: lib.get_symbol(b"vkDestroyPipeline\0")?,
                create_descriptor_pool: lib.get_symbol(b"vkCreateDescriptorPool\0")?,
                destroy_descriptor_pool: lib.get_symbol(b"vkDestroyDescriptorPool\0")?,
                allocate_descriptor_sets: lib.get_symbol(b"vkAllocateDescriptorSets\0")?,
                update_descriptor_sets: lib.get_symbol(b"vkUpdateDescriptorSets\0")?,
                create_command_pool: lib.get_symbol(b"vkCreateCommandPool\0")?,
                destroy_command_pool: lib.get_symbol(b"vkDestroyCommandPool\0")?,
                allocate_command_buffers: lib.get_symbol(b"vkAllocateCommandBuffers\0")?,
                reset_command_buffer: lib.get_symbol(b"vkResetCommandBuffer\0")?,
                begin_command_buffer: lib.get_symbol(b"vkBeginCommandBuffer\0")?,
                end_command_buffer: lib.get_symbol(b"vkEndCommandBuffer\0")?,
                cmd_bind_pipeline: lib.get_symbol(b"vkCmdBindPipeline\0")?,
                cmd_bind_descriptor_sets: lib.get_symbol(b"vkCmdBindDescriptorSets\0")?,
                cmd_dispatch: lib.get_symbol(b"vkCmdDispatch\0")?,
                queue_submit: lib.get_symbol(b"vkQueueSubmit\0")?,
                queue_wait_idle: lib.get_symbol(b"vkQueueWaitIdle\0")?,
                _lib: lib,
            }))
        }
    }
}

struct VkInstanceGuard {
    api: Arc<VulkanApi>,
    instance: VkInstance,
}

// SAFETY: `VkInstance` is an immutable process-wide handle destroyed once when the last
// `VulkanDeviceBackend` drops its `Arc<VkInstanceGuard>`.
unsafe impl Send for VkInstanceGuard {}
// SAFETY: Read-only Vulkan instance handle shared via `Arc`.
unsafe impl Sync for VkInstanceGuard {}

impl Drop for VkInstanceGuard {
    fn drop(&mut self) {
        if !self.instance.is_null() {
            // SAFETY: Destroyed only after every child `VkDevice` has already been destroyed.
            unsafe {
                (self.api.destroy_instance)(self.instance, ptr::null());
            }
        }
    }
}

// ── Pure-Rust SPIR-V 1.0 Compute Shader Emitter ──────────────────────────────

struct SpirvBuilder {
    words: Vec<u32>,
    next_id: u32,
}

impl SpirvBuilder {
    fn new() -> Self {
        Self {
            words: vec![
                0x0723_0203, // Magic
                0x0001_0000, // SPIR-V 1.0
                0x0000_0000, // Generator
                0,           // Bound (patched on finish)
                0,           // Schema
            ],
            next_id: 1,
        }
    }

    fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn inst(&mut self, opcode: u16, operands: &[u32]) {
        let word_count = (1 + operands.len()) as u32;
        self.words.push((word_count << 16) | u32::from(opcode));
        self.words.extend_from_slice(operands);
    }

    fn push_str_words(dst: &mut Vec<u32>, s: &str) {
        let bytes = s.as_bytes();
        let total = bytes.len() + 1; // include NUL terminator
        let n_words = total.div_ceil(4);
        for w in 0..n_words {
            let mut word = 0u32;
            for b in 0..4 {
                let idx = w * 4 + b;
                let byte = if idx < bytes.len() { bytes[idx] } else { 0 };
                word |= u32::from(byte) << (b * 8);
            }
            dst.push(word);
        }
    }

    fn finish(mut self) -> Vec<u32> {
        self.words[3] = self.next_id;
        self.words
    }
}

/// Emit a valid SPIR-V 1.0 `GLCompute` shader computing:
/// `Out[row * N + col] = act(sum_{t=0..K} A[row * K + t] * B[t * N + col] + bias)`
/// Bindings (DescriptorSet 0):
/// - 0: `A: f32[]`, 1: `B: f32[]`, 2: `Bias: f32[]`, 3: `Out: f32[]`,
/// - 4: `Params: u32[]` = `[M, N, K, has_bias, bias_len, act_code]`
///   (`act_code`: `0=None`, `1=Relu`, `2=Gelu`, `3=Silu`, `4=Tanh`, `5=Sigmoid`).
fn emit_spirv_fused_linear_f32() -> Vec<u32> {
    const OP_EXT_INST_IMPORT: u16 = 11;
    const OP_EXT_INST: u16 = 12;
    const OP_MEMORY_MODEL: u16 = 14;
    const OP_ENTRY_POINT: u16 = 15;
    const OP_EXECUTION_MODE: u16 = 16;
    const OP_CAPABILITY: u16 = 17;
    const OP_TYPE_VOID: u16 = 19;
    const OP_TYPE_BOOL: u16 = 20;
    const OP_TYPE_INT: u16 = 21;
    const OP_TYPE_FLOAT: u16 = 22;
    const OP_TYPE_VECTOR: u16 = 23;
    const OP_TYPE_RUNTIME_ARRAY: u16 = 29;
    const OP_TYPE_STRUCT: u16 = 30;
    const OP_TYPE_POINTER: u16 = 32;
    const OP_TYPE_FUNCTION: u16 = 33;
    const OP_CONSTANT: u16 = 43;
    const OP_FUNCTION: u16 = 54;
    const OP_FUNCTION_END: u16 = 56;
    const OP_VARIABLE: u16 = 59;
    const OP_LOAD: u16 = 61;
    const OP_STORE: u16 = 62;
    const OP_ACCESS_CHAIN: u16 = 65;
    const OP_DECORATE: u16 = 71;
    const OP_MEMBER_DECORATE: u16 = 72;
    const OP_COMPOSITE_EXTRACT: u16 = 81;
    const OP_FNEGATE: u16 = 127;
    const OP_IADD: u16 = 128;
    const OP_FADD: u16 = 129;
    const OP_IMUL: u16 = 132;
    const OP_FMUL: u16 = 133;
    const OP_FDIV: u16 = 136;
    const OP_UMOD: u16 = 137;
    const OP_LOGICAL_AND: u16 = 167;
    const OP_SELECT: u16 = 169;
    const OP_IEQUAL: u16 = 170;
    const OP_INOT_EQUAL: u16 = 171;
    const OP_ULESS_THAN: u16 = 176;
    const OP_PHI: u16 = 245;
    const OP_LOOP_MERGE: u16 = 246;
    const OP_SELECTION_MERGE: u16 = 247;
    const OP_LABEL: u16 = 248;
    const OP_BRANCH: u16 = 249;
    const OP_BRANCH_CONDITIONAL: u16 = 250;
    const OP_RETURN: u16 = 253;

    const GLSL_TANH: u32 = 21;
    const GLSL_EXP: u32 = 27;
    const GLSL_FMAX: u32 = 40;
    const GLSL_FCLAMP: u32 = 43;

    const SC_INPUT: u32 = 1;
    const SC_UNIFORM: u32 = 2;

    const DEC_BUFFER_BLOCK: u32 = 3;
    const DEC_ARRAY_STRIDE: u32 = 6;
    const DEC_BUILT_IN: u32 = 11;
    const DEC_BINDING: u32 = 33;
    const DEC_DESCRIPTOR_SET: u32 = 34;
    const DEC_OFFSET: u32 = 35;

    let mut b = SpirvBuilder::new();

    // Capabilities & GLSL.std.450 import
    b.inst(OP_CAPABILITY, &[1]); // Shader
    let glsl_ext = b.alloc_id();
    let mut ext_ops = vec![glsl_ext];
    SpirvBuilder::push_str_words(&mut ext_ops, "GLSL.std.450");
    b.inst(OP_EXT_INST_IMPORT, &ext_ops);
    b.inst(OP_MEMORY_MODEL, &[0, 1]); // Logical, GLSL450

    let main_fn = b.alloc_id();
    let var_gid = b.alloc_id();

    let mut ep_ops = vec![5, main_fn]; // GLCompute = 5
    SpirvBuilder::push_str_words(&mut ep_ops, "main");
    ep_ops.push(var_gid);
    b.inst(OP_ENTRY_POINT, &ep_ops);
    b.inst(OP_EXECUTION_MODE, &[main_fn, 17, 16, 16, 1]); // LocalSize = 17

    // Forward-allocate type & variable IDs used in decorations
    let ty_f32_arr = b.alloc_id();
    let ty_f32_buf = b.alloc_id();
    let ty_u32_arr = b.alloc_id();
    let ty_u32_buf = b.alloc_id();
    let var_a = b.alloc_id();
    let var_b = b.alloc_id();
    let var_bias = b.alloc_id();
    let var_out = b.alloc_id();
    let var_params = b.alloc_id();

    // Decorations
    b.inst(OP_DECORATE, &[var_gid, DEC_BUILT_IN, 28]); // GlobalInvocationId = 28
    b.inst(OP_DECORATE, &[ty_f32_arr, DEC_ARRAY_STRIDE, 4]);
    b.inst(OP_DECORATE, &[ty_f32_buf, DEC_BUFFER_BLOCK]);
    b.inst(OP_MEMBER_DECORATE, &[ty_f32_buf, 0, DEC_OFFSET, 0]);
    b.inst(OP_DECORATE, &[ty_u32_arr, DEC_ARRAY_STRIDE, 4]);
    b.inst(OP_DECORATE, &[ty_u32_buf, DEC_BUFFER_BLOCK]);
    b.inst(OP_MEMBER_DECORATE, &[ty_u32_buf, 0, DEC_OFFSET, 0]);
    for (binding, var_id) in [
        (0u32, var_a),
        (1, var_b),
        (2, var_bias),
        (3, var_out),
        (4, var_params),
    ] {
        b.inst(OP_DECORATE, &[var_id, DEC_DESCRIPTOR_SET, 0]);
        b.inst(OP_DECORATE, &[var_id, DEC_BINDING, binding]);
    }

    // Types
    let ty_void = b.alloc_id();
    b.inst(OP_TYPE_VOID, &[ty_void]);
    let ty_fn = b.alloc_id();
    b.inst(OP_TYPE_FUNCTION, &[ty_fn, ty_void]);
    let ty_bool = b.alloc_id();
    b.inst(OP_TYPE_BOOL, &[ty_bool]);
    let ty_u32 = b.alloc_id();
    b.inst(OP_TYPE_INT, &[ty_u32, 32, 0]);
    let ty_f32 = b.alloc_id();
    b.inst(OP_TYPE_FLOAT, &[ty_f32, 32]);
    let ty_uvec3 = b.alloc_id();
    b.inst(OP_TYPE_VECTOR, &[ty_uvec3, ty_u32, 3]);
    let ptr_in_uvec3 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_in_uvec3, SC_INPUT, ty_uvec3]);

    b.inst(OP_TYPE_RUNTIME_ARRAY, &[ty_f32_arr, ty_f32]);
    b.inst(OP_TYPE_STRUCT, &[ty_f32_buf, ty_f32_arr]);
    let ptr_unif_f32_buf = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_unif_f32_buf, SC_UNIFORM, ty_f32_buf]);
    let ptr_unif_f32 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_unif_f32, SC_UNIFORM, ty_f32]);

    b.inst(OP_TYPE_RUNTIME_ARRAY, &[ty_u32_arr, ty_u32]);
    b.inst(OP_TYPE_STRUCT, &[ty_u32_buf, ty_u32_arr]);
    let ptr_unif_u32_buf = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_unif_u32_buf, SC_UNIFORM, ty_u32_buf]);
    let ptr_unif_u32 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_unif_u32, SC_UNIFORM, ty_u32]);

    // Constants
    let c_u0 = b.alloc_id();
    b.inst(OP_CONSTANT, &[ty_u32, c_u0, 0]);
    let c_u1 = b.alloc_id();
    b.inst(OP_CONSTANT, &[ty_u32, c_u1, 1]);
    let c_u2 = b.alloc_id();
    b.inst(OP_CONSTANT, &[ty_u32, c_u2, 2]);
    let c_u3 = b.alloc_id();
    b.inst(OP_CONSTANT, &[ty_u32, c_u3, 3]);
    let c_u4 = b.alloc_id();
    b.inst(OP_CONSTANT, &[ty_u32, c_u4, 4]);
    let c_u5 = b.alloc_id();
    b.inst(OP_CONSTANT, &[ty_u32, c_u5, 5]);

    let c_f0 = b.alloc_id();
    b.inst(OP_CONSTANT, &[ty_f32, c_f0, 0.0f32.to_bits()]);
    let c_f1 = b.alloc_id();
    b.inst(OP_CONSTANT, &[ty_f32, c_f1, 1.0f32.to_bits()]);
    let c_f05 = b.alloc_id();
    b.inst(OP_CONSTANT, &[ty_f32, c_f05, 0.5f32.to_bits()]);
    let c_gelu_a = b.alloc_id();
    b.inst(OP_CONSTANT, &[ty_f32, c_gelu_a, 0.797_884_6_f32.to_bits()]);
    let c_gelu_b = b.alloc_id();
    b.inst(OP_CONSTANT, &[ty_f32, c_gelu_b, 0.044_715_f32.to_bits()]);
    let c_f_lo = b.alloc_id();
    b.inst(OP_CONSTANT, &[ty_f32, c_f_lo, (-40.0f32).to_bits()]);
    let c_f_hi = b.alloc_id();
    b.inst(OP_CONSTANT, &[ty_f32, c_f_hi, 40.0f32.to_bits()]);

    // Module-scope variables
    b.inst(OP_VARIABLE, &[ptr_in_uvec3, var_gid, SC_INPUT]);
    b.inst(OP_VARIABLE, &[ptr_unif_f32_buf, var_a, SC_UNIFORM]);
    b.inst(OP_VARIABLE, &[ptr_unif_f32_buf, var_b, SC_UNIFORM]);
    b.inst(OP_VARIABLE, &[ptr_unif_f32_buf, var_bias, SC_UNIFORM]);
    b.inst(OP_VARIABLE, &[ptr_unif_f32_buf, var_out, SC_UNIFORM]);
    b.inst(OP_VARIABLE, &[ptr_unif_u32_buf, var_params, SC_UNIFORM]);

    // Function main()
    b.inst(OP_FUNCTION, &[ty_void, main_fn, 0, ty_fn]);
    let lbl_entry = b.alloc_id();
    let lbl_body = b.alloc_id();
    let lbl_loop_hdr = b.alloc_id();
    let lbl_loop_body = b.alloc_id();
    let lbl_loop_cont = b.alloc_id();
    let lbl_loop_exit = b.alloc_id();
    let lbl_exit = b.alloc_id();

    b.inst(OP_LABEL, &[lbl_entry]);

    // Load GlobalInvocationId.x (col) & .y (row)
    let gid_val = b.alloc_id();
    b.inst(OP_LOAD, &[ty_uvec3, gid_val, var_gid]);
    let col = b.alloc_id();
    b.inst(OP_COMPOSITE_EXTRACT, &[ty_u32, col, gid_val, 0]);
    let row = b.alloc_id();
    b.inst(OP_COMPOSITE_EXTRACT, &[ty_u32, row, gid_val, 1]);

    // Load Params[0..5] = M, N, K, has_bias, bias_len, act_code
    let load_param = |b: &mut SpirvBuilder, idx_c: u32| -> u32 {
        let p = b.alloc_id();
        b.inst(OP_ACCESS_CHAIN, &[ptr_unif_u32, p, var_params, c_u0, idx_c]);
        let v = b.alloc_id();
        b.inst(OP_LOAD, &[ty_u32, v, p]);
        v
    };
    let p_m = load_param(&mut b, c_u0);
    let p_n = load_param(&mut b, c_u1);
    let p_k = load_param(&mut b, c_u2);
    let p_has_bias = load_param(&mut b, c_u3);
    let p_bias_len = load_param(&mut b, c_u4);
    let p_act = load_param(&mut b, c_u5);

    let cmp_row = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, cmp_row, row, p_m]);
    let cmp_col = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, cmp_col, col, p_n]);
    let in_bounds = b.alloc_id();
    b.inst(OP_LOGICAL_AND, &[ty_bool, in_bounds, cmp_row, cmp_col]);
    b.inst(OP_SELECTION_MERGE, &[lbl_exit, 0]);
    b.inst(OP_BRANCH_CONDITIONAL, &[in_bounds, lbl_body, lbl_exit]);

    // Body: row_k = row * K
    b.inst(OP_LABEL, &[lbl_body]);
    let row_k = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, row_k, row, p_k]);
    b.inst(OP_BRANCH, &[lbl_loop_hdr]);

    // Loop header (SSA Phi nodes for t and acc)
    let t_phi = b.alloc_id();
    let acc_phi = b.alloc_id();
    let t_next = b.alloc_id();
    let acc_next = b.alloc_id();

    b.inst(OP_LABEL, &[lbl_loop_hdr]);
    b.inst(
        OP_PHI,
        &[ty_u32, t_phi, c_u0, lbl_body, t_next, lbl_loop_cont],
    );
    b.inst(
        OP_PHI,
        &[ty_f32, acc_phi, c_f0, lbl_body, acc_next, lbl_loop_cont],
    );
    let loop_cond = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, loop_cond, t_phi, p_k]);
    b.inst(OP_LOOP_MERGE, &[lbl_loop_exit, lbl_loop_cont, 0]);
    b.inst(
        OP_BRANCH_CONDITIONAL,
        &[loop_cond, lbl_loop_body, lbl_loop_exit],
    );

    // Loop body
    b.inst(OP_LABEL, &[lbl_loop_body]);
    let a_idx = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, a_idx, row_k, t_phi]);
    let a_ptr = b.alloc_id();
    b.inst(OP_ACCESS_CHAIN, &[ptr_unif_f32, a_ptr, var_a, c_u0, a_idx]);
    let a_val = b.alloc_id();
    b.inst(OP_LOAD, &[ty_f32, a_val, a_ptr]);

    let tn = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, tn, t_phi, p_n]);
    let b_idx = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, b_idx, tn, col]);
    let b_ptr = b.alloc_id();
    b.inst(OP_ACCESS_CHAIN, &[ptr_unif_f32, b_ptr, var_b, c_u0, b_idx]);
    let b_val = b.alloc_id();
    b.inst(OP_LOAD, &[ty_f32, b_val, b_ptr]);

    let prod = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, prod, a_val, b_val]);
    b.inst(OP_FADD, &[ty_f32, acc_next, acc_phi, prod]);
    b.inst(OP_BRANCH, &[lbl_loop_cont]);

    // Loop continue
    b.inst(OP_LABEL, &[lbl_loop_cont]);
    b.inst(OP_IADD, &[ty_u32, t_next, t_phi, c_u1]);
    b.inst(OP_BRANCH, &[lbl_loop_hdr]);

    // After dot product: optional bias + activation
    b.inst(OP_LABEL, &[lbl_loop_exit]);
    let bias_idx = b.alloc_id();
    b.inst(OP_UMOD, &[ty_u32, bias_idx, col, p_bias_len]);
    let bias_ptr = b.alloc_id();
    b.inst(
        OP_ACCESS_CHAIN,
        &[ptr_unif_f32, bias_ptr, var_bias, c_u0, bias_idx],
    );
    let bias_val = b.alloc_id();
    b.inst(OP_LOAD, &[ty_f32, bias_val, bias_ptr]);
    let acc_plus_bias = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, acc_plus_bias, acc_phi, bias_val]);
    let has_bias_bool = b.alloc_id();
    b.inst(OP_INOT_EQUAL, &[ty_bool, has_bias_bool, p_has_bias, c_u0]);
    let x = b.alloc_id();
    b.inst(
        OP_SELECT,
        &[ty_f32, x, has_bias_bool, acc_plus_bias, acc_phi],
    );

    // Compute activations cleanly (clamped for exp stability)
    let v_relu = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, v_relu, glsl_ext, GLSL_FMAX, x, c_f0]);

    let neg_x = b.alloc_id();
    b.inst(OP_FNEGATE, &[ty_f32, neg_x, x]);
    let neg_x_c = b.alloc_id();
    b.inst(
        OP_EXT_INST,
        &[
            ty_f32,
            neg_x_c,
            glsl_ext,
            GLSL_FCLAMP,
            neg_x,
            c_f_lo,
            c_f_hi,
        ],
    );
    let exp_neg_x = b.alloc_id();
    b.inst(
        OP_EXT_INST,
        &[ty_f32, exp_neg_x, glsl_ext, GLSL_EXP, neg_x_c],
    );
    let one_p_exp = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, one_p_exp, c_f1, exp_neg_x]);
    let v_sig = b.alloc_id();
    b.inst(OP_FDIV, &[ty_f32, v_sig, c_f1, one_p_exp]);
    let v_silu = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, v_silu, x, v_sig]);

    let v_tanh = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, v_tanh, glsl_ext, GLSL_TANH, x]);

    // GELU = 0.5 * x * (1 + tanh(sqrt(2/pi) * (x + 0.044715 * x^3)))
    let x2 = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, x2, x, x]);
    let x3 = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, x3, x2, x]);
    let cx3 = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, cx3, c_gelu_b, x3]);
    let inner1 = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, inner1, x, cx3]);
    let inner2 = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, inner2, c_gelu_a, inner1]);
    let th = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, th, glsl_ext, GLSL_TANH, inner2]);
    let one_p_th = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, one_p_th, c_f1, th]);
    let half_x = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, half_x, c_f05, x]);
    let v_gelu = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, v_gelu, half_x, one_p_th]);

    // Select final activation value based on act_code (0..=5)
    let mut cur = x;
    for (code_c, act_val) in [
        (c_u1, v_relu),
        (c_u2, v_gelu),
        (c_u3, v_silu),
        (c_u4, v_tanh),
        (c_u5, v_sig),
    ] {
        let eq = b.alloc_id();
        b.inst(OP_IEQUAL, &[ty_bool, eq, p_act, code_c]);
        let next = b.alloc_id();
        b.inst(OP_SELECT, &[ty_f32, next, eq, act_val, cur]);
        cur = next;
    }

    // Out[row * N + col] = cur
    let row_n = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, row_n, row, p_n]);
    let out_idx = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, out_idx, row_n, col]);
    let out_ptr = b.alloc_id();
    b.inst(
        OP_ACCESS_CHAIN,
        &[ptr_unif_f32, out_ptr, var_out, c_u0, out_idx],
    );
    b.inst(OP_STORE, &[out_ptr, cur]);
    b.inst(OP_BRANCH, &[lbl_exit]);

    b.inst(OP_LABEL, &[lbl_exit]);
    b.inst(OP_RETURN, &[]);
    b.inst(OP_FUNCTION_END, &[]);

    b.finish()
}

struct VulkanState {
    vk_dev: VkDevice,
    queue: VkQueue,
    shader: VkShaderModule,
    ds_layout: VkDescriptorSetLayout,
    pipe_layout: VkPipelineLayout,
    pipeline: VkPipeline,
    desc_pool: VkDescriptorPool,
    desc_set: VkDescriptorSet,
    cmd_pool: VkCommandPool,
    cmd_buf: VkCommandBuffer,
    mem_props: VkPhysicalDeviceMemoryProperties,
}

/// Live Vulkan 1.0 hardware compute [`DeviceBackend`] dispatching pure-Rust SPIR-V shaders.
pub(crate) struct VulkanDeviceBackend {
    device: Device,
    _inst_guard: Arc<VkInstanceGuard>,
    api: Arc<VulkanApi>,
    info: GpuDeviceInfo,
    state: Mutex<VulkanState>,
}

// SAFETY: All Vulkan device/queue/command-buffer operations are serialized under `self.state.lock()`.
unsafe impl Send for VulkanDeviceBackend {}
// SAFETY: Shared across tasks via `Mutex<VulkanState>` ensuring external synchronization rules.
unsafe impl Sync for VulkanDeviceBackend {}

impl Drop for VulkanDeviceBackend {
    fn drop(&mut self) {
        let st = self.state.lock();
        // SAFETY: Handles are destroyed in strict reverse creation order before `vkDestroyDevice`.
        unsafe {
            let _ = (self.api.queue_wait_idle)(st.queue);
            (self.api.destroy_command_pool)(st.vk_dev, st.cmd_pool, ptr::null());
            (self.api.destroy_descriptor_pool)(st.vk_dev, st.desc_pool, ptr::null());
            (self.api.destroy_pipeline)(st.vk_dev, st.pipeline, ptr::null());
            (self.api.destroy_pipeline_layout)(st.vk_dev, st.pipe_layout, ptr::null());
            (self.api.destroy_descriptor_set_layout)(st.vk_dev, st.ds_layout, ptr::null());
            (self.api.destroy_shader_module)(st.vk_dev, st.shader, ptr::null());
            (self.api.destroy_device)(st.vk_dev, ptr::null());
        }
    }
}

struct VkBufferAlloc {
    buffer: VkBuffer,
    memory: VkDeviceMemory,
}

impl VulkanDeviceBackend {
    fn find_host_mem_type(props: &VkPhysicalDeviceMemoryProperties, type_bits: u32) -> Option<u32> {
        let needed = VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT | VK_MEMORY_PROPERTY_HOST_COHERENT_BIT;
        (0..props.memory_type_count.min(32)).find(|&i| {
            (type_bits & (1u32 << i)) != 0
                && (props.memory_types[i as usize].property_flags & needed) == needed
        })
    }

    fn alloc_host_buffer(
        api: &VulkanApi,
        st: &VulkanState,
        size_bytes: usize,
    ) -> MoraResult<VkBufferAlloc> {
        let alloc_size = size_bytes.max(16) as VkDeviceSize;
        let bci = VkBufferCreateInfo {
            s_type: VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
            p_next: ptr::null(),
            flags: 0,
            size: alloc_size,
            usage: VK_BUFFER_USAGE_STORAGE_BUFFER_BIT,
            sharing_mode: VK_SHARING_MODE_EXCLUSIVE,
            queue_family_index_count: 0,
            p_queue_family_indices: ptr::null(),
        };
        let mut buffer: VkBuffer = 0;
        // SAFETY: Valid `VkBufferCreateInfo` and live `VkDevice`.
        let rc = unsafe { (api.create_buffer)(st.vk_dev, &bci, ptr::null(), &mut buffer) };
        if rc != VK_SUCCESS {
            return Err(MoraError::Runtime(format!(
                "Vulkan vkCreateBuffer failed: VkResult({rc})"
            )));
        }
        let mut reqs = VkMemoryRequirements {
            size: 0,
            alignment: 0,
            memory_type_bits: 0,
        };
        // SAFETY: `buffer` is a valid `VkBuffer` owned by `st.vk_dev`.
        unsafe {
            (api.get_buffer_memory_requirements)(st.vk_dev, buffer, &mut reqs);
        }
        let Some(mem_idx) = Self::find_host_mem_type(&st.mem_props, reqs.memory_type_bits) else {
            // SAFETY: Clean up `buffer` on memory type lookup failure.
            unsafe {
                (api.destroy_buffer)(st.vk_dev, buffer, ptr::null());
            }
            return Err(MoraError::Runtime(
                "Vulkan: no HOST_VISIBLE|HOST_COHERENT memory type available".to_string(),
            ));
        };
        let mai = VkMemoryAllocateInfo {
            s_type: VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
            p_next: ptr::null(),
            allocation_size: reqs.size,
            memory_type_index: mem_idx,
        };
        let mut memory: VkDeviceMemory = 0;
        // SAFETY: Valid `VkMemoryAllocateInfo` matching buffer memory requirements.
        let rc = unsafe { (api.allocate_memory)(st.vk_dev, &mai, ptr::null(), &mut memory) };
        if rc != VK_SUCCESS {
            // SAFETY: Destroy orphan buffer before returning error.
            unsafe {
                (api.destroy_buffer)(st.vk_dev, buffer, ptr::null());
            }
            return Err(MoraError::Runtime(format!(
                "Vulkan vkAllocateMemory failed: VkResult({rc})"
            )));
        }
        // SAFETY: Binds `memory` to `buffer` at offset 0.
        let rc = unsafe { (api.bind_buffer_memory)(st.vk_dev, buffer, memory, 0) };
        if rc != VK_SUCCESS {
            // SAFETY: Release both buffer and memory on bind failure.
            unsafe {
                (api.destroy_buffer)(st.vk_dev, buffer, ptr::null());
                (api.free_memory)(st.vk_dev, memory, ptr::null());
            }
            return Err(MoraError::Runtime(format!(
                "Vulkan vkBindBufferMemory failed: VkResult({rc})"
            )));
        }
        Ok(VkBufferAlloc { buffer, memory })
    }

    fn write_host_buffer<T: Copy>(
        api: &VulkanApi,
        vk_dev: VkDevice,
        alloc: &VkBufferAlloc,
        data: &[T],
    ) -> MoraResult<()> {
        if data.is_empty() {
            return Ok(());
        }
        let bytes = std::mem::size_of_val(data);
        let mut mapped: *mut c_void = ptr::null_mut();
        let rc = {
            // SAFETY: `alloc.memory` is HOST_VISIBLE | HOST_COHERENT and large enough for `bytes`.
            unsafe { (api.map_memory)(vk_dev, alloc.memory, 0, VK_WHOLE_SIZE, 0, &mut mapped) }
        };
        if rc != VK_SUCCESS || mapped.is_null() {
            return Err(MoraError::Runtime(format!(
                "Vulkan vkMapMemory write failed: VkResult({rc})"
            )));
        }
        // SAFETY: Non-overlapping host memcpy into mapped coherent device memory.
        unsafe {
            ptr::copy_nonoverlapping(data.as_ptr() as *const u8, mapped as *mut u8, bytes);
            (api.unmap_memory)(vk_dev, alloc.memory);
        }
        Ok(())
    }

    fn read_host_buffer<T: Copy>(
        api: &VulkanApi,
        vk_dev: VkDevice,
        alloc: &VkBufferAlloc,
        dst: &mut [T],
    ) -> MoraResult<()> {
        if dst.is_empty() {
            return Ok(());
        }
        let bytes = std::mem::size_of_val(dst);
        let mut mapped: *mut c_void = ptr::null_mut();
        let rc = {
            // SAFETY: `alloc.memory` is HOST_VISIBLE | HOST_COHERENT and GPU queue is already idle.
            unsafe { (api.map_memory)(vk_dev, alloc.memory, 0, VK_WHOLE_SIZE, 0, &mut mapped) }
        };
        if rc != VK_SUCCESS || mapped.is_null() {
            return Err(MoraError::Runtime(format!(
                "Vulkan vkMapMemory read failed: VkResult({rc})"
            )));
        }
        // SAFETY: Non-overlapping memcpy from coherent mapped memory into `dst`.
        unsafe {
            ptr::copy_nonoverlapping(mapped as *const u8, dst.as_mut_ptr() as *mut u8, bytes);
            (api.unmap_memory)(vk_dev, alloc.memory);
        }
        Ok(())
    }

    fn free_host_buffer(api: &VulkanApi, vk_dev: VkDevice, alloc: VkBufferAlloc) {
        // SAFETY: Caller has synchronized the Vulkan queue before destroying buffers.
        unsafe {
            (api.destroy_buffer)(vk_dev, alloc.buffer, ptr::null());
            (api.free_memory)(vk_dev, alloc.memory, ptr::null());
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_linear_f32(
        &self,
        a: &[f32],
        b: &[f32],
        bias: &[f32],
        m: usize,
        n: usize,
        k: usize,
        act_code: u32,
        out_f32: &mut [f32],
    ) -> MoraResult<()> {
        let st = self.state.lock();
        let has_bias = if bias.is_empty() { 0u32 } else { 1u32 };
        let bias_len = bias.len().max(1) as u32;
        let dummy_bias = [0.0f32];
        let eff_bias = if bias.is_empty() {
            &dummy_bias[..]
        } else {
            bias
        };
        let params: [u32; 6] = [m as u32, n as u32, k as u32, has_bias, bias_len, act_code];

        let mut bufs = Vec::with_capacity(5);
        let run = (|| -> MoraResult<()> {
            let buf_a = Self::alloc_host_buffer(&self.api, &st, std::mem::size_of_val(a))?;
            bufs.push(buf_a);
            let buf_b = Self::alloc_host_buffer(&self.api, &st, std::mem::size_of_val(b))?;
            bufs.push(buf_b);
            let buf_bias =
                Self::alloc_host_buffer(&self.api, &st, std::mem::size_of_val(eff_bias))?;
            bufs.push(buf_bias);
            let buf_out = Self::alloc_host_buffer(&self.api, &st, std::mem::size_of_val(out_f32))?;
            bufs.push(buf_out);
            let buf_params =
                Self::alloc_host_buffer(&self.api, &st, std::mem::size_of_val(&params))?;
            bufs.push(buf_params);

            Self::write_host_buffer(&self.api, st.vk_dev, &bufs[0], a)?;
            Self::write_host_buffer(&self.api, st.vk_dev, &bufs[1], b)?;
            Self::write_host_buffer(&self.api, st.vk_dev, &bufs[2], eff_bias)?;
            Self::write_host_buffer(&self.api, st.vk_dev, &bufs[4], &params)?;

            let infos: [VkDescriptorBufferInfo; 5] = [
                VkDescriptorBufferInfo {
                    buffer: bufs[0].buffer,
                    offset: 0,
                    range: VK_WHOLE_SIZE,
                },
                VkDescriptorBufferInfo {
                    buffer: bufs[1].buffer,
                    offset: 0,
                    range: VK_WHOLE_SIZE,
                },
                VkDescriptorBufferInfo {
                    buffer: bufs[2].buffer,
                    offset: 0,
                    range: VK_WHOLE_SIZE,
                },
                VkDescriptorBufferInfo {
                    buffer: bufs[3].buffer,
                    offset: 0,
                    range: VK_WHOLE_SIZE,
                },
                VkDescriptorBufferInfo {
                    buffer: bufs[4].buffer,
                    offset: 0,
                    range: VK_WHOLE_SIZE,
                },
            ];
            let writes: [VkWriteDescriptorSet; 5] = std::array::from_fn(|i| VkWriteDescriptorSet {
                s_type: VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
                p_next: ptr::null(),
                dst_set: st.desc_set,
                dst_binding: i as u32,
                dst_array_element: 0,
                descriptor_count: 1,
                descriptor_type: VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
                p_image_info: ptr::null(),
                p_buffer_info: &infos[i],
                p_texel_buffer_view: ptr::null(),
            });
            // SAFETY: All 5 buffers and descriptor set belong to `st.vk_dev`.
            unsafe {
                (self.api.update_descriptor_sets)(
                    st.vk_dev,
                    writes.len() as u32,
                    writes.as_ptr(),
                    0,
                    ptr::null(),
                );
            }

            let begin = VkCommandBufferBeginInfo {
                s_type: VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
                p_next: ptr::null(),
                flags: VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT,
                p_inheritance_info: ptr::null(),
            };
            let gx = (n as u32).div_ceil(16).max(1);
            let gy = (m as u32).div_ceil(16).max(1);
            // SAFETY: Command buffer is serialized under `self.state.lock()` and reset before recording.
            unsafe {
                let rc = (self.api.reset_command_buffer)(st.cmd_buf, 0);
                if rc != VK_SUCCESS {
                    return Err(MoraError::Runtime(format!(
                        "Vulkan vkResetCommandBuffer failed: VkResult({rc})"
                    )));
                }
                let rc = (self.api.begin_command_buffer)(st.cmd_buf, &begin);
                if rc != VK_SUCCESS {
                    return Err(MoraError::Runtime(format!(
                        "Vulkan vkBeginCommandBuffer failed: VkResult({rc})"
                    )));
                }
                (self.api.cmd_bind_pipeline)(
                    st.cmd_buf,
                    VK_PIPELINE_BIND_POINT_COMPUTE,
                    st.pipeline,
                );
                (self.api.cmd_bind_descriptor_sets)(
                    st.cmd_buf,
                    VK_PIPELINE_BIND_POINT_COMPUTE,
                    st.pipe_layout,
                    0,
                    1,
                    &st.desc_set,
                    0,
                    ptr::null(),
                );
                (self.api.cmd_dispatch)(st.cmd_buf, gx, gy, 1);
                let rc = (self.api.end_command_buffer)(st.cmd_buf);
                if rc != VK_SUCCESS {
                    return Err(MoraError::Runtime(format!(
                        "Vulkan vkEndCommandBuffer failed: VkResult({rc})"
                    )));
                }
                let submit = VkSubmitInfo {
                    s_type: VK_STRUCTURE_TYPE_SUBMIT_INFO,
                    p_next: ptr::null(),
                    wait_semaphore_count: 0,
                    p_wait_semaphores: ptr::null(),
                    p_wait_dst_stage_mask: ptr::null(),
                    command_buffer_count: 1,
                    p_command_buffers: &st.cmd_buf,
                    signal_semaphore_count: 0,
                    p_signal_semaphores: ptr::null(),
                };
                let rc = (self.api.queue_submit)(st.queue, 1, &submit, 0);
                if rc != VK_SUCCESS {
                    return Err(MoraError::Runtime(format!(
                        "Vulkan vkQueueSubmit failed: VkResult({rc})"
                    )));
                }
                let rc = (self.api.queue_wait_idle)(st.queue);
                if rc != VK_SUCCESS {
                    return Err(MoraError::Runtime(format!(
                        "Vulkan vkQueueWaitIdle failed: VkResult({rc})"
                    )));
                }
            }
            Self::read_host_buffer(&self.api, st.vk_dev, &bufs[3], out_f32)?;
            Ok(())
        })();

        for alloc in bufs {
            Self::free_host_buffer(&self.api, st.vk_dev, alloc);
        }
        run
    }
}

fn act_code(act: FusedAct) -> u32 {
    match act {
        FusedAct::None => 0,
        FusedAct::Relu => 1,
        FusedAct::Gelu => 2,
        FusedAct::Silu => 3,
        FusedAct::Tanh => 4,
        FusedAct::Sigmoid => 5,
    }
}

impl DeviceBackend for VulkanDeviceBackend {
    fn kind(&self) -> Device {
        self.device.clone()
    }

    fn name(&self) -> &str {
        "vulkan"
    }

    fn simd_isa(&self) -> &'static str {
        "spirv-1.0-f32"
    }

    fn worker_threads(&self) -> usize {
        self.info.multiprocessors as usize
    }

    fn is_available(&self) -> bool {
        true
    }

    fn run_matmul_into(
        &self,
        a: &[f64],
        b: &[f64],
        a_shape: &[usize],
        b_shape: &[usize],
        dtype: DType,
        out: &mut Vec<f64>,
    ) -> MoraResult<()> {
        if a_shape.len() != 2 || b_shape.len() != 2 {
            simd_kernels::matmul_simd_into(a, b, a_shape, b_shape, out)?;
            if dtype != DType::F64 {
                for v in out.iter_mut() {
                    *v = dtype.quantize_f64(*v);
                }
            }
            return Ok(());
        }
        let m = a_shape[0];
        let k = a_shape[1];
        let k2 = b_shape[0];
        let n = b_shape[1];
        if k != k2 {
            return Err(MoraError::Runtime(format!(
                "Vulkan MatMul inner dim mismatch: {k} != {k2}"
            )));
        }
        let out_numel = m * n;
        out.resize(out_numel, 0.0);
        if out_numel == 0 || k == 0 {
            out.fill(0.0);
            return Ok(());
        }
        let a_f32: Vec<f32> = a.iter().map(|&v| v as f32).collect();
        let b_f32: Vec<f32> = b.iter().map(|&v| v as f32).collect();
        let mut out_f32 = vec![0.0f32; out_numel];
        self.dispatch_linear_f32(&a_f32, &b_f32, &[], m, n, k, 0, &mut out_f32)?;
        for (dst, &v) in out.iter_mut().zip(out_f32.iter()) {
            *dst = dtype.quantize_f64(f64::from(v));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn run_fused_linear_act_into(
        &self,
        x: &[f64],
        w: &[f64],
        bias: Option<(&[f64], &[usize])>,
        act: FusedAct,
        m_total: usize,
        k: usize,
        n: usize,
        out_shape: &[usize],
        dtype: DType,
        out: &mut Vec<f64>,
    ) -> MoraResult<()> {
        let out_numel = m_total * n;
        out.resize(out_numel, 0.0);
        if out_numel == 0 {
            return Ok(());
        }
        let bias_f32 = match bias {
            None => Vec::new(),
            Some((bvals, _)) if bvals.len() == n || bvals.len() == 1 => {
                bvals.iter().map(|&v| v as f32).collect()
            }
            Some(_) => {
                simd_kernels::fused_linear_act_into(
                    x, w, bias, act, m_total, k, n, out_shape, dtype, out,
                );
                return Ok(());
            }
        };
        let x_f32: Vec<f32> = x.iter().map(|&v| v as f32).collect();
        let w_f32: Vec<f32> = w.iter().map(|&v| v as f32).collect();
        let mut out_f32 = vec![0.0f32; out_numel];
        self.dispatch_linear_f32(
            &x_f32,
            &w_f32,
            &bias_f32,
            m_total,
            n,
            k,
            act_code(act),
            &mut out_f32,
        )?;
        for (dst, &v) in out.iter_mut().zip(out_f32.iter()) {
            *dst = dtype.quantize_f64(f64::from(v));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn run_fused_rms_norm_linear_into(
        &self,
        x: &[f64],
        eps: f64,
        gamma: Option<(&[f64], &[usize])>,
        w: &[f64],
        bias: Option<(&[f64], &[usize])>,
        act: FusedAct,
        m_total: usize,
        k: usize,
        n: usize,
        x_shape: &[usize],
        out_shape: &[usize],
        dtype: DType,
        out: &mut Vec<f64>,
    ) -> MoraResult<()> {
        // Compute normalized input on host, then dispatch fused Linear+Bias+Act on Vulkan GPU!
        let gamma_slice = match gamma {
            None => None,
            Some((gv, _)) if gv.len() == k || gv.len() == 1 => Some(gv),
            Some(_) => {
                simd_kernels::fused_rms_norm_linear_into(
                    x, eps, gamma, w, bias, act, m_total, k, n, x_shape, out_shape, dtype, out,
                );
                return Ok(());
            }
        };
        let mut x_norm = vec![0.0f64; m_total * k];
        for row in 0..m_total {
            let src = &x[row * k..(row + 1) * k];
            let dst = &mut x_norm[row * k..(row + 1) * k];
            let sq: f64 = src.iter().map(|&v| v * v).sum();
            let inv = 1.0 / (sq / (k as f64) + eps).sqrt();
            for j in 0..k {
                let g = gamma_slice.map_or(1.0, |gv| gv[j % gv.len()]);
                dst[j] = src[j] * inv * g;
            }
        }
        self.run_fused_linear_act_into(&x_norm, w, bias, act, m_total, k, n, out_shape, dtype, out)
    }

    #[allow(clippy::too_many_arguments)]
    fn run_fused_flash_attention_into(
        &self,
        q: &[f64],
        k: &[f64],
        v: &[f64],
        mask: Option<(&[f64], &[usize])>,
        q_shape: &[usize],
        k_shape: &[usize],
        v_shape: &[usize],
        out_shape: &[usize],
        scale: f64,
        k_is_transposed: bool,
        dtype: DType,
        out: &mut Vec<f64>,
    ) -> MoraResult<()> {
        simd_kernels::fused_flash_attention_into(
            q,
            k,
            v,
            mask,
            q_shape,
            k_shape,
            v_shape,
            out_shape,
            scale,
            k_is_transposed,
            dtype,
            out,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn run_fused_elementwise_into(
        &self,
        input_flats: &[&[f64]],
        input_shapes: &[&[usize]],
        steps: &[FusedEwStep],
        out_shape: &[usize],
        out_numel: usize,
        dtype: DType,
        out: &mut Vec<f64>,
    ) -> MoraResult<()> {
        simd_kernels::fused_elementwise_into(
            input_flats,
            input_shapes,
            steps,
            out_shape,
            out_numel,
            dtype,
            out,
        )
    }
}

fn init_vulkan_device(
    api: &VulkanApi,
    phys: VkPhysicalDevice,
    queue_family: u32,
    mem_props: VkPhysicalDeviceMemoryProperties,
) -> Option<VulkanState> {
    let prio: f32 = 1.0;
    let qci = VkDeviceQueueCreateInfo {
        s_type: VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
        p_next: ptr::null(),
        flags: 0,
        queue_family_index: queue_family,
        queue_count: 1,
        p_queue_priorities: &prio,
    };
    let dci = VkDeviceCreateInfo {
        s_type: VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
        p_next: ptr::null(),
        flags: 0,
        queue_create_info_count: 1,
        p_queue_create_infos: &qci,
        enabled_layer_count: 0,
        pp_enabled_layer_names: ptr::null(),
        enabled_extension_count: 0,
        pp_enabled_extension_names: ptr::null(),
        p_enabled_features: ptr::null(),
    };
    let mut vk_dev: VkDevice = ptr::null_mut();
    // SAFETY: Creates a Vulkan logical device with one compute queue on `phys`.
    if unsafe { (api.create_device)(phys, &dci, ptr::null(), &mut vk_dev) } != VK_SUCCESS
        || vk_dev.is_null()
    {
        return None;
    }

    let mut queue: VkQueue = ptr::null_mut();
    // SAFETY: Retrieves queue 0 of `queue_family` from `vk_dev`.
    unsafe {
        (api.get_device_queue)(vk_dev, queue_family, 0, &mut queue);
    }

    let spv = emit_spirv_fused_linear_f32();
    let smci = VkShaderModuleCreateInfo {
        s_type: VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,
        p_next: ptr::null(),
        flags: 0,
        code_size: spv.len() * 4,
        p_code: spv.as_ptr(),
    };
    let mut shader: VkShaderModule = 0;
    // SAFETY: `spv` is a 4-byte aligned valid SPIR-V 1.0 module.
    if unsafe { (api.create_shader_module)(vk_dev, &smci, ptr::null(), &mut shader) } != VK_SUCCESS
    {
        // SAFETY: Destroy logical device on shader creation failure.
        unsafe {
            (api.destroy_device)(vk_dev, ptr::null());
        }
        return None;
    }

    let bindings: [VkDescriptorSetLayoutBinding; 5] =
        std::array::from_fn(|i| VkDescriptorSetLayoutBinding {
            binding: i as u32,
            descriptor_type: VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
            descriptor_count: 1,
            stage_flags: VK_SHADER_STAGE_COMPUTE_BIT,
            p_immutable_samplers: ptr::null(),
        });
    let dslci = VkDescriptorSetLayoutCreateInfo {
        s_type: VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
        p_next: ptr::null(),
        flags: 0,
        binding_count: bindings.len() as u32,
        p_bindings: bindings.as_ptr(),
    };
    let mut ds_layout: VkDescriptorSetLayout = 0;
    // SAFETY: Valid descriptor set layout info on `vk_dev`.
    if unsafe { (api.create_descriptor_set_layout)(vk_dev, &dslci, ptr::null(), &mut ds_layout) }
        != VK_SUCCESS
    {
        // SAFETY: Clean up shader + device on failure.
        unsafe {
            (api.destroy_shader_module)(vk_dev, shader, ptr::null());
            (api.destroy_device)(vk_dev, ptr::null());
        }
        return None;
    }

    let plci = VkPipelineLayoutCreateInfo {
        s_type: VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO,
        p_next: ptr::null(),
        flags: 0,
        set_layout_count: 1,
        p_set_layouts: &ds_layout,
        push_constant_range_count: 0,
        p_push_constant_ranges: ptr::null(),
    };
    let mut pipe_layout: VkPipelineLayout = 0;
    // SAFETY: Valid pipeline layout info referencing `ds_layout`.
    if unsafe { (api.create_pipeline_layout)(vk_dev, &plci, ptr::null(), &mut pipe_layout) }
        != VK_SUCCESS
    {
        // SAFETY: Clean up previously allocated Vulkan objects.
        unsafe {
            (api.destroy_descriptor_set_layout)(vk_dev, ds_layout, ptr::null());
            (api.destroy_shader_module)(vk_dev, shader, ptr::null());
            (api.destroy_device)(vk_dev, ptr::null());
        }
        return None;
    }

    let entry_name = b"main\0";
    let cpci = VkComputePipelineCreateInfo {
        s_type: VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO,
        p_next: ptr::null(),
        flags: 0,
        stage: VkPipelineShaderStageCreateInfo {
            s_type: VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO,
            p_next: ptr::null(),
            flags: 0,
            stage: VK_SHADER_STAGE_COMPUTE_BIT,
            module: shader,
            p_name: entry_name.as_ptr() as *const c_char,
            p_specialization_info: ptr::null(),
        },
        layout: pipe_layout,
        base_pipeline_handle: 0,
        base_pipeline_index: -1,
    };
    let mut pipeline: VkPipeline = 0;
    // SAFETY: Compiles SPIR-V compute pipeline on the Vulkan driver.
    if unsafe { (api.create_compute_pipelines)(vk_dev, 0, 1, &cpci, ptr::null(), &mut pipeline) }
        != VK_SUCCESS
    {
        // SAFETY: Clean up previously allocated Vulkan objects.
        unsafe {
            (api.destroy_pipeline_layout)(vk_dev, pipe_layout, ptr::null());
            (api.destroy_descriptor_set_layout)(vk_dev, ds_layout, ptr::null());
            (api.destroy_shader_module)(vk_dev, shader, ptr::null());
            (api.destroy_device)(vk_dev, ptr::null());
        }
        return None;
    }

    let pool_size = VkDescriptorPoolSize {
        descriptor_type: VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
        descriptor_count: 5,
    };
    let dpci = VkDescriptorPoolCreateInfo {
        s_type: VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO,
        p_next: ptr::null(),
        flags: 0,
        max_sets: 1,
        pool_size_count: 1,
        p_pool_sizes: &pool_size,
    };
    let mut desc_pool: VkDescriptorPool = 0;
    // SAFETY: Creates descriptor pool holding 1 set of 5 SSBO bindings.
    if unsafe { (api.create_descriptor_pool)(vk_dev, &dpci, ptr::null(), &mut desc_pool) }
        != VK_SUCCESS
    {
        // SAFETY: Clean up on failure.
        unsafe {
            (api.destroy_pipeline)(vk_dev, pipeline, ptr::null());
            (api.destroy_pipeline_layout)(vk_dev, pipe_layout, ptr::null());
            (api.destroy_descriptor_set_layout)(vk_dev, ds_layout, ptr::null());
            (api.destroy_shader_module)(vk_dev, shader, ptr::null());
            (api.destroy_device)(vk_dev, ptr::null());
        }
        return None;
    }

    let dsai = VkDescriptorSetAllocateInfo {
        s_type: VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO,
        p_next: ptr::null(),
        descriptor_pool: desc_pool,
        descriptor_set_count: 1,
        p_set_layouts: &ds_layout,
    };
    let mut desc_set: VkDescriptorSet = 0;
    // SAFETY: Allocates one descriptor set from `desc_pool`.
    if unsafe { (api.allocate_descriptor_sets)(vk_dev, &dsai, &mut desc_set) } != VK_SUCCESS {
        // SAFETY: Clean up on failure.
        unsafe {
            (api.destroy_descriptor_pool)(vk_dev, desc_pool, ptr::null());
            (api.destroy_pipeline)(vk_dev, pipeline, ptr::null());
            (api.destroy_pipeline_layout)(vk_dev, pipe_layout, ptr::null());
            (api.destroy_descriptor_set_layout)(vk_dev, ds_layout, ptr::null());
            (api.destroy_shader_module)(vk_dev, shader, ptr::null());
            (api.destroy_device)(vk_dev, ptr::null());
        }
        return None;
    }

    let cmdpci = VkCommandPoolCreateInfo {
        s_type: VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
        p_next: ptr::null(),
        flags: VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT,
        queue_family_index: queue_family,
    };
    let mut cmd_pool: VkCommandPool = 0;
    // SAFETY: Creates resettable command pool on compute queue family.
    if unsafe { (api.create_command_pool)(vk_dev, &cmdpci, ptr::null(), &mut cmd_pool) }
        != VK_SUCCESS
    {
        // SAFETY: Clean up on failure.
        unsafe {
            (api.destroy_descriptor_pool)(vk_dev, desc_pool, ptr::null());
            (api.destroy_pipeline)(vk_dev, pipeline, ptr::null());
            (api.destroy_pipeline_layout)(vk_dev, pipe_layout, ptr::null());
            (api.destroy_descriptor_set_layout)(vk_dev, ds_layout, ptr::null());
            (api.destroy_shader_module)(vk_dev, shader, ptr::null());
            (api.destroy_device)(vk_dev, ptr::null());
        }
        return None;
    }

    let cbai = VkCommandBufferAllocateInfo {
        s_type: VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
        p_next: ptr::null(),
        command_pool: cmd_pool,
        level: VK_COMMAND_BUFFER_LEVEL_PRIMARY,
        command_buffer_count: 1,
    };
    let mut cmd_buf: VkCommandBuffer = ptr::null_mut();
    // SAFETY: Allocates one primary command buffer from `cmd_pool`.
    if unsafe { (api.allocate_command_buffers)(vk_dev, &cbai, &mut cmd_buf) } != VK_SUCCESS {
        // SAFETY: Clean up on failure.
        unsafe {
            (api.destroy_command_pool)(vk_dev, cmd_pool, ptr::null());
            (api.destroy_descriptor_pool)(vk_dev, desc_pool, ptr::null());
            (api.destroy_pipeline)(vk_dev, pipeline, ptr::null());
            (api.destroy_pipeline_layout)(vk_dev, pipe_layout, ptr::null());
            (api.destroy_descriptor_set_layout)(vk_dev, ds_layout, ptr::null());
            (api.destroy_shader_module)(vk_dev, shader, ptr::null());
            (api.destroy_device)(vk_dev, ptr::null());
        }
        return None;
    }

    Some(VulkanState {
        vk_dev,
        queue,
        shader,
        ds_layout,
        pipe_layout,
        pipeline,
        desc_pool,
        desc_set,
        cmd_pool,
        cmd_buf,
        mem_props,
    })
}

/// Dynamically probe all compute-capable Vulkan GPUs via the OS Vulkan loader.
pub(crate) fn probe_vulkan_backends() -> Vec<HardwareBackendEntry> {
    let Some(api) = VulkanApi::load() else {
        return Vec::new();
    };
    let app_name = b"mora\0";
    let app_info = VkApplicationInfo {
        s_type: VK_STRUCTURE_TYPE_APPLICATION_INFO,
        p_next: ptr::null(),
        p_application_name: app_name.as_ptr() as *const c_char,
        application_version: 1,
        p_engine_name: app_name.as_ptr() as *const c_char,
        engine_version: 1,
        api_version: VK_API_VERSION_1_0,
    };
    let ici = VkInstanceCreateInfo {
        s_type: VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
        p_next: ptr::null(),
        flags: 0,
        p_application_info: &app_info,
        enabled_layer_count: 0,
        pp_enabled_layer_names: ptr::null(),
        enabled_extension_count: 0,
        pp_enabled_extension_names: ptr::null(),
    };
    let mut instance: VkInstance = ptr::null_mut();
    // SAFETY: Standard Vulkan 1.0 instance creation with no extensions/layers.
    if unsafe { (api.create_instance)(&ici, ptr::null(), &mut instance) } != VK_SUCCESS
        || instance.is_null()
    {
        return Vec::new();
    }
    let inst_guard = Arc::new(VkInstanceGuard {
        api: Arc::clone(&api),
        instance,
    });

    let mut phys_count: u32 = 0;
    // SAFETY: Two-call Vulkan enumeration pattern for physical devices.
    if unsafe { (api.enumerate_physical_devices)(instance, &mut phys_count, ptr::null_mut()) }
        != VK_SUCCESS
        || phys_count == 0
    {
        return Vec::new();
    }
    let mut phys_devs = vec![ptr::null_mut(); phys_count as usize];
    // SAFETY: `phys_devs` has exact length `phys_count`.
    if unsafe {
        (api.enumerate_physical_devices)(instance, &mut phys_count, phys_devs.as_mut_ptr())
    } != VK_SUCCESS
    {
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut vk_idx = 0u16;
    for phys in phys_devs.into_iter().take(phys_count as usize) {
        if phys.is_null() {
            continue;
        }
        // SAFETY: `VkPhysicalDevicePropertiesBuf` is zero-initialized and oversized for padding safety.
        let mut props: VkPhysicalDevicePropertiesBuf = unsafe { std::mem::zeroed() };
        // SAFETY: Populates physical device properties into `props`.
        unsafe {
            (api.get_physical_device_properties)(phys, &mut props);
        }
        // SAFETY: `VkPhysicalDeviceMemoryProperties` is POD zero-initialized.
        let mut mem_props: VkPhysicalDeviceMemoryProperties = unsafe { std::mem::zeroed() };
        // SAFETY: Populates physical device memory heap/type properties.
        unsafe {
            (api.get_physical_device_memory_properties)(phys, &mut mem_props);
        }

        let mut qf_count: u32 = 0;
        // SAFETY: Query queue family count.
        unsafe {
            (api.get_physical_device_queue_family_properties)(phys, &mut qf_count, ptr::null_mut());
        }
        if qf_count == 0 {
            continue;
        }
        let mut qfs = vec![
            VkQueueFamilyProperties {
                queue_flags: 0,
                queue_count: 0,
                timestamp_valid_bits: 0,
                min_image_transfer_granularity: VkExtent3D {
                    width: 0,
                    height: 0,
                    depth: 0,
                },
            };
            qf_count as usize
        ];
        // SAFETY: `qfs` has exact length `qf_count`.
        unsafe {
            (api.get_physical_device_queue_family_properties)(
                phys,
                &mut qf_count,
                qfs.as_mut_ptr(),
            );
        }
        let Some((q_idx, q_prop)) = qfs
            .iter()
            .enumerate()
            .find(|(_, q)| (q.queue_flags & VK_QUEUE_COMPUTE_BIT) != 0 && q.queue_count > 0)
        else {
            continue;
        };

        let Some(state) = init_vulkan_device(&api, phys, q_idx as u32, mem_props) else {
            continue;
        };

        // Ensure `device_name` is safely NUL-terminated before reading via `CStr`.
        props.device_name[255] = 0;
        // SAFETY: `device_name` is guaranteed NUL-terminated within 256 bytes.
        let gpu_name = unsafe { CStr::from_ptr(props.device_name.as_ptr()) }
            .to_string_lossy()
            .into_owned();

        let total_vram: u64 = (0..mem_props.memory_heap_count.min(16) as usize)
            .filter(|&i| (mem_props.memory_heaps[i].flags & VK_MEMORY_HEAP_DEVICE_LOCAL_BIT) != 0)
            .map(|i| mem_props.memory_heaps[i].size)
            .sum();

        let api_major = (props.api_version >> 22) & 0x7f;
        let api_minor = (props.api_version >> 12) & 0x3ff;
        let api_patch = props.api_version & 0xfff;
        let device = Device::Vulkan(vk_idx);
        vk_idx += 1;

        let info = GpuDeviceInfo {
            device: device.clone(),
            driver: "vulkan".to_string(),
            name: gpu_name,
            total_vram_bytes: total_vram,
            compute_capability: format!("vulkan-{api_major}.{api_minor}.{api_patch}"),
            multiprocessors: q_prop.queue_count.max(1),
            hardware_accelerated: true,
            driver_version: format!("0x{:x}", props.driver_version),
        };
        let backend = Arc::new(VulkanDeviceBackend {
            device,
            _inst_guard: Arc::clone(&inst_guard),
            api: Arc::clone(&api),
            info: info.clone(),
            state: Mutex::new(state),
        });
        out.push(HardwareBackendEntry { info, backend });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spirv_emitter_produces_well_formed_header_and_bound() {
        let words = emit_spirv_fused_linear_f32();
        assert_eq!(words[0], 0x0723_0203, "SPIR-V magic");
        assert_eq!(words[1], 0x0001_0000, "SPIR-V 1.0 version");
        assert!(words[3] > 20, "valid non-zero SSA ID bound");
        assert!(words.len() > 80, "non-trivial compute shader payload");
    }
}
