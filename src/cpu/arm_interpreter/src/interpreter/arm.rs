/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! ARM (A32) common integer instruction group for the pure-Rust interpreter.
//!
//! Implements ARMv7-A (DDI 0406C) A32 data-processing, MOVW/MOVT, multiply,
//! load/store (immediate + register, single + halfword/signed/dual), LDM/STM,
//! branches (B/BL/BX/BLX), sign/zero extends, bitfield ops, REV/CLZ and the
//! barrier no-ops. IT blocks and VFP/coprocessor are handled elsewhere.
//!
//! Contract (see mod.rs): `exec_arm` returns `Option<CpuState>`:
//!   * `None`          — encoding not recognised by this group.
//!   * `Some(Normal)`  — executed; PC has been advanced / branched.
//!   * `Some(Svc(imm))`— SVC trap.
//!   * `Some(Error(MemoryError))` — a data access faulted (PC already advanced).

use crate::mem::{GuestMem, Ptr};
use crate::{CpuError, CpuState};

impl super::InterpreterCpu {
    /// ARMExpandImm_C: rotate an 8-bit immediate by 2*rot, with carry-out.
    /// `imm12` is the low 12 bits of the instruction. Returns (value, carry_out).
    fn arm_expand_imm_c(imm12: u32, carry_in: bool) -> (u32, bool) {
        let unrotated = imm12 & 0xff;
        let rot = ((imm12 >> 8) & 0xf) * 2;
        if rot == 0 {
            (unrotated, carry_in)
        } else {
            Self::ror_c(unrotated, rot)
        }
    }

    /// Decode the register-form shifter operand (bits[11:0]) producing
    /// (value, carry_out). `bit4` selects immediate (0) vs register (1) shift
    /// amount. Caller guarantees the data-processing register form.
    fn arm_shifted_reg(&self, insn: u32) -> (u32, bool) {
        let rm = (insn & 0xf) as usize;
        let stype = (insn >> 5) & 3;
        let carry_in = self.flag_c();
        let m = self.get_reg(rm);
        if insn & (1 << 4) == 0 {
            // Immediate shift amount.
            let imm5 = (insn >> 7) & 0x1f;
            if imm5 == 0 {
                match stype {
                    0 => (m, carry_in),            // LSL #0
                    1 => Self::lsr_c(m, 32),       // LSR #32
                    2 => Self::asr_c(m, 32),       // ASR #32
                    _ => Self::rrx_c(m, carry_in), // RRX
                }
            } else {
                Self::shift_c(m, stype, imm5, carry_in)
            }
        } else {
            // Register-controlled shift amount (uses Rs[7:0]).
            let rs = ((insn >> 8) & 0xf) as usize;
            let amount = self.get_reg(rs) & 0xff;
            if amount == 0 {
                (m, carry_in)
            } else {
                match stype {
                    0 => Self::lsl_c(m, amount),
                    1 => Self::lsr_c(m, amount),
                    2 => Self::asr_c(m, amount),
                    _ => {
                        // ROR by register: amount==0 handled above; ROR by 32 → no
                        // change but carry from bit31; ror_c masks &31 so feed the
                        // masked amount, but ROR#0-mod-32 keeps value & carry bit31.
                        let m5 = amount & 0x1f;
                        if m5 == 0 {
                            (m, (m >> 31) & 1 != 0)
                        } else {
                            Self::ror_c(m, m5)
                        }
                    }
                }
            }
        }
    }

    pub(super) fn exec_arm(
        &mut self,
        insn: u32,
        pc: u32,
        mem: &mut impl GuestMem,
    ) -> Option<CpuState> {
        let cond = insn >> 28;

        // Unconditional (cond == 0xF) space: BLX(imm), PLD, barriers (the latter
        // also appear here). Handle a few, otherwise fall through to None.
        if cond == 0xF {
            return self.exec_arm_uncond(insn, pc, mem);
        }

        // Conditional-but-failed: skip (advance PC).
        if !self.cond_passed(cond) {
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }

        // ----- SVC (host-call) fallback: svc #imm (0xefxxxxxx after cond strip) -----
        if (insn & 0x0f00_0000) == 0x0f00_0000 {
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Svc(insn & 0x00ff_ffff));
        }

        // BKPT is used by the existing HLE's native overlay/clock hooks.
        if (insn & 0x0ff0_00f0) == 0x0120_0070 {
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Error(CpuError::Breakpoint));
        }
        if (insn & 0x0fff_0fff) == 0x010f_0000 {
            // MRS CPSR
            let destination = ((insn >> 12) & 15) as usize;
            if destination == 15 {
                return None;
            }
            self.regs[destination] = self.cpsr;
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }
        if (insn & 0x0ff0_fff0) == 0x0120_f000 {
            // MSR register
            let source = (insn & 15) as usize;
            if source == 15 || !self.write_status((insn >> 16) & 15, self.get_reg(source)) {
                return None;
            }
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }
        if (insn & 0x0ff0_f000) == 0x0320_f000 {
            // MSR immediate
            let value = (insn & 255).rotate_right(((insn >> 8) & 15) * 2);
            if !self.write_status((insn >> 16) & 15, value) {
                return None;
            }
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }

        // ----- Branch / branch-with-link (immediate): cond 101L imm24 -----
        if (insn & 0x0e00_0000) == 0x0a00_0000 {
            let link = insn & (1 << 24) != 0;
            let imm24 = insn & 0x00ff_ffff;
            let off = ((imm24 << 8) as i32 >> 6) as u32; // sign-extend, <<2
            let pc_read = self.get_reg(15); // PC+8
            if link {
                self.regs[14] = pc.wrapping_add(4);
            }
            self.regs[15] = pc_read.wrapping_add(off);
            return Some(CpuState::Normal);
        }

        // ----- BX / BLX / BXJ (register), CLZ : cond 0001 0010 ... 0001/0011 Rm -----
        // Miscellaneous: 0001 0xx0 ...
        if (insn & 0x0fff_fff0) == 0x012f_ff10 {
            // BX Rm
            let rm = (insn & 0xf) as usize;
            let target = self.get_reg(rm);
            self.bx_write_pc(target);
            return Some(CpuState::Normal);
        }
        if (insn & 0x0fff_fff0) == 0x012f_ff30 {
            // BLX Rm
            let rm = (insn & 0xf) as usize;
            let target = self.get_reg(rm);
            self.regs[14] = pc.wrapping_add(4);
            self.bx_write_pc(target);
            return Some(CpuState::Normal);
        }
        if (insn & 0x0fff_0ff0) == 0x016f_0f10 {
            // CLZ Rd, Rm
            let rd = ((insn >> 12) & 0xf) as usize;
            let rm = (insn & 0xf) as usize;
            self.regs[rd] = self.get_reg(rm).leading_zeros();
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }

        // The exclusive family must be decoded before general data processing.
        if (insn & 0x0f80_0ff0) == 0x0180_0f90 {
            return Some(self.arm_exclusive(insn, pc, mem));
        }

        // ARMv5TE signed halfword multiply sits in miscellaneous, not
        // generic TST/TEQ/CMP/CMN with S=0. Decode before data processing.
        if insn & 0x0f90_0090 == 0x0100_0080 {
            return self.exec_halfword_multiply(insn, pc);
        }

        // MLS is outside the MUL/MLA mask (bit22 set, bit20 clear).
        if insn & 0x0ff0_00f0 == 0x0060_0090 {
            let rd = ((insn >> 16) & 15) as usize;
            let ra = ((insn >> 12) & 15) as usize;
            let rm = ((insn >> 8) & 15) as usize;
            let rn = (insn & 15) as usize;
            if [rd, ra, rm, rn].contains(&15) {
                return None;
            }
            let product = self.get_reg(rn).wrapping_mul(self.get_reg(rm));
            self.regs[rd] = self.get_reg(ra).wrapping_sub(product);
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }

        // ----- Multiply family: cond 0000 ... 1001 (bits 27..24 == 0, bits7..4==1001)
        if (insn & 0x0fc0_00f0) == 0x0000_0090 {
            // MUL / MLA  (bit21 = A)
            return Some(self.exec_mul_mla(insn, pc));
        }
        if (insn & 0x0f80_00f0) == 0x0080_0090 {
            // UMULL/UMLAL/SMULL/SMLAL (bits23..21 select)
            return Some(self.exec_mull(insn, pc));
        }

        // ----- Extra load/store: halfword, signed byte/halfword, doubleword -----
        // These share bits: 000 ... 1xx1 with bit4==1 and bit7==1.
        if (insn & 0x0e00_0090) == 0x0000_0090 && (insn & 0x0000_0060) != 0 {
            return Some(self.exec_extra_ldst(insn, pc, mem));
        }

        // ----- Media: bitfield / extends / rev (cond 011x xxxx ... bit4==1) -----
        if (insn & 0x0e00_0010) == 0x0600_0010 {
            if let Some(st) = self.exec_media(insn, pc) {
                return Some(st);
            }
        }

        // ----- MOVW / MOVT (16-bit immediate) : cond 0011 0x00 -----
        if (insn & 0x0fb0_0000) == 0x0300_0000 {
            let imm4 = (insn >> 16) & 0xf;
            let imm12 = insn & 0xfff;
            let imm16 = (imm4 << 12) | imm12;
            let rd = ((insn >> 12) & 0xf) as usize;
            if insn & (1 << 22) == 0 {
                // MOVW: Rd = imm16
                self.regs[rd] = imm16;
            } else {
                // MOVT: Rd[31:16] = imm16, keep low half
                self.regs[rd] = (self.get_reg(rd) & 0x0000_ffff) | (imm16 << 16);
            }
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }

        // ----- Data processing (immediate / reg / reg-shifted-reg) -----
        // cond 00x ... opcode at bits24..21, S at bit20.
        if (insn & 0x0c00_0000) == 0x0000_0000 {
            if let Some(st) = self.exec_data_proc(insn, pc) {
                return Some(st);
            }
        }

        // ----- Single load/store (LDR/STR/LDRB/STRB) : cond 01x ... -----
        if (insn & 0x0c00_0000) == 0x0400_0000 {
            // Exclude media (bit25==1 && bit4==1) which is handled above.
            let media = (insn & (1 << 25)) != 0 && (insn & (1 << 4)) != 0;
            if !media {
                return Some(self.exec_single_ldst(insn, pc, mem));
            }
        }

        // ----- LDM / STM : cond 100 ... -----
        if (insn & 0x0e00_0000) == 0x0800_0000 {
            return Some(self.exec_ldm_stm(insn, pc, mem));
        }

        // ----- Coprocessor / VFP (cond 11xx): VLDM/VSTM/VLDR/VSTR/VFP -----
        if (insn & 0x0c00_0000) == 0x0c00_0000 {
            return self.exec_vfp(insn, pc, mem);
        }

        None
    }

    // A32 load/store exclusive encodings, matched against the pinned Dynarmic
    // frontend decoder (ARMv6/v6K). Execute on the owning guest thread only.
    fn arm_exclusive(&mut self, insn: u32, pc: u32, mem: &mut impl GuestMem) -> CpuState {
        self.regs[15] = pc.wrapping_add(4);
        let load = insn & (1 << 20) != 0;
        let rn = ((insn >> 16) & 15) as usize;
        let rd = ((insn >> 12) & 15) as usize;
        let rt = if load { rd } else { (insn & 15) as usize };
        let size = match (insn >> 21) & 3 {
            0 => 4,
            1 => 8,
            2 => 1,
            _ => 2,
        };
        if rn == 15
            || rd == 15
            || rt == 15
            || (load && insn & 15 != 15)
            || (size == 8 && (rt & 1 != 0 || rt >= 14))
            || (!load && (rd == rn || rd == rt || (size == 8 && rd == rt + 1)))
        {
            return CpuState::Error(CpuError::UndefinedInstruction);
        }
        let address = self.get_reg(rn);
        if address < self.null_segment_size || address & (size - 1) != 0 {
            self.excl_clear();
            return CpuState::Error(CpuError::MemoryError);
        }
        if load {
            let Some(bytes) = mem.get_bytes_fallible(Ptr::from_bits(address), size) else {
                self.excl_clear();
                return CpuState::Error(CpuError::MemoryError);
            };
            let mut value = [0_u8; 8];
            value[..size as usize].copy_from_slice(bytes);
            self.regs[rt] = u32::from_le_bytes(value[..4].try_into().unwrap());
            if size == 8 {
                self.regs[rt + 1] = u32::from_le_bytes(value[4..].try_into().unwrap());
            }
            self.excl_set(address, size);
        } else if self.excl_check_clear(address, size) {
            let mut value = [0_u8; 8];
            value[..4].copy_from_slice(&self.get_reg(rt).to_le_bytes());
            if size == 8 {
                value[4..].copy_from_slice(&self.get_reg(rt + 1).to_le_bytes());
            }
            let Some(bytes) = mem.get_bytes_fallible_mut(Ptr::from_bits(address), size) else {
                return CpuState::Error(CpuError::MemoryError);
            };
            bytes.copy_from_slice(&value[..size as usize]);
            self.regs[rd] = 0;
        } else {
            self.regs[rd] = 1;
        }
        CpuState::Normal
    }

    fn exec_arm_uncond(&mut self, insn: u32, pc: u32, mem: &mut impl GuestMem) -> Option<CpuState> {
        if insn == 0xf57f_f01f {
            // CLREX
            self.excl_clear();
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }
        // BLX (immediate): 1111 101H imm24 — always switches to Thumb.
        if (insn & 0xfe00_0000) == 0xfa00_0000 {
            let h = (insn >> 24) & 1;
            let imm24 = insn & 0x00ff_ffff;
            let off = (((imm24 << 8) as i32 >> 6) as u32) | (h << 1);
            let pc_read = self.get_reg(15); // PC+8
            self.regs[14] = pc.wrapping_add(4);
            // Target is current ARM PC + off, set Thumb bit.
            self.bx_write_pc(pc_read.wrapping_add(off) | 1);
            return Some(CpuState::Normal);
        }
        // PLD / PLI / barriers (DMB/DSB/ISB) / CLREX / NOP-ish: treat as no-op.
        // DMB: 1111 0101 0111 1111 1111 0000 0101 xxxx
        // DSB: ... 0100 xxxx ; ISB: ... 0110 xxxx ; CLREX: ... 0001 1111
        if (insn & 0xfff0_0000) == 0xf550_0000 {
            // PLD (immediate/literal) — hint, no-op.
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }
        if (insn & 0xffff_fff0) == 0xf57f_f040 // DSB
            || (insn & 0xffff_fff0) == 0xf57f_f050 // DMB
            || (insn & 0xffff_fff0) == 0xf57f_f060 // ISB
            || (insn & 0xffff_ffff) == 0xf57f_f01f
        // CLREX
        {
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }
        // Advanced SIMD (NEON) — the unconditional encodings: data-processing
        // 1111 001x (0xf2/0xf3) and element/structure load/store 1111 0100
        // (0xf4). VFP data-proc/transfers are conditional (cond != 1111) and
        // reach exec_vfp via the coprocessor path in exec_arm instead.
        let top = insn >> 24;
        if top == 0xf2 || top == 0xf3 || top == 0xf4 {
            return self.exec_vfp(insn, pc, mem);
        }
        None
    }

    /// Data-processing: AND/EOR/SUB/RSB/ADD/ADC/SBC/RSC/TST/TEQ/CMP/CMN/ORR/MOV/BIC/MVN.
    fn exec_data_proc(&mut self, insn: u32, pc: u32) -> Option<CpuState> {
        let is_imm = insn & (1 << 25) != 0;
        // Register-shifted-register requires bit4==1 && bit7==0 in the reg form;
        // bit7==1 with bit4==1 in non-imm space is the extra-ldst/media we already
        // excluded. Guard here: in reg form, if bit4==1 && bit7==1 it's not DP.
        if !is_imm && (insn & (1 << 4)) != 0 && (insn & (1 << 7)) != 0 {
            return None;
        }

        let opcode = (insn >> 21) & 0xf;
        let s = insn & (1 << 20) != 0;
        // Test/compare opcodes require S=1. S=0 belongs to miscellaneous
        // instruction space; accepting it here silently corrupts guest state.
        if (8..=11).contains(&opcode) && !s {
            return None;
        }

        let rn = ((insn >> 16) & 0xf) as usize;
        let rd = ((insn >> 12) & 0xf) as usize;

        let carry_in = self.flag_c();
        let (operand2, shifter_c) = if is_imm {
            Self::arm_expand_imm_c(insn & 0xfff, carry_in)
        } else {
            self.arm_shifted_reg(insn)
        };

        let n = self.get_reg(rn);

        // Compute result + flags depending on opcode.
        // Logical ops use shifter carry; arithmetic ops use AddWithCarry carry/v.
        let mut result: u32 = 0;
        let mut write_result = true;
        let mut set_c = shifter_c;
        let mut set_v = self.flag_v();
        let mut arith = false;

        match opcode {
            0x0 => result = n & operand2, // AND
            0x1 => result = n ^ operand2, // EOR
            0x2 => {
                // SUB
                let (r, c, v) = Self::add_with_carry(n, !operand2, true);
                result = r;
                set_c = c;
                set_v = v;
                arith = true;
            }
            0x3 => {
                // RSB
                let (r, c, v) = Self::add_with_carry(!n, operand2, true);
                result = r;
                set_c = c;
                set_v = v;
                arith = true;
            }
            0x4 => {
                // ADD
                let (r, c, v) = Self::add_with_carry(n, operand2, false);
                result = r;
                set_c = c;
                set_v = v;
                arith = true;
            }
            0x5 => {
                // ADC
                let (r, c, v) = Self::add_with_carry(n, operand2, carry_in);
                result = r;
                set_c = c;
                set_v = v;
                arith = true;
            }
            0x6 => {
                // SBC
                let (r, c, v) = Self::add_with_carry(n, !operand2, carry_in);
                result = r;
                set_c = c;
                set_v = v;
                arith = true;
            }
            0x7 => {
                // RSC
                let (r, c, v) = Self::add_with_carry(!n, operand2, carry_in);
                result = r;
                set_c = c;
                set_v = v;
                arith = true;
            }
            0x8 => {
                // TST (always S)
                result = n & operand2;
                write_result = false;
            }
            0x9 => {
                // TEQ
                result = n ^ operand2;
                write_result = false;
            }
            0xA => {
                // CMP
                let (r, c, v) = Self::add_with_carry(n, !operand2, true);
                result = r;
                set_c = c;
                set_v = v;
                arith = true;
                write_result = false;
            }
            0xB => {
                // CMN
                let (r, c, v) = Self::add_with_carry(n, operand2, false);
                result = r;
                set_c = c;
                set_v = v;
                arith = true;
                write_result = false;
            }
            0xC => result = n | operand2,  // ORR
            0xD => result = operand2,      // MOV (and MOVS/LSL/etc reg form)
            0xE => result = n & !operand2, // BIC
            0xF => result = !operand2,     // MVN
            _ => unreachable!(),
        }

        // TST/TEQ/CMP/CMN always set flags.
        let always_s = matches!(opcode, 0x8 | 0x9 | 0xA | 0xB);

        if write_result {
            if rd == 15 {
                // Writing PC. In user code: ALUWritePC (branch). If S — exception
                // return (not modelled here); we approximate with branch + flags
                // from result is not standard, but S+PC is rare in app code. Do a
                // plain branch (bx semantics for ARM DP write to PC keeps ARM
                // state); set flags if S as best-effort.
                if s && !always_s {
                    if arith {
                        self.set_nzcv(result, set_c, set_v);
                    } else {
                        self.set_nzcv(result, set_c, self.flag_v());
                    }
                }
                // BXWritePC in ARMv7 for DP write: actually BranchWritePC (stays
                // ARM, aligns to 4). Use bx_write_pc with bit0 cleared.
                self.bx_write_pc(result & !1);
                return Some(CpuState::Normal);
            }
            self.regs[rd] = result;
        }

        if s || always_s {
            if arith {
                self.set_nzcv(result, set_c, set_v);
            } else {
                // Logical: N,Z from result, C from shifter, V unchanged.
                self.set_nzcv(result, set_c, self.flag_v());
            }
        }

        self.regs[15] = pc.wrapping_add(4);
        Some(CpuState::Normal)
    }

    fn exec_halfword_multiply(&mut self, insn: u32, pc: u32) -> Option<CpuState> {
        let group = (insn >> 21) & 3;
        let rd = ((insn >> 16) & 15) as usize;
        let ra = ((insn >> 12) & 15) as usize;
        let rm = ((insn >> 8) & 15) as usize;
        let rn = (insn & 15) as usize;
        let no_accumulate = group == 3 || (group == 1 && insn & 0x20 != 0);
        if rd == 15
            || rn == 15
            || rm == 15
            || (no_accumulate && ra != 0)
            || (!no_accumulate && ra == 15)
            || (group == 2 && rd == ra)
        {
            return None;
        }
        let half =
            |value: u32, top: bool| ((value >> if top { 16 } else { 0 }) as u16) as i16 as i64;
        let n = self.get_reg(rn);
        let m = self.get_reg(rm);
        let product = if group == 1 {
            (n as i32 as i64) * half(m, insn & 0x40 != 0)
        } else {
            half(n, insn & 0x20 != 0) * half(m, insn & 0x40 != 0)
        };
        if group == 2 {
            let accumulator = (self.get_reg(ra) as u64) | ((self.get_reg(rd) as u64) << 32);
            let result = accumulator.wrapping_add(product as u64);
            self.regs[ra] = result as u32;
            self.regs[rd] = (result >> 32) as u32;
        } else {
            let product = if group == 1 { product >> 16 } else { product };
            let result = product
                + if no_accumulate {
                    0
                } else {
                    self.get_reg(ra) as i32 as i64
                };
            self.regs[rd] = result as u32;
            if !no_accumulate && (result < i32::MIN as i64 || result > i32::MAX as i64) {
                self.cpsr |= 1 << 27;
            }
        }
        self.regs[15] = pc.wrapping_add(4);
        Some(CpuState::Normal)
    }

    /// MUL (A==0) / MLA (A==1). 32-bit result; S sets N,Z.
    fn exec_mul_mla(&mut self, insn: u32, pc: u32) -> CpuState {
        let a = insn & (1 << 21) != 0;
        let s = insn & (1 << 20) != 0;
        let rd = ((insn >> 16) & 0xf) as usize;
        let ra = ((insn >> 12) & 0xf) as usize; // accumulate (MLA)
        let rs = ((insn >> 8) & 0xf) as usize;
        let rm = (insn & 0xf) as usize;
        let prod = self.get_reg(rm).wrapping_mul(self.get_reg(rs));
        let result = if a {
            prod.wrapping_add(self.get_reg(ra))
        } else {
            prod
        };
        self.regs[rd] = result;
        if s {
            self.set_nz(result);
        }
        self.regs[15] = pc.wrapping_add(4);
        CpuState::Normal
    }

    /// UMULL/UMLAL/SMULL/SMLAL. bit22=U/S(0=unsigned),bit21=A(accumulate).
    fn exec_mull(&mut self, insn: u32, pc: u32) -> CpuState {
        let signed = insn & (1 << 22) != 0;
        let accumulate = insn & (1 << 21) != 0;
        let s = insn & (1 << 20) != 0;
        let rdhi = ((insn >> 16) & 0xf) as usize;
        let rdlo = ((insn >> 12) & 0xf) as usize;
        let rs = ((insn >> 8) & 0xf) as usize;
        let rm = (insn & 0xf) as usize;
        let m = self.get_reg(rm);
        let n = self.get_reg(rs);

        let mut full: u64 = if signed {
            ((m as i32 as i64).wrapping_mul(n as i32 as i64)) as u64
        } else {
            (m as u64).wrapping_mul(n as u64)
        };
        if accumulate {
            let acc = ((self.get_reg(rdhi) as u64) << 32) | (self.get_reg(rdlo) as u64);
            full = full.wrapping_add(acc);
        }
        self.regs[rdlo] = full as u32;
        self.regs[rdhi] = (full >> 32) as u32;
        if s {
            // N from bit63, Z from whole 64-bit result.
            self.cpsr &= !(0b11 << 30);
            self.cpsr |= (((full >> 63) & 1) as u32) << 31;
            if full == 0 {
                self.cpsr |= 1 << 30;
            }
        }
        self.regs[15] = pc.wrapping_add(4);
        CpuState::Normal
    }

    /// Extra load/store: LDRH/STRH/LDRSB/LDRSH/LDRD/STRD.
    fn exec_extra_ldst(&mut self, insn: u32, pc: u32, mem: &mut impl GuestMem) -> CpuState {
        let p = insn & (1 << 24) != 0; // pre-index
        let u = insn & (1 << 23) != 0; // add
        let imm_form = insn & (1 << 22) != 0; // 1 = immediate offset
        let w = insn & (1 << 21) != 0; // writeback (or post always writes back)
        let l = insn & (1 << 20) != 0; // load
        let rn = ((insn >> 16) & 0xf) as usize;
        let rt = ((insn >> 12) & 0xf) as usize;
        let op = (insn >> 5) & 3; // bits[6:5]: SH

        let offset = if imm_form {
            ((insn >> 4) & 0xf0) | (insn & 0xf)
        } else {
            self.get_reg((insn & 0xf) as usize)
        };

        let base = self.get_reg(rn);
        let offset_addr = if u {
            base.wrapping_add(offset)
        } else {
            base.wrapping_sub(offset)
        };
        let address = if p { offset_addr } else { base };
        // Writeback for pre-index requires W; post-index always writes back.
        let wback = (!p) || w;

        let mut fault = false;

        // op encodes the access:
        //   L=0: op=1 STRH, op=2 LDRD, op=3 STRD
        //   L=1: op=1 LDRH, op=2 LDRSB, op=3 LDRSH
        match (l, op) {
            (false, 1) => {
                // STRH
                if !self.data_w_u16(mem, address, self.get_reg(rt) as u16) {
                    fault = true;
                }
            }
            (true, 1) => {
                // LDRH
                match self.data_r_u16(mem, address) {
                    Some(v) => self.regs[rt] = v as u32,
                    None => fault = true,
                }
            }
            (true, 2) => {
                // LDRSB
                match self.data_r_u8(mem, address) {
                    Some(v) => self.regs[rt] = v as i8 as i32 as u32,
                    None => fault = true,
                }
            }
            (true, 3) => {
                // LDRSH
                match self.data_r_u16(mem, address) {
                    Some(v) => self.regs[rt] = v as i16 as i32 as u32,
                    None => fault = true,
                }
            }
            (false, 2) => {
                // LDRD (Rt, Rt+1) — op=2,L=0
                let lo = self.data_r_u32(mem, address);
                let hi = self.data_r_u32(mem, address.wrapping_add(4));
                match (lo, hi) {
                    (Some(a), Some(b)) => {
                        self.regs[rt] = a;
                        self.regs[rt + 1] = b;
                    }
                    _ => fault = true,
                }
            }
            (false, 3) => {
                // STRD (Rt, Rt+1) — op=3,L=0
                let ok1 = self.data_w_u32(mem, address, self.get_reg(rt));
                let ok2 = self.data_w_u32(mem, address.wrapping_add(4), self.get_reg(rt + 1));
                if !ok1 || !ok2 {
                    fault = true;
                }
            }
            _ => {
                // Shouldn't happen given the dispatch guard.
            }
        }

        if fault {
            self.regs[15] = pc.wrapping_add(4);
            return CpuState::Error(CpuError::MemoryError);
        }

        if wback && rn != 15 {
            self.regs[rn] = offset_addr;
        }
        self.regs[15] = pc.wrapping_add(4);
        CpuState::Normal
    }

    /// Single data transfer: LDR/STR/LDRB/STRB (immediate-12 or register+shift).
    fn exec_single_ldst(&mut self, insn: u32, pc: u32, mem: &mut impl GuestMem) -> CpuState {
        let is_reg = insn & (1 << 25) != 0;
        let p = insn & (1 << 24) != 0;
        let u = insn & (1 << 23) != 0;
        let b = insn & (1 << 22) != 0; // byte
        let w = insn & (1 << 21) != 0;
        let l = insn & (1 << 20) != 0;
        let rn = ((insn >> 16) & 0xf) as usize;
        let rt = ((insn >> 12) & 0xf) as usize;

        let offset = if is_reg {
            // Register offset with optional immediate shift.
            let rm = (insn & 0xf) as usize;
            let stype = (insn >> 5) & 3;
            let imm5 = (insn >> 7) & 0x1f;
            let m = self.get_reg(rm);
            let carry_in = self.flag_c();
            let (val, _) = if imm5 == 0 {
                match stype {
                    0 => (m, carry_in),
                    1 => Self::lsr_c(m, 32),
                    2 => Self::asr_c(m, 32),
                    _ => Self::rrx_c(m, carry_in),
                }
            } else {
                Self::shift_c(m, stype, imm5, carry_in)
            };
            val
        } else {
            insn & 0xfff
        };

        // For PC-relative (Rn==15) use Align(PC,4).
        let base = if rn == 15 {
            self.get_reg_align(15)
        } else {
            self.get_reg(rn)
        };
        let offset_addr = if u {
            base.wrapping_add(offset)
        } else {
            base.wrapping_sub(offset)
        };
        let address = if p { offset_addr } else { base };
        let wback = !p || w;

        let mut fault = false;

        if l {
            if b {
                match self.data_r_u8(mem, address) {
                    Some(v) => self.regs[rt] = v as u32,
                    None => fault = true,
                }
            } else {
                match self.data_r_u32(mem, address) {
                    Some(v) => {
                        if rt == 15 {
                            // LDR to PC: interworking branch.
                            // Defer the write until after wback.
                            self.regs[rt] = v; // temp; corrected below
                        } else {
                            self.regs[rt] = v;
                        }
                    }
                    None => fault = true,
                }
            }
        } else {
            let v = self.get_reg(rt);
            if b {
                if !self.data_w_u8(mem, address, v as u8) {
                    fault = true;
                }
            } else if !self.data_w_u32(mem, address, v) {
                fault = true;
            }
        }

        if fault {
            self.regs[15] = pc.wrapping_add(4);
            return CpuState::Error(CpuError::MemoryError);
        }

        if wback && rn != 15 {
            self.regs[rn] = offset_addr;
        }

        if l && !b && rt == 15 {
            // LDR PC: interworking.
            let v = self.regs[15];
            self.bx_write_pc(v);
            return CpuState::Normal;
        }

        self.regs[15] = pc.wrapping_add(4);
        CpuState::Normal
    }

    /// LDM/STM (IA/IB/DA/DB), writeback, LDM with PC in list.
    fn exec_ldm_stm(&mut self, insn: u32, pc: u32, mem: &mut impl GuestMem) -> CpuState {
        let p = insn & (1 << 24) != 0; // before
        let u = insn & (1 << 23) != 0; // increment
        let w = insn & (1 << 21) != 0; // writeback
        let l = insn & (1 << 20) != 0; // load
        let rn = ((insn >> 16) & 0xf) as usize;
        let reg_list = insn & 0xffff;
        let count = reg_list.count_ones();
        if count == 0 {
            // Unpredictable; treat as no-op advancing PC.
            self.regs[15] = pc.wrapping_add(4);
            return CpuState::Normal;
        }

        let base = self.get_reg(rn);
        // Lowest address accessed.
        let start = if u {
            if p {
                base.wrapping_add(4) // IB
            } else {
                base // IA
            }
        } else if p {
            base.wrapping_sub(count * 4) // DB
        } else {
            base.wrapping_sub(count * 4).wrapping_add(4) // DA
        };

        let mut addr = start;
        let mut fault = false;
        let mut pc_loaded: Option<u32> = None;

        // Registers are accessed lowest-numbered at lowest address.
        for i in 0..16 {
            if reg_list & (1 << i) == 0 {
                continue;
            }
            if l {
                match self.data_r_u32(mem, addr) {
                    Some(v) => {
                        if i == 15 {
                            pc_loaded = Some(v);
                        } else {
                            self.regs[i] = v;
                        }
                    }
                    None => {
                        fault = true;
                        break;
                    }
                }
            } else {
                // STM stores the original base for Rn even with writeback if Rn is
                // the lowest register; we store current reg value (R15 = PC+8).
                let val = self.get_reg(i);
                if !self.data_w_u32(mem, addr, val) {
                    fault = true;
                    break;
                }
            }
            addr = addr.wrapping_add(4);
        }

        if fault {
            self.regs[15] = pc.wrapping_add(4);
            return CpuState::Error(CpuError::MemoryError);
        }

        // Writeback: new base = base ± count*4.
        if w {
            let new_base = if u {
                base.wrapping_add(count * 4)
            } else {
                base.wrapping_sub(count * 4)
            };
            // If Rn is in the list and it's a load, value loaded wins (already set).
            // For store with Rn in list and writeback, architecturally the stored
            // value should be the original/updated base depending on position; we
            // keep it simple (lowest-in-list stores original base, which our loop
            // already did since we read get_reg before writeback).
            self.regs[rn] = new_base;
        }

        if let Some(v) = pc_loaded {
            self.bx_write_pc(v);
            return CpuState::Normal;
        }

        self.regs[15] = pc.wrapping_add(4);
        CpuState::Normal
    }

    /// Media instructions: UXTB/SXTB/UXTH/SXTH/UBFX/SBFX/BFI/BFC/REV.
    fn exec_media(&mut self, insn: u32, pc: u32) -> Option<CpuState> {
        // Signed most-significant-word multiply: SMMUL{R}, SMMLA{R}, SMMLS{R}.
        // A32 decoder fields: cccc01110101ddddaaaammmm00R1nnnn (add),
        // or ...11R1nnnn (subtract). Ra=PC denotes the non-accumulating alias.
        let high_multiply = insn & 0x0ff0_00d0;
        if high_multiply == 0x0750_0010 || high_multiply == 0x0750_00d0 {
            let rd = ((insn >> 16) & 15) as usize;
            let ra = ((insn >> 12) & 15) as usize;
            let rm = ((insn >> 8) & 15) as usize;
            let rn = (insn & 15) as usize;
            let subtract = high_multiply == 0x0750_00d0;
            if rd == 15 || rn == 15 || rm == 15 || (subtract && ra == 15) {
                return None;
            }
            let product =
                ((self.get_reg(rn) as i32 as i64) * (self.get_reg(rm) as i32 as i64)) as u64;
            let accumulator = if ra == 15 {
                0
            } else {
                (self.get_reg(ra) as u64) << 32
            };
            let result = if subtract {
                accumulator.wrapping_sub(product)
            } else {
                accumulator.wrapping_add(product)
            };
            let rounded = result.wrapping_add(if insn & 0x20 != 0 { 0x8000_0000 } else { 0 });
            self.regs[rd] = (rounded >> 32) as u32;
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }

        // Signed/unsigned byte, halfword and byte-pair extensions, with
        // optional rotate and accumulation. Rn=PC denotes the no-add alias.
        if insn & 0x0f80_03f0 == 0x0680_0070 {
            let kind = (insn >> 20) & 3; // 0: two bytes -> two halfwords, 2: byte, 3: halfword
            let unsigned = insn & (1 << 22) != 0;
            let rn = ((insn >> 16) & 15) as usize;
            let rd = ((insn >> 12) & 15) as usize;
            let rm = (insn & 15) as usize;
            if kind == 1 || rd == 15 || rm == 15 {
                return None;
            }
            let source = self.get_reg(rm).rotate_right(((insn >> 10) & 3) * 8);
            let addend = if rn == 15 { 0 } else { self.get_reg(rn) };
            let result = if kind == 0 {
                let low = if unsigned {
                    source & 255
                } else {
                    source as u8 as i8 as i32 as u32
                };
                let high = if unsigned {
                    (source >> 16) & 255
                } else {
                    (source >> 16) as u8 as i8 as i32 as u32
                };
                ((low.wrapping_add(addend) & 65535)
                    | ((high.wrapping_add(addend >> 16) & 65535) << 16)) as u32
            } else {
                let value = match (unsigned, kind) {
                    (true, 2) => source & 255,
                    (true, _) => source & 65535,
                    (false, 2) => source as u8 as i8 as i32 as u32,
                    (false, _) => source as u16 as i16 as i32 as u32,
                };
                value.wrapping_add(addend)
            };
            self.regs[rd] = result;
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }

        // REV: cond 0110 1011 1111 Rd 1111 0011 Rm  (op 0x6B, bits 11..4 == 0xF3)
        if (insn & 0x0fff_0ff0) == 0x06bf_0f30 {
            let rd = ((insn >> 12) & 0xf) as usize;
            let rm = (insn & 0xf) as usize;
            self.regs[rd] = self.get_reg(rm).swap_bytes();
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }
        // REV16: 0110 1011 1111 Rd 1111 1011 Rm
        if (insn & 0x0fff_0ff0) == 0x06bf_0fb0 {
            let rd = ((insn >> 12) & 0xf) as usize;
            let rm = (insn & 0xf) as usize;
            let v = self.get_reg(rm);
            self.regs[rd] = ((v & 0x00ff_00ff) << 8) | ((v >> 8) & 0x00ff_00ff);
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }

        // SBFX: cond 0111 101 widthm1 Rd lsb 101 Rn
        // UBFX: cond 0111 111 widthm1 Rd lsb 101 Rn
        if (insn & 0x0fe0_0070) == 0x07a0_0050 || (insn & 0x0fe0_0070) == 0x07e0_0050 {
            let unsigned = (insn & (1 << 22)) != 0;
            let widthm1 = (insn >> 16) & 0x1f;
            let rd = ((insn >> 12) & 0xf) as usize;
            let lsb = (insn >> 7) & 0x1f;
            let rn = (insn & 0xf) as usize;
            let width = widthm1 + 1;
            let src = self.get_reg(rn);
            let field = if lsb + width >= 32 {
                src >> lsb
            } else {
                (src >> lsb) & ((1u32 << width) - 1)
            };
            self.regs[rd] = if unsigned {
                field
            } else {
                // Sign-extend from bit (width-1).
                let shift = 32 - width;
                (((field << shift) as i32) >> shift) as u32
            };
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }

        // BFI / BFC: cond 0111 110 msb Rd lsb 001 Rn
        //   BFC if Rn==1111 (clear); else BFI (insert from Rn).
        if (insn & 0x0fe0_0070) == 0x07c0_0010 {
            let msb = (insn >> 16) & 0x1f;
            let rd = ((insn >> 12) & 0xf) as usize;
            let lsb = (insn >> 7) & 0x1f;
            let rn = (insn & 0xf) as usize;
            if msb >= lsb {
                let width = msb - lsb + 1;
                let mask = if width >= 32 {
                    0xffff_ffffu32
                } else {
                    ((1u32 << width) - 1) << lsb
                };
                let cur = self.get_reg(rd);
                if rn == 0xf {
                    // BFC
                    self.regs[rd] = cur & !mask;
                } else {
                    let src = self.get_reg(rn);
                    let ins = (src << lsb) & mask;
                    self.regs[rd] = (cur & !mask) | ins;
                }
            }
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }

        None
    }
}
