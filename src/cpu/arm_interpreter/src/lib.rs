/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Portable ARMv7 user-mode interpreter; does not allocate executable memory.
macro_rules! echo {
    () => { log::info!("") };
    ($($arg:tt)+) => { log::info!($($arg)+) };
}
mod interpreter;
pub mod mem;
pub use interpreter::{CpuContext, InterpreterCpu};
pub use mem::{ConstPtr, ConstVoidPtr, GuestMem, MutPtr, MutVoidPtr, Ptr, VAddr};
pub type Cpu = InterpreterCpu;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CpuState {
    Normal,
    Svc(u32),
    Error(CpuError),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CpuError {
    MemoryError,
    UndefinedInstruction,
    UnsupportedInstruction { pc: u32, encoding: u32, thumb: bool },
    Breakpoint,
}
/// Instruction location for the last nonempty run's error. An absent encoding
/// means fetching the instruction failed. This is diagnostic, not guest context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaultInfo {
    pub pc: u32,
    pub encoding: Option<u32>,
    pub thumb: bool,
}
pub const SP: usize = 13;
pub const LR: usize = 14;
pub const PC: usize = 15;
pub const CPSR_THUMB: u32 = 0x20;
pub const CPSR_USER_MODE: u32 = 0x10;
