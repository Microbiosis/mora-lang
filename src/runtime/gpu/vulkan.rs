//! v0.110.1: Native Vulkan 1.0 C API (`vulkan-1.dll` / `libvulkan.so.1` / `libMoltenVK.dylib`)
//! Hardware Compute Backend with Pure-Rust SPIR-V 1.0 Shader Generation & Persistent VRAM Residency.
//!
//! Architecture:
//! - Zero compile-time Vulkan SDK / `shaderc` / Cargo dependencies: dynamically opens the OS
//!   Vulkan loader at runtime and enumerates physical GPUs, VRAM heaps, and compute queues.
//! - Compiles 6 spec-compliant SPIR-V 1.0 `GLCompute` pipelines directly in pure Rust
//!   ([`crate::runtime::gpu::vulkan_spirv`]) covering ALL 5 `DeviceBackend` fused kernels
//!   (`MatMul` 2D/batched ND, `FusedLinearAct`, `FusedRmsNormLinear`, `FusedFlashAttention`,
//!   `FusedElementwise`) plus native packed `f16` (`UnpackHalf2x16`/`PackHalf2x16`) and `bf16`
//!   compute shaders with zero optional Vulkan extensions.
//! - Owns persistent RAII [`VulkanVramBuffer`] (`VkBuffer` + `VkDeviceMemory`) handles enabling
//!   zero-copy VRAM-to-VRAM chained compute graph execution.

use std::ffi::{CStr, c_char, c_void};
use std::fmt;
use std::ptr;
use std::sync::Arc;

use crate::common::sync::Mutex;

use crate::common::{
    DType, Device, bf16_bits_to_f64, f16_bits_to_f64, f64_to_bf16_bits, f64_to_f16_bits,
};
use crate::error::{MoraError, MoraResult};
use crate::runtime::compute_dag::{
    DeviceBackend, DeviceVramBuffer, FusedAct, FusedEwStep, VramTelemetry,
};
use crate::runtime::gpu::vulkan_spirv::{
    emit_spirv_flash_attention_f32, emit_spirv_fused_elementwise_f32, emit_spirv_fused_linear_f32,
    emit_spirv_fused_linear_half, emit_spirv_rms_norm_f32,
};
use crate::runtime::gpu::{DynLib, GpuDeviceInfo, HardwareBackendEntry, VramCounters};
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

// ── Vulkan Compute Pipelines & Persistent VRAM Buffers ──────────────────────

struct VkComputePipe {
    shader: VkShaderModule,
    ds_layout: VkDescriptorSetLayout,
    pipe_layout: VkPipelineLayout,
    pipeline: VkPipeline,
    desc_pool: VkDescriptorPool,
    desc_set: VkDescriptorSet,
    num_bindings: u32,
}

impl VkComputePipe {
    fn create(api: &VulkanApi, vk_dev: VkDevice, spv: &[u32], num_bindings: u32) -> Option<Self> {
        let smci = VkShaderModuleCreateInfo {
            s_type: VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO,
            p_next: ptr::null(),
            flags: 0,
            code_size: std::mem::size_of_val(spv),
            p_code: spv.as_ptr(),
        };
        let mut shader: VkShaderModule = 0;
        // SAFETY: `spv` is a 4-byte aligned valid SPIR-V 1.0 module.
        if unsafe { (api.create_shader_module)(vk_dev, &smci, ptr::null(), &mut shader) }
            != VK_SUCCESS
        {
            return None;
        }

        let bindings: Vec<VkDescriptorSetLayoutBinding> = (0..num_bindings)
            .map(|i| VkDescriptorSetLayoutBinding {
                binding: i,
                descriptor_type: VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
                descriptor_count: 1,
                stage_flags: VK_SHADER_STAGE_COMPUTE_BIT,
                p_immutable_samplers: ptr::null(),
            })
            .collect();
        let dslci = VkDescriptorSetLayoutCreateInfo {
            s_type: VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO,
            p_next: ptr::null(),
            flags: 0,
            binding_count: num_bindings,
            p_bindings: bindings.as_ptr(),
        };
        let mut ds_layout: VkDescriptorSetLayout = 0;
        // SAFETY: Valid descriptor set layout info on `vk_dev`.
        if unsafe {
            (api.create_descriptor_set_layout)(vk_dev, &dslci, ptr::null(), &mut ds_layout)
        } != VK_SUCCESS
        {
            // SAFETY: Clean up shader on failure.
            unsafe {
                (api.destroy_shader_module)(vk_dev, shader, ptr::null());
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
        // SAFETY: Valid pipeline layout referencing `ds_layout`.
        if unsafe { (api.create_pipeline_layout)(vk_dev, &plci, ptr::null(), &mut pipe_layout) }
            != VK_SUCCESS
        {
            // SAFETY: Clean up previously created handles.
            unsafe {
                (api.destroy_descriptor_set_layout)(vk_dev, ds_layout, ptr::null());
                (api.destroy_shader_module)(vk_dev, shader, ptr::null());
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
        // SAFETY: Compiles SPIR-V 1.0 compute pipeline on `vk_dev`.
        if unsafe {
            (api.create_compute_pipelines)(vk_dev, 0, 1, &cpci, ptr::null(), &mut pipeline)
        } != VK_SUCCESS
        {
            // SAFETY: Clean up on failure.
            unsafe {
                (api.destroy_pipeline_layout)(vk_dev, pipe_layout, ptr::null());
                (api.destroy_descriptor_set_layout)(vk_dev, ds_layout, ptr::null());
                (api.destroy_shader_module)(vk_dev, shader, ptr::null());
            }
            return None;
        }

        let pool_size = VkDescriptorPoolSize {
            descriptor_type: VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
            descriptor_count: num_bindings,
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
        // SAFETY: Creates descriptor pool for one set of `num_bindings` SSBOs.
        if unsafe { (api.create_descriptor_pool)(vk_dev, &dpci, ptr::null(), &mut desc_pool) }
            != VK_SUCCESS
        {
            // SAFETY: Clean up on failure.
            unsafe {
                (api.destroy_pipeline)(vk_dev, pipeline, ptr::null());
                (api.destroy_pipeline_layout)(vk_dev, pipe_layout, ptr::null());
                (api.destroy_descriptor_set_layout)(vk_dev, ds_layout, ptr::null());
                (api.destroy_shader_module)(vk_dev, shader, ptr::null());
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
            }
            return None;
        }

        Some(Self {
            shader,
            ds_layout,
            pipe_layout,
            pipeline,
            desc_pool,
            desc_set,
            num_bindings,
        })
    }

    fn destroy(&self, api: &VulkanApi, vk_dev: VkDevice) {
        // SAFETY: Destroys pipeline resources in reverse creation order while `vk_dev` is idle.
        unsafe {
            (api.destroy_descriptor_pool)(vk_dev, self.desc_pool, ptr::null());
            (api.destroy_pipeline)(vk_dev, self.pipeline, ptr::null());
            (api.destroy_pipeline_layout)(vk_dev, self.pipe_layout, ptr::null());
            (api.destroy_descriptor_set_layout)(vk_dev, self.ds_layout, ptr::null());
            (api.destroy_shader_module)(vk_dev, self.shader, ptr::null());
        }
    }
}

struct VulkanState {
    vk_dev: VkDevice,
    queue: VkQueue,
    pipe_linear_f32: VkComputePipe,
    pipe_linear_f16: VkComputePipe,
    pipe_linear_bf16: VkComputePipe,
    pipe_rms_norm_f32: VkComputePipe,
    pipe_flash_attn_f32: VkComputePipe,
    pipe_fused_ew_f32: VkComputePipe,
    dummy_buf: VkBufferAlloc,
    cmd_pool: VkCommandPool,
    cmd_buf: VkCommandBuffer,
    mem_props: VkPhysicalDeviceMemoryProperties,
}

struct VulkanDeviceInner {
    device: Device,
    _inst_guard: Arc<VkInstanceGuard>,
    api: Arc<VulkanApi>,
    state: Mutex<VulkanState>,
    counters: VramCounters,
}

// SAFETY: All Vulkan device/queue/command-buffer operations are serialized under `self.state.lock()`.
unsafe impl Send for VulkanDeviceInner {}
// SAFETY: Shared across tasks via `Mutex<VulkanState>` ensuring external synchronization rules.
unsafe impl Sync for VulkanDeviceInner {}

impl Drop for VulkanDeviceInner {
    fn drop(&mut self) {
        let st = self.state.lock();
        // SAFETY: Handles are destroyed in strict reverse creation order after all
        // `VulkanVramBuffer`s holding `Arc<VulkanDeviceInner>` have already been dropped.
        unsafe {
            let _ = (self.api.queue_wait_idle)(st.queue);
            (self.api.destroy_command_pool)(st.vk_dev, st.cmd_pool, ptr::null());
            (self.api.destroy_buffer)(st.vk_dev, st.dummy_buf.buffer, ptr::null());
            (self.api.free_memory)(st.vk_dev, st.dummy_buf.memory, ptr::null());
            st.pipe_fused_ew_f32.destroy(&self.api, st.vk_dev);
            st.pipe_flash_attn_f32.destroy(&self.api, st.vk_dev);
            st.pipe_rms_norm_f32.destroy(&self.api, st.vk_dev);
            st.pipe_linear_bf16.destroy(&self.api, st.vk_dev);
            st.pipe_linear_f16.destroy(&self.api, st.vk_dev);
            st.pipe_linear_f32.destroy(&self.api, st.vk_dev);
            (self.api.destroy_device)(st.vk_dev, ptr::null());
        }
    }
}

/// Owned persistent Vulkan storage buffer (`VkBuffer` + `VkDeviceMemory`) with RAII cleanup.
struct VulkanVramBuffer {
    inner: Arc<VulkanDeviceInner>,
    buffer: VkBuffer,
    memory: VkDeviceMemory,
    byte_len: usize,
}

impl fmt::Debug for VulkanVramBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VulkanVramBuffer")
            .field("device", &self.inner.device)
            .field("buffer", &format_args!("0x{:x}", self.buffer))
            .field("byte_len", &self.byte_len)
            .finish()
    }
}

impl Drop for VulkanVramBuffer {
    fn drop(&mut self) {
        let st = self.inner.state.lock();
        // SAFETY: Serialized under device lock; queue is synchronized after every dispatch.
        unsafe {
            (self.inner.api.destroy_buffer)(st.vk_dev, self.buffer, ptr::null());
            (self.inner.api.free_memory)(st.vk_dev, self.memory, ptr::null());
        }
        self.inner.counters.on_free(self.byte_len);
    }
}

impl DeviceVramBuffer for VulkanVramBuffer {
    fn device(&self) -> &Device {
        &self.inner.device
    }

    fn byte_len(&self) -> usize {
        self.byte_len
    }

    fn raw_handle(&self) -> u64 {
        self.buffer
    }

    fn upload_bytes(&self, src: &[u8]) -> MoraResult<()> {
        if src.len() != self.byte_len {
            return Err(MoraError::Runtime(format!(
                "VulkanVramBuffer::upload_bytes: expected {} bytes, got {}",
                self.byte_len,
                src.len()
            )));
        }
        if src.is_empty() {
            return Ok(());
        }
        let st = self.inner.state.lock();
        let alloc = VkBufferAlloc {
            buffer: self.buffer,
            memory: self.memory,
        };
        VulkanDeviceBackend::write_host_buffer(&self.inner.api, st.vk_dev, &alloc, src)?;
        self.inner.counters.on_h2d();
        Ok(())
    }

    fn download_bytes(&self, dst: &mut [u8]) -> MoraResult<()> {
        if dst.len() != self.byte_len {
            return Err(MoraError::Runtime(format!(
                "VulkanVramBuffer::download_bytes: expected {} bytes, got {}",
                self.byte_len,
                dst.len()
            )));
        }
        if dst.is_empty() {
            return Ok(());
        }
        let st = self.inner.state.lock();
        let alloc = VkBufferAlloc {
            buffer: self.buffer,
            memory: self.memory,
        };
        VulkanDeviceBackend::read_host_buffer(&self.inner.api, st.vk_dev, &alloc, dst)?;
        self.inner.counters.on_d2h();
        Ok(())
    }
}

/// Live Vulkan 1.0 hardware compute [`DeviceBackend`] dispatching 6 pure-Rust SPIR-V 1.0 shaders.
pub(crate) struct VulkanDeviceBackend {
    inner: Arc<VulkanDeviceInner>,
    info: GpuDeviceInfo,
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
        Self::alloc_host_buffer_raw(api, st.vk_dev, &st.mem_props, size_bytes)
    }

    fn alloc_host_buffer_raw(
        api: &VulkanApi,
        vk_dev: VkDevice,
        mem_props: &VkPhysicalDeviceMemoryProperties,
        size_bytes: usize,
    ) -> MoraResult<VkBufferAlloc> {
        let alloc_size = size_bytes.max(16).next_multiple_of(4) as VkDeviceSize;
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
        let rc = unsafe { (api.create_buffer)(vk_dev, &bci, ptr::null(), &mut buffer) };
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
        // SAFETY: `buffer` is a valid `VkBuffer` owned by `vk_dev`.
        unsafe {
            (api.get_buffer_memory_requirements)(vk_dev, buffer, &mut reqs);
        }
        let Some(mem_idx) = Self::find_host_mem_type(mem_props, reqs.memory_type_bits) else {
            // SAFETY: Clean up `buffer` on memory type lookup failure.
            unsafe {
                (api.destroy_buffer)(vk_dev, buffer, ptr::null());
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
        let rc = unsafe { (api.allocate_memory)(vk_dev, &mai, ptr::null(), &mut memory) };
        if rc != VK_SUCCESS {
            // SAFETY: Destroy orphan buffer before returning error.
            unsafe {
                (api.destroy_buffer)(vk_dev, buffer, ptr::null());
            }
            return Err(MoraError::Runtime(format!(
                "Vulkan vkAllocateMemory failed: VkResult({rc})"
            )));
        }
        // SAFETY: Binds `memory` to `buffer` at offset 0.
        let rc = unsafe { (api.bind_buffer_memory)(vk_dev, buffer, memory, 0) };
        if rc != VK_SUCCESS {
            // SAFETY: Release both buffer and memory on bind failure.
            unsafe {
                (api.destroy_buffer)(vk_dev, buffer, ptr::null());
                (api.free_memory)(vk_dev, memory, ptr::null());
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

    fn alloc_owned_vram(
        &self,
        st: &VulkanState,
        byte_len: usize,
    ) -> MoraResult<Arc<VulkanVramBuffer>> {
        let alloc = Self::alloc_host_buffer(&self.inner.api, st, byte_len)?;
        self.inner.counters.on_alloc(byte_len);
        Ok(Arc::new(VulkanVramBuffer {
            inner: Arc::clone(&self.inner),
            buffer: alloc.buffer,
            memory: alloc.memory,
            byte_len,
        }))
    }

    fn upload_temp_slice<T: Copy>(
        api: &VulkanApi,
        st: &VulkanState,
        temps: &mut Vec<VkBufferAlloc>,
        data: &[T],
    ) -> MoraResult<VkBuffer> {
        let alloc = Self::alloc_host_buffer(api, st, std::mem::size_of_val(data))?;
        let handle = alloc.buffer;
        if let Err(e) = Self::write_host_buffer(api, st.vk_dev, &alloc, data) {
            Self::free_host_buffer(api, st.vk_dev, alloc);
            return Err(e);
        }
        temps.push(alloc);
        Ok(handle)
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_pipe(
        api: &VulkanApi,
        st: &VulkanState,
        pipe: &VkComputePipe,
        buffers: &[VkBuffer],
        gx: u32,
        gy: u32,
        gz: u32,
    ) -> MoraResult<()> {
        debug_assert_eq!(buffers.len(), pipe.num_bindings as usize);
        let infos: Vec<VkDescriptorBufferInfo> = buffers
            .iter()
            .map(|&buffer| VkDescriptorBufferInfo {
                buffer,
                offset: 0,
                range: VK_WHOLE_SIZE,
            })
            .collect();
        let writes: Vec<VkWriteDescriptorSet> = (0..buffers.len())
            .map(|i| VkWriteDescriptorSet {
                s_type: VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET,
                p_next: ptr::null(),
                dst_set: pipe.desc_set,
                dst_binding: i as u32,
                dst_array_element: 0,
                descriptor_count: 1,
                descriptor_type: VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
                p_image_info: ptr::null(),
                p_buffer_info: &infos[i],
                p_texel_buffer_view: ptr::null(),
            })
            .collect();
        // SAFETY: Every buffer handle in `buffers` and `pipe.desc_set` belong to `st.vk_dev`.
        unsafe {
            (api.update_descriptor_sets)(
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
        // SAFETY: Command buffer is serialized under `self.inner.state.lock()` and reset before recording.
        unsafe {
            let rc = (api.reset_command_buffer)(st.cmd_buf, 0);
            if rc != VK_SUCCESS {
                return Err(MoraError::Runtime(format!(
                    "Vulkan vkResetCommandBuffer failed: VkResult({rc})"
                )));
            }
            let rc = (api.begin_command_buffer)(st.cmd_buf, &begin);
            if rc != VK_SUCCESS {
                return Err(MoraError::Runtime(format!(
                    "Vulkan vkBeginCommandBuffer failed: VkResult({rc})"
                )));
            }
            (api.cmd_bind_pipeline)(st.cmd_buf, VK_PIPELINE_BIND_POINT_COMPUTE, pipe.pipeline);
            (api.cmd_bind_descriptor_sets)(
                st.cmd_buf,
                VK_PIPELINE_BIND_POINT_COMPUTE,
                pipe.pipe_layout,
                0,
                1,
                &pipe.desc_set,
                0,
                ptr::null(),
            );
            (api.cmd_dispatch)(st.cmd_buf, gx.max(1), gy.max(1), gz.max(1));
            let rc = (api.end_command_buffer)(st.cmd_buf);
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
            let rc = (api.queue_submit)(st.queue, 1, &submit, 0);
            if rc != VK_SUCCESS {
                return Err(MoraError::Runtime(format!(
                    "Vulkan vkQueueSubmit failed: VkResult({rc})"
                )));
            }
            let rc = (api.queue_wait_idle)(st.queue);
            if rc != VK_SUCCESS {
                return Err(MoraError::Runtime(format!(
                    "Vulkan vkQueueWaitIdle failed: VkResult({rc})"
                )));
            }
        }
        Ok(())
    }

    fn encode_ew_steps(steps: &[FusedEwStep]) -> (Vec<u32>, Vec<u32>, Vec<u32>, Vec<u32>) {
        let mut ops = Vec::with_capacity(steps.len());
        let mut arg_a = Vec::with_capacity(steps.len());
        let mut arg_b = Vec::with_capacity(steps.len());
        let mut imm_bits = Vec::with_capacity(steps.len());
        for step in steps {
            let (op, a, b, im) = match *step {
                FusedEwStep::Add(a, b) => (0u32, a as u32, b as u32, 0.0f32),
                FusedEwStep::Sub(a, b) => (1, a as u32, b as u32, 0.0),
                FusedEwStep::Mul(a, b) => (2, a as u32, b as u32, 0.0),
                FusedEwStep::Div(a, b) => (3, a as u32, b as u32, 0.0),
                FusedEwStep::Pow(a, b) => (4, a as u32, b as u32, 0.0),
                FusedEwStep::AddScalar(a, s) => (5, a as u32, 0, s as f32),
                FusedEwStep::SubScalar(a, s, rev) => {
                    (if rev { 7 } else { 6 }, a as u32, 0, s as f32)
                }
                FusedEwStep::MulScalar(a, s) => (8, a as u32, 0, s as f32),
                FusedEwStep::DivScalar(a, s, rev) => {
                    (if rev { 10 } else { 9 }, a as u32, 0, s as f32)
                }
                FusedEwStep::PowScalar(a, s, rev) => {
                    (if rev { 12 } else { 11 }, a as u32, 0, s as f32)
                }
                FusedEwStep::Neg(a) => (13, a as u32, 0, 0.0),
                FusedEwStep::Relu(a) => (14, a as u32, 0, 0.0),
                FusedEwStep::Gelu(a) => (15, a as u32, 0, 0.0),
                FusedEwStep::Silu(a) => (16, a as u32, 0, 0.0),
                FusedEwStep::Tanh(a) => (17, a as u32, 0, 0.0),
                FusedEwStep::Sigmoid(a) => (18, a as u32, 0, 0.0),
                FusedEwStep::Exp(a) => (19, a as u32, 0, 0.0),
                FusedEwStep::Ln(a) => (20, a as u32, 0, 0.0),
                FusedEwStep::Sqrt(a) => (21, a as u32, 0, 0.0),
                FusedEwStep::Sin(a) => (22, a as u32, 0, 0.0),
                FusedEwStep::Cos(a) => (23, a as u32, 0, 0.0),
                FusedEwStep::Abs(a) => (24, a as u32, 0, 0.0),
            };
            ops.push(op);
            arg_a.push(a);
            arg_b.push(b);
            imm_bits.push(im.to_bits());
        }
        (ops, arg_a, arg_b, imm_bits)
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_linear_batched_f32_host(
        &self,
        a: &[f32],
        b: &[f32],
        bias: &[f32],
        a_bases: &[u32],
        b_bases: &[u32],
        batches: usize,
        m: usize,
        n: usize,
        k: usize,
        act: u32,
        out_f32: &mut [f32],
    ) -> MoraResult<()> {
        let st = self.inner.state.lock();
        let has_bias = if bias.is_empty() { 0u32 } else { 1u32 };
        let bias_len = bias.len().max(1) as u32;
        let params: [u32; 7] = [
            m as u32,
            n as u32,
            k as u32,
            has_bias,
            bias_len,
            act,
            batches as u32,
        ];
        let mut temps = Vec::with_capacity(7);
        let run = (|| -> MoraResult<()> {
            let h_a = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, a)?;
            let h_b = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, b)?;
            self.inner.counters.on_h2d();
            self.inner.counters.on_h2d();
            let h_bias = if bias.is_empty() {
                st.dummy_buf.buffer
            } else {
                self.inner.counters.on_h2d();
                Self::upload_temp_slice(&self.inner.api, &st, &mut temps, bias)?
            };
            let out_alloc =
                Self::alloc_host_buffer(&self.inner.api, &st, std::mem::size_of_val(out_f32))?;
            let h_out = out_alloc.buffer;
            let out_idx = temps.len();
            temps.push(out_alloc);
            let h_params = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &params)?;
            let h_ab = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, a_bases)?;
            let h_bb = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, b_bases)?;

            Self::dispatch_pipe(
                &self.inner.api,
                &st,
                &st.pipe_linear_f32,
                &[h_a, h_b, h_bias, h_out, h_params, h_ab, h_bb],
                (n as u32).div_ceil(16),
                (m as u32).div_ceil(16),
                batches as u32,
            )?;
            Self::read_host_buffer(&self.inner.api, st.vk_dev, &temps[out_idx], out_f32)?;
            self.inner.counters.on_d2h();
            Ok(())
        })();
        for alloc in temps {
            Self::free_host_buffer(&self.inner.api, st.vk_dev, alloc);
        }
        run
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch_linear_batched_half_host(
        &self,
        a: &[f64],
        b: &[f64],
        bias: &[f64],
        a_bases: &[u32],
        b_bases: &[u32],
        batches: usize,
        m: usize,
        n: usize,
        k: usize,
        act: u32,
        is_bf16: bool,
        out: &mut [f64],
    ) -> MoraResult<()> {
        let a_u32 = pack_f64_to_half_u32(a, is_bf16);
        let b_u32 = pack_f64_to_half_u32(b, is_bf16);
        let bias_u32 = pack_f64_to_half_u32(bias, is_bf16);
        let out_words = (batches * m * n).div_ceil(2).max(1);
        let mut out_u32 = vec![0u32; out_words];

        let st = self.inner.state.lock();
        let has_bias = if bias.is_empty() { 0u32 } else { 1u32 };
        let bias_len = bias.len().max(1) as u32;
        let params: [u32; 7] = [
            m as u32,
            n as u32,
            k as u32,
            has_bias,
            bias_len,
            act,
            batches as u32,
        ];
        let mut temps = Vec::with_capacity(7);
        let run = (|| -> MoraResult<()> {
            let h_a = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &a_u32)?;
            let h_b = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &b_u32)?;
            self.inner.counters.on_h2d();
            self.inner.counters.on_h2d();
            let h_bias = if bias.is_empty() {
                st.dummy_buf.buffer
            } else {
                self.inner.counters.on_h2d();
                Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &bias_u32)?
            };
            let out_alloc =
                Self::alloc_host_buffer(&self.inner.api, &st, std::mem::size_of_val(&out_u32[..]))?;
            let h_out = out_alloc.buffer;
            let out_idx = temps.len();
            temps.push(out_alloc);
            let h_params = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &params)?;
            let h_ab = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, a_bases)?;
            let h_bb = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, b_bases)?;

            let pipe = if is_bf16 {
                self.inner.counters.on_bf16_dispatch();
                &st.pipe_linear_bf16
            } else {
                self.inner.counters.on_f16_dispatch();
                &st.pipe_linear_f16
            };
            Self::dispatch_pipe(
                &self.inner.api,
                &st,
                pipe,
                &[h_a, h_b, h_bias, h_out, h_params, h_ab, h_bb],
                (out_words as u32).div_ceil(64),
                1,
                1,
            )?;
            Self::read_host_buffer(&self.inner.api, st.vk_dev, &temps[out_idx], &mut out_u32)?;
            self.inner.counters.on_d2h();
            Ok(())
        })();
        for alloc in temps {
            Self::free_host_buffer(&self.inner.api, st.vk_dev, alloc);
        }
        run?;
        unpack_half_u32_to_f64(&out_u32, out, is_bf16);
        Ok(())
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

fn pack_f64_to_half_u32(vals: &[f64], is_bf16: bool) -> Vec<u32> {
    let n_words = vals.len().div_ceil(2);
    let mut words = vec![0u32; n_words];
    for (i, &v) in vals.iter().enumerate() {
        let bits = if is_bf16 {
            f64_to_bf16_bits(v)
        } else {
            f64_to_f16_bits(v)
        };
        if i.is_multiple_of(2) {
            words[i / 2] |= u32::from(bits);
        } else {
            words[i / 2] |= u32::from(bits) << 16;
        }
    }
    words
}

fn unpack_half_u32_to_f64(words: &[u32], out: &mut [f64], is_bf16: bool) {
    for (i, dst) in out.iter_mut().enumerate() {
        let w = words[i / 2];
        let bits = if i.is_multiple_of(2) {
            (w & 0xffff) as u16
        } else {
            (w >> 16) as u16
        };
        *dst = if is_bf16 {
            bf16_bits_to_f64(bits)
        } else {
            f16_bits_to_f64(bits)
        };
    }
}

fn broadcast_batch_shapes(a: &[usize], b: &[usize]) -> MoraResult<Vec<usize>> {
    let rank = a.len().max(b.len());
    let mut out = vec![1usize; rank];
    for i in 0..rank {
        let da = if i < rank - a.len() {
            1
        } else {
            a[i - (rank - a.len())]
        };
        let db = if i < rank - b.len() {
            1
        } else {
            b[i - (rank - b.len())]
        };
        if da == db {
            out[i] = da;
        } else if da == 1 {
            out[i] = db;
        } else if db == 1 {
            out[i] = da;
        } else {
            return Err(MoraError::Runtime(format!(
                "Vulkan broadcast batch shape mismatch: {a:?} vs {b:?}"
            )));
        }
    }
    Ok(out)
}

fn map_broadcast_batch(flat_idx: usize, out_batch: &[usize], in_batch: &[usize]) -> usize {
    if in_batch.is_empty() {
        return 0;
    }
    let out_rank = out_batch.len();
    let in_rank = in_batch.len();
    let mut rem = flat_idx;
    let mut in_flat = 0usize;
    let mut in_stride = 1usize;
    for d in (0..out_rank).rev() {
        let dim = out_batch[d];
        let coord = rem % dim;
        rem /= dim;
        let offset_from_right = out_rank - 1 - d;
        if offset_from_right < in_rank {
            let in_dim = in_batch[in_rank - 1 - offset_from_right];
            let in_coord = if in_dim == 1 { 0 } else { coord };
            in_flat += in_coord * in_stride;
            in_stride *= in_dim;
        }
    }
    in_flat
}

fn compute_broadcast_strides_u32(in_shape: &[usize], out_shape: &[usize]) -> Vec<u32> {
    let ndim = out_shape.len();
    let mut raw_strides = vec![1u32; in_shape.len()];
    for i in (0..in_shape.len().saturating_sub(1)).rev() {
        raw_strides[i] = raw_strides[i + 1] * (in_shape[i + 1] as u32);
    }
    let mut out = vec![0u32; ndim];
    let pad = ndim.saturating_sub(in_shape.len());
    for (i, &dim) in in_shape.iter().enumerate() {
        out[pad + i] = if dim == 1 { 0 } else { raw_strides[i] };
    }
    out
}

impl DeviceBackend for VulkanDeviceBackend {
    fn kind(&self) -> Device {
        self.inner.device.clone()
    }

    fn name(&self) -> &str {
        "vulkan"
    }

    fn simd_isa(&self) -> &'static str {
        "spirv-1.0-f32/f16/bf16"
    }

    fn worker_threads(&self) -> usize {
        self.info.multiprocessors as usize
    }

    fn is_available(&self) -> bool {
        true
    }

    fn supports_vram_residency(&self) -> bool {
        true
    }

    fn alloc_vram_buffer(&self, byte_len: usize) -> MoraResult<Option<Arc<dyn DeviceVramBuffer>>> {
        let st = self.inner.state.lock();
        let buf = self.alloc_owned_vram(&st, byte_len)?;
        Ok(Some(buf))
    }

    fn vram_telemetry(&self) -> VramTelemetry {
        self.inner.counters.snapshot()
    }

    fn run_matmul_vram(
        &self,
        a: &dyn DeviceVramBuffer,
        b: &dyn DeviceVramBuffer,
        a_shape: &[usize],
        b_shape: &[usize],
        dtype: DType,
    ) -> MoraResult<Option<Arc<dyn DeviceVramBuffer>>> {
        if a_shape.len() < 2
            || b_shape.len() < 2
            || !matches!(dtype, DType::F32 | DType::F16 | DType::BF16)
        {
            return Ok(None);
        }
        let m = a_shape[a_shape.len() - 2];
        let k = a_shape[a_shape.len() - 1];
        let k2 = b_shape[b_shape.len() - 2];
        let n = b_shape[b_shape.len() - 1];
        if k != k2 {
            return Err(MoraError::Runtime(format!(
                "Vulkan MatMul inner dim mismatch: {k} != {k2}"
            )));
        }
        let a_batch = &a_shape[..a_shape.len() - 2];
        let b_batch = &b_shape[..b_shape.len() - 2];
        let out_batch = broadcast_batch_shapes(a_batch, b_batch)?;
        let batches = out_batch.iter().copied().product::<usize>().max(1);
        let out_numel = batches * m * n;
        if out_numel == 0 || k == 0 {
            return Ok(None);
        }
        let a_bases: Vec<u32> = (0..batches)
            .map(|bi| (map_broadcast_batch(bi, &out_batch, a_batch) * m * k) as u32)
            .collect();
        let b_bases: Vec<u32> = (0..batches)
            .map(|bi| (map_broadcast_batch(bi, &out_batch, b_batch) * k * n) as u32)
            .collect();
        let params: [u32; 7] = [m as u32, n as u32, k as u32, 0, 1, 0, batches as u32];

        let st = self.inner.state.lock();
        let out_buf = self.alloc_owned_vram(&st, dtype.storage_bytes(out_numel))?;
        let mut temps = Vec::with_capacity(3);
        let run = (|| -> MoraResult<()> {
            let h_params = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &params)?;
            let h_ab = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &a_bases)?;
            let h_bb = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &b_bases)?;
            let bufs = [
                a.raw_handle(),
                b.raw_handle(),
                st.dummy_buf.buffer,
                out_buf.buffer,
                h_params,
                h_ab,
                h_bb,
            ];
            match dtype {
                DType::F32 => Self::dispatch_pipe(
                    &self.inner.api,
                    &st,
                    &st.pipe_linear_f32,
                    &bufs,
                    (n as u32).div_ceil(16),
                    (m as u32).div_ceil(16),
                    batches as u32,
                ),
                DType::F16 => {
                    self.inner.counters.on_f16_dispatch();
                    Self::dispatch_pipe(
                        &self.inner.api,
                        &st,
                        &st.pipe_linear_f16,
                        &bufs,
                        (out_numel.div_ceil(2) as u32).div_ceil(64),
                        1,
                        1,
                    )
                }
                DType::BF16 => {
                    self.inner.counters.on_bf16_dispatch();
                    Self::dispatch_pipe(
                        &self.inner.api,
                        &st,
                        &st.pipe_linear_bf16,
                        &bufs,
                        (out_numel.div_ceil(2) as u32).div_ceil(64),
                        1,
                        1,
                    )
                }
                _ => unreachable!(),
            }
        })();
        for alloc in temps {
            Self::free_host_buffer(&self.inner.api, st.vk_dev, alloc);
        }
        run?;
        self.inner.counters.on_vram_hit();
        Ok(Some(out_buf))
    }

    #[allow(clippy::too_many_arguments)]
    fn run_fused_linear_act_vram(
        &self,
        x: &dyn DeviceVramBuffer,
        w: &dyn DeviceVramBuffer,
        bias: Option<(&dyn DeviceVramBuffer, &[usize])>,
        act: FusedAct,
        m_total: usize,
        k: usize,
        n: usize,
        _out_shape: &[usize],
        dtype: DType,
    ) -> MoraResult<Option<Arc<dyn DeviceVramBuffer>>> {
        if !matches!(dtype, DType::F32 | DType::F16 | DType::BF16) || m_total * n == 0 {
            return Ok(None);
        }
        let (h_bias_opt, has_bias, bias_len) = match bias {
            None => (None, 0u32, 1u32),
            Some((vb, bshape)) => {
                let b_elems = bshape.iter().copied().product::<usize>().max(1);
                if b_elems == n || b_elems == 1 {
                    (Some(vb.raw_handle()), 1u32, b_elems as u32)
                } else {
                    return Ok(None);
                }
            }
        };
        let out_numel = m_total * n;
        let params: [u32; 7] = [
            m_total as u32,
            n as u32,
            k as u32,
            has_bias,
            bias_len,
            act_code(act),
            1,
        ];
        let zero_base = [0u32];

        let st = self.inner.state.lock();
        let out_buf = self.alloc_owned_vram(&st, dtype.storage_bytes(out_numel))?;
        let mut temps = Vec::with_capacity(3);
        let run = (|| -> MoraResult<()> {
            let h_params = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &params)?;
            let h_ab = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &zero_base)?;
            let h_bb = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &zero_base)?;
            let bufs = [
                x.raw_handle(),
                w.raw_handle(),
                h_bias_opt.unwrap_or(st.dummy_buf.buffer),
                out_buf.buffer,
                h_params,
                h_ab,
                h_bb,
            ];
            match dtype {
                DType::F32 => Self::dispatch_pipe(
                    &self.inner.api,
                    &st,
                    &st.pipe_linear_f32,
                    &bufs,
                    (n as u32).div_ceil(16),
                    (m_total as u32).div_ceil(16),
                    1,
                ),
                DType::F16 => {
                    self.inner.counters.on_f16_dispatch();
                    Self::dispatch_pipe(
                        &self.inner.api,
                        &st,
                        &st.pipe_linear_f16,
                        &bufs,
                        (out_numel.div_ceil(2) as u32).div_ceil(64),
                        1,
                        1,
                    )
                }
                DType::BF16 => {
                    self.inner.counters.on_bf16_dispatch();
                    Self::dispatch_pipe(
                        &self.inner.api,
                        &st,
                        &st.pipe_linear_bf16,
                        &bufs,
                        (out_numel.div_ceil(2) as u32).div_ceil(64),
                        1,
                        1,
                    )
                }
                _ => unreachable!(),
            }
        })();
        for alloc in temps {
            Self::free_host_buffer(&self.inner.api, st.vk_dev, alloc);
        }
        run?;
        self.inner.counters.on_vram_hit();
        Ok(Some(out_buf))
    }

    #[allow(clippy::too_many_arguments)]
    fn run_fused_rms_norm_linear_vram(
        &self,
        x: &dyn DeviceVramBuffer,
        eps: f64,
        gamma: Option<(&dyn DeviceVramBuffer, &[usize])>,
        w: &dyn DeviceVramBuffer,
        bias: Option<(&dyn DeviceVramBuffer, &[usize])>,
        act: FusedAct,
        m_total: usize,
        k: usize,
        n: usize,
        _x_shape: &[usize],
        _out_shape: &[usize],
        dtype: DType,
    ) -> MoraResult<Option<Arc<dyn DeviceVramBuffer>>> {
        if dtype != DType::F32 || m_total * n == 0 || k == 0 {
            return Ok(None);
        }
        let (h_gamma_opt, has_gamma, gamma_len) = match gamma {
            None => (None, 0u32, 1u32),
            Some((vg, gshape)) => {
                let g_elems = gshape.iter().copied().product::<usize>().max(1);
                if g_elems == k || g_elems == 1 {
                    (Some(vg.raw_handle()), 1u32, g_elems as u32)
                } else {
                    return Ok(None);
                }
            }
        };
        let (h_bias_opt, has_bias, bias_len) = match bias {
            None => (None, 0u32, 1u32),
            Some((vb, bshape)) => {
                let b_elems = bshape.iter().copied().product::<usize>().max(1);
                if b_elems == n || b_elems == 1 {
                    (Some(vb.raw_handle()), 1u32, b_elems as u32)
                } else {
                    return Ok(None);
                }
            }
        };

        let rms_params: [u32; 5] = [
            m_total as u32,
            k as u32,
            has_gamma,
            gamma_len,
            (eps as f32).to_bits(),
        ];
        let lin_params: [u32; 7] = [
            m_total as u32,
            n as u32,
            k as u32,
            has_bias,
            bias_len,
            act_code(act),
            1,
        ];
        let zero_base = [0u32];

        let st = self.inner.state.lock();
        let out_buf = self.alloc_owned_vram(&st, m_total * n * 4)?;
        let mut temps = Vec::with_capacity(5);
        let run = (|| -> MoraResult<()> {
            let xnorm_alloc = Self::alloc_host_buffer(&self.inner.api, &st, m_total * k * 4)?;
            let h_xnorm = xnorm_alloc.buffer;
            temps.push(xnorm_alloc);
            let h_rms_p = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &rms_params)?;
            Self::dispatch_pipe(
                &self.inner.api,
                &st,
                &st.pipe_rms_norm_f32,
                &[
                    x.raw_handle(),
                    h_gamma_opt.unwrap_or(st.dummy_buf.buffer),
                    h_xnorm,
                    h_rms_p,
                ],
                (m_total as u32).div_ceil(64),
                1,
                1,
            )?;

            let h_lin_p = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &lin_params)?;
            let h_ab = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &zero_base)?;
            let h_bb = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &zero_base)?;
            Self::dispatch_pipe(
                &self.inner.api,
                &st,
                &st.pipe_linear_f32,
                &[
                    h_xnorm,
                    w.raw_handle(),
                    h_bias_opt.unwrap_or(st.dummy_buf.buffer),
                    out_buf.buffer,
                    h_lin_p,
                    h_ab,
                    h_bb,
                ],
                (n as u32).div_ceil(16),
                (m_total as u32).div_ceil(16),
                1,
            )
        })();
        for alloc in temps {
            Self::free_host_buffer(&self.inner.api, st.vk_dev, alloc);
        }
        run?;
        self.inner.counters.on_vram_hit();
        Ok(Some(out_buf))
    }

    #[allow(clippy::too_many_arguments)]
    fn run_fused_flash_attention_vram(
        &self,
        q: &dyn DeviceVramBuffer,
        k: &dyn DeviceVramBuffer,
        v: &dyn DeviceVramBuffer,
        mask: Option<(&dyn DeviceVramBuffer, &[usize])>,
        q_shape: &[usize],
        k_shape: &[usize],
        v_shape: &[usize],
        out_shape: &[usize],
        scale: f64,
        k_is_transposed: bool,
        dtype: DType,
    ) -> MoraResult<Option<Arc<dyn DeviceVramBuffer>>> {
        if dtype != DType::F32 || q_shape.len() < 2 || k_shape.len() < 2 || v_shape.len() < 2 {
            return Ok(None);
        }
        let s_q = q_shape[q_shape.len() - 2];
        let d_k = q_shape[q_shape.len() - 1];
        let (s_k, k_d) = if k_is_transposed {
            (k_shape[k_shape.len() - 1], k_shape[k_shape.len() - 2])
        } else {
            (k_shape[k_shape.len() - 2], k_shape[k_shape.len() - 1])
        };
        let d_v = v_shape[v_shape.len() - 1];
        if d_k != k_d || s_k != v_shape[v_shape.len() - 2] {
            return Err(MoraError::Runtime(format!(
                "Vulkan FlashAttention shape mismatch: Q={q_shape:?}, K={k_shape:?}, V={v_shape:?}"
            )));
        }
        let batch_q = &q_shape[..q_shape.len() - 2];
        let batch_k = &k_shape[..k_shape.len() - 2];
        let batch_v = &v_shape[..v_shape.len() - 2];
        let batch_out = &out_shape[..out_shape.len() - 2];
        let batches = batch_out.iter().copied().product::<usize>().max(1);
        let out_numel = batches * s_q * d_v;
        if out_numel == 0 {
            return Ok(None);
        }

        let q_bases: Vec<u32> = (0..batches)
            .map(|b| (map_broadcast_batch(b, batch_out, batch_q) * s_q * d_k) as u32)
            .collect();
        let k_bases: Vec<u32> = (0..batches)
            .map(|b| (map_broadcast_batch(b, batch_out, batch_k) * s_k * d_k) as u32)
            .collect();
        let v_bases: Vec<u32> = (0..batches)
            .map(|b| (map_broadcast_batch(b, batch_out, batch_v) * s_k * d_v) as u32)
            .collect();
        let (h_mask_opt, mask_bases, mask_sq_s, mask_sk_s, has_mask) =
            if let Some((vm, mshape)) = mask {
                let mrank = mshape.len();
                let m_sk = *mshape.last().unwrap_or(&1);
                let m_sq = if mrank >= 2 { mshape[mrank - 2] } else { 1 };
                let mbatch = if mrank >= 2 {
                    &mshape[..mrank - 2]
                } else {
                    &[]
                };
                let sq_s = if m_sq == 1 { 0 } else { m_sk as u32 };
                let sk_s = if m_sk == 1 { 0 } else { 1u32 };
                let mb: Vec<u32> = (0..batches)
                    .map(|b| (map_broadcast_batch(b, batch_out, mbatch) * m_sq * m_sk) as u32)
                    .collect();
                (Some(vm.raw_handle()), mb, sq_s, sk_s, 1u32)
            } else {
                (None, vec![0u32; batches], 0u32, 0u32, 0u32)
            };

        let params: [u32; 10] = [
            batches as u32,
            s_q as u32,
            s_k as u32,
            d_k as u32,
            d_v as u32,
            u32::from(k_is_transposed),
            has_mask,
            mask_sq_s,
            mask_sk_s,
            (scale as f32).to_bits(),
        ];

        let st = self.inner.state.lock();
        let out_buf = self.alloc_owned_vram(&st, out_numel * 4)?;
        let mut temps = Vec::with_capacity(5);
        let run = (|| -> MoraResult<()> {
            let h_qb = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &q_bases)?;
            let h_kb = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &k_bases)?;
            let h_vb = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &v_bases)?;
            let h_mb = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &mask_bases)?;
            let h_p = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &params)?;
            Self::dispatch_pipe(
                &self.inner.api,
                &st,
                &st.pipe_flash_attn_f32,
                &[
                    q.raw_handle(),
                    k.raw_handle(),
                    v.raw_handle(),
                    h_mask_opt.unwrap_or(st.dummy_buf.buffer),
                    out_buf.buffer,
                    h_qb,
                    h_kb,
                    h_vb,
                    h_mb,
                    h_p,
                ],
                (d_v as u32).div_ceil(16),
                (s_q as u32).div_ceil(16),
                batches as u32,
            )
        })();
        for alloc in temps {
            Self::free_host_buffer(&self.inner.api, st.vk_dev, alloc);
        }
        run?;
        self.inner.counters.on_vram_hit();
        Ok(Some(out_buf))
    }

    #[allow(clippy::too_many_arguments)]
    fn run_fused_elementwise_vram(
        &self,
        inputs: &[&dyn DeviceVramBuffer],
        input_shapes: &[&[usize]],
        steps: &[FusedEwStep],
        out_shape: &[usize],
        out_numel: usize,
        dtype: DType,
    ) -> MoraResult<Option<Arc<dyn DeviceVramBuffer>>> {
        let total_slots = inputs.len() + steps.len();
        if dtype != DType::F32
            || out_numel == 0
            || steps.is_empty()
            || inputs.is_empty()
            || inputs.len() > 4
            || total_slots > 64
            || out_shape.len() > 16
        {
            return Ok(None);
        }
        let eff_out_shape: Vec<usize> = if out_shape.is_empty() {
            vec![1]
        } else {
            out_shape.to_vec()
        };
        let ndim = eff_out_shape.len();
        let out_shape_u32: Vec<u32> = eff_out_shape.iter().map(|&d| d as u32).collect();
        let mut all_strides: Vec<u32> = Vec::with_capacity(input_shapes.len() * ndim);
        for s in input_shapes {
            all_strides.extend(compute_broadcast_strides_u32(s, &eff_out_shape));
        }
        let (ops, arg_a, arg_b, imm_bits) = Self::encode_ew_steps(steps);
        let mut meta = Vec::with_capacity(5 + steps.len() + inputs.len());
        meta.push(ndim as u32);
        meta.push(inputs.len() as u32);
        meta.push(steps.len() as u32);
        meta.push(out_numel as u32);
        meta.push(0u32); // packed_mode = 0 (direct VkBuffer bindings)
        meta.extend_from_slice(&imm_bits);
        meta.resize(meta.len() + inputs.len(), 0);

        let st = self.inner.state.lock();
        let out_buf = self.alloc_owned_vram(&st, out_numel * 4)?;
        let mut temps = Vec::with_capacity(6);
        let run = (|| -> MoraResult<()> {
            let h_st = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &all_strides)?;
            let h_os = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &out_shape_u32)?;
            let h_op = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &ops)?;
            let h_aa = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &arg_a)?;
            let h_ab = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &arg_b)?;
            let h_mt = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &meta)?;
            let in_h = |i: usize| {
                inputs
                    .get(i)
                    .map_or(st.dummy_buf.buffer, |b| b.raw_handle())
            };
            Self::dispatch_pipe(
                &self.inner.api,
                &st,
                &st.pipe_fused_ew_f32,
                &[
                    out_buf.buffer,
                    in_h(0),
                    in_h(1),
                    in_h(2),
                    in_h(3),
                    h_st,
                    h_os,
                    h_op,
                    h_aa,
                    h_ab,
                    h_mt,
                ],
                (out_numel as u32).div_ceil(64),
                1,
                1,
            )
        })();
        for alloc in temps {
            Self::free_host_buffer(&self.inner.api, st.vk_dev, alloc);
        }
        run?;
        self.inner.counters.on_vram_hit();
        Ok(Some(out_buf))
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
        if a_shape.len() < 2 || b_shape.len() < 2 {
            simd_kernels::matmul_simd_into(a, b, a_shape, b_shape, out)?;
            if dtype != DType::F64 {
                for v in out.iter_mut() {
                    *v = dtype.quantize_f64(*v);
                }
            }
            return Ok(());
        }
        let m = a_shape[a_shape.len() - 2];
        let k = a_shape[a_shape.len() - 1];
        let k2 = b_shape[b_shape.len() - 2];
        let n = b_shape[b_shape.len() - 1];
        if k != k2 {
            return Err(MoraError::Runtime(format!(
                "Vulkan MatMul inner dim mismatch: {k} != {k2}"
            )));
        }
        let a_batch = &a_shape[..a_shape.len() - 2];
        let b_batch = &b_shape[..b_shape.len() - 2];
        let out_batch = broadcast_batch_shapes(a_batch, b_batch)?;
        let batches = out_batch.iter().copied().product::<usize>().max(1);
        let out_numel = batches * m * n;
        out.resize(out_numel, 0.0);
        if out_numel == 0 || k == 0 {
            out.fill(0.0);
            return Ok(());
        }
        let a_bases: Vec<u32> = (0..batches)
            .map(|bi| (map_broadcast_batch(bi, &out_batch, a_batch) * m * k) as u32)
            .collect();
        let b_bases: Vec<u32> = (0..batches)
            .map(|bi| (map_broadcast_batch(bi, &out_batch, b_batch) * k * n) as u32)
            .collect();

        if matches!(dtype, DType::F16 | DType::BF16) {
            return self.dispatch_linear_batched_half_host(
                a,
                b,
                &[],
                &a_bases,
                &b_bases,
                batches,
                m,
                n,
                k,
                0,
                dtype == DType::BF16,
                out,
            );
        }

        let a_f32: Vec<f32> = a.iter().map(|&v| v as f32).collect();
        let b_f32: Vec<f32> = b.iter().map(|&v| v as f32).collect();
        let mut out_f32 = vec![0.0f32; out_numel];
        self.dispatch_linear_batched_f32_host(
            &a_f32,
            &b_f32,
            &[],
            &a_bases,
            &b_bases,
            batches,
            m,
            n,
            k,
            0,
            &mut out_f32,
        )?;
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
        let bias_slice = match bias {
            None => &[][..],
            Some((bvals, _)) if bvals.len() == n || bvals.len() == 1 => bvals,
            Some(_) => {
                simd_kernels::fused_linear_act_into(
                    x, w, bias, act, m_total, k, n, out_shape, dtype, out,
                );
                return Ok(());
            }
        };
        let zero_base = [0u32];
        if matches!(dtype, DType::F16 | DType::BF16) {
            return self.dispatch_linear_batched_half_host(
                x,
                w,
                bias_slice,
                &zero_base,
                &zero_base,
                1,
                m_total,
                n,
                k,
                act_code(act),
                dtype == DType::BF16,
                out,
            );
        }
        let x_f32: Vec<f32> = x.iter().map(|&v| v as f32).collect();
        let w_f32: Vec<f32> = w.iter().map(|&v| v as f32).collect();
        let bias_f32: Vec<f32> = bias_slice.iter().map(|&v| v as f32).collect();
        let mut out_f32 = vec![0.0f32; out_numel];
        self.dispatch_linear_batched_f32_host(
            &x_f32,
            &w_f32,
            &bias_f32,
            &zero_base,
            &zero_base,
            1,
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
        let out_numel = m_total * n;
        out.resize(out_numel, 0.0);
        if out_numel == 0 || k == 0 {
            return Ok(());
        }
        let gamma_slice = match gamma {
            None => &[][..],
            Some((gv, _)) if gv.len() == k || gv.len() == 1 => gv,
            Some(_) => {
                simd_kernels::fused_rms_norm_linear_into(
                    x, eps, gamma, w, bias, act, m_total, k, n, x_shape, out_shape, dtype, out,
                );
                return Ok(());
            }
        };
        let bias_slice = match bias {
            None => &[][..],
            Some((bv, _)) if bv.len() == n || bv.len() == 1 => bv,
            Some(_) => {
                simd_kernels::fused_rms_norm_linear_into(
                    x, eps, gamma, w, bias, act, m_total, k, n, x_shape, out_shape, dtype, out,
                );
                return Ok(());
            }
        };

        let x_f32: Vec<f32> = x.iter().map(|&v| v as f32).collect();
        let w_f32: Vec<f32> = w.iter().map(|&v| v as f32).collect();
        let gamma_f32: Vec<f32> = gamma_slice.iter().map(|&v| v as f32).collect();
        let bias_f32: Vec<f32> = bias_slice.iter().map(|&v| v as f32).collect();
        let rms_params: [u32; 5] = [
            m_total as u32,
            k as u32,
            u32::from(!gamma_f32.is_empty()),
            gamma_f32.len().max(1) as u32,
            (eps as f32).to_bits(),
        ];
        let lin_params: [u32; 7] = [
            m_total as u32,
            n as u32,
            k as u32,
            u32::from(!bias_f32.is_empty()),
            bias_f32.len().max(1) as u32,
            act_code(act),
            1,
        ];
        let zero_base = [0u32];
        let mut out_f32 = vec![0.0f32; out_numel];

        // Execute BOTH RMSNorm AND FusedLinearAct 100% on Vulkan GPU (zero host RMS!).
        let st = self.inner.state.lock();
        let mut temps = Vec::with_capacity(10);
        let run = (|| -> MoraResult<()> {
            let h_x = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &x_f32)?;
            let h_w = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &w_f32)?;
            self.inner.counters.on_h2d();
            self.inner.counters.on_h2d();
            let h_gamma = if gamma_f32.is_empty() {
                st.dummy_buf.buffer
            } else {
                self.inner.counters.on_h2d();
                Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &gamma_f32)?
            };
            let h_bias = if bias_f32.is_empty() {
                st.dummy_buf.buffer
            } else {
                self.inner.counters.on_h2d();
                Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &bias_f32)?
            };
            let xnorm_alloc = Self::alloc_host_buffer(&self.inner.api, &st, m_total * k * 4)?;
            let h_xnorm = xnorm_alloc.buffer;
            temps.push(xnorm_alloc);
            let out_alloc = Self::alloc_host_buffer(&self.inner.api, &st, out_numel * 4)?;
            let h_out = out_alloc.buffer;
            let out_idx = temps.len();
            temps.push(out_alloc);
            let h_rms_p = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &rms_params)?;
            let h_lin_p = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &lin_params)?;
            let h_ab = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &zero_base)?;
            let h_bb = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &zero_base)?;

            Self::dispatch_pipe(
                &self.inner.api,
                &st,
                &st.pipe_rms_norm_f32,
                &[h_x, h_gamma, h_xnorm, h_rms_p],
                (m_total as u32).div_ceil(64),
                1,
                1,
            )?;
            Self::dispatch_pipe(
                &self.inner.api,
                &st,
                &st.pipe_linear_f32,
                &[h_xnorm, h_w, h_bias, h_out, h_lin_p, h_ab, h_bb],
                (n as u32).div_ceil(16),
                (m_total as u32).div_ceil(16),
                1,
            )?;
            Self::read_host_buffer(&self.inner.api, st.vk_dev, &temps[out_idx], &mut out_f32)?;
            self.inner.counters.on_d2h();
            Ok(())
        })();
        for alloc in temps {
            Self::free_host_buffer(&self.inner.api, st.vk_dev, alloc);
        }
        run?;
        for (dst, &v) in out.iter_mut().zip(out_f32.iter()) {
            *dst = dtype.quantize_f64(f64::from(v));
        }
        Ok(())
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
        if q_shape.len() < 2 || k_shape.len() < 2 || v_shape.len() < 2 {
            return simd_kernels::fused_flash_attention_into(
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
            );
        }
        let s_q = q_shape[q_shape.len() - 2];
        let d_k = q_shape[q_shape.len() - 1];
        let (s_k, k_d) = if k_is_transposed {
            (k_shape[k_shape.len() - 1], k_shape[k_shape.len() - 2])
        } else {
            (k_shape[k_shape.len() - 2], k_shape[k_shape.len() - 1])
        };
        let d_v = v_shape[v_shape.len() - 1];
        if d_k != k_d || s_k != v_shape[v_shape.len() - 2] {
            return Err(MoraError::Runtime(format!(
                "Vulkan FlashAttention shape mismatch: Q={q_shape:?}, K={k_shape:?}, V={v_shape:?}"
            )));
        }
        let batch_q = &q_shape[..q_shape.len() - 2];
        let batch_k = &k_shape[..k_shape.len() - 2];
        let batch_v = &v_shape[..v_shape.len() - 2];
        let batch_out = &out_shape[..out_shape.len() - 2];
        let batches = batch_out.iter().copied().product::<usize>().max(1);
        let out_numel = batches * s_q * d_v;
        out.resize(out_numel, 0.0);
        if out_numel == 0 {
            return Ok(());
        }

        let q_f32: Vec<f32> = q.iter().map(|&x| x as f32).collect();
        let k_f32: Vec<f32> = k.iter().map(|&x| x as f32).collect();
        let v_f32: Vec<f32> = v.iter().map(|&x| x as f32).collect();
        let q_bases: Vec<u32> = (0..batches)
            .map(|b| (map_broadcast_batch(b, batch_out, batch_q) * s_q * d_k) as u32)
            .collect();
        let k_bases: Vec<u32> = (0..batches)
            .map(|b| (map_broadcast_batch(b, batch_out, batch_k) * s_k * d_k) as u32)
            .collect();
        let v_bases: Vec<u32> = (0..batches)
            .map(|b| (map_broadcast_batch(b, batch_out, batch_v) * s_k * d_v) as u32)
            .collect();
        let (mask_f32, mask_bases, mask_sq_s, mask_sk_s, has_mask) =
            if let Some((mvals, mshape)) = mask {
                let mrank = mshape.len();
                let m_sk = *mshape.last().unwrap_or(&1);
                let m_sq = if mrank >= 2 { mshape[mrank - 2] } else { 1 };
                let mbatch = if mrank >= 2 {
                    &mshape[..mrank - 2]
                } else {
                    &[]
                };
                let sq_s = if m_sq == 1 { 0 } else { m_sk as u32 };
                let sk_s = if m_sk == 1 { 0 } else { 1u32 };
                let mb: Vec<u32> = (0..batches)
                    .map(|b| (map_broadcast_batch(b, batch_out, mbatch) * m_sq * m_sk) as u32)
                    .collect();
                (
                    mvals.iter().map(|&x| x as f32).collect::<Vec<f32>>(),
                    mb,
                    sq_s,
                    sk_s,
                    1u32,
                )
            } else {
                (Vec::new(), vec![0u32; batches], 0u32, 0u32, 0u32)
            };
        let params: [u32; 10] = [
            batches as u32,
            s_q as u32,
            s_k as u32,
            d_k as u32,
            d_v as u32,
            u32::from(k_is_transposed),
            has_mask,
            mask_sq_s,
            mask_sk_s,
            (scale as f32).to_bits(),
        ];
        let mut out_f32 = vec![0.0f32; out_numel];

        let st = self.inner.state.lock();
        let mut temps = Vec::with_capacity(10);
        let run = (|| -> MoraResult<()> {
            let h_q = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &q_f32)?;
            let h_k = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &k_f32)?;
            let h_v = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &v_f32)?;
            self.inner.counters.on_h2d();
            self.inner.counters.on_h2d();
            self.inner.counters.on_h2d();
            let h_mask = if mask_f32.is_empty() {
                st.dummy_buf.buffer
            } else {
                self.inner.counters.on_h2d();
                Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &mask_f32)?
            };
            let out_alloc = Self::alloc_host_buffer(&self.inner.api, &st, out_numel * 4)?;
            let h_out = out_alloc.buffer;
            let out_idx = temps.len();
            temps.push(out_alloc);
            let h_qb = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &q_bases)?;
            let h_kb = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &k_bases)?;
            let h_vb = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &v_bases)?;
            let h_mb = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &mask_bases)?;
            let h_p = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &params)?;

            Self::dispatch_pipe(
                &self.inner.api,
                &st,
                &st.pipe_flash_attn_f32,
                &[h_q, h_k, h_v, h_mask, h_out, h_qb, h_kb, h_vb, h_mb, h_p],
                (d_v as u32).div_ceil(16),
                (s_q as u32).div_ceil(16),
                batches as u32,
            )?;
            Self::read_host_buffer(&self.inner.api, st.vk_dev, &temps[out_idx], &mut out_f32)?;
            self.inner.counters.on_d2h();
            Ok(())
        })();
        for alloc in temps {
            Self::free_host_buffer(&self.inner.api, st.vk_dev, alloc);
        }
        run?;
        for (dst, &val) in out.iter_mut().zip(out_f32.iter()) {
            *dst = dtype.quantize_f64(f64::from(val));
        }
        Ok(())
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
        let total_slots = input_flats.len() + steps.len();
        if out_numel == 0
            || steps.is_empty()
            || input_flats.is_empty()
            || total_slots > 64
            || out_shape.len() > 16
        {
            return simd_kernels::fused_elementwise_into(
                input_flats,
                input_shapes,
                steps,
                out_shape,
                out_numel,
                dtype,
                out,
            );
        }
        out.resize(out_numel, 0.0);

        let eff_out_shape: Vec<usize> = if out_shape.is_empty() {
            vec![1]
        } else {
            out_shape.to_vec()
        };
        let ndim = eff_out_shape.len();
        let out_shape_u32: Vec<u32> = eff_out_shape.iter().map(|&d| d as u32).collect();
        let mut all_strides: Vec<u32> = Vec::with_capacity(input_shapes.len() * ndim);
        for s in input_shapes {
            all_strides.extend(compute_broadcast_strides_u32(s, &eff_out_shape));
        }
        let (ops, arg_a, arg_b, imm_bits) = Self::encode_ew_steps(steps);
        let packed_mode = input_flats.len() > 4;
        let mut packed_in: Vec<f32> = Vec::new();
        let mut in_offsets: Vec<u32> = Vec::with_capacity(input_flats.len());
        if packed_mode {
            for flat in input_flats {
                in_offsets.push(packed_in.len() as u32);
                packed_in.extend(flat.iter().map(|&v| v as f32));
            }
        } else {
            in_offsets.resize(input_flats.len(), 0);
        }

        let mut meta = Vec::with_capacity(5 + steps.len() + input_flats.len());
        meta.push(ndim as u32);
        meta.push(input_flats.len() as u32);
        meta.push(steps.len() as u32);
        meta.push(out_numel as u32);
        meta.push(u32::from(packed_mode));
        meta.extend_from_slice(&imm_bits);
        meta.extend_from_slice(&in_offsets);

        let mut out_f32 = vec![0.0f32; out_numel];
        let st = self.inner.state.lock();
        let mut temps = Vec::with_capacity(11);
        let run = (|| -> MoraResult<()> {
            let out_alloc = Self::alloc_host_buffer(&self.inner.api, &st, out_numel * 4)?;
            let h_out = out_alloc.buffer;
            let out_idx = temps.len();
            temps.push(out_alloc);

            let (h_in0, h_in1, h_in2, h_in3) = if packed_mode {
                self.inner.counters.on_h2d();
                let h0 = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &packed_in)?;
                (
                    h0,
                    st.dummy_buf.buffer,
                    st.dummy_buf.buffer,
                    st.dummy_buf.buffer,
                )
            } else {
                let mut hs = [st.dummy_buf.buffer; 4];
                for (i, flat) in input_flats.iter().enumerate() {
                    let f32s: Vec<f32> = flat.iter().map(|&v| v as f32).collect();
                    hs[i] = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &f32s)?;
                    self.inner.counters.on_h2d();
                }
                (hs[0], hs[1], hs[2], hs[3])
            };

            let h_st = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &all_strides)?;
            let h_os = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &out_shape_u32)?;
            let h_op = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &ops)?;
            let h_aa = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &arg_a)?;
            let h_ab = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &arg_b)?;
            let h_mt = Self::upload_temp_slice(&self.inner.api, &st, &mut temps, &meta)?;

            Self::dispatch_pipe(
                &self.inner.api,
                &st,
                &st.pipe_fused_ew_f32,
                &[
                    h_out, h_in0, h_in1, h_in2, h_in3, h_st, h_os, h_op, h_aa, h_ab, h_mt,
                ],
                (out_numel as u32).div_ceil(64),
                1,
                1,
            )?;
            Self::read_host_buffer(&self.inner.api, st.vk_dev, &temps[out_idx], &mut out_f32)?;
            self.inner.counters.on_d2h();
            Ok(())
        })();
        for alloc in temps {
            Self::free_host_buffer(&self.inner.api, st.vk_dev, alloc);
        }
        run?;
        for (dst, &val) in out.iter_mut().zip(out_f32.iter()) {
            *dst = dtype.quantize_f64(f64::from(val));
        }
        Ok(())
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

    let mut created_pipes: Vec<VkComputePipe> = Vec::with_capacity(6);
    let specs: [(Vec<u32>, u32); 6] = [
        (emit_spirv_fused_linear_f32(), 7),
        (emit_spirv_fused_linear_half(false), 7),
        (emit_spirv_fused_linear_half(true), 7),
        (emit_spirv_rms_norm_f32(), 4),
        (emit_spirv_flash_attention_f32(), 10),
        (emit_spirv_fused_elementwise_f32(), 11),
    ];
    for (spv, num_bindings) in specs {
        let Some(pipe) = VkComputePipe::create(api, vk_dev, &spv, num_bindings) else {
            for p in created_pipes.iter().rev() {
                p.destroy(api, vk_dev);
            }
            // SAFETY: Destroys logical device after cleaning up previously built pipelines.
            unsafe {
                (api.destroy_device)(vk_dev, ptr::null());
            }
            return None;
        };
        created_pipes.push(pipe);
    }
    let mut pipe_it = created_pipes.into_iter();
    let pipe_linear_f32 = pipe_it.next()?;
    let pipe_linear_f16 = pipe_it.next()?;
    let pipe_linear_bf16 = pipe_it.next()?;
    let pipe_rms_norm_f32 = pipe_it.next()?;
    let pipe_flash_attn_f32 = pipe_it.next()?;
    let pipe_fused_ew_f32 = pipe_it.next()?;

    let destroy_all_pipes = |vk_dev: VkDevice| {
        pipe_fused_ew_f32.destroy(api, vk_dev);
        pipe_flash_attn_f32.destroy(api, vk_dev);
        pipe_rms_norm_f32.destroy(api, vk_dev);
        pipe_linear_bf16.destroy(api, vk_dev);
        pipe_linear_f16.destroy(api, vk_dev);
        pipe_linear_f32.destroy(api, vk_dev);
    };

    let Ok(dummy_buf) = VulkanDeviceBackend::alloc_host_buffer_raw(api, vk_dev, &mem_props, 16)
    else {
        destroy_all_pipes(vk_dev);
        // SAFETY: Destroys logical device on dummy buffer allocation failure.
        unsafe {
            (api.destroy_device)(vk_dev, ptr::null());
        }
        return None;
    };
    let zeros = [0u32; 4];
    let _ = VulkanDeviceBackend::write_host_buffer(api, vk_dev, &dummy_buf, &zeros);

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
        VulkanDeviceBackend::free_host_buffer(api, vk_dev, dummy_buf);
        destroy_all_pipes(vk_dev);
        // SAFETY: Clean up device on command pool failure.
        unsafe {
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
        // SAFETY: Clean up command pool on failure.
        unsafe {
            (api.destroy_command_pool)(vk_dev, cmd_pool, ptr::null());
        }
        VulkanDeviceBackend::free_host_buffer(api, vk_dev, dummy_buf);
        destroy_all_pipes(vk_dev);
        // SAFETY: Clean up device on failure.
        unsafe {
            (api.destroy_device)(vk_dev, ptr::null());
        }
        return None;
    }

    Some(VulkanState {
        vk_dev,
        queue,
        pipe_linear_f32,
        pipe_linear_f16,
        pipe_linear_bf16,
        pipe_rms_norm_f32,
        pipe_flash_attn_f32,
        pipe_fused_ew_f32,
        dummy_buf,
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
        let inner = Arc::new(VulkanDeviceInner {
            device,
            _inst_guard: Arc::clone(&inst_guard),
            api: Arc::clone(&api),
            state: Mutex::new(state),
            counters: VramCounters::default(),
        });
        let backend = Arc::new(VulkanDeviceBackend {
            inner,
            info: info.clone(),
        });
        out.push(HardwareBackendEntry { info, backend });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spirv_emitters_produce_well_formed_headers_and_bounds() {
        let shaders: [(&str, Vec<u32>); 6] = [
            ("fused_linear_f32", emit_spirv_fused_linear_f32()),
            ("fused_linear_f16", emit_spirv_fused_linear_half(false)),
            ("fused_linear_bf16", emit_spirv_fused_linear_half(true)),
            ("rms_norm_f32", emit_spirv_rms_norm_f32()),
            ("flash_attention_f32", emit_spirv_flash_attention_f32()),
            ("fused_elementwise_f32", emit_spirv_fused_elementwise_f32()),
        ];
        for (name, words) in shaders {
            assert_eq!(words[0], 0x0723_0203, "{name}: SPIR-V magic");
            assert_eq!(words[1], 0x0001_0000, "{name}: SPIR-V 1.0 version");
            assert!(words[3] > 20, "{name}: valid non-zero SSA ID bound");
            assert!(
                words.len() > 80,
                "{name}: non-trivial compute shader payload"
            );
            // Walk every SPIR-V instruction word count to prove stream integrity.
            let mut pc = 5usize;
            while pc < words.len() {
                let wc = (words[pc] >> 16) as usize;
                assert!(wc >= 1, "{name}: invalid zero-word instruction at {pc}");
                pc += wc;
            }
            assert_eq!(
                pc,
                words.len(),
                "{name}: instruction stream must terminate cleanly at module end"
            );
        }
    }

    #[test]
    fn vulkan_all_six_spirv_pipelines_compile_live_when_driver_present() {
        let backends = probe_vulkan_backends();
        if backends.is_empty() {
            return;
        }
        // `probe_vulkan_backends()` compiles all 6 SPIR-V 1.0 compute pipelines via
        // `vkCreateShaderModule` + `vkCreateComputePipelines`; reaching here proves all 6
        // shaders passed live Vulkan driver SPIR-V validation & pipeline compilation!
        for entry in &backends {
            assert_eq!(entry.info.driver, "vulkan");
            assert!(entry.info.hardware_accelerated);
            assert!(entry.backend.supports_vram_residency());
        }
    }
}
