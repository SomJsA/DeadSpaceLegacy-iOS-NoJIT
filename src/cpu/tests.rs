//! Controlled backend comparisons. Each backend owns a separate guest memory.
use super::{Cpu, CpuContext, CpuState};
use crate::mem::{Mem, Ptr};

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    regs: [u32; 16],
    extregs: [u32; 64],
    cpsr: u32,
    fpscr: u32,
}
fn snapshot(cpu: &Cpu) -> Snapshot {
    Snapshot {
        regs: *cpu.regs(),
        extregs: *cpu.extregs(),
        cpsr: cpu.cpsr(),
        fpscr: cpu.fpscr(),
    }
}
struct Pair {
    jit: Cpu,
    interpreter: Cpu,
    jit_mem: Mem,
    interpreter_mem: Mem,
}
impl Pair {
    fn new() -> Self {
        Self {
            jit: Cpu::with_backend(None, "dynarmic"),
            interpreter: Cpu::with_backend(None, "interpreter"),
            jit_mem: Mem::new(),
            interpreter_mem: Mem::new(),
        }
    }
    fn reset(&mut self, thumb: bool, flags: u32) {
        for cpu in [&mut self.jit, &mut self.interpreter] {
            cpu.regs_mut().fill(0);
            cpu.extregs_mut().fill(0);
            cpu.regs_mut()[15] = 0x1000;
            cpu.regs_mut()[13] = 0x8000;
            cpu.set_cpsr(0x10 | flags | if thumb { 0x20 } else { 0 });
            cpu.set_fpscr(0);
        }
    }
    fn register(&mut self, index: usize, value: u32) {
        self.jit.regs_mut()[index] = value;
        self.interpreter.regs_mut()[index] = value;
    }
    fn code(&mut self, bytes: &[u8]) {
        for mem in [&mut self.jit_mem, &mut self.interpreter_mem] {
            mem.bytes_at_mut(Ptr::from_bits(0x1000), bytes.len() as u32)
                .copy_from_slice(bytes);
        }
        self.jit.invalidate_cache_range(0x1000, bytes.len() as u32);
        self.interpreter
            .invalidate_cache_range(0x1000, bytes.len() as u32);
    }
    fn step(&mut self, description: &str) -> CpuState {
        let actual = self
            .interpreter
            .run_or_step(&mut self.interpreter_mem, None);
        let reference = self.jit.run_or_step(&mut self.jit_mem, None);
        assert_eq!(actual, reference, "halt state: {description}");
        assert_eq!(
            snapshot(&self.interpreter),
            snapshot(&self.jit),
            "CPU state: {description}"
        );
        actual
    }
    fn compare_data(&self, start: u32, length: u32) {
        assert_eq!(
            self.interpreter_mem
                .get_bytes_fallible(Ptr::from_bits(start), length),
            self.jit_mem
                .get_bytes_fallible(Ptr::from_bits(start), length),
        );
    }
}
fn next(seed: &mut u32) -> u32 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 17;
    *seed ^= *seed << 5;
    *seed
}

#[test]
fn arm_integer_flags_match_across_deterministic_inputs() {
    let operations: [u32; 15] = [
        0xe0910002, 0xe0b10002, 0xe0510002, 0xe0d10002, 0xe0f10002, 0xe0110002, 0xe0310002,
        0xe1910002, 0xe1d10002, 0xe1f00002, 0xe1b00002, 0xe1510002, 0xe1710002, 0xe1110002,
        0xe1310002,
    ];
    let mut pair = Pair::new();
    let mut seed = 0x31415926;
    for operation in operations {
        pair.code(&operation.to_le_bytes());
        for sample in 0..128 {
            pair.reset(false, next(&mut seed) & 0xf0000000);
            pair.register(0, next(&mut seed));
            pair.register(1, next(&mut seed));
            pair.register(2, next(&mut seed));
            assert_eq!(
                pair.step(&format!("A32 {operation:08x} sample {sample}")),
                CpuState::Normal
            );
        }
    }
}

#[test]
fn thumb_integer_flags_match_across_deterministic_inputs() {
    let operations: [u16; 14] = [
        0x1888, 0x1a88, 0x4010, 0x4050, 0x4150, 0x4190, 0x4310, 0x4350, 0x4390, 0x43d0, 0x4090,
        0x40d0, 0x4110, 0x41d0,
    ];
    let mut pair = Pair::new();
    let mut seed = 0x27182818;
    for operation in operations {
        pair.code(&operation.to_le_bytes());
        for sample in 0..128 {
            pair.reset(true, next(&mut seed) & 0xf0000000);
            pair.register(0, next(&mut seed));
            pair.register(1, next(&mut seed));
            pair.register(2, next(&mut seed));
            assert_eq!(
                pair.step(&format!("T16 {operation:04x} sample {sample}")),
                CpuState::Normal
            );
        }
    }
}

#[test]
fn arm_memory_writes_and_svc_match() {
    let mut pair = Pair::new();
    pair.reset(false, 0);
    let bytes: Vec<u8> = [0xe5810000_u32, 0xe5912000, 0xef000007]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    pair.code(&bytes);
    pair.register(0, 0x87654321);
    pair.register(1, 0x4000);
    for _ in 0..2 {
        assert_eq!(pair.step("memory roundtrip"), CpuState::Normal);
    }
    pair.compare_data(0x4000, 4);
    assert_eq!(pair.step("host SVC"), CpuState::Svc(7));
}

#[test]
fn thumb_it_and_interworking_match() {
    for zero in [0, 0x40000000] {
        let mut pair = Pair::new();
        pair.reset(true, zero);
        let bytes: Vec<u8> = [0xbf08_u16, 0x2001, 0x4708]
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect();
        pair.code(&bytes);
        pair.register(1, 0x2000);
        for _ in 0..3 {
            assert_eq!(pair.step("IT and BX"), CpuState::Normal);
        }
    }
}

#[test]
fn vfp_normal_finite_arithmetic_matches() {
    let mut pair = Pair::new();
    for operation in [0xee300a00_u32, 0xee300a40, 0xee200a00, 0xee800a00] {
        pair.code(&operation.to_le_bytes());
        for value in [1.25_f32, -3.5, 0.125, 123.0] {
            pair.reset(false, 0);
            pair.jit.extregs_mut()[0] = value.to_bits();
            pair.interpreter.extregs_mut()[0] = value.to_bits();
            assert_eq!(
                pair.step(&format!("VFP {operation:08x} value {value}")),
                CpuState::Normal
            );
        }
    }
}

#[test]
fn context_roundtrip_and_zero_budget_match() {
    let mut pair = Pair::new();
    pair.reset(false, 0xa0000000);
    pair.code(&0xe2800001_u32.to_le_bytes());
    pair.register(0, 42);
    for cpu in [&mut pair.jit, &mut pair.interpreter] {
        cpu.extregs_mut()[63] = 0x12345678;
        cpu.set_fpscr(0x01000000);
        let before = snapshot(cpu);
        let mut context = CpuContext::new();
        cpu.swap_context(&mut context);
        cpu.swap_context(&mut context);
        assert_eq!(snapshot(cpu), before);
    }
    let mut budget = 0;
    let before = snapshot(&pair.jit);
    assert_eq!(
        pair.jit.run_or_step(&mut pair.jit_mem, Some(&mut budget)),
        CpuState::Normal
    );
    assert_eq!(snapshot(&pair.jit), before);
    assert_eq!(
        pair.interpreter
            .run_or_step(&mut pair.interpreter_mem, Some(&mut budget)),
        CpuState::Normal
    );
    assert_eq!(snapshot(&pair.interpreter), before);
}

#[test]
fn app_null_segment_data_policy_matches_with_direct_memory_enabled() {
    for backend in ["dynarmic", "interpreter"] {
        let mut mem = Mem::new();
        mem.set_null_segment_size(0x1000);
        let bytes: Vec<u8> = [0xe5910000_u32, 0xe5812000, 0xe5913000]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        mem.bytes_at_mut(Ptr::from_bits(0x1000), bytes.len() as u32)
            .copy_from_slice(&bytes);
        let mut cpu = Cpu::with_backend(Some(&mut mem), backend);
        cpu.regs_mut()[15] = 0x1000;
        cpu.regs_mut()[1] = 0;
        cpu.regs_mut()[2] = 0x12345678;
        cpu.set_cpsr(0x10);
        for _ in 0..3 {
            assert_eq!(
                cpu.run_or_step(&mut mem, None),
                CpuState::Normal,
                "{backend}"
            );
        }
        assert_eq!(cpu.regs()[0], 0, "{backend}");
        assert_eq!(cpu.regs()[3], 0, "{backend}");
    }
}

#[test]
fn neon_logic_arm_thumb_high_register_and_alias_cases_match() {
    let mut pair = Pair::new();
    for thumb in [false, true] {
        for u in 0..=1 {
            for operation in 0..4 {
                for quad in [0, 1] {
                    for banks in 0_u32..8 {
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
                            let mut encoding = 0xf2000110_u32
                                | (u << 24)
                                | (operation << 20)
                                | (quad << 6)
                                | ((rd >> 4) << 22)
                                | ((rd & 15) << 12)
                                | ((rn >> 4) << 7)
                                | ((rn & 15) << 16)
                                | ((rm >> 4) << 5)
                                | (rm & 15);
                            let bytes = if thumb {
                                encoding = (encoding & 0x00ffffff) | 0xef000000 | (u << 28);
                                [(encoding >> 16) as u16, encoding as u16]
                                    .into_iter()
                                    .flat_map(u16::to_le_bytes)
                                    .collect::<Vec<_>>()
                            } else {
                                encoding.to_le_bytes().to_vec()
                            };
                            pair.code(&bytes);
                            pair.reset(thumb, 0xa0000000);
                            for cpu in [&mut pair.jit, &mut pair.interpreter] {
                                for (index, register) in cpu.extregs_mut().iter_mut().enumerate() {
                                    *register = (index as u32)
                                        .wrapping_mul(0x9e3779b9)
                                        .wrapping_add(0x31415926);
                                }
                            }
                            assert_eq!(
                                pair.step(&format!("NEON {encoding:08x}")),
                                CpuState::Normal
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn signed_high_multiply_variants_match_across_backends() {
    let mut pair = Pair::new();
    let mut seed = 0x78563412;
    for thumb in [false, true] {
        for operation in 0..3 {
            for round in 0..2 {
                for destination in 0..4 {
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
                    let bytes = if thumb {
                        [(encoding >> 16) as u16, encoding as u16]
                            .into_iter()
                            .flat_map(u16::to_le_bytes)
                            .collect::<Vec<_>>()
                    } else {
                        encoding.to_le_bytes().to_vec()
                    };
                    pair.code(&bytes);
                    for sample in 0..64 {
                        pair.reset(thumb, next(&mut seed) & 0xf80f0000);
                        for register in 0..4 {
                            pair.register(register, next(&mut seed));
                        }
                        assert_eq!(
                            pair.step(&format!("high multiply {encoding:08x}, sample {sample}")),
                            CpuState::Normal
                        );
                    }
                }
            }
        }
    }
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
fn neon_scalar_integer_float_and_alias_cases_match() {
    let mut pair = Pair::new();
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
                                    let bytes = if thumb {
                                        [(encoding >> 16) as u16, encoding as u16]
                                            .into_iter()
                                            .flat_map(u16::to_le_bytes)
                                            .collect::<Vec<_>>()
                                    } else {
                                        encoding.to_le_bytes().to_vec()
                                    };
                                    pair.code(&bytes);
                                    pair.reset(thumb, 0xa0000000);
                                    for cpu in [&mut pair.jit, &mut pair.interpreter] {
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
                                    }
                                    assert_eq!(
                                        pair.step(&format!("NEON scalar {encoding:08x}")),
                                        CpuState::Normal
                                    );
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
fn arm_thumb_mls_aliases_match() {
    let mut pair = Pair::new();
    let mut seed = 0x1234abcd;
    for thumb in [false, true] {
        for destination in 0..4 {
            let encoding = if thumb {
                0xfb013012_u32 | (destination << 8)
            } else {
                0xe0603291_u32 | (destination << 16)
            };
            let bytes = if thumb {
                [(encoding >> 16) as u16, encoding as u16]
                    .into_iter()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>()
            } else {
                encoding.to_le_bytes().to_vec()
            };
            pair.code(&bytes);
            for sample in 0..64 {
                pair.reset(thumb, next(&mut seed) & 0xf80f0000);
                for register in 0..4 {
                    pair.register(register, next(&mut seed));
                }
                assert_eq!(
                    pair.step(&format!("MLS {encoding:08x} sample {sample}")),
                    CpuState::Normal
                );
            }
        }
    }
}

#[test]
fn vfp_register_list_upper_bounds_match() {
    let mut pair = Pair::new();
    // Last single, last double, and the largest supported double list ending D31.
    for encoding in [
        0xecc1fa01_u32,
        0xecc1fb02,
        0xecc10b20,
        0xec8f0a01,
        0xec9f0a01,
    ] {
        let bytes: Vec<_> = [encoding, 0xe320f000, 0xdeadbeef]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        pair.code(&bytes);
        pair.reset(false, 0xa0000000);
        pair.register(1, 0x2000);
        for cpu in [&mut pair.jit, &mut pair.interpreter] {
            for (index, register) in cpu.extregs_mut().iter_mut().enumerate() {
                *register = (index as u32).wrapping_mul(0x9e3779b9);
            }
        }
        assert_eq!(
            pair.step(&format!("VSTM edge {encoding:08x}")),
            CpuState::Normal
        );
        pair.compare_data(0x2000, 128);
        pair.compare_data(0x1008, 4);
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
fn arm_thumb_rotate_extend_accumulate_and_alias_cases_match() {
    let mut pair = Pair::new();
    let mut seed = 0x0abcdef1;
    for thumb in [false, true] {
        for kind in 0..6 {
            for accumulate in [false, true] {
                for rotation in 0..4 {
                    for destination in 0..3 {
                        let encoding =
                            extension_encoding(thumb, kind, accumulate, rotation, destination);
                        let bytes = if thumb {
                            [(encoding >> 16) as u16, encoding as u16]
                                .into_iter()
                                .flat_map(u16::to_le_bytes)
                                .collect::<Vec<_>>()
                        } else {
                            encoding.to_le_bytes().to_vec()
                        };
                        pair.code(&bytes);
                        for sample in 0..32 {
                            pair.reset(thumb, next(&mut seed) & 0xf80f0000);
                            for register in 0..3 {
                                pair.register(register, next(&mut seed));
                            }
                            assert_eq!(
                                pair.step(&format!("extend {encoding:08x} sample {sample}")),
                                CpuState::Normal
                            );
                        }
                    }
                }
            }
        }
    }
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
fn neon_vector_finite_float_variants_and_aliases_match() {
    let mut pair = Pair::new();
    for thumb in [false, true] {
        for operation in 0..7 {
            for quad in 0..2 {
                for banks in 0..8 {
                    for alias in 0..3 {
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
                        let bytes = if thumb {
                            [(encoding >> 16) as u16, encoding as u16]
                                .into_iter()
                                .flat_map(u16::to_le_bytes)
                                .collect::<Vec<_>>()
                        } else {
                            encoding.to_le_bytes().to_vec()
                        };
                        pair.code(&bytes);
                        for sample in 0..8 {
                            pair.reset(thumb, 0xa0000000);
                            for cpu in [&mut pair.jit, &mut pair.interpreter] {
                                for (index, register) in cpu.extregs_mut().iter_mut().enumerate() {
                                    *register = vector_fp_initial(index, sample);
                                }
                            }
                            assert_eq!(
                                pair.step(&format!("vector F32 {encoding:08x} sample {sample}")),
                                CpuState::Normal
                            );
                        }
                    }
                }
            }
        }
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
fn neon_duplicate_all_lane_sizes_banks_and_aliases_match() {
    let mut pair = Pair::new();
    for thumb in [false, true] {
        for quad in 0..2 {
            for bank in 0..4 {
                for source_bank in 0..2 {
                    for alias in 0..3 {
                        for immediate in [1, 3, 5, 7, 9, 11, 13, 15, 2, 6, 10, 14, 4, 12] {
                            let rd =
                                (bank & 1) * 16 + ((bank >> 1) & 1) * if quad == 1 { 2 } else { 1 };
                            let rm = match alias {
                                1 => rd,
                                2 => rd + 1,
                                _ => 4 + source_bank * 16,
                            };
                            let encoding = neon_duplicate_encoding(thumb, quad, rd, rm, immediate);
                            let bytes = if thumb {
                                [(encoding >> 16) as u16, encoding as u16]
                                    .into_iter()
                                    .flat_map(u16::to_le_bytes)
                                    .collect::<Vec<_>>()
                            } else {
                                encoding.to_le_bytes().to_vec()
                            };
                            pair.code(&bytes);
                            for sample in 0..3 {
                                pair.reset(thumb, 0xa0000000);
                                for cpu in [&mut pair.jit, &mut pair.interpreter] {
                                    for (index, register) in
                                        cpu.extregs_mut().iter_mut().enumerate()
                                    {
                                        *register = (index as u32)
                                            .wrapping_mul(0x9e3779b9)
                                            .wrapping_add(0x31415926 + sample);
                                    }
                                }
                                assert_eq!(
                                    pair.step(&format!("VDUP {encoding:08x}")),
                                    CpuState::Normal
                                );
                            }
                        }
                    }
                }
            }
        }
    }
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
fn arm_halfword_multiply_signed_accumulate_and_q_cases_match() {
    let mut pair = Pair::new();
    let mut seed = 0x1937face;
    for kind in 0..5 {
        for n_top in 0..if kind <= 2 { 2 } else { 1 } {
            for m_top in 0..2 {
                for destination in 0..3 {
                    let encoding = halfword_multiply_encoding(kind, destination, n_top, m_top);
                    pair.code(&encoding.to_le_bytes());
                    for sample in 0..128 {
                        pair.reset(false, next(&mut seed) & 0xf80f0000);
                        for register in 0..4 {
                            pair.register(register, next(&mut seed));
                        }
                        assert_eq!(
                            pair.step(&format!("half multiply {encoding:08x}, sample {sample}")),
                            CpuState::Normal
                        );
                    }
                }
            }
        }
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
fn neon_single_element_memory_structure_stride_and_writeback_match() {
    let mut pair = Pair::new();
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
                                    let bytes = if thumb {
                                        [(encoding >> 16) as u16, encoding as u16]
                                            .into_iter()
                                            .flat_map(u16::to_le_bytes)
                                            .collect::<Vec<_>>()
                                    } else {
                                        encoding.to_le_bytes().to_vec()
                                    };
                                    pair.code(&bytes);
                                    pair.reset(thumb, 0xa0000000);
                                    pair.register(1, 0x2000);
                                    pair.register(2, 0x24);
                                    for cpu in [&mut pair.jit, &mut pair.interpreter] {
                                        for (index, register) in
                                            cpu.extregs_mut().iter_mut().enumerate()
                                        {
                                            *register = (index as u32)
                                                .wrapping_mul(0x9e3779b9)
                                                .wrapping_add(0x31415926);
                                        }
                                    }
                                    for memory in [&mut pair.jit_mem, &mut pair.interpreter_mem] {
                                        for (index, value) in memory
                                            .bytes_at_mut(Ptr::from_bits(0x1ff0), 96)
                                            .iter_mut()
                                            .enumerate()
                                        {
                                            *value =
                                                (index as u8).wrapping_mul(17).wrapping_add(0x3c);
                                        }
                                    }
                                    assert_eq!(
                                        pair.step(&format!("NEON lane memory {encoding:08x}")),
                                        CpuState::Normal
                                    );
                                    pair.compare_data(0x1ff0, 96);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
