/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Thumb-16 instruction group executor for the pure-Rust ARMv7 interpreter.
//!
//! Implements the full set of common 16-bit Thumb encodings per the ARMv7-A
//! Architecture Reference Manual (DDI 0406C), section A6.2 "16-bit Thumb
//! instruction encoding". Flag behaviour (NZCV, shifter carry-out) follows the
//! ARM ARM pseudocode exactly so the differential harness (diff.rs) matches the
//! dynarmic oracle bit-for-bit.
//!
//! Not handled here (return `None`, so the dispatcher falls through):
//!  - 32-bit Thumb-2 (`hw & 0xf800 >= 0xe800`): another group.
//!  - IT blocks (`0xBF00..=0xBFFF` with cond nibble): another group.
//!  - VFP / coprocessor: another group.

use crate::mem::GuestMem;
use crate::{CpuError, CpuState};

const PC: usize = 15;

impl super::InterpreterCpu {
    pub(super) fn exec_thumb16(
        &mut self,
        hw: u16,
        pc: u32,
        mem: &mut impl GuestMem,
    ) -> Option<CpuState> {
        let next = pc.wrapping_add(2);
        let hw = hw as u32;

        match hw >> 12 {
            // ============================================================
            // 0b000x / 0b0001: shift (imm) and add/sub  (0x0000-0x1FFF)
            // ============================================================
            0b0000 | 0b0001 => {
                // ARM ARM A6.2.1: bits[13:9] (opcode5) select within this group.
                //   0b00xxx / 0b01011 ... : shift (imm) LSL/LSR/ASR
                //   0b01100 ADD reg   0b01101 SUB reg
                //   0b01110 ADD imm3  0b01111 SUB imm3
                let opcode5 = (hw >> 9) & 0x1f; // bits[13:9]
                if (opcode5 >> 1) == 0b0110 {
                    // ADD/SUB register. bit9 selects: 0=ADD, 1=SUB.
                    let is_sub = opcode5 & 1 != 0;
                    let rd = (hw & 7) as usize;
                    let rn = ((hw >> 3) & 7) as usize;
                    let rm = ((hw >> 6) & 7) as usize;
                    let (res, c, v) = if is_sub {
                        Self::add_with_carry(self.get_reg(rn), !self.get_reg(rm), true)
                    } else {
                        Self::add_with_carry(self.get_reg(rn), self.get_reg(rm), false)
                    };
                    self.set_reg(rd, res);
                    self.set_nzcv_dp(res, c, v);
                    self.regs[PC] = next;
                    return Some(CpuState::Normal);
                }
                if (opcode5 >> 1) == 0b0111 {
                    // ADD/SUB immediate-3. bit9 selects: 0=ADD, 1=SUB.
                    let is_sub = opcode5 & 1 != 0;
                    let rd = (hw & 7) as usize;
                    let rn = ((hw >> 3) & 7) as usize;
                    let imm3 = (hw >> 6) & 7;
                    let (res, c, v) = if is_sub {
                        Self::add_with_carry(self.get_reg(rn), !imm3, true)
                    } else {
                        Self::add_with_carry(self.get_reg(rn), imm3, false)
                    };
                    self.set_reg(rd, res);
                    self.set_nzcv_dp(res, c, v);
                    self.regs[PC] = next;
                    return Some(CpuState::Normal);
                }
                // Otherwise: LSL/LSR/ASR (immediate). bits[12:11] = op2
                //   00 = LSL, 01 = LSR, 10 = ASR  (11 belongs to add/sub above,
                //   already returned). bit13 is 0 for all shift encodings.
                let stype = (hw >> 11) & 3;
                let imm5 = (hw >> 6) & 0x1f;
                let rd = (hw & 7) as usize;
                let rm = ((hw >> 3) & 7) as usize;
                let cin = self.flag_c();
                // For LSR/ASR an imm5 of 0 encodes a shift of 32.
                let amount = if imm5 == 0 && stype != 0 { 32 } else { imm5 };
                let (res, c) = Self::shift_c(self.get_reg(rm), stype, amount, cin);
                self.set_reg(rd, res);
                self.set_nzcv_dp(res, c, self.flag_v());
                self.regs[PC] = next;
                Some(CpuState::Normal)
            }

            // ============================================================
            // 0b001x: MOV/CMP/ADD/SUB (immediate-8)  (0x2000-0x3FFF)
            // ============================================================
            0b0010 | 0b0011 => {
                let op = (hw >> 11) & 3; // bits[12:11]
                let rdn = ((hw >> 8) & 7) as usize;
                let imm = hw & 0xff;
                match op {
                    0b00 => {
                        // MOV  Rd, #imm8   (sets N,Z; leaves C,V)
                        self.set_reg(rdn, imm);
                        self.set_nz_dp(imm);
                    }
                    0b01 => {
                        // CMP  Rn, #imm8  (always sets flags, even in an IT block)
                        let (res, c, v) = Self::add_with_carry(self.get_reg(rdn), !imm, true);
                        self.set_nzcv(res, c, v);
                    }
                    0b10 => {
                        // ADD  Rdn, #imm8
                        let (res, c, v) = Self::add_with_carry(self.get_reg(rdn), imm, false);
                        self.set_reg(rdn, res);
                        self.set_nzcv_dp(res, c, v);
                    }
                    _ => {
                        // SUB  Rdn, #imm8
                        let (res, c, v) = Self::add_with_carry(self.get_reg(rdn), !imm, true);
                        self.set_reg(rdn, res);
                        self.set_nzcv_dp(res, c, v);
                    }
                }
                self.regs[PC] = next;
                Some(CpuState::Normal)
            }

            // ============================================================
            // 0b0100: data-proc reg / special data / BX,BLX / LDR literal
            // ============================================================
            0b0100 => {
                if hw < 0x4400 {
                    // 0x4000-0x43FF: data-processing register
                    return self.dp_register(hw, next);
                }
                if hw < 0x4800 {
                    // 0x4400-0x47FF: special data instructions + BX/BLX
                    return self.special_data(hw, pc, next);
                }
                // 0x4800-0x4FFF: LDR (literal) — PC relative
                let rt = ((hw >> 8) & 7) as usize;
                let imm8 = (hw & 0xff) * 4;
                let base = self.get_reg_align(15);
                let addr = base.wrapping_add(imm8);
                self.regs[PC] = next; // advance before any fault
                match self.data_r_u32(mem, addr) {
                    Some(v) => {
                        self.set_reg(rt, v);
                        Some(CpuState::Normal)
                    }
                    None => Some(CpuState::Error(CpuError::MemoryError)),
                }
            }

            // ============================================================
            // 0b0101: load/store register offset  (0x5000-0x5FFF)
            // ============================================================
            0b0101 => self.ldst_reg_offset(hw, next, mem),

            // ============================================================
            // 0b0110: STR/LDR (imm)         word    (0x6000-0x6FFF)
            // ============================================================
            0b0110 => {
                let load = (hw >> 11) & 1 != 0;
                let imm5 = (hw >> 6) & 0x1f;
                let rn = ((hw >> 3) & 7) as usize;
                let rt = (hw & 7) as usize;
                let addr = self.get_reg(rn).wrapping_add(imm5 * 4);
                self.regs[PC] = next;
                if load {
                    match self.data_r_u32(mem, addr) {
                        Some(v) => {
                            self.set_reg(rt, v);
                            Some(CpuState::Normal)
                        }
                        None => Some(CpuState::Error(CpuError::MemoryError)),
                    }
                } else if self.data_w_u32(mem, addr, self.get_reg(rt)) {
                    Some(CpuState::Normal)
                } else {
                    Some(CpuState::Error(CpuError::MemoryError))
                }
            }

            // ============================================================
            // 0b0111: STRB/LDRB (imm)       byte    (0x7000-0x7FFF)
            // ============================================================
            0b0111 => {
                let load = (hw >> 11) & 1 != 0;
                let imm5 = (hw >> 6) & 0x1f;
                let rn = ((hw >> 3) & 7) as usize;
                let rt = (hw & 7) as usize;
                let addr = self.get_reg(rn).wrapping_add(imm5);
                self.regs[PC] = next;
                if load {
                    match self.data_r_u8(mem, addr) {
                        Some(v) => {
                            self.set_reg(rt, v as u32);
                            Some(CpuState::Normal)
                        }
                        None => Some(CpuState::Error(CpuError::MemoryError)),
                    }
                } else if self.data_w_u8(mem, addr, self.get_reg(rt) as u8) {
                    Some(CpuState::Normal)
                } else {
                    Some(CpuState::Error(CpuError::MemoryError))
                }
            }

            // ============================================================
            // 0b1000: STRH/LDRH (imm)       halfword (0x8000-0x8FFF)
            // ============================================================
            0b1000 => {
                let load = (hw >> 11) & 1 != 0;
                let imm5 = (hw >> 6) & 0x1f;
                let rn = ((hw >> 3) & 7) as usize;
                let rt = (hw & 7) as usize;
                let addr = self.get_reg(rn).wrapping_add(imm5 * 2);
                self.regs[PC] = next;
                if load {
                    match self.data_r_u16(mem, addr) {
                        Some(v) => {
                            self.set_reg(rt, v as u32);
                            Some(CpuState::Normal)
                        }
                        None => Some(CpuState::Error(CpuError::MemoryError)),
                    }
                } else if self.data_w_u16(mem, addr, self.get_reg(rt) as u16) {
                    Some(CpuState::Normal)
                } else {
                    Some(CpuState::Error(CpuError::MemoryError))
                }
            }

            // ============================================================
            // 0b1001: SP-relative load/store (imm8)  (0x9000-0x9FFF)
            // ============================================================
            0b1001 => {
                let load = (hw >> 11) & 1 != 0;
                let rt = ((hw >> 8) & 7) as usize;
                let imm8 = (hw & 0xff) * 4;
                let addr = self.get_reg(13).wrapping_add(imm8);
                self.regs[PC] = next;
                if load {
                    match self.data_r_u32(mem, addr) {
                        Some(v) => {
                            self.set_reg(rt, v);
                            Some(CpuState::Normal)
                        }
                        None => Some(CpuState::Error(CpuError::MemoryError)),
                    }
                } else if self.data_w_u32(mem, addr, self.get_reg(rt)) {
                    Some(CpuState::Normal)
                } else {
                    Some(CpuState::Error(CpuError::MemoryError))
                }
            }

            // ============================================================
            // 0b1010: ADR / ADD (SP plus imm)  (0xA000-0xAFFF)
            // ============================================================
            0b1010 => {
                let rd = ((hw >> 8) & 7) as usize;
                let imm8 = (hw & 0xff) * 4;
                if (hw >> 11) & 1 == 0 {
                    // ADR: Rd = Align(PC,4) + imm8
                    let v = self.get_reg_align(15).wrapping_add(imm8);
                    self.set_reg(rd, v);
                } else {
                    // ADD Rd, SP, #imm8
                    let v = self.get_reg(13).wrapping_add(imm8);
                    self.set_reg(rd, v);
                }
                self.regs[PC] = next;
                Some(CpuState::Normal)
            }

            // ============================================================
            // 0b1011: miscellaneous  (0xB000-0xBFFF)
            // ============================================================
            0b1011 => self.misc(hw, pc, next, mem),

            // ============================================================
            // 0b1100: LDM/STM (multiple)  (0xC000-0xCFFF)
            // ============================================================
            0b1100 => self.ldm_stm(hw, next, mem),

            // ============================================================
            // 0b1101: conditional branch + SVC  (0xD000-0xDFFF)
            // ============================================================
            0b1101 => {
                let cond = (hw >> 8) & 0xf;
                if cond == 0xe {
                    // UDF (permanently undefined)
                    self.regs[PC] = next;
                    return Some(CpuState::Error(CpuError::UndefinedInstruction));
                }
                if cond == 0xf {
                    // SVC #imm8
                    let imm8 = hw & 0xff;
                    self.regs[PC] = next;
                    return Some(CpuState::Svc(imm8));
                }
                // B<cond> label  (imm8 << 1, sign-extended, relative to PC+4)
                let imm32 = (((hw & 0xff) as i8 as i32) << 1) as u32;
                if self.cond_passed(cond) {
                    let target = self.get_reg(15).wrapping_add(imm32);
                    self.bx_write_pc(target | 1); // stay in Thumb
                } else {
                    self.regs[PC] = next;
                }
                Some(CpuState::Normal)
            }

            // ============================================================
            // 0b1110: unconditional branch B  (0xE000-0xE7FF)
            //         0xE800-0xFFFF is Thumb-32 → None
            // ============================================================
            0b1110 => {
                if hw >= 0xe800 {
                    return None; // 32-bit Thumb-2
                }
                // imm11 << 1, sign-extended, relative to PC+4
                let imm11 = hw & 0x7ff;
                let signed = ((imm11 << 1) as i32) << 20 >> 20; // sign-extend bit 11→bit31
                let target = self.get_reg(15).wrapping_add(signed as u32);
                self.bx_write_pc(target | 1);
                Some(CpuState::Normal)
            }

            // 0b1111: Thumb-32 (BL/BLX/coproc/etc.) → not us
            _ => None,
        }
    }

    // ----------------------------------------------------------------
    // 0x4000-0x43FF: data-processing (register)
    // ----------------------------------------------------------------
    fn dp_register(&mut self, hw: u32, next: u32) -> Option<CpuState> {
        let op = (hw >> 6) & 0xf;
        let rdn = (hw & 7) as usize; // Rd / Rdn
        let rm = ((hw >> 3) & 7) as usize; // Rm / Rs
        let x = self.get_reg(rdn);
        let y = self.get_reg(rm);
        let cin = self.flag_c();
        let vin = self.flag_v();
        // Every op below sets NZCV, but for the non-compare ops `setflags =
        // !InITBlock()`: inside an IT block they must leave the flags untouched.
        // TST(0x8)/CMP(0xa)/CMN(0xb) are pure compares and always set flags. So
        // snapshot NZCV up front and, for the writeback ops inside an IT block,
        // restore it after (simpler + less error-prone than gating 13 arms).
        let saved_nzcv = self.cpsr & 0xf000_0000;
        let restore_flags = self.in_it_block() && !matches!(op, 0x8 | 0xa | 0xb);

        match op {
            0x0 => {
                // ANDS
                let res = x & y;
                self.set_reg(rdn, res);
                self.set_nzcv(res, cin, vin);
            }
            0x1 => {
                // EORS
                let res = x ^ y;
                self.set_reg(rdn, res);
                self.set_nzcv(res, cin, vin);
            }
            0x2 => {
                // LSLS Rdn, Rm  (shift by Rm[7:0])
                let amt = y & 0xff;
                let (res, c) = if amt == 0 {
                    (x, cin)
                } else {
                    Self::lsl_c(x, amt)
                };
                self.set_reg(rdn, res);
                self.set_nzcv(res, c, vin);
            }
            0x3 => {
                // LSRS
                let amt = y & 0xff;
                let (res, c) = if amt == 0 {
                    (x, cin)
                } else {
                    Self::lsr_c(x, amt)
                };
                self.set_reg(rdn, res);
                self.set_nzcv(res, c, vin);
            }
            0x4 => {
                // ASRS
                let amt = y & 0xff;
                let (res, c) = if amt == 0 {
                    (x, cin)
                } else {
                    Self::asr_c(x, amt)
                };
                self.set_reg(rdn, res);
                self.set_nzcv(res, c, vin);
            }
            0x5 => {
                // ADCS
                let (res, c, v) = Self::add_with_carry(x, y, cin);
                self.set_reg(rdn, res);
                self.set_nzcv(res, c, v);
            }
            0x6 => {
                // SBCS
                let (res, c, v) = Self::add_with_carry(x, !y, cin);
                self.set_reg(rdn, res);
                self.set_nzcv(res, c, v);
            }
            0x7 => {
                // RORS  (shift by Rm[7:0])
                let amt = y & 0xff;
                let (res, c) = if amt == 0 {
                    (x, cin)
                } else if amt & 0x1f == 0 {
                    // multiple of 32: result unchanged, carry = bit31
                    (x, (x >> 31) & 1 != 0)
                } else {
                    Self::ror_c(x, amt & 0x1f)
                };
                self.set_reg(rdn, res);
                self.set_nzcv(res, c, vin);
            }
            0x8 => {
                // TST (AND, flags only)
                let res = x & y;
                self.set_nzcv(res, cin, vin);
            }
            0x9 => {
                // RSBS Rd, Rn, #0  (negate)
                let (res, c, v) = Self::add_with_carry(!y, 0, true);
                self.set_reg(rdn, res);
                self.set_nzcv(res, c, v);
            }
            0xa => {
                // CMP
                let (res, c, v) = Self::add_with_carry(x, !y, true);
                self.set_nzcv(res, c, v);
            }
            0xb => {
                // CMN
                let (res, c, v) = Self::add_with_carry(x, y, false);
                self.set_nzcv(res, c, v);
            }
            0xc => {
                // ORRS
                let res = x | y;
                self.set_reg(rdn, res);
                self.set_nzcv(res, cin, vin);
            }
            0xd => {
                // MULS Rdm, Rn, Rdm  -> Rdm = Rn * Rdm (low 32 bits); sets N,Z
                let res = x.wrapping_mul(y);
                self.set_reg(rdn, res);
                self.set_nzcv(res, cin, vin);
            }
            0xe => {
                // BICS
                let res = x & !y;
                self.set_reg(rdn, res);
                self.set_nzcv(res, cin, vin);
            }
            _ => {
                // 0xf: MVNS
                let res = !y;
                self.set_reg(rdn, res);
                self.set_nzcv(res, cin, vin);
            }
        }
        if restore_flags {
            self.cpsr = (self.cpsr & !0xf000_0000) | saved_nzcv;
        }
        self.regs[PC] = next;
        Some(CpuState::Normal)
    }

    // ----------------------------------------------------------------
    // 0x4400-0x47FF: special data instructions, branch/exchange
    // ----------------------------------------------------------------
    fn special_data(&mut self, hw: u32, pc: u32, next: u32) -> Option<CpuState> {
        let op = (hw >> 8) & 3; // bits[9:8]
                                // Rm is bits[6:3] (full 4-bit register number).
        let rm = ((hw >> 3) & 0xf) as usize;
        // Rd/Rn = D:bits[2:0]   where D = bit7.
        let rdn = (((hw >> 4) & 0x8) | (hw & 7)) as usize;

        match op {
            0b00 => {
                // ADD Rdn, Rm  (high registers; no flags)
                let res = self.get_reg(rdn).wrapping_add(self.get_reg(rm));
                if rdn == 15 {
                    // Architecturally ALUWritePC; with bit0 clear it is
                    // UNPREDICTABLE and the dynarmic oracle stays in Thumb.
                    // Match that: branch within the current ISA (force bit0).
                    self.bx_write_pc(res | 1);
                } else {
                    self.set_reg(rdn, res);
                    self.regs[PC] = next;
                }
                Some(CpuState::Normal)
            }
            0b01 => {
                // CMP Rn, Rm  (high registers); sets flags
                let (res, c, v) = Self::add_with_carry(self.get_reg(rdn), !self.get_reg(rm), true);
                self.set_nzcv(res, c, v);
                self.regs[PC] = next;
                Some(CpuState::Normal)
            }
            0b10 => {
                // MOV Rd, Rm  (high registers; no flags)
                let v = self.get_reg(rm);
                if rdn == 15 {
                    // MOV PC,Rm: with bit0 clear it is UNPREDICTABLE and the
                    // dynarmic oracle stays in Thumb; match it (force bit0).
                    self.bx_write_pc(v | 1);
                } else {
                    self.set_reg(rdn, v);
                    self.regs[PC] = next;
                }
                Some(CpuState::Normal)
            }
            _ => {
                // 0b11: BX / BLX (register).  bit7 selects BLX (link).
                let link = (hw >> 7) & 1 != 0;
                let target = self.get_reg(rm);
                if link {
                    // BLX: LR = (address of next instruction) | 1
                    self.set_reg(14, next | 1);
                }
                self.bx_write_pc(target);
                let _ = pc;
                Some(CpuState::Normal)
            }
        }
    }

    // ----------------------------------------------------------------
    // 0x5000-0x5FFF: load/store register offset
    // ----------------------------------------------------------------
    fn ldst_reg_offset(&mut self, hw: u32, next: u32, mem: &mut impl GuestMem) -> Option<CpuState> {
        let op = (hw >> 9) & 7; // bits[11:9]
        let rm = ((hw >> 6) & 7) as usize;
        let rn = ((hw >> 3) & 7) as usize;
        let rt = (hw & 7) as usize;
        let addr = self.get_reg(rn).wrapping_add(self.get_reg(rm));
        self.regs[PC] = next;

        let ok = match op {
            0b000 => {
                // STR
                self.data_w_u32(mem, addr, self.get_reg(rt))
            }
            0b001 => {
                // STRH
                self.data_w_u16(mem, addr, self.get_reg(rt) as u16)
            }
            0b010 => {
                // STRB
                self.data_w_u8(mem, addr, self.get_reg(rt) as u8)
            }
            0b011 => {
                // LDRSB
                match self.data_r_u8(mem, addr) {
                    Some(v) => {
                        self.set_reg(rt, v as i8 as i32 as u32);
                        true
                    }
                    None => false,
                }
            }
            0b100 => {
                // LDR
                match self.data_r_u32(mem, addr) {
                    Some(v) => {
                        self.set_reg(rt, v);
                        true
                    }
                    None => false,
                }
            }
            0b101 => {
                // LDRH
                match self.data_r_u16(mem, addr) {
                    Some(v) => {
                        self.set_reg(rt, v as u32);
                        true
                    }
                    None => false,
                }
            }
            0b110 => {
                // LDRB
                match self.data_r_u8(mem, addr) {
                    Some(v) => {
                        self.set_reg(rt, v as u32);
                        true
                    }
                    None => false,
                }
            }
            _ => {
                // 0b111: LDRSH
                match self.data_r_u16(mem, addr) {
                    Some(v) => {
                        self.set_reg(rt, v as i16 as i32 as u32);
                        true
                    }
                    None => false,
                }
            }
        };
        if ok {
            Some(CpuState::Normal)
        } else {
            Some(CpuState::Error(CpuError::MemoryError))
        }
    }

    // ----------------------------------------------------------------
    // 0xB000-0xBFFF: miscellaneous 16-bit instructions
    // ----------------------------------------------------------------
    fn misc(&mut self, hw: u32, pc: u32, next: u32, mem: &mut impl GuestMem) -> Option<CpuState> {
        let top = (hw >> 8) & 0xf; // bits[11:8]
        match top {
            0b0000 => {
                // ADD/SUB SP, SP, #imm7  (imm7 << 2)
                let imm = (hw & 0x7f) * 4;
                let sp = self.get_reg(13);
                let v = if (hw >> 7) & 1 == 0 {
                    sp.wrapping_add(imm) // ADD
                } else {
                    sp.wrapping_sub(imm) // SUB
                };
                self.set_reg(13, v);
                self.regs[PC] = next;
                Some(CpuState::Normal)
            }
            0b0001 | 0b0011 | 0b1001 | 0b1011 => {
                // CBZ / CBNZ  (1011 x0 i1 1 imm5 Rn ; bit11 = nonzero, bit8 = i)
                let rn = (hw & 7) as usize;
                let nonzero = (hw >> 11) & 1 != 0;
                let i = (hw >> 9) & 1;
                let imm5 = (hw >> 3) & 0x1f;
                let imm32 = (i << 6) | (imm5 << 1); // zero-extended, always forward
                let taken = if nonzero {
                    self.get_reg(rn) != 0
                } else {
                    self.get_reg(rn) == 0
                };
                if taken {
                    let target = self.get_reg(15).wrapping_add(imm32);
                    self.bx_write_pc(target | 1);
                } else {
                    self.regs[PC] = next;
                }
                Some(CpuState::Normal)
            }
            0b0010 => {
                // SXTH/SXTB/UXTH/UXTB  (bits[7:6] select)
                let rm = ((hw >> 3) & 7) as usize;
                let rd = (hw & 7) as usize;
                let v = self.get_reg(rm);
                let res = match (hw >> 6) & 3 {
                    0b00 => v as i16 as i32 as u32, // SXTH
                    0b01 => v as i8 as i32 as u32,  // SXTB
                    0b10 => v & 0xffff,             // UXTH
                    _ => v & 0xff,                  // UXTB
                };
                self.set_reg(rd, res);
                self.regs[PC] = next;
                Some(CpuState::Normal)
            }
            0b0100 | 0b0101 | 0b1100 | 0b1101 => {
                // PUSH (0b010) / POP (0b110): bit11 = L (1=POP).
                let is_pop = (hw >> 11) & 1 != 0;
                let mut list = hw & 0xff;
                if is_pop {
                    // bit8 = P (include PC)
                    if (hw >> 8) & 1 != 0 {
                        list |= 1 << 15;
                    }
                    self.pop(list, next, mem)
                } else {
                    // bit8 = M (include LR)
                    if (hw >> 8) & 1 != 0 {
                        list |= 1 << 14;
                    }
                    self.push(list, next, mem)
                }
            }
            0b1010 => {
                // REV / REV16 / REVSH  (bits[7:6] select); also CBZ handled above.
                let rm = ((hw >> 3) & 7) as usize;
                let rd = (hw & 7) as usize;
                let v = self.get_reg(rm);
                let res = match (hw >> 6) & 3 {
                    0b00 => v.swap_bytes(), // REV
                    0b01 => {
                        // REV16: swap bytes within each halfword
                        ((v & 0x00ff_00ff) << 8) | ((v & 0xff00_ff00) >> 8)
                    }
                    0b11 => {
                        // REVSH: rev low halfword bytes, sign-extend
                        let lo = ((v & 0xff) << 8) | ((v >> 8) & 0xff);
                        lo as u16 as i16 as i32 as u32
                    }
                    _ => {
                        // 0b10 not a valid REV op here → undefined
                        self.regs[PC] = next;
                        return Some(CpuState::Error(CpuError::UndefinedInstruction));
                    }
                };
                self.set_reg(rd, res);
                self.regs[PC] = next;
                Some(CpuState::Normal)
            }
            0b1110 => {
                // BKPT #imm8
                self.regs[PC] = next;
                Some(CpuState::Error(CpuError::Breakpoint))
            }
            0b1111 => {
                // IT and hints (NOP/YIELD/WFE/WFI/SEV).
                if hw & 0x000f != 0 {
                    // IT block — handled by another group.
                    None
                } else {
                    // Hints: treat as NOP.
                    self.regs[PC] = next;
                    Some(CpuState::Normal)
                }
            }
            _ => {
                let _ = pc;
                None
            }
        }
    }

    /// PUSH: store registers in `list` to a full-descending stack at SP.
    fn push(&mut self, list: u32, next: u32, mem: &mut impl GuestMem) -> Option<CpuState> {
        let count = list.count_ones();
        let sp = self.get_reg(13);
        let start = sp.wrapping_sub(4 * count);
        let mut addr = start;
        for i in 0..16 {
            if list & (1 << i) != 0 {
                // Lowest-numbered register at lowest address.
                if !self.data_w_u32(mem, addr, self.get_reg(i)) {
                    self.set_reg(13, start);
                    self.regs[PC] = next;
                    return Some(CpuState::Error(CpuError::MemoryError));
                }
                addr = addr.wrapping_add(4);
            }
        }
        self.set_reg(13, start);
        self.regs[PC] = next;
        Some(CpuState::Normal)
    }

    /// POP: load registers in `list` from a full-descending stack at SP.
    fn pop(&mut self, list: u32, next: u32, mem: &mut impl GuestMem) -> Option<CpuState> {
        let count = list.count_ones();
        let sp = self.get_reg(13);
        let mut addr = sp;
        let new_sp = sp.wrapping_add(4 * count);
        let mut pc_target: Option<u32> = None;
        for i in 0..16 {
            if list & (1 << i) != 0 {
                let Some(v) = self.data_r_u32(mem, addr) else {
                    self.set_reg(13, new_sp);
                    self.regs[PC] = next;
                    return Some(CpuState::Error(CpuError::MemoryError));
                };
                if i == 15 {
                    pc_target = Some(v);
                } else {
                    self.set_reg(i, v);
                }
                addr = addr.wrapping_add(4);
            }
        }
        self.set_reg(13, new_sp);
        if let Some(t) = pc_target {
            self.bx_write_pc(t);
        } else {
            self.regs[PC] = next;
        }
        Some(CpuState::Normal)
    }

    // ----------------------------------------------------------------
    // 0xC000-0xCFFF: LDM/STM (increment after)
    // ----------------------------------------------------------------
    fn ldm_stm(&mut self, hw: u32, next: u32, mem: &mut impl GuestMem) -> Option<CpuState> {
        let load = (hw >> 11) & 1 != 0;
        let rn = ((hw >> 8) & 7) as usize;
        let list = hw & 0xff;
        let count = list.count_ones();
        let base = self.get_reg(rn);
        let new_base = base.wrapping_add(4 * count);
        // Writeback unless (LDM and Rn in list): then no writeback.
        let wback = !(load && (list & (1 << rn) != 0));
        let mut addr = base;

        if load {
            for i in 0..8 {
                if list & (1 << i) != 0 {
                    let Some(v) = self.data_r_u32(mem, addr) else {
                        if wback {
                            self.set_reg(rn, new_base);
                        }
                        self.regs[PC] = next;
                        return Some(CpuState::Error(CpuError::MemoryError));
                    };
                    self.set_reg(i, v);
                    addr = addr.wrapping_add(4);
                }
            }
        } else {
            for i in 0..8 {
                if list & (1 << i) != 0 {
                    if !self.data_w_u32(mem, addr, self.get_reg(i)) {
                        if wback {
                            self.set_reg(rn, new_base);
                        }
                        self.regs[PC] = next;
                        return Some(CpuState::Error(CpuError::MemoryError));
                    }
                    addr = addr.wrapping_add(4);
                }
            }
        }
        if wback {
            self.set_reg(rn, new_base);
        }
        self.regs[PC] = next;
        Some(CpuState::Normal)
    }
}

// ====================================================================
// Differential tests vs. the dynarmic oracle.
// ====================================================================
