use touchhle_arm_interpreter::mem::{ConstVoidPtr, GuestUSize, MutPtr, VAddr};
use touchhle_arm_interpreter::{Cpu, CpuContext, CpuError, CpuState, GuestMem, Ptr, CPSR_THUMB};

#[derive(Clone)]
struct TestMem(Vec<u8>);
impl TestMem {
    fn new() -> Self {
        Self(vec![0; 0x10000])
    }
    fn words(&mut self, address: usize, words: &[u32]) {
        for (index, word) in words.iter().enumerate() {
            self.0[address + index * 4..address + index * 4 + 4]
                .copy_from_slice(&word.to_le_bytes());
        }
    }
    fn halves(&mut self, address: usize, halves: &[u16]) {
        for (index, half) in halves.iter().enumerate() {
            self.0[address + index * 2..address + index * 2 + 2]
                .copy_from_slice(&half.to_le_bytes());
        }
    }
    fn word(&self, address: usize) -> u32 {
        u32::from_le_bytes(self.0[address..address + 4].try_into().unwrap())
    }
}
impl GuestMem for TestMem {
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
fn cpu() -> Box<Cpu> {
    let mut cpu = Cpu::new(1);
    cpu.regs_mut()[15] = 0x1000;
    cpu
}
fn run(cpu: &mut Cpu, mem: &mut TestMem, budget: u64) -> (CpuState, u64) {
    let mut remaining = budget;
    (cpu.run_or_step(mem, Some(&mut remaining)), remaining)
}

#[test]
fn arm_arithmetic_and_svc_stop_before_next_instruction() {
    let mut mem = TestMem::new();
    mem.words(0x1000, &[0xe3a0002a, 0xe2800001, 0xef000007, 0xe2800001]);
    let mut cpu = cpu();
    assert_eq!(run(&mut cpu, &mut mem, 100), (CpuState::Svc(7), 97));
    assert_eq!(cpu.regs()[0], 43);
    assert_eq!(cpu.regs()[15], 0x100c);
}

#[test]
fn arm_memory_roundtrip_preserves_little_endian_bytes() {
    let mut mem = TestMem::new();
    mem.words(0x1000, &[0xe3a000ab, 0xe5810000, 0xe5912000, 0xef000000]);
    let mut cpu = cpu();
    cpu.regs_mut()[1] = 0x2000;
    assert_eq!(run(&mut cpu, &mut mem, 100).0, CpuState::Svc(0));
    assert_eq!(cpu.regs()[2], 0xab);
    assert_eq!(&mem.0[0x2000..0x2004], &[0xab, 0, 0, 0]);
}

#[test]
fn thumb_arithmetic_obeys_instruction_budget() {
    let mut mem = TestMem::new();
    mem.halves(0x1000, &[0x2005, 0x3003, 0x3001]);
    let mut cpu = cpu();
    cpu.set_cpsr(0x10 | CPSR_THUMB);
    assert_eq!(run(&mut cpu, &mut mem, 2), (CpuState::Normal, 0));
    assert_eq!(cpu.regs()[0], 8);
    assert_eq!(cpu.regs()[15], 0x1004);
}

#[test]
fn empty_budget_does_not_execute_or_modify_guest() {
    let mut mem = TestMem::new();
    mem.words(0x1000, &[0xe2800001]);
    let mut cpu = cpu();
    let registers = *cpu.regs();
    assert_eq!(run(&mut cpu, &mut mem, 0), (CpuState::Normal, 0));
    assert_eq!(*cpu.regs(), registers);
}

#[test]
fn integer_add_sets_architectural_nzcv() {
    let cases = [
        (0x7fffffff_u32, 1_u32, 0x80000000_u32, 0x90000000_u32),
        (0xffffffff, 1, 0, 0x60000000),
        (0x80000000, 0x80000000, 0, 0x70000000),
        (1, 2, 3, 0),
    ];
    for (left, right, result, flags) in cases {
        let mut mem = TestMem::new();
        mem.words(0x1000, &[0xe0910002]); // adds r0,r1,r2
        let mut cpu = cpu();
        cpu.regs_mut()[1] = left;
        cpu.regs_mut()[2] = right;
        assert_eq!(cpu.run_or_step(&mut mem, None), CpuState::Normal);
        assert_eq!(cpu.regs()[0], result);
        assert_eq!(cpu.cpsr() & 0xf0000000, flags);
    }
}

#[test]
fn conditional_arm_instruction_skips_side_effects() {
    let mut mem = TestMem::new();
    mem.words(0x1000, &[0x02800001]); // addeq r0,r0,#1
    let mut cpu = cpu();
    cpu.regs_mut()[0] = 9;
    assert_eq!(cpu.run_or_step(&mut mem, None), CpuState::Normal);
    assert_eq!(cpu.regs()[0], 9);
    assert_eq!(cpu.regs()[15], 0x1004);
}

#[test]
fn thumb_it_preserves_flags_for_implicit_flag_setting_instruction() {
    let mut mem = TestMem::new();
    mem.halves(0x1000, &[0xbf08, 0x2001]); // it eq; moveq r0,#1
    let mut cpu = cpu();
    cpu.set_cpsr(0x40000010 | CPSR_THUMB);
    assert_eq!(run(&mut cpu, &mut mem, 2), (CpuState::Normal, 0));
    assert_eq!(cpu.regs()[0], 1);
    assert_eq!(cpu.cpsr() & 0xf0000000, 0x40000000);
    assert_eq!(cpu.cpsr() & 0x0600fc00, 0); // ITSTATE consumed
}

#[test]
fn thumb_it_false_condition_skips_instruction() {
    let mut mem = TestMem::new();
    mem.halves(0x1000, &[0xbf08, 0x2001]);
    let mut cpu = cpu();
    cpu.set_cpsr(0x10 | CPSR_THUMB);
    cpu.regs_mut()[0] = 9;
    assert_eq!(run(&mut cpu, &mut mem, 2).0, CpuState::Normal);
    assert_eq!(cpu.regs()[0], 9);
}

#[test]
fn thumb2_movw_decodes_both_halfwords() {
    let mut mem = TestMem::new();
    mem.halves(0x1000, &[0xf241, 0x2034]); // movw r0,#0x1234
    let mut cpu = cpu();
    cpu.set_cpsr(0x10 | CPSR_THUMB);
    assert_eq!(cpu.run_or_step(&mut mem, None), CpuState::Normal);
    assert_eq!(cpu.regs()[0], 0x1234);
    assert_eq!(cpu.regs()[15], 0x1004);
}

#[test]
fn thumb_bx_switches_to_arm_and_preserves_target() {
    let mut mem = TestMem::new();
    mem.halves(0x1000, &[0x4708]); // bx r1
    let mut cpu = cpu();
    cpu.set_cpsr(0x10 | CPSR_THUMB);
    cpu.regs_mut()[1] = 0x2000;
    assert_eq!(cpu.run_or_step(&mut mem, None), CpuState::Normal);
    assert_eq!(cpu.regs()[15], 0x2000);
    assert_eq!(cpu.cpsr() & CPSR_THUMB, 0);
}

#[test]
fn null_read_and_truncated_thumb2_are_reported_as_memory_errors() {
    let mut mem = TestMem::new();
    mem.words(0x1000, &[0xe5910000]);
    let mut cpu = cpu();
    assert_eq!(
        cpu.run_or_step(&mut mem, None),
        CpuState::Error(CpuError::MemoryError)
    );
    mem.halves(0xfffe, &[0xf241]);
    cpu.regs_mut()[15] = 0xfffe;
    cpu.set_cpsr(0x10 | CPSR_THUMB);
    assert_eq!(
        cpu.run_or_step(&mut mem, None),
        CpuState::Error(CpuError::MemoryError)
    );
}

#[test]
fn context_roundtrip_keeps_integer_float_and_status_registers() {
    let mut cpu = cpu();
    for (index, register) in cpu.regs_mut().iter_mut().enumerate() {
        *register = 0x1000 + index as u32;
    }
    for (index, register) in cpu.extregs_mut().iter_mut().enumerate() {
        *register = 0x80000000 + index as u32;
    }
    cpu.set_cpsr(0xa0000030);
    cpu.set_fpscr(0x03000000);
    let registers = *cpu.regs();
    let extensions = *cpu.extregs();
    let mut context = CpuContext::new();
    cpu.swap_context(&mut context);
    assert_eq!(*cpu.regs(), [0; 16]);
    assert_eq!(context.regs, registers);
    cpu.swap_context(&mut context);
    assert_eq!(*cpu.regs(), registers);
    assert_eq!(*cpu.extregs(), extensions);
    assert_eq!(cpu.cpsr(), 0xa0000030);
    assert_eq!(cpu.fpscr(), 0x03000000);
}

#[test]
fn context_switch_invalidates_exclusive_reservation() {
    let mut mem = TestMem::new();
    mem.words(0x1000, &[0xe1910f9f, 0xe1812f93]); // ldrex r0,[r1]; strex r2,r3,[r1]
    mem.words(0x2000, &[42]);
    let mut cpu = cpu();
    cpu.regs_mut()[1] = 0x2000;
    cpu.regs_mut()[3] = 99;
    assert_eq!(cpu.run_or_step(&mut mem, None), CpuState::Normal);
    assert_eq!(cpu.regs()[0], 42);
    let mut context = CpuContext::new();
    cpu.swap_context(&mut context);
    mem.words(0x2000, &[55]); // another guest thread's write
    cpu.swap_context(&mut context);
    assert_eq!(cpu.run_or_step(&mut mem, None), CpuState::Normal);
    assert_eq!(cpu.regs()[2], 1);
    assert_eq!(mem.word(0x2000), 55);
}

#[test]
fn vfp_add_uses_shared_single_precision_register_file() {
    let mut mem = TestMem::new();
    mem.words(0x1000, &[0xee300a00]); // vadd.f32 s0,s0,s0
    let mut cpu = cpu();
    cpu.extregs_mut()[0] = 1.25_f32.to_bits();
    assert_eq!(cpu.run_or_step(&mut mem, None), CpuState::Normal);
    assert_eq!(f32::from_bits(cpu.extregs()[0]), 2.5);
}

#[test]
fn guest_memory_rejects_address_overflow() {
    let mem = TestMem::new();
    assert!(mem
        .get_bytes_fallible(Ptr::from_bits(0xfffffffe), 4)
        .is_none());
}

#[test]
fn arm_exclusive_transfer_sizes_preserve_surrounding_bytes() {
    for (size, load, store) in [
        (1, 0xe1d40f9f, 0xe1c42f96),
        (2, 0xe1f40f9f, 0xe1e42f96),
        (4, 0xe1940f9f, 0xe1842f96),
        (8, 0xe1b40f9f, 0xe1a42f96),
    ] {
        let mut mem = TestMem::new();
        mem.words(0x1000, &[load, store]);
        mem.words(0x2000, &[0x12345678, 0x87654321, 0xaabbccdd]);
        let before = mem.0[0x2000..0x200c].to_vec();
        let mut cpu = cpu();
        cpu.regs_mut()[4] = 0x2000;
        cpu.regs_mut()[6] = 0x88776655;
        cpu.regs_mut()[7] = 0x44332211;
        assert_eq!(run(&mut cpu, &mut mem, 2).0, CpuState::Normal);
        assert_eq!(cpu.regs()[2], 0);
        assert_eq!(
            &mem.0[0x2000..0x2000 + size],
            &0x4433221188776655_u64.to_le_bytes()[..size]
        );
        assert_eq!(&mem.0[0x2000 + size..0x200c], &before[size..]);
        let mask = if size >= 4 {
            u32::MAX
        } else {
            (1_u32 << (size * 8)) - 1
        };
        assert_eq!(cpu.regs()[0], 0x12345678 & mask);
        if size == 8 {
            assert_eq!(cpu.regs()[1], 0x87654321);
        }
    }
}

#[test]
fn clrex_cancels_arm_exclusive_store() {
    let mut mem = TestMem::new();
    mem.words(0x1000, &[0xe1910f9f, 0xf57ff01f, 0xe1812f93]);
    mem.words(0x2000, &[42]);
    let mut cpu = cpu();
    cpu.regs_mut()[1] = 0x2000;
    cpu.regs_mut()[3] = 99;
    assert_eq!(run(&mut cpu, &mut mem, 3).0, CpuState::Normal);
    assert_eq!(cpu.regs()[2], 1);
    assert_eq!(mem.word(0x2000), 42);
}

#[test]
fn exclusive_store_requires_same_transfer_size() {
    let mut mem = TestMem::new();
    mem.words(0x1000, &[0xe1d40f9f, 0xe1842f96]); // ldrexb; strex (word)
    mem.words(0x2000, &[42]);
    let mut cpu = cpu();
    cpu.regs_mut()[4] = 0x2000;
    cpu.regs_mut()[6] = 99;
    assert_eq!(run(&mut cpu, &mut mem, 2).0, CpuState::Normal);
    assert_eq!(cpu.regs()[2], 1);
    assert_eq!(mem.word(0x2000), 42);
}

#[test]
fn unaligned_arm_exclusive_read_reports_memory_error() {
    let mut mem = TestMem::new();
    mem.words(0x1000, &[0xe1910f9f]);
    let mut cpu = cpu();
    cpu.regs_mut()[1] = 0x2001;
    assert_eq!(
        cpu.run_or_step(&mut mem, None),
        CpuState::Error(CpuError::MemoryError)
    );
}

#[test]
fn unsupported_instruction_has_its_own_fault_with_original_encoding() {
    let mut mem = TestMem::new();
    mem.words(0x1000, &[0xec000000]); // unsupported coprocessor 0 instruction
    let mut cpu = cpu();
    assert_eq!(
        cpu.run_or_step(&mut mem, None),
        CpuState::Error(CpuError::UnsupportedInstruction {
            pc: 0x1000,
            encoding: 0xec000000,
            thumb: false
        })
    );
}

#[test]
fn arm_status_transfers_and_breakpoint_are_executed() {
    let mut mem = TestMem::new();
    mem.words(0x1000, &[0xe10f1000, 0xe128f000, 0xe328f480, 0xe1200070]);
    let mut cpu = cpu();
    cpu.set_cpsr(0x600f0010);
    cpu.regs_mut()[0] = 0xa8000000;
    assert_eq!(cpu.run_or_step(&mut mem, None), CpuState::Normal);
    assert_eq!(cpu.regs()[1], 0x600f0010);
    assert_eq!(cpu.run_or_step(&mut mem, None), CpuState::Normal);
    assert_eq!(cpu.cpsr(), 0xa80f0010);
    assert_eq!(cpu.run_or_step(&mut mem, None), CpuState::Normal);
    assert_eq!(cpu.cpsr(), 0x800f0010);
    assert_eq!(
        cpu.run_or_step(&mut mem, None),
        CpuState::Error(CpuError::Breakpoint)
    );
}

#[test]
fn thumb_status_transfers_do_not_silently_become_nops() {
    let mut mem = TestMem::new();
    mem.halves(0x1000, &[0xf380, 0x8800, 0xf3ef, 0x8100]);
    let mut cpu = cpu();
    cpu.set_cpsr(0x10 | CPSR_THUMB);
    cpu.regs_mut()[0] = 0xa8000000;
    assert_eq!(run(&mut cpu, &mut mem, 2).0, CpuState::Normal);
    assert_eq!(cpu.cpsr(), 0xa8000030);
    assert_eq!(cpu.regs()[1], 0xa8000030);
}

#[test]
fn unknown_thumb_system_instruction_fails_distinctly() {
    let mut mem = TestMem::new();
    mem.halves(0x1000, &[0xf3ef, 0x8001]);
    let mut cpu = cpu();
    cpu.set_cpsr(0x10 | CPSR_THUMB);
    assert_eq!(
        cpu.run_or_step(&mut mem, None),
        CpuState::Error(CpuError::UnsupportedInstruction {
            pc: 0x1000,
            encoding: 0xf3ef8001,
            thumb: true
        })
    );
}

#[test]
fn thumb_clrex_invalidates_exclusive_store() {
    let mut mem = TestMem::new();
    mem.halves(0x1000, &[0xe851, 0x0f00, 0xf3bf, 0x8f2f, 0xe841, 0x3200]);
    mem.words(0x2000, &[42]);
    let mut cpu = cpu();
    cpu.set_cpsr(0x10 | CPSR_THUMB);
    cpu.regs_mut()[1] = 0x2000;
    cpu.regs_mut()[3] = 99;
    assert_eq!(run(&mut cpu, &mut mem, 3).0, CpuState::Normal);
    assert_eq!(cpu.regs()[2], 1);
    assert_eq!(mem.word(0x2000), 42);
}

// The HLE intentionally supports zero-filled low-address reads and discarded
// writes. Instruction fetch remains protected by the CPU's null segment.
struct LegacyNullMem {
    inner: TestMem,
    zero: [u8; 4096],
    sink: [u8; 4096],
}
impl GuestMem for LegacyNullMem {
    fn null_segment_size(&self) -> VAddr {
        0x1000
    }
    fn get_bytes_fallible(&self, address: ConstVoidPtr, count: GuestUSize) -> Option<&[u8]> {
        let start = address.to_bits() as usize;
        if start < 4096 {
            self.zero.get(start..start.checked_add(count as usize)?)
        } else {
            self.inner.get_bytes_fallible(address, count)
        }
    }
    fn get_bytes_fallible_mut(
        &mut self,
        address: ConstVoidPtr,
        count: GuestUSize,
    ) -> Option<&mut [u8]> {
        let start = address.to_bits() as usize;
        if start < 4096 {
            self.sink.get_mut(start..start.checked_add(count as usize)?)
        } else {
            self.inner.get_bytes_fallible_mut(address, count)
        }
    }
    fn bytes_at_mut(&mut self, address: MutPtr<u8>, count: GuestUSize) -> &mut [u8] {
        self.get_bytes_fallible_mut(address.cast_void().cast_const(), count)
            .unwrap()
    }
}

#[test]
fn interpreter_uses_host_data_policy_without_executing_null_memory() {
    let mut mem = LegacyNullMem {
        inner: TestMem::new(),
        zero: [0; 4096],
        sink: [0; 4096],
    };
    mem.inner
        .words(0x1000, &[0xe5910000, 0xe5812000, 0xe5913000]); // ldr; str; ldr
    let mut cpu = cpu();
    cpu.regs_mut()[1] = 0;
    cpu.regs_mut()[2] = 0x12345678;
    assert_eq!(cpu.run_or_step(&mut mem, None), CpuState::Normal);
    assert_eq!(cpu.regs()[0], 0);
    assert_eq!(cpu.run_or_step(&mut mem, None), CpuState::Normal);
    assert_eq!(cpu.run_or_step(&mut mem, None), CpuState::Normal);
    assert_eq!(cpu.regs()[3], 0);
    cpu.regs_mut()[15] = 0;
    assert_eq!(
        cpu.run_or_step(&mut mem, None),
        CpuState::Error(CpuError::MemoryError)
    );
}

fn neon_logic_encoding(u: u32, operation: u32, quad: bool, rd: usize, rn: usize, rm: usize) -> u32 {
    0xf2000110
        | (u << 24)
        | (operation << 20)
        | ((quad as u32) << 6)
        | (((rd as u32 >> 4) & 1) << 22)
        | ((rd as u32 & 15) << 12)
        | (((rn as u32 >> 4) & 1) << 7)
        | ((rn as u32 & 15) << 16)
        | (((rm as u32 >> 4) & 1) << 5)
        | (rm as u32 & 15)
}

#[test]
fn neon_logic_variants_in_arm_and_thumb_obey_bit_selection_rules() {
    for thumb in [false, true] {
        for u in 0..=1 {
            for operation in 0..4 {
                for quad in [false, true] {
                    for banks in 0_usize..8 {
                        for alias in 0..3 {
                            let (rd, mut rn, mut rm) = (
                                (banks & 1) * 16,
                                2 + ((banks >> 1) & 1) * 16,
                                4 + ((banks >> 2) & 1) * 16,
                            );
                            if alias == 1 {
                                rn = rd;
                            }
                            if alias == 2 {
                                rm = rd;
                            }
                            let arm = neon_logic_encoding(u, operation, quad, rd, rn, rm);
                            let mut mem = TestMem::new();
                            let mut cpu = cpu();
                            if thumb {
                                let encoding = (arm & 0x00ffffff) | 0xef000000 | (u << 28);
                                mem.halves(0x1000, &[(encoding >> 16) as u16, encoding as u16]);
                            } else {
                                mem.words(0x1000, &[arm]);
                            }
                            cpu.set_cpsr(0xa0000010 | if thumb { CPSR_THUMB } else { 0 });
                            cpu.set_fpscr(0x01000000);
                            for (index, register) in cpu.extregs_mut().iter_mut().enumerate() {
                                *register = (index as u32)
                                    .wrapping_mul(0x9e3779b9)
                                    .wrapping_add(0x31415926);
                            }
                            let before = *cpu.extregs();
                            let mut expected = before;
                            let dreg = |number: usize| {
                                before[number * 2] as u64 | ((before[number * 2 + 1] as u64) << 32)
                            };
                            for lane in 0..if quad { 2 } else { 1 } {
                                let (destination, left, right) =
                                    (dreg(rd + lane), dreg(rn + lane), dreg(rm + lane));
                                let value = match (u, operation) {
                                    (0, 0) => left & right,
                                    (0, 1) => left & !right,
                                    (0, 2) => left | right,
                                    (0, 3) => left | !right,
                                    (1, 0) => left ^ right,
                                    (1, 1) => (destination & left) | (!destination & right),
                                    (1, 2) => (destination & !right) | (left & right),
                                    (1, 3) => (destination & right) | (left & !right),
                                    _ => unreachable!(),
                                };
                                expected[(rd + lane) * 2] = value as u32;
                                expected[(rd + lane) * 2 + 1] = (value >> 32) as u32;
                            }
                            assert_eq!(
                                cpu.run_or_step(&mut mem, None),
                                CpuState::Normal,
                                "{arm:08x} thumb={thumb}"
                            );
                            assert_eq!(*cpu.extregs(), expected, "{arm:08x} thumb={thumb}");
                            assert_eq!(cpu.fpscr(), 0x01000000);
                            assert_eq!(cpu.cpsr(), 0xa0000010 | if thumb { CPSR_THUMB } else { 0 });
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn observed_deadspace_arm_neon_vorr_alias_executes() {
    let mut mem = TestMem::new();
    mem.words(0x1000, &[0xf2200110]);
    let mut cpu = cpu();
    cpu.extregs_mut()[0] = 0x12345678;
    cpu.extregs_mut()[1] = 0x87654321;
    assert_eq!(cpu.run_or_step(&mut mem, None), CpuState::Normal);
    assert_eq!(cpu.extregs()[0], 0x12345678);
    assert_eq!(cpu.extregs()[1], 0x87654321);
}

#[test]
fn neon_quad_odd_register_fails_without_panicking() {
    let encoding = neon_logic_encoding(0, 2, true, 1, 2, 4);
    let mut mem = TestMem::new();
    mem.words(0x1000, &[encoding]);
    let mut cpu = cpu();
    assert_eq!(
        cpu.run_or_step(&mut mem, None),
        CpuState::Error(CpuError::UnsupportedInstruction {
            pc: 0x1000,
            encoding,
            thumb: false
        })
    );
}

#[test]
fn signed_high_multiply_round_accumulate_subtract_and_aliases() {
    let values = [
        0_u32, 1, 0xffffffff, 0x80000000, 0x7fffffff, 0x40000000, 0x80000001,
    ];
    for thumb in [false, true] {
        for operation in 0..3 {
            for round in 0..2 {
                for destination in 0..4 {
                    for &left in &values {
                        for &right in &values {
                            let accumulator = 0x87654321_u32;
                            let ra = if operation == 0 { 15 } else { 3 };
                            let encoding = if thumb {
                                (if operation == 2 {
                                    0xfb600002_u32
                                } else {
                                    0xfb500002_u32
                                }) | (1 << 16)
                                    | (ra << 12)
                                    | (destination << 8)
                                    | (round << 4)
                            } else {
                                0xe7500211_u32
                                    | (destination << 16)
                                    | (ra << 12)
                                    | (round << 5)
                                    | if operation == 2 { 0xc0 } else { 0 }
                            };
                            let mut mem = TestMem::new();
                            let mut cpu = cpu();
                            if thumb {
                                mem.halves(0x1000, &[(encoding >> 16) as u16, encoding as u16]);
                            } else {
                                mem.words(0x1000, &[encoding]);
                            }
                            let status = 0xa80f0010 | if thumb { CPSR_THUMB } else { 0 };
                            cpu.set_cpsr(status);
                            cpu.regs_mut()[1] = left;
                            cpu.regs_mut()[2] = right;
                            cpu.regs_mut()[3] = accumulator;
                            // A wider signed oracle keeps wraparound and rounding independent
                            // from the production implementation's 64-bit operations.
                            let product = (left as i32 as i128) * (right as i32 as i128);
                            let acc = if operation == 0 {
                                0
                            } else {
                                (accumulator as i128) << 32
                            };
                            let result = if operation == 2 {
                                acc - product
                            } else {
                                acc + product
                            };
                            let expected = ((result + ((round as i128) << 31)) >> 32) as u32;
                            assert_eq!(
                                cpu.run_or_step(&mut mem, None),
                                CpuState::Normal,
                                "{encoding:08x} {left:08x} {right:08x}"
                            );
                            assert_eq!(
                                cpu.regs()[destination as usize],
                                expected,
                                "{encoding:08x}"
                            );
                            assert_eq!(cpu.regs()[15], 0x1004);
                            assert_eq!(cpu.cpsr(), status);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn observed_deadspace_arm_smmul_overwrites_its_source() {
    let mut mem = TestMem::new();
    mem.words(0x1000, &[0xe751f110]);
    let mut cpu = cpu();
    cpu.regs_mut()[0] = 0x7fffffff;
    cpu.regs_mut()[1] = 0x80000000;
    assert_eq!(cpu.run_or_step(&mut mem, None), CpuState::Normal);
    assert_eq!(cpu.regs()[1], 0xc0000000);
}

fn neon_scalar_encoding(
    thumb: bool,
    kind: u32,
    operation: u32,
    quad: u32,
    rd: u32,
    rn: u32,
    vm: u32,
    element_bit: u32,
) -> u32 {
    let size = if kind == 0 { 1 } else { 2 };
    let float = if kind == 2 { 1 } else { 0 };
    let arm = 0xf2800040_u32
        | (quad << 24)
        | ((rd >> 4) << 22)
        | (size << 20)
        | ((rn & 15) << 16)
        | ((rd & 15) << 12)
        | (operation << 9)
        | (float << 8)
        | ((rn >> 4) << 7)
        | (element_bit << 5)
        | vm;
    if thumb {
        (arm & 0x00ffffff) | 0xef000000 | (quad << 28)
    } else {
        arm
    }
}

#[test]
fn neon_scalar_multiply_accumulate_subtract_integer_float_and_aliases() {
    for thumb in [false, true] {
        for kind in 0..3 {
            for operation in [0, 2, 4] {
                for quad in 0..2 {
                    for banks in 0..4 {
                        for vm in [0, 7, 8, 15] {
                            for element_bit in 0..2 {
                                for alias in 0..3 {
                                    let rd = (banks & 1) * 16;
                                    let rn = if alias == 1 {
                                        rd
                                    } else {
                                        2 + ((banks >> 1) & 1) * 16
                                    };
                                    let vm = if alias == 2 { rd & 15 } else { vm };
                                    let encoding = neon_scalar_encoding(
                                        thumb,
                                        kind,
                                        operation,
                                        quad,
                                        rd,
                                        rn,
                                        vm,
                                        element_bit,
                                    );
                                    let mut mem = TestMem::new();
                                    let mut cpu = cpu();
                                    if thumb {
                                        mem.halves(
                                            0x1000,
                                            &[(encoding >> 16) as u16, encoding as u16],
                                        );
                                    } else {
                                        mem.words(0x1000, &[encoding]);
                                    }
                                    let status = 0xa0000010 | if thumb { CPSR_THUMB } else { 0 };
                                    cpu.set_cpsr(status);
                                    for (index, register) in
                                        cpu.extregs_mut().iter_mut().enumerate()
                                    {
                                        *register = if kind == 2 {
                                            [0.0_f32, 0.25, -0.5, 1.0, -2.0, 4.0, 0.5, -1.0]
                                                [index % 8]
                                                .to_bits()
                                        } else {
                                            (index as u32)
                                                .wrapping_mul(0x9e3779b9)
                                                .wrapping_add(0x31415926)
                                        };
                                    }
                                    let before: Vec<u8> = cpu
                                        .extregs()
                                        .iter()
                                        .flat_map(|word| word.to_le_bytes())
                                        .collect();
                                    let mut expected = before.clone();
                                    let width = if kind == 0 { 2 } else { 4 };
                                    let scalar_d = if kind == 0 { vm & 7 } else { vm };
                                    let scalar_lane = if kind == 0 {
                                        ((vm >> 3) & 1) | (element_bit << 1)
                                    } else {
                                        element_bit
                                    };
                                    let scalar_offset =
                                        scalar_d as usize * 8 + scalar_lane as usize * width;
                                    let read = |offset: usize| {
                                        if width == 2 {
                                            u16::from_le_bytes(
                                                before[offset..offset + 2].try_into().unwrap(),
                                            ) as u32
                                        } else {
                                            u32::from_le_bytes(
                                                before[offset..offset + 4].try_into().unwrap(),
                                            )
                                        }
                                    };
                                    let scalar = read(scalar_offset);
                                    for lane in 0..(8 * (quad as usize + 1) / width) {
                                        let source = read(rn as usize * 8 + lane * width);
                                        let destination_offset = rd as usize * 8 + lane * width;
                                        let destination = read(destination_offset);
                                        let value = if kind == 2 {
                                            let product =
                                                f32::from_bits(source) * f32::from_bits(scalar);
                                            (match operation {
                                                0 => f32::from_bits(destination) + product,
                                                2 => f32::from_bits(destination) - product,
                                                _ => product,
                                            })
                                            .to_bits()
                                        } else {
                                            let product = source.wrapping_mul(scalar);
                                            match operation {
                                                0 => destination.wrapping_add(product),
                                                2 => destination.wrapping_sub(product),
                                                _ => product,
                                            }
                                        };
                                        expected[destination_offset..destination_offset + width]
                                            .copy_from_slice(&value.to_le_bytes()[..width]);
                                    }
                                    assert_eq!(
                                        cpu.run_or_step(&mut mem, None),
                                        CpuState::Normal,
                                        "{encoding:08x}"
                                    );
                                    let actual: Vec<u8> = cpu
                                        .extregs()
                                        .iter()
                                        .flat_map(|word| word.to_le_bytes())
                                        .collect();
                                    assert_eq!(actual, expected, "{encoding:08x}");
                                    assert_eq!(cpu.cpsr(), status);
                                    assert_eq!(cpu.fpscr(), 0);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn observed_deadspace_neon_float_scalar_multiply() {
    let mut mem = TestMem::new();
    mem.words(0x1000, &[0xf3e089c0]); // VMUL.F32 q12,q8,d0[0]
    let mut cpu = cpu();
    cpu.extregs_mut()[0] = 2.0_f32.to_bits();
    for (index, value) in [2.0_f32, 3.0, -4.0, 0.5].into_iter().enumerate() {
        cpu.extregs_mut()[32 + index] = value.to_bits();
    }
    assert_eq!(cpu.run_or_step(&mut mem, None), CpuState::Normal);
    assert_eq!(
        &cpu.extregs()[48..52],
        &[
            4.0_f32.to_bits(),
            6.0_f32.to_bits(),
            (-8.0_f32).to_bits(),
            1.0_f32.to_bits()
        ]
    );
}

#[test]
fn invalid_scalar_neon_sizes_and_quad_registers_fail_distinctly() {
    let valid = neon_scalar_encoding(false, 2, 4, 1, 0, 2, 0, 0);
    for encoding in [
        valid & !0x00300000,
        valid | 0x00100000,
        (valid & !0x00300000) | 0x00100000,
        valid | 0x1000,
        valid | 0x10000,
    ] {
        let mut mem = TestMem::new();
        mem.words(0x1000, &[encoding]);
        let mut cpu = cpu();
        assert_eq!(
            cpu.run_or_step(&mut mem, None),
            CpuState::Error(CpuError::UnsupportedInstruction {
                pc: 0x1000,
                encoding,
                thumb: false
            })
        );
    }
}

#[test]
fn arm_thumb_multiply_subtract_wraps_and_snapshots_aliases() {
    for thumb in [false, true] {
        for destination in 0..4 {
            for left in [
                0_u32, 1, 0xffffffff, 0x80000000, 0x7fffffff, 0x40000000, 0x80000001,
            ] {
                for right in [
                    0_u32, 1, 0xffffffff, 0x80000000, 0x7fffffff, 0x40000000, 0x80000001,
                ] {
                    for accumulator in [0_u32, 0x87654321, 0xffffffff] {
                        let mut mem = TestMem::new();
                        let mut cpu = cpu();
                        let encoding = if thumb {
                            0xfb013012_u32 | (destination << 8)
                        } else {
                            0xe0603291_u32 | (destination << 16)
                        };
                        if thumb {
                            mem.halves(0x1000, &[(encoding >> 16) as u16, encoding as u16]);
                        } else {
                            mem.words(0x1000, &[encoding]);
                        }
                        cpu.regs_mut()[1] = left;
                        cpu.regs_mut()[2] = right;
                        cpu.regs_mut()[3] = accumulator;
                        let status = 0xf80f0010 | if thumb { CPSR_THUMB } else { 0 };
                        cpu.set_cpsr(status);
                        assert_eq!(
                            cpu.run_or_step(&mut mem, None),
                            CpuState::Normal,
                            "{encoding:08x}"
                        );
                        let expected =
                            (accumulator as i128 - (left as i128 * right as i128)) as u32;
                        assert_eq!(cpu.regs()[destination as usize], expected, "{encoding:08x}");
                        assert_eq!(cpu.cpsr(), status);
                    }
                }
            }
        }
    }
    let mut mem = TestMem::new();
    mem.words(0x1000, &[0xe0600291]); // observed MLS r0,r1,r2,r0
    let mut cpu = cpu();
    cpu.regs_mut()[0] = 11;
    cpu.regs_mut()[1] = 3;
    cpu.regs_mut()[2] = 5;
    assert_eq!(cpu.run_or_step(&mut mem, None), CpuState::Normal);
    assert_eq!(cpu.regs()[0], (-4_i32) as u32);
}

#[test]
fn invalid_vfp_and_neon_register_lists_do_not_panic_or_partially_write() {
    for encoding in [
        0xec8efa35_u32,
        0xecc1ab14,
        0xec960be2,
        0xecffea8a,
        0xed6a2aff,
        0xf44fe28e,
        0xec810a00,
        0xecc1fb04,
    ] {
        let mut mem = TestMem::new();
        mem.words(0x1000, &[encoding]);
        let mut cpu = cpu();
        cpu.regs_mut().fill(0x8000);
        cpu.regs_mut()[15] = 0x1000;
        let bytes = mem.0.clone();
        let mut registers = *cpu.regs();
        // Existing unsupported-fault contract advances PC but reports the original
        // instruction address. All general/data registers must remain unchanged.
        registers[15] = 0x1004;
        let extension = *cpu.extregs();
        assert_eq!(
            cpu.run_or_step(&mut mem, None),
            CpuState::Error(CpuError::UnsupportedInstruction {
                pc: 0x1000,
                encoding,
                thumb: false
            }),
            "{encoding:08x}"
        );
        assert_eq!(mem.0, bytes, "{encoding:08x}");
        assert_eq!(*cpu.regs(), registers, "{encoding:08x}");
        assert_eq!(*cpu.extregs(), extension, "{encoding:08x}");
    }
}

fn extension_encoding(
    thumb: bool,
    kind: usize,
    accumulate: bool,
    rotation: u32,
    destination: u32,
) -> u32 {
    let rn = if accumulate { 2 } else { 15 };
    if thumb {
        0xfa00f081_u32
            | ([2, 4, 0, 3, 5, 1][kind] << 20)
            | (rn << 16)
            | (destination << 8)
            | (rotation << 4)
    } else {
        0xe0000071_u32
            | ([0x68, 0x6a, 0x6b, 0x6c, 0x6e, 0x6f][kind] << 20)
            | (rn << 16)
            | (destination << 12)
            | (rotation << 10)
    }
}

#[test]
fn arm_thumb_extensions_rotate_sign_and_accumulate_without_halfword_carry() {
    for thumb in [false, true] {
        for kind in 0..6 {
            for accumulate in [false, true] {
                for rotation in 0..4 {
                    for destination in 0..3 {
                        for source in [
                            0_u32, 0xffffffff, 0x80ff7f01, 0x0100ff80, 0x800180ff, 0x7f807f80,
                            0xdeadbeef,
                        ] {
                            for addend in [0_u32, 0xffff0001, 0x7fff8000] {
                                let encoding = extension_encoding(
                                    thumb,
                                    kind,
                                    accumulate,
                                    rotation,
                                    destination,
                                );
                                let mut mem = TestMem::new();
                                let mut cpu = cpu();
                                if thumb {
                                    mem.halves(0x1000, &[(encoding >> 16) as u16, encoding as u16]);
                                } else {
                                    mem.words(0x1000, &[encoding]);
                                }
                                cpu.regs_mut()[1] = source;
                                cpu.regs_mut()[2] = addend;
                                let status = 0xf80f0010 | if thumb { CPSR_THUMB } else { 0 };
                                cpu.set_cpsr(status);
                                let bytes = source.rotate_right(rotation * 8).to_le_bytes();
                                let unsigned = kind >= 3;
                                let acc = if accumulate { addend } else { 0 };
                                let expected = if kind % 3 == 0 {
                                    let extend = |v: u8| {
                                        if unsigned {
                                            v as u16
                                        } else {
                                            v as i8 as i16 as u16
                                        }
                                    };
                                    let low = extend(bytes[0]).wrapping_add(acc as u16);
                                    let high = extend(bytes[2]).wrapping_add((acc >> 16) as u16);
                                    low as u32 | ((high as u32) << 16)
                                } else {
                                    let value = if kind % 3 == 1 {
                                        if unsigned {
                                            bytes[0] as u32
                                        } else {
                                            bytes[0] as i8 as i32 as u32
                                        }
                                    } else if unsigned {
                                        u16::from_le_bytes([bytes[0], bytes[1]]) as u32
                                    } else {
                                        i16::from_le_bytes([bytes[0], bytes[1]]) as i32 as u32
                                    };
                                    value.wrapping_add(acc)
                                };
                                assert_eq!(
                                    cpu.run_or_step(&mut mem, None),
                                    CpuState::Normal,
                                    "{encoding:08x}"
                                );
                                assert_eq!(
                                    cpu.regs()[destination as usize],
                                    expected,
                                    "{encoding:08x}"
                                );
                                assert_eq!(cpu.cpsr(), status);
                            }
                        }
                    }
                }
            }
        }
    }
    let mut mem = TestMem::new();
    mem.words(0x1000, &[0xe6cf2073]); // observed UXTB16 r2,r3
    let mut cpu = cpu();
    cpu.regs_mut()[3] = 0x80ff7f01;
    assert_eq!(cpu.run_or_step(&mut mem, None), CpuState::Normal);
    assert_eq!(cpu.regs()[2], 0x00ff0001);
}

fn vector_fp_encoding(thumb: bool, operation: usize, quad: u32, rd: u32, rn: u32, rm: u32) -> u32 {
    let arm = [
        0xf2000d00_u32,
        0xf2200d00,
        0xf2000d10,
        0xf2200d10,
        0xf3000d10,
        0xf2000f00,
        0xf2200f00,
    ][operation]
        | ((rd >> 4) << 22)
        | ((rd & 15) << 12)
        | ((rn >> 4) << 7)
        | ((rn & 15) << 16)
        | ((rm >> 4) << 5)
        | (rm & 15)
        | (quad << 6);
    if thumb {
        (arm & 0x00ffffff) | 0xef000000 | (((arm >> 24) & 1) << 28)
    } else {
        arm
    }
}
fn vector_fp_initial(index: usize, sample: u32) -> u32 {
    let mixed = (index as u32).wrapping_mul(0x9e3779b9);
    let index = (((mixed ^ (mixed >> 16)).wrapping_add(sample)) & 7) as usize;
    [0.0_f32, -0.0, 0.25, -0.5, 1.0, -2.0, 4.0, -1.0][index].to_bits()
}

#[test]
fn neon_vector_finite_float_arithmetic_accumulation_and_signed_zero() {
    for thumb in [false, true] {
        for operation in 0..7 {
            for quad in 0..2 {
                for banks in 0..8 {
                    for alias in 0..3 {
                        for sample in 0..8 {
                            let rd = (banks & 1) * 16;
                            let rn = if alias == 1 {
                                rd
                            } else {
                                2 + ((banks >> 1) & 1) * 16
                            };
                            let rm = if alias == 2 {
                                rd
                            } else {
                                4 + ((banks >> 2) & 1) * 16
                            };
                            let encoding = vector_fp_encoding(thumb, operation, quad, rd, rn, rm);
                            let mut mem = TestMem::new();
                            let mut cpu = cpu();
                            if thumb {
                                mem.halves(0x1000, &[(encoding >> 16) as u16, encoding as u16]);
                            } else {
                                mem.words(0x1000, &[encoding]);
                            }
                            let status = 0xa0000010 | if thumb { CPSR_THUMB } else { 0 };
                            cpu.set_cpsr(status);
                            for (index, register) in cpu.extregs_mut().iter_mut().enumerate() {
                                *register = vector_fp_initial(index, sample);
                            }
                            let before = *cpu.extregs();
                            let mut expected = before;
                            for lane in 0..if quad == 1 { 4 } else { 2 } {
                                let a = f32::from_bits(before[(rn * 2 + lane) as usize]);
                                let b = f32::from_bits(before[(rm * 2 + lane) as usize]);
                                let d = f32::from_bits(before[(rd * 2 + lane) as usize]);
                                let value = match operation {
                                    0 => a + b,
                                    1 => a - b,
                                    2 => d + (a * b),
                                    3 => d - (a * b),
                                    4 => a * b,
                                    5 => {
                                        if a == 0.0 && b == 0.0 {
                                            f32::from_bits(a.to_bits() & b.to_bits())
                                        } else if a > b {
                                            a
                                        } else {
                                            b
                                        }
                                    }
                                    _ => {
                                        if a == 0.0 && b == 0.0 {
                                            f32::from_bits(a.to_bits() | b.to_bits())
                                        } else if a < b {
                                            a
                                        } else {
                                            b
                                        }
                                    }
                                };
                                expected[(rd * 2 + lane) as usize] = value.to_bits();
                            }
                            assert_eq!(
                                cpu.run_or_step(&mut mem, None),
                                CpuState::Normal,
                                "{encoding:08x}"
                            );
                            assert_eq!(*cpu.extregs(), expected, "{encoding:08x} sample {sample}");
                            assert_eq!(cpu.cpsr(), status);
                            assert_eq!(cpu.fpscr(), 0);
                        }
                    }
                }
            }
        }
    }
    let mut mem = TestMem::new();
    mem.words(0x1000, &[0xf2084d5c]); // observed VMLA.F32 q2,q4,q6
    let mut cpu = cpu();
    for (index, value) in [1.0_f32, 2.0, 3.0, 4.0].into_iter().enumerate() {
        cpu.extregs_mut()[8 + index] = 2.0_f32.to_bits();
        cpu.extregs_mut()[16 + index] = value.to_bits();
        cpu.extregs_mut()[24 + index] = [-1.0_f32, 0.5, 2.0, -2.0][index].to_bits();
    }
    assert_eq!(cpu.run_or_step(&mut mem, None), CpuState::Normal);
    assert_eq!(
        &cpu.extregs()[8..12],
        &[
            1.0_f32.to_bits(),
            3.0_f32.to_bits(),
            8.0_f32.to_bits(),
            (-6.0_f32).to_bits()
        ]
    );
}

#[test]
fn vector_float_does_not_accept_half_precision_or_odd_quad_registers() {
    let valid = vector_fp_encoding(false, 0, 1, 0, 2, 4);
    for encoding in [
        valid | 0x00100000,
        valid | 0x1000,
        valid | 0x10000,
        valid | 1,
    ] {
        let mut mem = TestMem::new();
        mem.words(0x1000, &[encoding]);
        let mut cpu = cpu();
        assert_eq!(
            cpu.run_or_step(&mut mem, None),
            CpuState::Error(CpuError::UnsupportedInstruction {
                pc: 0x1000,
                encoding,
                thumb: false
            })
        );
    }
}

fn neon_duplicate_encoding(thumb: bool, quad: u32, rd: u32, rm: u32, immediate: u32) -> u32 {
    let arm = 0xf3b00c00_u32
        | ((rd >> 4) << 22)
        | ((rd & 15) << 12)
        | ((rm >> 4) << 5)
        | (rm & 15)
        | (quad << 6)
        | (immediate << 16);
    if thumb {
        (arm & 0x00ffffff) | 0xff000000
    } else {
        arm
    }
}

#[test]
fn neon_duplicate_byte_halfword_word_lanes_and_source_aliases() {
    for thumb in [false, true] {
        for quad in 0..2 {
            for bank in 0..4 {
                for source_bank in 0..2 {
                    for alias in 0..3 {
                        for immediate in [1, 3, 5, 7, 9, 11, 13, 15, 2, 6, 10, 14, 4, 12] {
                            for sample in 0..3 {
                                let rd = (bank & 1) * 16
                                    + ((bank >> 1) & 1) * if quad == 1 { 2 } else { 1 };
                                let rm = match alias {
                                    1 => rd,
                                    2 => rd + 1,
                                    _ => 4 + source_bank * 16,
                                };
                                let encoding =
                                    neon_duplicate_encoding(thumb, quad, rd, rm, immediate);
                                let mut mem = TestMem::new();
                                let mut cpu = cpu();
                                if thumb {
                                    mem.halves(0x1000, &[(encoding >> 16) as u16, encoding as u16]);
                                } else {
                                    mem.words(0x1000, &[encoding]);
                                }
                                let status = 0xa0000010 | if thumb { CPSR_THUMB } else { 0 };
                                cpu.set_cpsr(status);
                                for (index, register) in cpu.extregs_mut().iter_mut().enumerate() {
                                    *register = (index as u32)
                                        .wrapping_mul(0x9e3779b9)
                                        .wrapping_add(0x31415926 + sample);
                                }
                                let before: Vec<u8> = cpu
                                    .extregs()
                                    .iter()
                                    .flat_map(|word| word.to_le_bytes())
                                    .collect();
                                let mut expected = before.clone();
                                let width = if immediate & 1 != 0 {
                                    1
                                } else if immediate & 2 != 0 {
                                    2
                                } else {
                                    4
                                };
                                let lane = immediate as usize / (width * 2);
                                let scalar = &before[rm as usize * 8 + lane * width
                                    ..rm as usize * 8 + (lane + 1) * width];
                                let start = rd as usize * 8;
                                for bytes in expected[start..start + 8 * (quad as usize + 1)]
                                    .chunks_exact_mut(width)
                                {
                                    bytes.copy_from_slice(scalar);
                                }
                                assert_eq!(
                                    cpu.run_or_step(&mut mem, None),
                                    CpuState::Normal,
                                    "{encoding:08x}"
                                );
                                let actual: Vec<u8> = cpu
                                    .extregs()
                                    .iter()
                                    .flat_map(|word| word.to_le_bytes())
                                    .collect();
                                assert_eq!(actual, expected, "{encoding:08x}");
                                assert_eq!(cpu.cpsr(), status);
                                assert_eq!(cpu.fpscr(), 0);
                            }
                        }
                    }
                }
            }
        }
    }
    let mut mem = TestMem::new();
    mem.words(0x1000, &[0xf3b44c04]);
    let mut cpu = cpu();
    cpu.extregs_mut()[8] = 0x12345678;
    cpu.extregs_mut()[9] = 0xabcdef01;
    assert_eq!(cpu.run_or_step(&mut mem, None), CpuState::Normal);
    assert_eq!(&cpu.extregs()[8..10], &[0x12345678, 0x12345678]);
}

#[test]
fn neon_duplicate_invalid_width_or_quad_register_is_rejected() {
    for encoding in [
        neon_duplicate_encoding(false, 0, 0, 2, 0),
        neon_duplicate_encoding(false, 0, 0, 2, 8),
        neon_duplicate_encoding(false, 1, 1, 2, 4),
    ] {
        let mut mem = TestMem::new();
        mem.words(0x1000, &[encoding]);
        let mut cpu = cpu();
        assert_eq!(
            cpu.run_or_step(&mut mem, None),
            CpuState::Error(CpuError::UnsupportedInstruction {
                pc: 0x1000,
                encoding,
                thumb: false
            })
        );
    }
}

#[test]
fn fault_details_preserve_instruction_location_and_reset_on_next_nonempty_run() {
    let mut mem = TestMem::new();
    let mut cpu = cpu();
    mem.words(0x1000, &[0xe5910000]);
    cpu.regs_mut()[1] = 0xfffe;
    assert_eq!(
        cpu.run_or_step(&mut mem, None),
        CpuState::Error(CpuError::MemoryError)
    );
    let expected = touchhle_arm_interpreter::FaultInfo {
        pc: 0x1000,
        encoding: Some(0xe5910000),
        thumb: false,
    };
    assert_eq!(cpu.last_fault(), Some(expected));
    assert_eq!(run(&mut cpu, &mut mem, 0).0, CpuState::Normal);
    assert_eq!(cpu.last_fault(), Some(expected));
    cpu.regs_mut()[15] = 0;
    cpu.set_cpsr(0x30);
    assert_eq!(
        cpu.run_or_step(&mut mem, None),
        CpuState::Error(CpuError::MemoryError)
    );
    assert_eq!(
        cpu.last_fault(),
        Some(touchhle_arm_interpreter::FaultInfo {
            pc: 0,
            encoding: None,
            thumb: true
        })
    );
    cpu.regs_mut()[15] = 0xfffe;
    mem.halves(0xfffe, &[0xf000]);
    assert_eq!(
        cpu.run_or_step(&mut mem, None),
        CpuState::Error(CpuError::MemoryError)
    );
    assert_eq!(
        cpu.last_fault(),
        Some(touchhle_arm_interpreter::FaultInfo {
            pc: 0xfffe,
            encoding: None,
            thumb: true
        })
    );
    cpu.set_cpsr(0x10);
    cpu.regs_mut()[15] = 0x1000;
    mem.words(0x1000, &[0xef000002]);
    assert_eq!(cpu.run_or_step(&mut mem, None), CpuState::Svc(2));
    assert_eq!(cpu.last_fault(), None);
}

fn halfword_multiply_encoding(kind: usize, destination: u32, n_top: u32, m_top: u32) -> u32 {
    let base = [
        0xe1600080_u32,
        0xe1000080,
        0xe1400080,
        0xe12000a0,
        0xe1200080,
    ][kind];
    let accumulator = if matches!(kind, 1 | 2 | 4) { 3 } else { 0 };
    base | (destination << 16)
        | (accumulator << 12)
        | (2 << 8)
        | 1
        | (m_top << 6)
        | if kind <= 2 { n_top << 5 } else { 0 }
}

#[test]
fn arm_signed_halfword_multiply_accumulation_aliases_and_sticky_q() {
    let values = [
        0_u32, 1, 0xffffffff, 0x80008000, 0x7fff7fff, 0x80017fff, 0xdeadbeef,
    ];
    for kind in 0..5 {
        for n_top in 0..if kind <= 2 { 2 } else { 1 } {
            for m_top in 0..2 {
                for destination in 0..3 {
                    for q in [0, 0x08000000] {
                        for left in values {
                            for right in values {
                                for accumulator in [0_u32, 0x7fffffff, 0x80000000] {
                                    let encoding =
                                        halfword_multiply_encoding(kind, destination, n_top, m_top);
                                    let mut mem = TestMem::new();
                                    mem.words(0x1000, &[encoding]);
                                    let mut cpu = cpu();
                                    let status = 0xa00f0010 | q;
                                    cpu.set_cpsr(status);
                                    cpu.regs_mut()[1] = left;
                                    cpu.regs_mut()[2] = right;
                                    cpu.regs_mut()[3] = accumulator;
                                    let before = *cpu.regs();
                                    let mut expected = before;
                                    let mut flags = status;
                                    let left = before[1];
                                    let right = before[2];
                                    let half = |value: u32, top: u32| {
                                        ((value >> (top * 16)) as u16) as i16 as i128
                                    };
                                    let product = if kind >= 3 {
                                        (left as i32 as i128) * half(right, m_top)
                                    } else {
                                        half(left, n_top) * half(right, m_top)
                                    };
                                    if kind == 2 {
                                        let add = ((before[destination as usize] as u64) << 32)
                                            | (before[3] as u64);
                                        let result = (add as i128 + product) as u64;
                                        expected[3] = result as u32;
                                        expected[destination as usize] = (result >> 32) as u32;
                                    } else {
                                        let product =
                                            if kind >= 3 { product >> 16 } else { product };
                                        let result = product
                                            + if matches!(kind, 1 | 4) {
                                                before[3] as i32 as i128
                                            } else {
                                                0
                                            };
                                        expected[destination as usize] = result as u32;
                                        if matches!(kind, 1 | 4)
                                            && (result < i32::MIN as i128
                                                || result > i32::MAX as i128)
                                        {
                                            flags |= 0x08000000;
                                        }
                                    }
                                    expected[15] = 0x1004;
                                    assert_eq!(
                                        cpu.run_or_step(&mut mem, None),
                                        CpuState::Normal,
                                        "{encoding:08x}"
                                    );
                                    assert_eq!(*cpu.regs(), expected, "{encoding:08x}");
                                    assert_eq!(cpu.cpsr(), flags, "{encoding:08x}");
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    let mut mem = TestMem::new();
    mem.words(0x1000, &[0xe1600382]); // SMULBB r0,r2,r3 near failing vertex loader
    let mut cpu = cpu();
    cpu.regs_mut()[2] = 0x00020003;
    cpu.regs_mut()[3] = 0xfffe0004;
    assert_eq!(cpu.run_or_step(&mut mem, None), CpuState::Normal);
    assert_eq!(cpu.regs()[0], 12);
}

#[test]
fn misc_space_without_s_bit_cannot_silently_execute_as_compare() {
    for encoding in [0xe1000000_u32, 0xe1200000, 0xe1400000, 0xe1600000] {
        let mut mem = TestMem::new();
        mem.words(0x1000, &[encoding]);
        let mut cpu = cpu();
        cpu.set_cpsr(0xa0000010);
        assert_eq!(
            cpu.run_or_step(&mut mem, None),
            CpuState::Error(CpuError::UnsupportedInstruction {
                pc: 0x1000,
                encoding,
                thumb: false
            })
        );
        assert_eq!(cpu.cpsr(), 0xa0000010);
    }
}

fn neon_lane_memory_encoding(
    thumb: bool,
    load: u32,
    size: u32,
    elements: u32,
    stride: u32,
    lane: u32,
    rd: u32,
    writeback: u32,
) -> u32 {
    let index_align = (lane << (size + 1)) | if stride == 2 { 1 << size } else { 0 };
    let arm = 0xf4800000_u32
        | (load << 21)
        | ((rd >> 4) << 22)
        | (1 << 16)
        | ((rd & 15) << 12)
        | (size << 10)
        | ((elements - 1) << 8)
        | (index_align << 4)
        | writeback;
    if thumb {
        (arm & 0x00ffffff) | 0xf9000000
    } else {
        arm
    }
}

#[test]
fn neon_single_element_structure_memory_preserves_other_lanes_and_writeback() {
    for thumb in [false, true] {
        for load in 0..2 {
            for size in 0..3 {
                for elements in 1..5 {
                    for stride in 1..if size == 0 || elements == 1 { 2 } else { 3 } {
                        for lane in 0..(8 >> size) {
                            for rd in [0, 1, 16, 17] {
                                for writeback in [15, 13, 2, 1] {
                                    let encoding = neon_lane_memory_encoding(
                                        thumb, load, size, elements, stride, lane, rd, writeback,
                                    );
                                    let mut mem = TestMem::new();
                                    let mut cpu = cpu();
                                    if thumb {
                                        mem.halves(
                                            0x1000,
                                            &[(encoding >> 16) as u16, encoding as u16],
                                        );
                                    } else {
                                        mem.words(0x1000, &[encoding]);
                                    }
                                    cpu.set_cpsr(0xa0000010 | if thumb { CPSR_THUMB } else { 0 });
                                    cpu.regs_mut()[1] = 0x2000;
                                    cpu.regs_mut()[2] = 0x24;
                                    for (index, register) in
                                        cpu.extregs_mut().iter_mut().enumerate()
                                    {
                                        *register = (index as u32)
                                            .wrapping_mul(0x9e3779b9)
                                            .wrapping_add(0x31415926);
                                    }
                                    for index in 0..64 {
                                        mem.0[0x2000 + index] =
                                            (index as u8).wrapping_mul(17).wrapping_add(0x3c);
                                    }
                                    let mut expected_mem = mem.0.clone();
                                    let before: Vec<u8> = cpu
                                        .extregs()
                                        .iter()
                                        .flat_map(|word| word.to_le_bytes())
                                        .collect();
                                    let mut expected = before.clone();
                                    let width = 1_usize << size;
                                    for element in 0..elements {
                                        let register_offset = (rd + element * stride) as usize * 8
                                            + lane as usize * width;
                                        let memory_offset = 0x2000 + element as usize * width;
                                        if load == 1 {
                                            expected[register_offset..register_offset + width]
                                                .copy_from_slice(
                                                    &expected_mem
                                                        [memory_offset..memory_offset + width],
                                                );
                                        } else {
                                            expected_mem[memory_offset..memory_offset + width]
                                                .copy_from_slice(
                                                    &before
                                                        [register_offset..register_offset + width],
                                                );
                                        }
                                    }
                                    assert_eq!(
                                        cpu.run_or_step(&mut mem, None),
                                        CpuState::Normal,
                                        "{encoding:08x}"
                                    );
                                    let actual: Vec<u8> = cpu
                                        .extregs()
                                        .iter()
                                        .flat_map(|word| word.to_le_bytes())
                                        .collect();
                                    assert_eq!(actual, expected, "{encoding:08x}");
                                    assert!(
                                        mem.0.iter().zip(&expected_mem).all(|(a, b)| a == b),
                                        "memory changed incorrectly for {encoding:08x}"
                                    );
                                    let base = match writeback {
                                        15 => 0x2000,
                                        13 => 0x2000 + (elements << size),
                                        1 => 0x4000,
                                        _ => 0x2024,
                                    };
                                    assert_eq!(cpu.regs()[1], base, "{encoding:08x}");
                                    assert_eq!(
                                        cpu.cpsr(),
                                        0xa0000010 | if thumb { CPSR_THUMB } else { 0 }
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    let mut mem = TestMem::new();
    mem.words(0x1000, &[0xf4ab280f]);
    mem.words(0x2000, &[0x3f800000]);
    let mut cpu = cpu();
    cpu.regs_mut()[11] = 0x2000;
    cpu.extregs_mut()[5] = 0xdeadbeef;
    assert_eq!(cpu.run_or_step(&mut mem, None), CpuState::Normal);
    assert_eq!(&cpu.extregs()[4..6], &[0x3f800000, 0xdeadbeef]);
}

#[test]
fn neon_single_element_rejects_invalid_encoding_before_any_transfer() {
    let valid = neon_lane_memory_encoding(false, 1, 2, 1, 1, 0, 0, 15);
    for encoding in [
        valid | 0x10,
        valid | 0x20,
        valid | 0x40,
        (valid & !0x00ff0000) | 0x000f0000,
        neon_lane_memory_encoding(false, 1, 0, 4, 1, 0, 31, 13),
    ] {
        let mut mem = TestMem::new();
        mem.words(0x1000, &[encoding]);
        let mut cpu = cpu();
        cpu.regs_mut()[1] = 0x2000;
        let before = *cpu.extregs();
        assert_eq!(
            cpu.run_or_step(&mut mem, None),
            CpuState::Error(CpuError::UnsupportedInstruction {
                pc: 0x1000,
                encoding,
                thumb: false
            }),
            "{encoding:08x}"
        );
        assert_eq!(*cpu.extregs(), before);
        assert_eq!(cpu.regs()[1], 0x2000);
    }
    // Highest D register and explicit 32-bit alignment remain valid.
    let encoding = neon_lane_memory_encoding(false, 1, 2, 1, 1, 1, 31, 15) | 0x30;
    let mut mem = TestMem::new();
    mem.words(0x1000, &[encoding]);
    mem.words(0x2000, &[0x12345678]);
    let mut cpu = cpu();
    cpu.regs_mut()[1] = 0x2000;
    assert_eq!(cpu.run_or_step(&mut mem, None), CpuState::Normal);
    assert_eq!(cpu.extregs()[63], 0x12345678);
}
