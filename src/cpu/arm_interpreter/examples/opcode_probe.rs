/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Syntactic preflight only: one synthetic state cannot establish correctness.
use std::io::{self, BufRead};
use touchhle_arm_interpreter::mem::{ConstVoidPtr, GuestUSize, MutPtr, VAddr};
use touchhle_arm_interpreter::{Cpu, CpuError, CpuState, GuestMem};
struct Memory(Vec<u8>);
impl GuestMem for Memory {
    fn null_segment_size(&self) -> VAddr {
        0x1000
    }
    fn get_bytes_fallible(&self, address: ConstVoidPtr, count: GuestUSize) -> Option<&[u8]> {
        let start = address.to_bits() as usize;
        if start < 0x1000 {
            return None;
        }
        self.0.get(start..start.checked_add(count as usize)?)
    }
    fn get_bytes_fallible_mut(
        &mut self,
        address: ConstVoidPtr,
        count: GuestUSize,
    ) -> Option<&mut [u8]> {
        let start = address.to_bits() as usize;
        if start < 0x1000 {
            return None;
        }
        self.0.get_mut(start..start.checked_add(count as usize)?)
    }
    fn bytes_at_mut(&mut self, address: MutPtr<u8>, count: GuestUSize) -> &mut [u8] {
        self.get_bytes_fallible_mut(address.cast_void().cast_const(), count)
            .unwrap()
    }
}
fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    let mut memory = Memory(vec![0; 0x10000]);
    for line in io::stdin().lock().lines() {
        let word = u32::from_str_radix(&line.unwrap(), 16).unwrap();
        memory.0.fill(0);
        memory.0[0x1000..0x1004].copy_from_slice(&word.to_le_bytes());
        let mut cpu = Cpu::new(1);
        cpu.regs_mut().fill(0x8000);
        cpu.regs_mut()[15] = 0x1000;
        cpu.set_cpsr(0xf0000010); // all condition combinations are not covered
        let state = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cpu.run_or_step(&mut memory, None)
        }));
        let class = match state {
            Err(_) => "panic",
            Ok(CpuState::Error(CpuError::UnsupportedInstruction { .. })) => "unsupported",
            Ok(CpuState::Error(CpuError::UndefinedInstruction)) => "undefined",
            Ok(CpuState::Error(CpuError::MemoryError)) => "memory",
            Ok(CpuState::Error(CpuError::Breakpoint)) => "breakpoint",
            Ok(CpuState::Svc(_)) => "svc",
            Ok(CpuState::Normal) => "recognized",
        };
        println!("{word:08x}\t{class}");
    }
}
