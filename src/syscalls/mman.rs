use core::cmp;
use core::ffi::{c_int, c_void};

use align_address::Align;
use free_list::{AllocError, PageLayout, PageRange};
use memory_addresses::{PhysAddr, VirtAddr};

#[cfg(target_arch = "x86_64")]
use crate::arch::mm::paging::PageTableEntryFlagsExt;
use crate::arch::mm::paging::{self, BasePageSize, PageSize, PageTableEntryFlags};
use crate::mm::physicalmem::PHYSICAL_FREE_LIST;
use crate::mm::virtualmem::KERNEL_FREE_LIST;
use crate::{arch, mm};

bitflags! {
	#[repr(transparent)]
	#[derive(Debug, Copy, Clone, Default)]
	pub struct MemoryProtection: u32 {
		/// Pages may not be accessed.
		const None = 0;
		/// Indicates that the memory region should be readable.
		const Read = 1 << 0;
		/// Indicates that the memory region should be writable.
		const Write = 1 << 1;
		/// Indicates that the memory region should be executable.
		const Exec = 1 << 2;
	}
}

/// Logs the current physical and virtual free list
#[hermit_macro::system]
#[unsafe(no_mangle)]
pub extern "C" fn sys_print_freelist() -> i32 {
	mm::print_information();
	0
}

#[hermit_macro::system]
#[unsafe(no_mangle)]
pub extern "C" fn sys_print_page_tables() -> i32 {
	unsafe {
		paging::log_page_tables();
	}
	0
}

/// Marks an aligned virtual memory region as used.
///
/// Returns the VirtAddr of the allocated region.
#[hermit_macro::system]
#[unsafe(no_mangle)]
pub extern "C" fn sys_valloc(size: usize, align: usize, ret: *mut u64) -> i32 {
	assert!(!ret.is_null());
	let size = size.align_up(align);
	let layout = PageLayout::from_size_align(size, align).unwrap();
	let page_range = KERNEL_FREE_LIST.lock().allocate(layout).unwrap();
	let virtual_address = VirtAddr::from(page_range.start());
	unsafe {
		ret.write(virtual_address.as_u64());
	}
	0
}

/// Marks an aligned physical memory region as used.
///
/// Returns the PhysAddr of the allocated region.
#[hermit_macro::system]
#[unsafe(no_mangle)]
pub extern "C" fn sys_palloc(size: usize, align: usize, ret: *mut u64) -> i32 {
	assert!(!ret.is_null());
	let size = size.align_up(align);
	let layout = PageLayout::from_size_align(size, align).unwrap();
	let frame_range = PHYSICAL_FREE_LIST.lock().allocate(layout).unwrap();
	let physical_address = PhysAddr::from(frame_range.start());
	unsafe {
		ret.write(physical_address.as_u64());
	}
	0
}

/// Allocate at most `max_count` contiguous frames, each aligned to `align`.
///
/// Returns (the aligned window into) the first range that contains at least one
/// frame with the required alignment.
pub fn allocate_max(max_size: usize, align: usize) -> Result<PageRange, AllocError> {
	assert!(max_size > 0);
	assert!(align > 0);
	assert_eq!(
		max_size % align,
		0,
		"Max size {max_size:#X} is not a multiple of the given alignment {align:#X}"
	);
	assert_eq!(
		align % BasePageSize::SIZE as usize,
		0,
		"Alignment {:#X} is not a multiple of {:#X}",
		align,
		BasePageSize::SIZE,
	);

	Ok(PHYSICAL_FREE_LIST.lock().allocate_with(|range| {
		let start = range.start().align_up(align);
		let end = cmp::min(start + max_size, range.end().align_down(align));
		PageRange::new(start, end).ok()
	})?)
}

/// Allocates at most `max_count` frames (each of size `page_size`).
///
/// Returns the PhysAddr of the start and end of the allocated frame range (end
/// refers to the first frame after the range).
#[hermit_macro::system]
#[unsafe(no_mangle)]
pub extern "C" fn sys_palloc_scattered(
	max_count: usize,
	page_size: usize,
	ret_start: *mut u64,
	ret_end: *mut u64,
) -> i32 {
	assert!(!ret_start.is_null());
	assert!(!ret_end.is_null());
	let frames = allocate_max(max_count.checked_mul(page_size).unwrap(), page_size).unwrap();
	let start = PhysAddr::new(frames.start().try_into().unwrap());
	let end = PhysAddr::new(frames.end().try_into().unwrap());
	println!(
		"allocated frames {:#x}..{:#x} ({} frames of size {:#x})",
		start,
		end,
		frames.len().get() / page_size,
		page_size,
	);
	unsafe {
		ret_start.write(start.as_u64());
		ret_end.write(end.as_u64());
	}
	0
}

/// Deallocates the virtual memory at VirtAddr `addr`.
#[hermit_macro::system]
#[unsafe(no_mangle)]
pub extern "C" fn sys_vfree(addr: usize, size: usize) -> i32 {
	let range = PageRange::from_start_len(addr, size).unwrap();
	unsafe {
		KERNEL_FREE_LIST.lock().deallocate(range).unwrap();
	}
	0
}

/// Deallocates the physical memory at PhysAddr `addr`.
///
/// [`physicalmem::deallocate`] warns that the call may fail due to an empty
/// node pool if it isn't called from `mm::deallocate`.
///
/// [`physicalmem::deallocate`]: crate::arch::x86_64::mm::physicalmem::deallocate
#[hermit_macro::system]
#[unsafe(no_mangle)]
pub extern "C" fn sys_pfree(addr: usize, size: usize) -> i32 {
	let range = PageRange::from_start_len(addr, size).unwrap();
	unsafe {
		PHYSICAL_FREE_LIST.lock().deallocate(range).unwrap();
	}
	0
}

/// Flushes the global tlb buffer.
///
/// If the kernel is compiled without the smp feature, this call is a nop.
#[hermit_macro::system]
#[unsafe(no_mangle)]
pub extern "C" fn sys_flush_tlb() -> i32 {
	#[cfg(feature = "smp")]
	crate::arch::x86_64::kernel::apic::ipi_tlb_flush();
	0
}

/// Creates a new virtual memory mapping of the `size` specified with
/// protection bits specified in `prot_flags`.
#[hermit_macro::system(errno)]
#[unsafe(no_mangle)]
pub extern "C" fn sys_mmap(size: usize, prot_flags: MemoryProtection, ret: &mut *mut u8) -> i32 {
	let size = size.align_up(BasePageSize::SIZE as usize);
	let layout = PageLayout::from_size(size).unwrap();
	let page_range = KERNEL_FREE_LIST.lock().allocate(layout).unwrap();
	let virtual_address = VirtAddr::from(page_range.start());
	if prot_flags.is_empty() {
		*ret = virtual_address.as_mut_ptr();
		return 0;
	}
	let frame_layout = PageLayout::from_size(size).unwrap();
	let frame_range = PHYSICAL_FREE_LIST.lock().allocate(frame_layout).unwrap();
	let physical_address = PhysAddr::from(frame_range.start());

	debug!("Mmap {physical_address:X} -> {virtual_address:X} ({size})");
	let count = size / BasePageSize::SIZE as usize;
	let mut flags = PageTableEntryFlags::empty();
	flags.normal().writable();
	if prot_flags.contains(MemoryProtection::Write) {
		flags.writable();
	}
	if !prot_flags.contains(MemoryProtection::Exec) {
		flags.execute_disable();
	}

	arch::mm::paging::map::<BasePageSize>(virtual_address, physical_address, count, flags);

	*ret = virtual_address.as_mut_ptr();

	0
}

/// Unmaps memory at the specified `ptr` for `size` bytes.
#[hermit_macro::system(errno)]
#[unsafe(no_mangle)]
pub extern "C" fn sys_munmap(ptr: *mut u8, size: usize) -> i32 {
	let virtual_address = VirtAddr::from_ptr(ptr);
	let size = size.align_up(BasePageSize::SIZE as usize);

	if let Some(physical_address) = arch::mm::paging::virtual_to_physical(virtual_address) {
		arch::mm::paging::unmap::<BasePageSize>(
			virtual_address,
			size / BasePageSize::SIZE as usize,
		);
		debug!("Unmapping {virtual_address:X} ({size}) -> {physical_address:X}");

		let range = PageRange::from_start_len(physical_address.as_u64() as usize, size).unwrap();
		if let Err(_err) = unsafe { PHYSICAL_FREE_LIST.lock().deallocate(range) } {
			// FIXME: return EINVAL instead, once wasmtime can handle it
			error!("Unable to deallocate {range:?}");
		}
	}

	let range = PageRange::from_start_len(virtual_address.as_usize(), size).unwrap();
	unsafe {
		KERNEL_FREE_LIST.lock().deallocate(range).unwrap();
	}

	0
}

/// Configures the protections associated with a region of virtual memory
/// starting at `ptr` and going to `size`.
///
/// Returns 0 on success and an error code on failure.
#[hermit_macro::system(errno)]
#[unsafe(no_mangle)]
pub extern "C" fn sys_mprotect(ptr: *mut u8, size: usize, prot_flags: MemoryProtection) -> i32 {
	let count = size / BasePageSize::SIZE as usize;
	let mut flags = PageTableEntryFlags::empty();
	flags.normal().writable();
	if prot_flags.contains(MemoryProtection::Write) {
		flags.writable();
	}
	if !prot_flags.contains(MemoryProtection::Exec) {
		flags.execute_disable();
	}

	let virtual_address = VirtAddr::from_ptr(ptr);

	debug!("Mprotect {virtual_address:X} ({size}) -> {prot_flags:?})");
	if let Some(physical_address) = arch::mm::paging::virtual_to_physical(virtual_address) {
		arch::mm::paging::map::<BasePageSize>(virtual_address, physical_address, count, flags);
		0
	} else {
		let frame_layout = PageLayout::from_size(size).unwrap();
		let frame_range = PHYSICAL_FREE_LIST.lock().allocate(frame_layout).unwrap();
		let physical_address = PhysAddr::from(frame_range.start());
		arch::mm::paging::map::<BasePageSize>(virtual_address, physical_address, count, flags);
		0
	}
}

#[hermit_macro::system(errno)]
#[unsafe(no_mangle)]
pub extern "C" fn sys_mlock(_addr: *const c_void, _size: usize) -> i32 {
	// Hermit does not do any swapping yet.
	0
}

#[hermit_macro::system(errno)]
#[unsafe(no_mangle)]
pub extern "C" fn sys_munlock(_addr: *const c_void, _size: usize) -> i32 {
	// Hermit does not do any swapping yet.
	0
}

#[hermit_macro::system(errno)]
#[unsafe(no_mangle)]
pub extern "C" fn sys_mlockall(_flags: c_int) -> i32 {
	// Hermit does not do any swapping yet.
	0
}

#[hermit_macro::system(errno)]
#[unsafe(no_mangle)]
pub extern "C" fn sys_munlockall(_flags: c_int) -> i32 {
	// Hermit does not do any swapping yet.
	0
}
