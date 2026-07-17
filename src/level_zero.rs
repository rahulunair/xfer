#![allow(unsafe_code, clippy::borrow_as_ptr, clippy::missing_errors_doc)]

use std::cell::Cell;
use std::error::Error as StdError;
use std::ffi::c_void;
use std::fmt;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::ptr::{self, NonNull};
use std::rc::Rc;

#[allow(
    clippy::all,
    clippy::pedantic,
    dead_code,
    improper_ctypes,
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    unused_imports,
    unsafe_code
)]
mod raw {
    include!(concat!(env!("OUT_DIR"), "/level_zero_bindings.rs"));
}

pub type Result<T> = std::result::Result<T, LevelZeroError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LevelZeroError {
    Ze {
        operation: &'static str,
        result: u32,
    },
    NullHandle {
        operation: &'static str,
    },
    TooManyItems {
        operation: &'static str,
        count: usize,
    },
    CopyTooLarge {
        requested: usize,
        dst_len: usize,
        src_len: usize,
    },
    DeviceDriverMismatch,
    IncompatibleObjects {
        operation: &'static str,
        reason: &'static str,
    },
    SubmissionStateUnknown {
        operation: &'static str,
    },
}

impl fmt::Display for LevelZeroError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ze { operation, result } => {
                write!(f, "{operation} failed with Level Zero result {result}")
            }
            Self::NullHandle { operation } => write!(f, "{operation} returned a null handle"),
            Self::TooManyItems { operation, count } => {
                write!(f, "{operation} cannot pass {count} items to Level Zero")
            }
            Self::CopyTooLarge {
                requested,
                dst_len,
                src_len,
            } => write!(
                f,
                "copy requested {requested} bytes but destination has {dst_len} and source has {src_len}"
            ),
            Self::DeviceDriverMismatch => {
                f.write_str("Level Zero device does not belong to the context's driver")
            }
            Self::IncompatibleObjects { operation, reason } => {
                write!(
                    f,
                    "{operation} received incompatible Level Zero objects: {reason}"
                )
            }
            Self::SubmissionStateUnknown { operation } => write!(
                f,
                "{operation} was not attempted because queue completion is unconfirmed"
            ),
        }
    }
}

impl StdError for LevelZeroError {}

impl LevelZeroError {
    #[must_use]
    pub fn is_capability_unavailable(&self) -> bool {
        matches!(
            self,
            Self::Ze { result, .. }
                if *result == raw::ZE_RESULT_ERROR_UNSUPPORTED_FEATURE
                    || *result == raw::ZE_RESULT_ERROR_DEPENDENCY_UNAVAILABLE
                    || *result == raw::ZE_RESULT_ERROR_NOT_AVAILABLE
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceType {
    Gpu,
    Cpu,
    Fpga,
    Mca,
    Vpu,
    Unknown(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceProperties {
    pub device_type: DeviceType,
    pub vendor_id: u32,
    pub device_id: u32,
    pub flags: u32,
    pub subdevice_id: u32,
    pub core_clock_rate: u32,
    pub max_mem_alloc_size: u64,
    pub timer_resolution: u64,
    pub timestamp_valid_bits: u32,
    pub kernel_timestamp_valid_bits: u32,
    pub uuid: [u8; 16],
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciAddress {
    pub domain: u32,
    pub bus: u32,
    pub device: u32,
    pub function: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueGroupProperties {
    pub ordinal: u32,
    pub flags: u32,
    pub max_memory_fill_pattern_size: usize,
    pub num_queues: u32,
}

impl QueueGroupProperties {
    #[must_use]
    pub fn supports_compute(&self) -> bool {
        self.flags & raw::ZE_COMMAND_QUEUE_GROUP_PROPERTY_FLAG_COMPUTE != 0
    }

    #[must_use]
    pub fn supports_copy(&self) -> bool {
        self.flags & raw::ZE_COMMAND_QUEUE_GROUP_PROPERTY_FLAG_COPY != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlobalTimestamps {
    pub host: u64,
    pub device: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelTimestampResult {
    pub global_kernel_start: u64,
    pub global_kernel_end: u64,
    pub context_kernel_start: u64,
    pub context_kernel_end: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct Driver {
    handle: raw::ze_driver_handle_t,
    index: usize,
}

impl Driver {
    #[must_use]
    pub fn index(&self) -> usize {
        self.index
    }

    pub fn devices(&self) -> Result<Vec<Device>> {
        let mut count = 0_u32;
        let result = unsafe {
            // SAFETY: pCount is a valid out pointer and phDevices is null for count query.
            raw::zeDeviceGet(self.handle, &mut count, ptr::null_mut())
        };
        check("zeDeviceGet(count)", result)?;

        let mut handles = vec![ptr::null_mut(); count as usize];
        let result = unsafe {
            // SAFETY: handles has count writable entries when count > 0; null is accepted for zero-length Vec.
            raw::zeDeviceGet(self.handle, &mut count, handles.as_mut_ptr())
        };
        check("zeDeviceGet(handles)", result)?;

        handles
            .into_iter()
            .take(count as usize)
            .enumerate()
            .map(|(index, handle)| {
                if handle.is_null() {
                    Err(LevelZeroError::NullHandle {
                        operation: "zeDeviceGet",
                    })
                } else {
                    Ok(Device {
                        driver: self.handle,
                        handle,
                        driver_index: self.index,
                        index,
                    })
                }
            })
            .collect()
    }

    pub fn gpu_devices(&self) -> Result<Vec<Device>> {
        self.devices()?
            .into_iter()
            .filter_map(|device| match device.properties() {
                Ok(properties) if properties.device_type == DeviceType::Gpu => Some(Ok(device)),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            })
            .collect()
    }

    pub fn create_context(&self) -> Result<Context<'_>> {
        Context::new(self)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Device {
    driver: raw::ze_driver_handle_t,
    handle: raw::ze_device_handle_t,
    driver_index: usize,
    index: usize,
}

impl Device {
    #[must_use]
    pub fn driver_index(&self) -> usize {
        self.driver_index
    }

    #[must_use]
    pub fn index(&self) -> usize {
        self.index
    }

    pub fn properties(&self) -> Result<DeviceProperties> {
        let mut properties = device_properties_template();
        let result = unsafe {
            // SAFETY: self.handle is a Level Zero device handle and properties points to initialized storage.
            raw::zeDeviceGetProperties(self.handle, &mut properties)
        };
        check("zeDeviceGetProperties", result)?;
        Ok(DeviceProperties::from_raw(&properties))
    }

    pub fn queue_groups(&self) -> Result<Vec<QueueGroupProperties>> {
        let mut count = 0_u32;
        let result = unsafe {
            // SAFETY: pCount is a valid out pointer and pCommandQueueGroupProperties is null for count query.
            raw::zeDeviceGetCommandQueueGroupProperties(self.handle, &mut count, ptr::null_mut())
        };
        check("zeDeviceGetCommandQueueGroupProperties(count)", result)?;

        let mut raw_properties = Vec::with_capacity(count as usize);
        raw_properties.resize_with(count as usize, queue_group_properties_template);
        let result = unsafe {
            // SAFETY: raw_properties has count writable descriptor slots initialized with stype and null pNext.
            raw::zeDeviceGetCommandQueueGroupProperties(
                self.handle,
                &mut count,
                raw_properties.as_mut_ptr(),
            )
        };
        check("zeDeviceGetCommandQueueGroupProperties(values)", result)?;

        raw_properties
            .iter()
            .take(count as usize)
            .enumerate()
            .map(|(ordinal, properties)| {
                Ok(QueueGroupProperties {
                    ordinal: u32::try_from(ordinal).map_err(|_| LevelZeroError::TooManyItems {
                        operation: "zeDeviceGetCommandQueueGroupProperties(ordinal)",
                        count: ordinal,
                    })?,
                    flags: properties.flags,
                    max_memory_fill_pattern_size: properties.maxMemoryFillPatternSize,
                    num_queues: properties.numQueues,
                })
            })
            .collect()
    }

    pub fn pci_address(&self) -> Result<PciAddress> {
        let mut properties = pci_properties_template();
        let result = unsafe {
            // SAFETY: self.handle is valid and properties is initialized writable storage.
            raw::zeDevicePciGetPropertiesExt(self.handle, &mut properties)
        };
        check("zeDevicePciGetPropertiesExt", result)?;
        Ok(PciAddress {
            domain: properties.address.domain,
            bus: properties.address.bus,
            device: properties.address.device,
            function: properties.address.function,
        })
    }

    pub fn can_access_peer(&self, peer: &Self) -> Result<bool> {
        let mut can_access: raw::ze_bool_t = 0;
        let result = unsafe {
            // SAFETY: both handles are Level Zero device handles and can_access is a valid out pointer.
            raw::zeDeviceCanAccessPeer(self.handle, peer.handle, &mut can_access)
        };
        check("zeDeviceCanAccessPeer", result)?;
        Ok(can_access != 0)
    }

    pub fn global_timestamps(&self) -> Result<GlobalTimestamps> {
        let mut host = 0_u64;
        let mut device = 0_u64;
        let result = unsafe {
            // SAFETY: self.handle is a Level Zero device handle and both timestamp pointers are valid.
            raw::zeDeviceGetGlobalTimestamps(self.handle, &mut host, &mut device)
        };
        check("zeDeviceGetGlobalTimestamps", result)?;
        Ok(GlobalTimestamps { host, device })
    }

    fn handle(&self) -> raw::ze_device_handle_t {
        self.handle
    }
}

pub fn initialize() -> Result<Vec<Driver>> {
    let result = unsafe {
        // SAFETY: zeInit accepts a bitmask value and initializes process-global Level Zero loader state.
        raw::zeInit(raw::ZE_INIT_FLAG_GPU_ONLY)
    };
    check("zeInit", result)?;

    let mut count = 0_u32;
    let result = unsafe {
        // SAFETY: pCount is a valid out pointer and phDrivers is null for count query.
        raw::zeDriverGet(&mut count, ptr::null_mut())
    };
    check("zeDriverGet(count)", result)?;

    let mut handles = vec![ptr::null_mut(); count as usize];
    let result = unsafe {
        // SAFETY: handles has count writable entries when count > 0; null is accepted for zero-length Vec.
        raw::zeDriverGet(&mut count, handles.as_mut_ptr())
    };
    check("zeDriverGet(handles)", result)?;

    handles
        .into_iter()
        .take(count as usize)
        .enumerate()
        .map(|(index, handle)| {
            if handle.is_null() {
                Err(LevelZeroError::NullHandle {
                    operation: "zeDriverGet",
                })
            } else {
                Ok(Driver { handle, index })
            }
        })
        .collect()
}

pub fn enumerate_gpus() -> Result<Vec<Device>> {
    let mut devices = Vec::new();
    for driver in initialize()? {
        devices.extend(driver.gpu_devices()?);
    }
    Ok(devices)
}

#[derive(Debug, Default)]
struct ContextState {
    active_queues: Cell<usize>,
    poisoned: Cell<bool>,
}

impl ContextState {
    fn mark_submitted(&self, queue_was_pending: bool) {
        if !queue_was_pending {
            self.active_queues
                .set(self.active_queues.get().saturating_add(1));
        }
    }

    fn mark_synchronized(&self, queue_was_pending: bool) {
        if queue_was_pending {
            self.active_queues
                .set(self.active_queues.get().saturating_sub(1));
        }
    }

    fn poison(&self) {
        self.poisoned.set(true);
    }

    fn release_is_safe(&self) -> bool {
        !self.poisoned.get() && self.active_queues.get() == 0
    }

    fn require_release_safe(&self, operation: &'static str) -> Result<()> {
        if self.release_is_safe() {
            Ok(())
        } else {
            Err(LevelZeroError::SubmissionStateUnknown { operation })
        }
    }

    fn require_submission_allowed(&self, operation: &'static str) -> Result<()> {
        if self.poisoned.get() {
            Err(LevelZeroError::SubmissionStateUnknown { operation })
        } else {
            Ok(())
        }
    }
}

#[derive(Debug)]
pub struct Context<'driver> {
    handle: raw::ze_context_handle_t,
    driver: raw::ze_driver_handle_t,
    state: Rc<ContextState>,
    _driver: PhantomData<&'driver Driver>,
}

impl<'driver> Context<'driver> {
    pub fn new(driver: &'driver Driver) -> Result<Self> {
        let desc = raw::ze_context_desc_t {
            stype: raw::ZE_STRUCTURE_TYPE_CONTEXT_DESC,
            pNext: ptr::null(),
            flags: 0,
        };
        let mut handle = ptr::null_mut();
        let result = unsafe {
            // SAFETY: driver.handle is valid, desc points to a valid descriptor, and handle is writable.
            raw::zeContextCreate(driver.handle, &desc, &mut handle)
        };
        check("zeContextCreate", result)?;
        if handle.is_null() {
            return Err(LevelZeroError::NullHandle {
                operation: "zeContextCreate",
            });
        }

        Ok(Self {
            handle,
            driver: driver.handle,
            state: Rc::new(ContextState::default()),
            _driver: PhantomData,
        })
    }

    pub fn create_command_queue(
        &self,
        device: &Device,
        queue_group_ordinal: u32,
    ) -> Result<CommandQueue<'_>> {
        self.create_command_queue_at(device, queue_group_ordinal, 0)
    }

    pub fn create_command_queue_at(
        &self,
        device: &Device,
        queue_group_ordinal: u32,
        queue_index: u32,
    ) -> Result<CommandQueue<'_>> {
        CommandQueue::new(self, device, queue_group_ordinal, queue_index)
    }

    pub fn create_command_list(
        &self,
        device: &Device,
        queue_group_ordinal: u32,
    ) -> Result<CommandList<'_>> {
        CommandList::new(self, device, queue_group_ordinal)
    }

    pub fn alloc_host(&self, bytes: usize, alignment: usize) -> Result<HostAllocation<'_>> {
        HostAllocation::new(self, bytes, alignment)
    }

    pub fn alloc_device(
        &self,
        device: &Device,
        bytes: usize,
        alignment: usize,
        memory_ordinal: u32,
    ) -> Result<DeviceAllocation<'_>> {
        DeviceAllocation::new(self, device, bytes, alignment, memory_ordinal)
    }

    pub fn close(mut self) -> Result<()> {
        self.destroy()
    }

    fn handle(&self) -> raw::ze_context_handle_t {
        self.handle
    }

    fn ensure_device(&self, device: &Device) -> Result<()> {
        if self.driver == device.driver {
            Ok(())
        } else {
            Err(LevelZeroError::DeviceDriverMismatch)
        }
    }

    fn destroy(&mut self) -> Result<()> {
        let handle = std::mem::replace(&mut self.handle, ptr::null_mut());
        if handle.is_null() {
            return Ok(());
        }
        self.state.require_release_safe("zeContextDestroy")?;

        let result = unsafe {
            // SAFETY: handle is the owned context handle and this wrapper attempts release only once.
            raw::zeContextDestroy(handle)
        };
        check("zeContextDestroy", result)
    }
}

impl Drop for Context<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.destroy() {
            report_drop_error(&error);
        }
    }
}

#[derive(Debug)]
pub struct CommandQueue<'context> {
    handle: raw::ze_command_queue_handle_t,
    context: raw::ze_context_handle_t,
    device: raw::ze_device_handle_t,
    queue_group_ordinal: u32,
    pending: Cell<bool>,
    context_state: Rc<ContextState>,
    _context: PhantomData<&'context Context<'context>>,
}

impl<'context> CommandQueue<'context> {
    pub fn new(
        context: &'context Context<'_>,
        device: &Device,
        queue_group_ordinal: u32,
        queue_index: u32,
    ) -> Result<Self> {
        context.ensure_device(device)?;
        let desc = raw::ze_command_queue_desc_t {
            stype: raw::ZE_STRUCTURE_TYPE_COMMAND_QUEUE_DESC,
            pNext: ptr::null(),
            ordinal: queue_group_ordinal,
            index: queue_index,
            flags: 0,
            mode: raw::ZE_COMMAND_QUEUE_MODE_DEFAULT,
            priority: raw::ZE_COMMAND_QUEUE_PRIORITY_NORMAL,
        };
        let mut handle = ptr::null_mut();
        let result = unsafe {
            // SAFETY: context/device handles and desc are valid; handle is writable.
            raw::zeCommandQueueCreate(context.handle(), device.handle(), &desc, &mut handle)
        };
        check("zeCommandQueueCreate", result)?;
        if handle.is_null() {
            return Err(LevelZeroError::NullHandle {
                operation: "zeCommandQueueCreate",
            });
        }

        Ok(Self {
            handle,
            context: context.handle(),
            device: device.handle(),
            queue_group_ordinal,
            pending: Cell::new(false),
            context_state: Rc::clone(&context.state),
            _context: PhantomData,
        })
    }

    pub fn execute(&self, lists: &[&CommandList<'_>]) -> Result<()> {
        self.context_state
            .require_submission_allowed("zeCommandQueueExecuteCommandLists")?;
        for list in lists {
            if self.context != list.context {
                return Err(LevelZeroError::IncompatibleObjects {
                    operation: "zeCommandQueueExecuteCommandLists",
                    reason: "queue and command list have different contexts",
                });
            }
            if self.device != list.device {
                return Err(LevelZeroError::IncompatibleObjects {
                    operation: "zeCommandQueueExecuteCommandLists",
                    reason: "queue and command list target different devices",
                });
            }
            if self.queue_group_ordinal != list.queue_group_ordinal {
                return Err(LevelZeroError::IncompatibleObjects {
                    operation: "zeCommandQueueExecuteCommandLists",
                    reason: "queue and command list use different queue-group ordinals",
                });
            }
        }
        let count = len_to_u32("zeCommandQueueExecuteCommandLists", lists.len())?;
        let mut handles = lists.iter().map(|list| list.handle()).collect::<Vec<_>>();
        let result = unsafe {
            // SAFETY: self.handle is valid; handles contains count command-list handles; fence is null by choice.
            raw::zeCommandQueueExecuteCommandLists(
                self.handle,
                count,
                handles.as_mut_ptr(),
                ptr::null_mut(),
            )
        };
        match check("zeCommandQueueExecuteCommandLists", result) {
            Ok(()) => {
                let was_pending = self.pending.replace(true);
                self.context_state.mark_submitted(was_pending);
                Ok(())
            }
            Err(error) => {
                self.context_state.poison();
                Err(error)
            }
        }
    }

    pub fn synchronize(&self, timeout_ns: u64) -> Result<()> {
        let result = unsafe {
            // SAFETY: self.handle is a valid command queue handle; timeout is passed by value.
            raw::zeCommandQueueSynchronize(self.handle, timeout_ns)
        };
        match check("zeCommandQueueSynchronize", result) {
            Ok(()) => {
                let was_pending = self.pending.replace(false);
                self.context_state.mark_synchronized(was_pending);
                Ok(())
            }
            Err(error) => {
                self.context_state.poison();
                Err(error)
            }
        }
    }

    pub fn close(mut self) -> Result<()> {
        self.destroy()
    }

    fn destroy(&mut self) -> Result<()> {
        let handle = std::mem::replace(&mut self.handle, ptr::null_mut());
        if handle.is_null() {
            return Ok(());
        }
        self.context_state
            .require_release_safe("zeCommandQueueDestroy")?;

        let result = unsafe {
            // SAFETY: handle is the owned queue handle and this wrapper attempts release only once.
            raw::zeCommandQueueDestroy(handle)
        };
        check("zeCommandQueueDestroy", result)
    }
}

impl Drop for CommandQueue<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.destroy() {
            report_drop_error(&error);
        }
    }
}

#[derive(Debug)]
pub struct CommandList<'context> {
    handle: raw::ze_command_list_handle_t,
    context: raw::ze_context_handle_t,
    device: raw::ze_device_handle_t,
    queue_group_ordinal: u32,
    context_state: Rc<ContextState>,
    _context: PhantomData<&'context Context<'context>>,
}

impl<'context> CommandList<'context> {
    pub fn new(
        context: &'context Context<'_>,
        device: &Device,
        queue_group_ordinal: u32,
    ) -> Result<Self> {
        context.ensure_device(device)?;
        let desc = raw::ze_command_list_desc_t {
            stype: raw::ZE_STRUCTURE_TYPE_COMMAND_LIST_DESC,
            pNext: ptr::null(),
            commandQueueGroupOrdinal: queue_group_ordinal,
            flags: 0,
        };
        let mut handle = ptr::null_mut();
        let result = unsafe {
            // SAFETY: context/device handles and desc are valid; handle is writable.
            raw::zeCommandListCreate(context.handle(), device.handle(), &desc, &mut handle)
        };
        check("zeCommandListCreate", result)?;
        if handle.is_null() {
            return Err(LevelZeroError::NullHandle {
                operation: "zeCommandListCreate",
            });
        }

        Ok(Self {
            handle,
            context: context.handle(),
            device: device.handle(),
            queue_group_ordinal,
            context_state: Rc::clone(&context.state),
            _context: PhantomData,
        })
    }

    pub fn close(&self) -> Result<()> {
        self.context_state
            .require_release_safe("zeCommandListClose")?;
        let result = unsafe {
            // SAFETY: self.handle is a valid open command list handle.
            raw::zeCommandListClose(self.handle)
        };
        check("zeCommandListClose", result)
    }

    pub fn reset(&self) -> Result<()> {
        self.context_state
            .require_release_safe("zeCommandListReset")?;
        let result = unsafe {
            // SAFETY: self.handle is a valid command list handle that the caller is not submitting concurrently.
            raw::zeCommandListReset(self.handle)
        };
        check("zeCommandListReset", result)
    }

    /// # Safety
    ///
    /// The caller must keep `dst`, `src`, `signal`, and all `wait_events` alive
    /// until any queue execution that uses this command list has completed.
    /// The caller must not access either allocation, or reset, destroy, or
    /// reuse the command list or events, while Level Zero may access them.
    pub unsafe fn append_host_to_device(
        &self,
        dst: &DeviceAllocation<'_>,
        src: &HostAllocation<'_>,
        bytes: usize,
        signal: Option<&Event<'_>>,
        wait_events: &[&Event<'_>],
    ) -> Result<()> {
        unsafe {
            // SAFETY: forwarded from this function's caller contract.
            self.append_host_to_device_region(dst, 0, src, 0, bytes, signal, wait_events)
        }
    }

    /// # Safety
    ///
    /// The caller must keep `dst`, `src`, `signal`, and all `wait_events` alive
    /// until queue execution completes. Source and destination regions must not
    /// be accessed or overlapped incompatibly while Level Zero may use them.
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn append_host_to_device_region(
        &self,
        dst: &DeviceAllocation<'_>,
        dst_offset: usize,
        src: &HostAllocation<'_>,
        src_offset: usize,
        bytes: usize,
        signal: Option<&Event<'_>>,
        wait_events: &[&Event<'_>],
    ) -> Result<()> {
        self.context_state
            .require_release_safe("zeCommandListAppendMemoryCopy")?;
        self.ensure_context(dst.context, "zeCommandListAppendMemoryCopy(destination)")?;
        self.ensure_context(src.context, "zeCommandListAppendMemoryCopy(source)")?;
        self.ensure_device(dst.device, "zeCommandListAppendMemoryCopy(destination)")?;
        self.ensure_event_contexts(signal, wait_events)?;
        unsafe {
            // SAFETY: forwarded from this function's caller contract.
            self.append_memory_copy_raw(
                dst.ptr(),
                dst.len,
                dst_offset,
                src.ptr(),
                src.len,
                src_offset,
                bytes,
                signal,
                wait_events,
            )
        }
    }

    /// # Safety
    ///
    /// The caller must keep `dst`, `src`, `signal`, and all `wait_events` alive
    /// until any queue execution that uses this command list has completed.
    /// The caller must not access either allocation, or reset, destroy, or
    /// reuse the command list or events, while Level Zero may access them.
    pub unsafe fn append_device_to_host(
        &self,
        dst: &HostAllocation<'_>,
        src: &DeviceAllocation<'_>,
        bytes: usize,
        signal: Option<&Event<'_>>,
        wait_events: &[&Event<'_>],
    ) -> Result<()> {
        unsafe {
            // SAFETY: forwarded from this function's caller contract.
            self.append_device_to_host_region(dst, 0, src, 0, bytes, signal, wait_events)
        }
    }

    /// # Safety
    ///
    /// The caller must keep `dst`, `src`, `signal`, and all `wait_events` alive
    /// until queue execution completes. Source and destination regions must not
    /// be accessed or overlapped incompatibly while Level Zero may use them.
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn append_device_to_host_region(
        &self,
        dst: &HostAllocation<'_>,
        dst_offset: usize,
        src: &DeviceAllocation<'_>,
        src_offset: usize,
        bytes: usize,
        signal: Option<&Event<'_>>,
        wait_events: &[&Event<'_>],
    ) -> Result<()> {
        self.context_state
            .require_release_safe("zeCommandListAppendMemoryCopy")?;
        self.ensure_context(dst.context, "zeCommandListAppendMemoryCopy(destination)")?;
        self.ensure_context(src.context, "zeCommandListAppendMemoryCopy(source)")?;
        self.ensure_device(src.device, "zeCommandListAppendMemoryCopy(source)")?;
        self.ensure_event_contexts(signal, wait_events)?;
        unsafe {
            // SAFETY: forwarded from this function's caller contract.
            self.append_memory_copy_raw(
                dst.ptr(),
                dst.len,
                dst_offset,
                src.ptr(),
                src.len,
                src_offset,
                bytes,
                signal,
                wait_events,
            )
        }
    }

    /// # Safety
    ///
    /// The caller must keep `dst`, `src`, `signal`, and all `wait_events` alive
    /// until any queue execution that uses this command list has completed.
    /// The caller must not access either allocation, or reset, destroy, or
    /// reuse the command list or events, while Level Zero may access them.
    pub unsafe fn append_device_to_device(
        &self,
        dst: &DeviceAllocation<'_>,
        src: &DeviceAllocation<'_>,
        bytes: usize,
        signal: Option<&Event<'_>>,
        wait_events: &[&Event<'_>],
    ) -> Result<()> {
        unsafe {
            // SAFETY: forwarded from this function's caller contract.
            self.append_device_to_device_region(dst, 0, src, 0, bytes, signal, wait_events)
        }
    }

    /// # Safety
    ///
    /// The caller must keep `dst`, `src`, `signal`, and all `wait_events` alive
    /// until queue execution completes. Source and destination regions must not
    /// be accessed or overlapped incompatibly while Level Zero may use them.
    #[allow(clippy::too_many_arguments)]
    pub unsafe fn append_device_to_device_region(
        &self,
        dst: &DeviceAllocation<'_>,
        dst_offset: usize,
        src: &DeviceAllocation<'_>,
        src_offset: usize,
        bytes: usize,
        signal: Option<&Event<'_>>,
        wait_events: &[&Event<'_>],
    ) -> Result<()> {
        self.context_state
            .require_release_safe("zeCommandListAppendMemoryCopy")?;
        self.ensure_context(dst.context, "zeCommandListAppendMemoryCopy(destination)")?;
        self.ensure_context(src.context, "zeCommandListAppendMemoryCopy(source)")?;
        self.ensure_device(src.device, "zeCommandListAppendMemoryCopy(source)")?;
        self.ensure_event_contexts(signal, wait_events)?;
        unsafe {
            // SAFETY: forwarded from this function's caller contract.
            self.append_memory_copy_raw(
                dst.ptr(),
                dst.len,
                dst_offset,
                src.ptr(),
                src.len,
                src_offset,
                bytes,
                signal,
                wait_events,
            )
        }
    }

    pub fn destroy(mut self) -> Result<()> {
        self.destroy_inner()
    }

    fn handle(&self) -> raw::ze_command_list_handle_t {
        self.handle
    }

    fn ensure_context(
        &self,
        context: raw::ze_context_handle_t,
        operation: &'static str,
    ) -> Result<()> {
        if self.context == context {
            Ok(())
        } else {
            Err(LevelZeroError::IncompatibleObjects {
                operation,
                reason: "command list and resource have different contexts",
            })
        }
    }

    fn ensure_event_contexts(
        &self,
        signal: Option<&Event<'_>>,
        wait_events: &[&Event<'_>],
    ) -> Result<()> {
        if signal.is_some_and(|event| event.context != self.context)
            || wait_events
                .iter()
                .any(|event| event.context != self.context)
        {
            Err(LevelZeroError::IncompatibleObjects {
                operation: "zeCommandListAppendMemoryCopy",
                reason: "command list and event have different contexts",
            })
        } else {
            Ok(())
        }
    }

    fn ensure_device(
        &self,
        device: raw::ze_device_handle_t,
        operation: &'static str,
    ) -> Result<()> {
        if self.device == device {
            Ok(())
        } else {
            Err(LevelZeroError::IncompatibleObjects {
                operation,
                reason: "command list targets the wrong allocation device",
            })
        }
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn append_memory_copy_raw(
        &self,
        dst: NonNull<c_void>,
        dst_len: usize,
        dst_offset: usize,
        src: NonNull<c_void>,
        src_len: usize,
        src_offset: usize,
        bytes: usize,
        signal: Option<&Event<'_>>,
        wait_events: &[&Event<'_>],
    ) -> Result<()> {
        let dst_remaining = remaining_region_len(dst_len, dst_offset, bytes)?;
        let src_remaining = remaining_region_len(src_len, src_offset, bytes)?;
        if bytes > dst_remaining || bytes > src_remaining {
            return Err(LevelZeroError::CopyTooLarge {
                requested: bytes,
                dst_len: dst_remaining,
                src_len: src_remaining,
            });
        }
        let dst: NonNull<c_void> = unsafe {
            // SAFETY: dst_offset is within the destination allocation as checked above.
            NonNull::new_unchecked(dst.as_ptr().cast::<u8>().add(dst_offset).cast())
        };
        let src: NonNull<c_void> = unsafe {
            // SAFETY: src_offset is within the source allocation as checked above.
            NonNull::new_unchecked(src.as_ptr().cast::<u8>().add(src_offset).cast())
        };

        let wait_count = len_to_u32("zeCommandListAppendMemoryCopy", wait_events.len())?;
        let mut wait_handles = wait_events
            .iter()
            .map(|event| event.handle())
            .collect::<Vec<_>>();
        let wait_ptr = if wait_handles.is_empty() {
            ptr::null_mut()
        } else {
            wait_handles.as_mut_ptr()
        };

        let result = unsafe {
            // SAFETY: caller guarantees allocation/event lifetimes through queue completion; pointers are non-null
            // and byte count was bounded against both allocation lengths.
            raw::zeCommandListAppendMemoryCopy(
                self.handle,
                dst.as_ptr(),
                src.as_ptr().cast_const(),
                bytes,
                signal.map_or(ptr::null_mut(), Event::handle),
                wait_count,
                wait_ptr,
            )
        };
        check("zeCommandListAppendMemoryCopy", result)
    }

    fn destroy_inner(&mut self) -> Result<()> {
        let handle = std::mem::replace(&mut self.handle, ptr::null_mut());
        if handle.is_null() {
            return Ok(());
        }
        self.context_state
            .require_release_safe("zeCommandListDestroy")?;

        let result = unsafe {
            // SAFETY: handle is the owned command-list handle and release is attempted only once.
            raw::zeCommandListDestroy(handle)
        };
        check("zeCommandListDestroy", result)
    }
}

fn remaining_region_len(len: usize, offset: usize, requested: usize) -> Result<usize> {
    len.checked_sub(offset).ok_or(LevelZeroError::CopyTooLarge {
        requested,
        dst_len: len,
        src_len: len,
    })
}

impl Drop for CommandList<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.destroy_inner() {
            report_drop_error(&error);
        }
    }
}

#[derive(Debug)]
pub struct HostAllocation<'context> {
    context: raw::ze_context_handle_t,
    ptr: Option<NonNull<c_void>>,
    len: usize,
    context_state: Rc<ContextState>,
    _context: PhantomData<&'context Context<'context>>,
}

impl<'context> HostAllocation<'context> {
    pub fn new(context: &'context Context<'_>, bytes: usize, alignment: usize) -> Result<Self> {
        let desc = raw::ze_host_mem_alloc_desc_t {
            stype: raw::ZE_STRUCTURE_TYPE_HOST_MEM_ALLOC_DESC,
            pNext: ptr::null(),
            flags: 0,
        };
        let mut ptr = ptr::null_mut();
        let result = unsafe {
            // SAFETY: context handle and desc are valid; ptr is writable for the returned allocation pointer.
            raw::zeMemAllocHost(context.handle(), &desc, bytes, alignment, &mut ptr)
        };
        check("zeMemAllocHost", result)?;

        let ptr = NonNull::new(ptr).ok_or(LevelZeroError::NullHandle {
            operation: "zeMemAllocHost",
        })?;

        Ok(Self {
            context: context.handle(),
            ptr: Some(ptr),
            len: bytes,
            context_state: Rc::clone(&context.state),
            _context: PhantomData,
        })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        assert!(
            self.context_state.release_is_safe(),
            "host allocation cannot be accessed while queue completion is unconfirmed"
        );
        unsafe {
            // SAFETY: Level Zero returned a non-null host allocation valid for self.len bytes.
            std::slice::from_raw_parts(self.ptr().as_ptr().cast::<u8>(), self.len)
        }
    }

    #[must_use]
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        assert!(
            self.context_state.release_is_safe(),
            "host allocation cannot be accessed while queue completion is unconfirmed"
        );
        unsafe {
            // SAFETY: &mut self guarantees unique access to the host allocation for self.len bytes.
            std::slice::from_raw_parts_mut(self.ptr().as_ptr().cast::<u8>(), self.len)
        }
    }

    pub fn free(mut self) -> Result<()> {
        self.free_inner()
    }

    fn ptr(&self) -> NonNull<c_void> {
        self.ptr
            .expect("host allocation pointer is present until free or drop")
    }

    fn free_inner(&mut self) -> Result<()> {
        let Some(ptr) = self.ptr.take() else {
            return Ok(());
        };
        self.context_state.require_release_safe("zeMemFree(host)")?;

        let result = unsafe {
            // SAFETY: ptr is the owned allocation and this wrapper attempts release only once.
            raw::zeMemFree(self.context, ptr.as_ptr())
        };
        check("zeMemFree(host)", result)
    }
}

impl Drop for HostAllocation<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.free_inner() {
            report_drop_error(&error);
        }
    }
}

#[derive(Debug)]
pub struct DeviceAllocation<'context> {
    context: raw::ze_context_handle_t,
    device: raw::ze_device_handle_t,
    ptr: Option<NonNull<c_void>>,
    len: usize,
    context_state: Rc<ContextState>,
    _context: PhantomData<&'context Context<'context>>,
}

impl<'context> DeviceAllocation<'context> {
    pub fn new(
        context: &'context Context<'_>,
        device: &Device,
        bytes: usize,
        alignment: usize,
        memory_ordinal: u32,
    ) -> Result<Self> {
        context.ensure_device(device)?;
        let desc = raw::ze_device_mem_alloc_desc_t {
            stype: raw::ZE_STRUCTURE_TYPE_DEVICE_MEM_ALLOC_DESC,
            pNext: ptr::null(),
            flags: 0,
            ordinal: memory_ordinal,
        };
        let mut ptr = ptr::null_mut();
        let result = unsafe {
            // SAFETY: context/device handles and desc are valid; ptr is writable for the allocation pointer.
            raw::zeMemAllocDevice(
                context.handle(),
                &desc,
                bytes,
                alignment,
                device.handle(),
                &mut ptr,
            )
        };
        check("zeMemAllocDevice", result)?;

        let ptr = NonNull::new(ptr).ok_or(LevelZeroError::NullHandle {
            operation: "zeMemAllocDevice",
        })?;

        Ok(Self {
            context: context.handle(),
            device: device.handle(),
            ptr: Some(ptr),
            len: bytes,
            context_state: Rc::clone(&context.state),
            _context: PhantomData,
        })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn free(mut self) -> Result<()> {
        self.free_inner()
    }

    fn ptr(&self) -> NonNull<c_void> {
        self.ptr
            .expect("device allocation pointer is present until free or drop")
    }

    fn free_inner(&mut self) -> Result<()> {
        let Some(ptr) = self.ptr.take() else {
            return Ok(());
        };
        self.context_state
            .require_release_safe("zeMemFree(device)")?;

        let result = unsafe {
            // SAFETY: ptr is the owned allocation and this wrapper attempts release only once.
            raw::zeMemFree(self.context, ptr.as_ptr())
        };
        check("zeMemFree(device)", result)
    }
}

impl Drop for DeviceAllocation<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.free_inner() {
            report_drop_error(&error);
        }
    }
}

#[derive(Debug)]
pub struct EventPool<'context> {
    handle: raw::ze_event_pool_handle_t,
    context: raw::ze_context_handle_t,
    count: u32,
    context_state: Rc<ContextState>,
    _context: PhantomData<&'context Context<'context>>,
}

impl<'context> EventPool<'context> {
    pub fn new(
        context: &'context Context<'_>,
        devices: &[&Device],
        flags: u32,
        count: u32,
    ) -> Result<Self> {
        let device_count = len_to_u32("zeEventPoolCreate", devices.len())?;
        let mut device_handles = devices
            .iter()
            .map(|device| {
                context.ensure_device(device)?;
                Ok(device.handle())
            })
            .collect::<Result<Vec<_>>>()?;
        let devices_ptr = if device_handles.is_empty() {
            ptr::null_mut()
        } else {
            device_handles.as_mut_ptr()
        };
        let desc = raw::ze_event_pool_desc_t {
            stype: raw::ZE_STRUCTURE_TYPE_EVENT_POOL_DESC,
            pNext: ptr::null(),
            flags,
            count,
        };
        let mut handle = ptr::null_mut();
        let result = unsafe {
            // SAFETY: context handle, desc, and optional device handle array are valid; handle is writable.
            raw::zeEventPoolCreate(
                context.handle(),
                &desc,
                device_count,
                devices_ptr,
                &mut handle,
            )
        };
        check("zeEventPoolCreate", result)?;
        if handle.is_null() {
            return Err(LevelZeroError::NullHandle {
                operation: "zeEventPoolCreate",
            });
        }

        Ok(Self {
            handle,
            context: context.handle(),
            count,
            context_state: Rc::clone(&context.state),
            _context: PhantomData,
        })
    }

    pub fn kernel_timestamps(
        context: &'context Context<'_>,
        devices: &[&Device],
        count: u32,
    ) -> Result<Self> {
        Self::new(
            context,
            devices,
            raw::ZE_EVENT_POOL_FLAG_KERNEL_TIMESTAMP,
            count,
        )
    }

    pub fn create_event(&self, index: u32) -> Result<Event<'_>> {
        if index >= self.count {
            return Err(LevelZeroError::TooManyItems {
                operation: "zeEventCreate(index)",
                count: index as usize,
            });
        }

        let desc = raw::ze_event_desc_t {
            stype: raw::ZE_STRUCTURE_TYPE_EVENT_DESC,
            pNext: ptr::null(),
            index,
            signal: 0,
            wait: 0,
        };
        let mut handle = ptr::null_mut();
        let result = unsafe {
            // SAFETY: event pool handle and desc are valid; handle is writable.
            raw::zeEventCreate(self.handle, &desc, &mut handle)
        };
        check("zeEventCreate", result)?;
        if handle.is_null() {
            return Err(LevelZeroError::NullHandle {
                operation: "zeEventCreate",
            });
        }

        Ok(Event {
            handle,
            context: self.context,
            context_state: Rc::clone(&self.context_state),
            _pool: PhantomData,
        })
    }

    pub fn destroy(mut self) -> Result<()> {
        self.destroy_inner()
    }

    fn destroy_inner(&mut self) -> Result<()> {
        let handle = std::mem::replace(&mut self.handle, ptr::null_mut());
        if handle.is_null() {
            return Ok(());
        }
        self.context_state
            .require_release_safe("zeEventPoolDestroy")?;

        let result = unsafe {
            // SAFETY: handle is the owned event-pool handle and release is attempted only once.
            raw::zeEventPoolDestroy(handle)
        };
        check("zeEventPoolDestroy", result)
    }
}

impl Drop for EventPool<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.destroy_inner() {
            report_drop_error(&error);
        }
    }
}

#[derive(Debug)]
pub struct Event<'pool> {
    handle: raw::ze_event_handle_t,
    context: raw::ze_context_handle_t,
    context_state: Rc<ContextState>,
    _pool: PhantomData<&'pool EventPool<'pool>>,
}

impl Event<'_> {
    pub fn host_synchronize(&self, timeout_ns: u64) -> Result<()> {
        let result = unsafe {
            // SAFETY: self.handle is a valid event handle; timeout is passed by value.
            raw::zeEventHostSynchronize(self.handle, timeout_ns)
        };
        check("zeEventHostSynchronize", result)
    }

    pub fn host_reset(&self) -> Result<()> {
        self.context_state
            .require_release_safe("zeEventHostReset")?;
        let result = unsafe {
            // SAFETY: self.handle is a valid event handle and the caller is not concurrently using it.
            raw::zeEventHostReset(self.handle)
        };
        check("zeEventHostReset", result)
    }

    pub fn query_kernel_timestamp(&self) -> Result<KernelTimestampResult> {
        self.context_state
            .require_release_safe("zeEventQueryKernelTimestamp")?;
        let mut timestamp = kernel_timestamp_result_template();
        let result = unsafe {
            // SAFETY: self.handle is a valid event handle and timestamp is writable output storage.
            raw::zeEventQueryKernelTimestamp(self.handle, &mut timestamp)
        };
        check("zeEventQueryKernelTimestamp", result)?;

        Ok(KernelTimestampResult {
            global_kernel_start: timestamp.global.kernelStart,
            global_kernel_end: timestamp.global.kernelEnd,
            context_kernel_start: timestamp.context.kernelStart,
            context_kernel_end: timestamp.context.kernelEnd,
        })
    }

    pub fn destroy(mut self) -> Result<()> {
        self.destroy_inner()
    }

    fn handle(&self) -> raw::ze_event_handle_t {
        self.handle
    }

    fn destroy_inner(&mut self) -> Result<()> {
        let handle = std::mem::replace(&mut self.handle, ptr::null_mut());
        if handle.is_null() {
            return Ok(());
        }
        self.context_state.require_release_safe("zeEventDestroy")?;

        let result = unsafe {
            // SAFETY: handle is the owned event handle and release is attempted only once.
            raw::zeEventDestroy(handle)
        };
        check("zeEventDestroy", result)
    }
}

impl Drop for Event<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.destroy_inner() {
            report_drop_error(&error);
        }
    }
}

fn report_drop_error(error: &LevelZeroError) {
    // The originating submit/synchronize error is reported by the benchmark.
    // Cleanup is deliberately abandoned while the device may still own memory.
    if !matches!(error, LevelZeroError::SubmissionStateUnknown { .. }) {
        eprintln!("xfer: {error}");
    }
}

fn check(operation: &'static str, result: raw::ze_result_t) -> Result<()> {
    if result == raw::ZE_RESULT_SUCCESS {
        Ok(())
    } else {
        Err(LevelZeroError::Ze { operation, result })
    }
}

fn len_to_u32(operation: &'static str, count: usize) -> Result<u32> {
    u32::try_from(count).map_err(|_| LevelZeroError::TooManyItems { operation, count })
}

fn device_properties_template() -> raw::ze_device_properties_t {
    let mut properties = unsafe {
        // SAFETY: zero is a valid initial bit pattern for this C POD output descriptor.
        MaybeUninit::<raw::ze_device_properties_t>::zeroed().assume_init()
    };
    properties.stype = raw::ZE_STRUCTURE_TYPE_DEVICE_PROPERTIES;
    properties.pNext = ptr::null_mut();
    properties
}

fn queue_group_properties_template() -> raw::ze_command_queue_group_properties_t {
    let mut properties = unsafe {
        // SAFETY: zero is a valid initial bit pattern for this C POD output descriptor.
        MaybeUninit::<raw::ze_command_queue_group_properties_t>::zeroed().assume_init()
    };
    properties.stype = raw::ZE_STRUCTURE_TYPE_COMMAND_QUEUE_GROUP_PROPERTIES;
    properties.pNext = ptr::null_mut();
    properties
}

fn pci_properties_template() -> raw::ze_pci_ext_properties_t {
    let mut properties = unsafe {
        // SAFETY: zero is a valid initial bit pattern for this C POD output descriptor.
        MaybeUninit::<raw::ze_pci_ext_properties_t>::zeroed().assume_init()
    };
    properties.stype = raw::ZE_STRUCTURE_TYPE_PCI_EXT_PROPERTIES;
    properties.pNext = ptr::null_mut();
    properties
}

fn kernel_timestamp_result_template() -> raw::ze_kernel_timestamp_result_t {
    unsafe {
        // SAFETY: zero is a valid initial bit pattern for this C POD output structure.
        MaybeUninit::<raw::ze_kernel_timestamp_result_t>::zeroed().assume_init()
    }
}

impl DeviceProperties {
    fn from_raw(properties: &raw::ze_device_properties_t) -> Self {
        Self {
            device_type: device_type(properties.type_),
            vendor_id: properties.vendorId,
            device_id: properties.deviceId,
            flags: properties.flags,
            subdevice_id: properties.subdeviceId,
            core_clock_rate: properties.coreClockRate,
            max_mem_alloc_size: properties.maxMemAllocSize,
            timer_resolution: properties.timerResolution,
            timestamp_valid_bits: properties.timestampValidBits,
            kernel_timestamp_valid_bits: properties.kernelTimestampValidBits,
            uuid: properties.uuid.id,
            name: c_char_array_to_string(&properties.name),
        }
    }
}

fn device_type(value: raw::ze_device_type_t) -> DeviceType {
    match value {
        raw::ZE_DEVICE_TYPE_GPU => DeviceType::Gpu,
        raw::ZE_DEVICE_TYPE_CPU => DeviceType::Cpu,
        raw::ZE_DEVICE_TYPE_FPGA => DeviceType::Fpga,
        raw::ZE_DEVICE_TYPE_MCA => DeviceType::Mca,
        raw::ZE_DEVICE_TYPE_VPU => DeviceType::Vpu,
        other => DeviceType::Unknown(other),
    }
}

fn c_char_array_to_string(value: &[std::ffi::c_char]) -> String {
    let bytes = value
        .iter()
        .map(|&byte| byte.to_ne_bytes()[0])
        .collect::<Vec<_>>();
    let end = bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_result_is_ok() {
        assert_eq!(check("unit", raw::ZE_RESULT_SUCCESS), Ok(()));
    }

    #[test]
    fn error_result_keeps_operation_and_numeric_code() {
        let error = check("zeUnitTest", raw::ZE_RESULT_ERROR_INVALID_ARGUMENT).unwrap_err();
        assert_eq!(
            error,
            LevelZeroError::Ze {
                operation: "zeUnitTest",
                result: raw::ZE_RESULT_ERROR_INVALID_ARGUMENT,
            }
        );
        assert!(error.to_string().contains("zeUnitTest"));
        assert!(
            error
                .to_string()
                .contains(&raw::ZE_RESULT_ERROR_INVALID_ARGUMENT.to_string())
        );
    }

    #[test]
    fn classifies_only_capability_results_as_unavailable() {
        for result in [
            raw::ZE_RESULT_ERROR_UNSUPPORTED_FEATURE,
            raw::ZE_RESULT_ERROR_DEPENDENCY_UNAVAILABLE,
            raw::ZE_RESULT_ERROR_NOT_AVAILABLE,
        ] {
            assert!(
                LevelZeroError::Ze {
                    operation: "unit",
                    result,
                }
                .is_capability_unavailable()
            );
        }

        assert!(
            !LevelZeroError::Ze {
                operation: "unit",
                result: raw::ZE_RESULT_ERROR_DEVICE_LOST,
            }
            .is_capability_unavailable()
        );
    }

    #[test]
    fn queue_group_capability_helpers_read_flags() {
        let properties = QueueGroupProperties {
            ordinal: 7,
            flags: raw::ZE_COMMAND_QUEUE_GROUP_PROPERTY_FLAG_COPY
                | raw::ZE_COMMAND_QUEUE_GROUP_PROPERTY_FLAG_COMPUTE,
            max_memory_fill_pattern_size: 0,
            num_queues: 1,
        };
        assert!(properties.supports_copy());
        assert!(properties.supports_compute());
    }

    #[test]
    fn c_string_conversion_handles_missing_nul() {
        let value = ['x' as std::ffi::c_char, 'f' as std::ffi::c_char];
        assert_eq!(c_char_array_to_string(&value), "xf");
    }

    #[test]
    fn c_string_conversion_stops_at_first_nul() {
        let value = [
            'x' as std::ffi::c_char,
            'f' as std::ffi::c_char,
            0,
            'z' as std::ffi::c_char,
        ];
        assert_eq!(c_char_array_to_string(&value), "xf");
    }

    #[test]
    fn rejects_copy_larger_than_recorded_allocations() {
        let error = LevelZeroError::CopyTooLarge {
            requested: 16,
            dst_len: 8,
            src_len: 12,
        };
        assert!(error.to_string().contains("16 bytes"));
    }

    #[test]
    fn region_bounds_reject_offset_past_allocation_even_for_zero_bytes() {
        assert_eq!(remaining_region_len(8, 8, 0), Ok(0));
        assert!(matches!(
            remaining_region_len(8, 9, 0),
            Err(LevelZeroError::CopyTooLarge { requested: 0, .. })
        ));
    }

    #[test]
    fn len_to_u32_rejects_overflow() {
        let error = len_to_u32("unit", usize::MAX).unwrap_err();
        assert_eq!(
            error,
            LevelZeroError::TooManyItems {
                operation: "unit",
                count: usize::MAX,
            }
        );
    }

    #[test]
    fn context_rejects_device_from_another_driver() {
        let context = Context {
            handle: ptr::null_mut(),
            driver: 1_usize as raw::ze_driver_handle_t,
            state: Rc::new(ContextState::default()),
            _driver: PhantomData,
        };
        let matching = Device {
            driver: 1_usize as raw::ze_driver_handle_t,
            handle: ptr::null_mut(),
            driver_index: 0,
            index: 0,
        };
        let mismatched = Device {
            driver: 2_usize as raw::ze_driver_handle_t,
            handle: ptr::null_mut(),
            driver_index: 1,
            index: 0,
        };

        assert_eq!(context.ensure_device(&matching), Ok(()));
        assert_eq!(
            context.ensure_device(&mismatched),
            Err(LevelZeroError::DeviceDriverMismatch)
        );
    }

    #[test]
    fn context_state_requires_successful_synchronization_before_release() {
        let state = ContextState::default();
        assert!(state.release_is_safe());

        state.mark_submitted(false);
        assert!(!state.release_is_safe());

        state.mark_synchronized(true);
        assert!(state.release_is_safe());

        state.poison();
        assert!(!state.release_is_safe());
        assert!(state.require_submission_allowed("unit submission").is_err());
    }

    #[test]
    fn context_state_allows_parallel_queue_submissions_until_poisoned() {
        let state = ContextState::default();
        state.mark_submitted(false);

        assert!(
            state
                .require_submission_allowed("second queue submission")
                .is_ok()
        );
        assert!(!state.release_is_safe());
    }

    #[test]
    fn queue_rejects_command_list_from_another_context_before_ffi() {
        let queue_state = Rc::new(ContextState::default());
        let list_state = Rc::new(ContextState::default());
        let queue = CommandQueue {
            handle: ptr::null_mut(),
            context: 1_usize as raw::ze_context_handle_t,
            device: 3_usize as raw::ze_device_handle_t,
            queue_group_ordinal: 2,
            pending: Cell::new(false),
            context_state: queue_state,
            _context: PhantomData,
        };
        let list = CommandList {
            handle: ptr::null_mut(),
            context: 2_usize as raw::ze_context_handle_t,
            device: 3_usize as raw::ze_device_handle_t,
            queue_group_ordinal: 2,
            context_state: list_state,
            _context: PhantomData,
        };

        assert!(matches!(
            queue.execute(&[&list]),
            Err(LevelZeroError::IncompatibleObjects { .. })
        ));
    }

    #[test]
    fn unsafe_cleanup_state_abandons_allocation_without_retry() {
        let state = Rc::new(ContextState::default());
        state.poison();
        let mut allocation = HostAllocation {
            context: ptr::null_mut(),
            ptr: Some(NonNull::<u8>::dangling().cast()),
            len: 1,
            context_state: state,
            _context: PhantomData,
        };

        assert!(matches!(
            allocation.free_inner(),
            Err(LevelZeroError::SubmissionStateUnknown { .. })
        ));
        assert!(allocation.ptr.is_none());
        assert_eq!(allocation.free_inner(), Ok(()));
    }
}
