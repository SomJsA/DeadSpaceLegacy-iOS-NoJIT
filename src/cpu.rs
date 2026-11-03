/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! ARMv6/ARMv7 CPU backends. Dynarmic remains the default when compiled.
use crate::abi::GuestFunction;
use crate::mem::{GuestUSize, Mem};

#[cfg(feature = "cpu-dynarmic")]
mod dynarmic;
#[cfg(feature = "cpu-interpreter")]
use touchHLE_arm_interpreter as interpreter;

#[cfg(feature = "cpu-dynarmic")]
pub use dynarmic::CpuContext;
#[cfg(all(not(feature = "cpu-dynarmic"), feature = "cpu-interpreter"))]
pub use interpreter::CpuContext;
#[cfg(not(any(feature = "cpu-dynarmic", feature = "cpu-interpreter")))]
compile_error!("Select cpu-dynarmic or cpu-interpreter; no CPU backend was compiled.");

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
    UnpredictableInstruction,
    UnsupportedInstruction { pc: u32, encoding: u32, thumb: bool },
    Breakpoint,
}

enum Backend {
    #[cfg(feature = "cpu-dynarmic")]
    Dynarmic(dynarmic::Cpu),
    #[cfg(feature = "cpu-interpreter")]
    Interpreter(Box<interpreter::Cpu>),
}
pub struct Cpu {
    backend: Backend,
}

impl Cpu {
    pub const SP: usize = 13;
    pub const LR: usize = 14;
    pub const PC: usize = 15;
    pub const CPSR_THUMB: u32 = 0x20;
    pub const CPSR_USER_MODE: u32 = 0x10;

    pub fn new(direct_memory_access: Option<&mut Mem>) -> Self {
        let default = if cfg!(feature = "cpu-dynarmic") {
            "dynarmic"
        } else {
            "interpreter"
        };
        let name = std::env::var("TOUCHHLE_CPU_BACKEND").unwrap_or_else(|_| default.to_owned());
        let cpu = Self::with_backend(direct_memory_access, &name);
        log!("CPU backend: {}", cpu.backend_name());
        cpu
    }

    /// Explicit selection also keeps tests independent of process environment.
    pub(crate) fn with_backend(direct_memory_access: Option<&mut Mem>, name: &str) -> Self {
        let backend = match name {
            #[cfg(feature = "cpu-dynarmic")]
            "dynarmic" => Backend::Dynarmic(dynarmic::Cpu::new(direct_memory_access)),
            #[cfg(feature = "cpu-interpreter")]
            "interpreter" => {
                let pages = direct_memory_access
                    .as_ref()
                    .map_or(0, |mem| mem.null_segment_size() / 0x1000);
                Backend::Interpreter(interpreter::Cpu::new(pages))
            }
            _ => panic!("CPU backend {name:?} is unknown or was not compiled into this binary"),
        };
        Self { backend }
    }

    pub fn backend_name(&self) -> &'static str {
        match &self.backend {
            #[cfg(feature = "cpu-dynarmic")]
            Backend::Dynarmic(_) => "dynarmic",
            #[cfg(feature = "cpu-interpreter")]
            Backend::Interpreter(_) => "interpreter",
        }
    }
    pub fn regs(&self) -> &[u32; 16] {
        match &self.backend {
            #[cfg(feature = "cpu-dynarmic")]
            Backend::Dynarmic(cpu) => cpu.regs(),
            #[cfg(feature = "cpu-interpreter")]
            Backend::Interpreter(cpu) => cpu.regs(),
        }
    }
    pub fn regs_mut(&mut self) -> &mut [u32; 16] {
        match &mut self.backend {
            #[cfg(feature = "cpu-dynarmic")]
            Backend::Dynarmic(cpu) => cpu.regs_mut(),
            #[cfg(feature = "cpu-interpreter")]
            Backend::Interpreter(cpu) => cpu.regs_mut(),
        }
    }
    pub fn extregs(&self) -> &[u32; 64] {
        match &self.backend {
            #[cfg(feature = "cpu-dynarmic")]
            Backend::Dynarmic(cpu) => cpu.extregs(),
            #[cfg(feature = "cpu-interpreter")]
            Backend::Interpreter(cpu) => cpu.extregs(),
        }
    }
    pub fn extregs_mut(&mut self) -> &mut [u32; 64] {
        match &mut self.backend {
            #[cfg(feature = "cpu-dynarmic")]
            Backend::Dynarmic(cpu) => cpu.extregs_mut(),
            #[cfg(feature = "cpu-interpreter")]
            Backend::Interpreter(cpu) => cpu.extregs_mut(),
        }
    }
    pub fn cpsr(&self) -> u32 {
        match &self.backend {
            #[cfg(feature = "cpu-dynarmic")]
            Backend::Dynarmic(cpu) => cpu.cpsr(),
            #[cfg(feature = "cpu-interpreter")]
            Backend::Interpreter(cpu) => cpu.cpsr(),
        }
    }
    pub fn set_cpsr(&mut self, value: u32) {
        match &mut self.backend {
            #[cfg(feature = "cpu-dynarmic")]
            Backend::Dynarmic(cpu) => cpu.set_cpsr(value),
            #[cfg(feature = "cpu-interpreter")]
            Backend::Interpreter(cpu) => cpu.set_cpsr(value),
        }
    }
    pub fn fpscr(&self) -> u32 {
        match &self.backend {
            #[cfg(feature = "cpu-dynarmic")]
            Backend::Dynarmic(cpu) => cpu.fpscr(),
            #[cfg(feature = "cpu-interpreter")]
            Backend::Interpreter(cpu) => cpu.fpscr(),
        }
    }
    pub fn set_fpscr(&mut self, value: u32) {
        match &mut self.backend {
            #[cfg(feature = "cpu-dynarmic")]
            Backend::Dynarmic(cpu) => cpu.set_fpscr(value),
            #[cfg(feature = "cpu-interpreter")]
            Backend::Interpreter(cpu) => cpu.set_fpscr(value),
        }
    }
    pub fn swap_context(&mut self, context: &mut CpuContext) {
        match &mut self.backend {
            #[cfg(feature = "cpu-dynarmic")]
            Backend::Dynarmic(cpu) => cpu.swap_context(context),
            #[cfg(feature = "cpu-interpreter")]
            Backend::Interpreter(cpu) => {
                // Explicit field conversion avoids assuming layout compatibility.
                let mut native = interpreter::CpuContext {
                    regs: context.regs,
                    extregs: context.extregs,
                    cpsr: context.cpsr,
                    fpscr: context.fpscr,
                };
                cpu.swap_context(&mut native);
                context.regs = native.regs;
                context.extregs = native.extregs;
                context.cpsr = native.cpsr;
                context.fpscr = native.fpscr;
            }
        }
    }
    pub fn invalidate_cache_range(&mut self, base: u32, size: GuestUSize) {
        match &mut self.backend {
            #[cfg(feature = "cpu-dynarmic")]
            Backend::Dynarmic(cpu) => cpu.invalidate_cache_range(base, size),
            #[cfg(feature = "cpu-interpreter")]
            Backend::Interpreter(cpu) => cpu.invalidate_cache_range(base, size),
        }
    }
    #[must_use]
    pub fn run_or_step(&mut self, mem: &mut Mem, ticks: Option<&mut u64>) -> CpuState {
        if ticks.as_ref().is_some_and(|budget| **budget == 0) {
            return CpuState::Normal;
        }
        // Existing opt-in performance trace; no clock read when disabled.
        let _cpu_timer = crate::ds_perf::Timer::new(crate::ds_perf::Stage::GuestCpu);
        match &mut self.backend {
            #[cfg(feature = "cpu-dynarmic")]
            Backend::Dynarmic(cpu) => cpu.run_or_step(mem, ticks),
            #[cfg(feature = "cpu-interpreter")]
            Backend::Interpreter(cpu) => {
                match cpu.run_or_step(mem, ticks) {
                    interpreter::CpuState::Normal => CpuState::Normal,
                    interpreter::CpuState::Svc(number) => CpuState::Svc(number),
                    interpreter::CpuState::Error(error) => CpuState::Error(match error {
                        interpreter::CpuError::MemoryError => {
                            if let Some(fault) = cpu.last_fault() {
                                log!("[INTERP-FAULT] pc={:#010x} encoding={:#010x} thumb={} fetch={} post_pc={:#010x}",
                                fault.pc,fault.encoding.unwrap_or(0),fault.thumb,fault.encoding.is_none(),cpu.regs()[15]);
                                for (label, address) in
                                    [("pc", fault.pc), ("lr", cpu.regs()[14] & !1)]
                                {
                                    let start = address.saturating_sub(16);
                                    if let Some(bytes) = mem
                                        .get_bytes_fallible(crate::mem::Ptr::from_bits(start), 32)
                                    {
                                        log!("[INTERP-FAULT-CODE] {label} start={start:#010x} bytes={bytes:02x?}");
                                    }
                                }
                            }
                            log!("[INTERP-FAULT-STATE] cpsr={:#010x} fpscr={:#010x} post_regs={:08x?}",cpu.cpsr(),cpu.fpscr(),cpu.regs());
                            log!("[INTERP-FAULT-EXT] extregs={:08x?}", cpu.extregs());
                            CpuError::MemoryError
                        }
                        interpreter::CpuError::UnsupportedInstruction {
                            pc,
                            encoding,
                            thumb,
                        } => {
                            log!("[INTERP-UNSUPPORTED] pc={pc:#010x} encoding={encoding:#010x} thumb={thumb}");
                            CpuError::UnsupportedInstruction {
                                pc,
                                encoding,
                                thumb,
                            }
                        }
                        interpreter::CpuError::UndefinedInstruction => {
                            CpuError::UndefinedInstruction
                        }
                        interpreter::CpuError::Breakpoint => CpuError::Breakpoint,
                    }),
                }
            }
        }
    }
    pub fn dump_regs(&self) {
        Self::echo_regs(self.regs());
    }
    pub fn echo_regs(regs: &[u32; 16]) {
        let _ = std::panic::catch_unwind(|| {
            for row in 0..4 {
                use std::fmt::Write;
                let mut line = String::new();
                for column in 0..4 {
                    let register = row * 4 + column;
                    match register {
                        Self::SP => write!(&mut line, "\t SP: "),
                        Self::LR => write!(&mut line, "\t LR: "),
                        Self::PC => write!(&mut line, "\t PC: "),
                        _ if register <= 9 => write!(&mut line, "\t R{register}: "),
                        _ => write!(&mut line, "\tR{register}: "),
                    }
                    .unwrap();
                    write!(&mut line, "{:#010x}", regs[register]).unwrap();
                }
                echo!("{}", line);
            }
        });
    }
    pub fn pc_with_thumb_bit(&self) -> GuestFunction {
        GuestFunction::from_addr_and_thumb_flag(
            self.regs()[Self::PC],
            self.cpsr() & Self::CPSR_THUMB != 0,
        )
    }
    pub fn branch(&mut self, function: GuestFunction) {
        self.regs_mut()[Self::PC] = function.addr_without_thumb_bit();
        self.set_cpsr(
            (self.cpsr() & !Self::CPSR_THUMB) | (u32::from(function.is_thumb()) * Self::CPSR_THUMB),
        );
    }
    pub fn branch_with_link(
        &mut self,
        function: GuestFunction,
        link: GuestFunction,
    ) -> (GuestFunction, GuestFunction) {
        let old_pc = self.pc_with_thumb_bit();
        let old_lr = GuestFunction::from_addr_with_thumb_bit(self.regs()[Self::LR]);
        self.branch(function);
        self.regs_mut()[Self::LR] = link.addr_with_thumb_bit();
        (old_pc, old_lr)
    }
}

#[cfg(feature = "cpu-interpreter")]
impl interpreter::GuestMem for Mem {
    fn null_segment_size(&self) -> u32 {
        Mem::null_segment_size(self)
    }
    fn get_bytes_fallible(&self, pointer: interpreter::ConstVoidPtr, count: u32) -> Option<&[u8]> {
        let address = pointer.to_bits();
        let result = Mem::get_bytes_fallible(self, crate::mem::Ptr::from_bits(address), count)
            .filter(|bytes| bytes.len() == count as usize);
        if result.is_none() {
            log!("[INTERP-MEM] address={address:#010x} count={count} write=false");
        }
        result
    }
    fn get_bytes_fallible_mut(
        &mut self,
        pointer: interpreter::ConstVoidPtr,
        count: u32,
    ) -> Option<&mut [u8]> {
        let address = pointer.to_bits();
        if address < Mem::null_segment_size(self) {
            // Preserve the existing HLE data-write sink without relaxing GDB's
            // separate low-address write restrictions. Never return a short slice.
            let available = crate::mem::PAGE_SIZE - address % crate::mem::PAGE_SIZE;
            if count > available {
                log!("[INTERP-MEM] address={address:#010x} count={count} write=true low-page-boundary=true");
                return None;
            }
            return Some(Mem::bytes_at_mut(
                self,
                crate::mem::Ptr::from_bits(address),
                count,
            ));
        }
        let result = Mem::get_bytes_fallible_mut(self, crate::mem::Ptr::from_bits(address), count)
            .filter(|bytes| bytes.len() == count as usize);
        if result.is_none() {
            log!("[INTERP-MEM] address={address:#010x} count={count} write=true");
        }
        result
    }
    fn bytes_at_mut(&mut self, pointer: interpreter::MutPtr<u8>, count: u32) -> &mut [u8] {
        Mem::bytes_at_mut(self, crate::mem::Ptr::from_bits(pointer.to_bits()), count)
    }
}

#[cfg(all(test, feature = "cpu-dynarmic", feature = "cpu-interpreter"))]
mod tests;
