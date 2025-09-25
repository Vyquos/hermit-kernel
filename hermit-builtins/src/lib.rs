#![no_std]
#![feature(linkage)]

use core::ffi::{c_int};

pub mod math;

#[unsafe(no_mangle)]
pub extern "C" fn fegetround() -> c_int {
    // Rust doesn't support rounding modes. The rounding mode is not determinable.
    -1
}

#[unsafe(no_mangle)]
pub extern "C" fn fesetround(_rounding_mode: c_int) -> c_int {
    // Rust doesn't support rounding modes. Always fail (!= 0)
    1
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
	loop {}
}
