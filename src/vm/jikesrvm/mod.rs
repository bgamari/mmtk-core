use self::entrypoint::*;
pub use self::collection::BOOT_THREAD;

mod entrypoint;

#[macro_use]
mod jtoc_call;

pub mod scanning;
pub mod collection;
pub mod object_model;
pub mod java_header;
pub mod java_size_constants;
pub mod java_header_constants;
pub mod memory_manager_constants;
pub mod misc_header_constants;
pub mod tib_layout_constants;
pub mod class_loader_constants;
pub mod scan_statics;
pub mod scan_boot_image;
pub mod active_plan;
pub mod heap_layout_constants;
pub mod boot_image_size;
pub mod scan_sanity;

use ::util::address::Address;
use plan::TraceLocal;

pub static mut JTOC_BASE: Address = Address(0);

pub struct JikesRVM {}

impl JikesRVM {
    #[inline(always)]
    pub fn test(input: usize) -> usize {
        unsafe {
            jtoc_call!(TEST_METHOD_OFFSET, BOOT_THREAD, input)
        }
    }

    #[inline(always)]
    pub fn test1() -> usize {
        unsafe {
            jtoc_call!(TEST1_METHOD_OFFSET, BOOT_THREAD)
        }
    }

    #[inline(always)]
    pub fn test2(input1: usize, input2: usize) -> usize {
        unsafe {
            jtoc_call!(TEST2_METHOD_OFFSET, BOOT_THREAD, input1, input2)
        }
    }

    #[inline(always)]
    pub fn test3(input1: usize, input2: usize, input3: usize, input4: usize) -> usize {
        unsafe {
            jtoc_call!(TEST3_METHOD_OFFSET, BOOT_THREAD, input1, input2, input3, input4)
        }
    }

    pub fn forward_refs<T: TraceLocal>(trace: &mut T, thread_id: usize) {
        let trace_ptr = trace as *mut T;
        unsafe {
            jtoc_call!(PROCESS_REFERENCE_TYPES_METHOD_OFFSET, thread_id, trace_ptr, false);
        }
    }
}