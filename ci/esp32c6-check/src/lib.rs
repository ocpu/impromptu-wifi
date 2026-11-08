//! Compile check only: links nothing and runs nothing. Building this crate
//! for `riscv32imac-unknown-none-elf` type-checks `impromptu_wifi::embassy::esp` and
//! the trouble-host bindings against a real chip configuration.
#![no_std]

pub use impromptu_wifi::embassy::esp;
pub use impromptu_wifi::embassy::trouble;
