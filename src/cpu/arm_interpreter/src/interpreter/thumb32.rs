/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Thumb-2 (Thumb-32) common integer instruction group for the pure-Rust
//! interpreter (optional portable no-JIT backend).
//!
//! Covers the high-frequency 32-bit Thumb encodings per ARMv7-A ARM (DDI
//! 0406C): data-processing modified-immediate / plain-binary-immediate /
//! shifted-register, multiply, single + multiple load/store, branches, and the
//! misc (CLZ/RBIT/REV/extends, hint NOPs / barriers).
//!
//! Entry point `exec_thumb32` returns:
//!   * `None`             — not recognised by this group (coprocessor/VFP space,
//!                          or a sub-encoding we don't implement) → dispatcher
//!                          falls through to the next group / UNIMPL.
//!   * `Some(Normal)`     — executed; PC already advanced (or branch target
//!                          written).
//!   * `Some(Svc(imm))`   — not produced here (Thumb SVC is a 16-bit insn).
//!   * `Some(Error(MemoryError))` — a load/store faulted; PC advanced first.

use super::InterpreterCpu;
use crate::mem::GuestMem;
use crate::{CpuError, CpuState};

impl InterpreterCpu {
    /// Decode + execute a 32-bit Thumb-2 instruction.
    /// `hw0`/`hw1` are the two halfwords (little-endian order in memory: hw0 is
    /// at `pc`, hw1 at `pc+2`). `pc` is the raw PC of the instruction.
    pub(super) fn exec_thumb32(
        &mut self,
        hw0: u16,
        hw1: u16,
        pc: u32,
        mem: &mut impl GuestMem,
    ) -> Option<CpuState> {
        let insn = ((hw0 as u32) << 16) | hw1 as u32;
        if let Some(state) = self.t32_system(insn, pc) {
            return Some(state);
        }

        // Top-level split per ARM ARM A6.3: bits[15:11] of hw0 select the class.
        //   op1 = insn[28:27]  (i.e. hw0[12:11])
        //   op  = insn[15]     (i.e. hw1[15])
        // We don't use that table verbatim; instead match on the well-known
        // fixed-bit patterns below.

        let op1 = (insn >> 27) & 0b11; // 0b11 always (Thumb32 marker is 0b111xx)
        debug_assert!((insn >> 29) == 0b111);

        match op1 {
            0b01 => self.t32_ldstm_dp_reg(insn, pc, mem),
            0b10 => {
                if (insn >> 15) & 1 != 0 {
                    // Branches and misc control.
                    self.t32_branches(insn, pc)
                } else {
                    // Data-processing (modified / plain immediate).
                    self.t32_data_immediate(insn, pc)
                }
            }
            0b11 => self.t32_ldst_single_dp_mul(insn, pc, mem),
            _ => None,
        }
    }

    // ===================================================================
    //  Helpers shared by this group
    // ===================================================================

    /// ThumbExpandImm_C (ARM ARM A6.3.2). Returns (imm32, carry_out).
    fn thumb_expand_imm_c(imm12: u32, carry_in: bool) -> (u32, bool) {
        if (imm12 >> 10) & 0b11 == 0 {
            // imm12[11:10] == 00 — simple zero-extended forms.
            let imm8 = imm12 & 0xff;
            let val = match (imm12 >> 8) & 0b11 {
                0b00 => imm8,
                0b01 => (imm8 << 16) | imm8,
                0b10 => (imm8 << 24) | (imm8 << 8),
                _ => (imm8 << 24) | (imm8 << 16) | (imm8 << 8) | imm8,
            };
            (val, carry_in)
        } else {
            // Rotate of (0b1:imm12[6:0]) right by imm12[11:7].
            let unrot = 0x80 | (imm12 & 0x7f);
            let rot = (imm12 >> 7) & 0x1f;
            let val = unrot.rotate_right(rot);
            (val, (val >> 31) & 1 != 0)
        }
    }

    /// Sign-extend `val` from `bits` bits.
    fn sign_extend(val: u32, bits: u32) -> u32 {
        let shift = 32 - bits;
        (((val << shift) as i32) >> shift) as u32
    }

    /// Decode (type, amount) for a 5-bit imm shift field (imm3:imm2 → imm5).
    /// Returns (stype, amount) per DecodeImmShift. Handles LSL/LSR/ASR/ROR/RRX.
    fn decode_imm_shift(stype: u32, imm5: u32) -> (u32, u32) {
        match stype & 3 {
            0 => (0, imm5),
            1 => (1, if imm5 == 0 { 32 } else { imm5 }),
            2 => (2, if imm5 == 0 { 32 } else { imm5 }),
            _ => {
                if imm5 == 0 {
                    (3, 0) // RRX: stype ROR, amount 0 → caller uses rrx
                } else {
                    (3, imm5)
                }
            }
        }
    }

    /// Apply shift with carry, honouring the RRX special case.
    fn shift_imm_c(&self, x: u32, stype: u32, amount: u32, carry_in: bool) -> (u32, bool) {
        if stype == 3 && amount == 0 {
            // RRX
            Self::rrx_c(x, carry_in)
        } else {
            Self::shift_c(x, stype, amount, carry_in)
        }
    }

    // ===================================================================
    //  Data-processing: immediate (op1 == 0b10, op == 0)
    //  Covers modified-immediate and plain-binary-immediate.
    // ===================================================================
    fn t32_data_immediate(&mut self, insn: u32, pc: u32) -> Option<CpuState> {
        let i = (insn >> 26) & 1; // hw0 bit 10
        let op = (insn >> 20) & 0x1f; // hw0[9:4] minus top? actually insn[24:20]
        let rn = ((insn >> 16) & 0xf) as usize;
        let s = (insn >> 20) & 1 != 0;
        let imm3 = (insn >> 12) & 0x7;
        let rd = ((insn >> 8) & 0xf) as usize;
        let imm8 = insn & 0xff;

        let plain = (insn >> 25) & 1 != 0; // bit 25 (hw0 bit 9): 1 = plain binary imm

        if !plain {
            // ---- Data-processing (modified immediate) A6.3.1 ----
            let imm12 = (i << 11) | (imm3 << 8) | imm8;
            let (imm32, carry) = Self::thumb_expand_imm_c(imm12, self.flag_c());
            let op4 = (insn >> 21) & 0xf;
            return self.t32_dp_modified(op4, s, rn, rd, imm32, carry, pc);
        }

        // ---- Plain binary immediate A6.3.3 ----
        let _ = op;
        let op_pb = (insn >> 20) & 0x1f; // insn[24:20]
        match op_pb {
            0b00000 => {
                // ADDW (ADD imm12, T4) / ADR (if rn==15)
                let imm12 = (i << 11) | (imm3 << 8) | imm8;
                if rn == 15 {
                    // ADR.W (add form): rd = Align(PC,4) + imm12
                    let base = self.get_reg_align(15);
                    self.set_reg(rd, base.wrapping_add(imm12));
                } else {
                    self.set_reg(rd, self.get_reg(rn).wrapping_add(imm12));
                }
                self.regs[15] = pc.wrapping_add(4);
                Some(CpuState::Normal)
            }
            0b00100 => {
                // MOVW (T3): imm16 = imm4:i:imm3:imm8
                let imm4 = (insn >> 16) & 0xf;
                let imm16 = (imm4 << 12) | (i << 11) | (imm3 << 8) | imm8;
                self.set_reg(rd, imm16);
                self.regs[15] = pc.wrapping_add(4);
                Some(CpuState::Normal)
            }
            0b01010 => {
                // SUBW (SUB imm12, T4) / ADR (sub form, if rn==15)
                let imm12 = (i << 11) | (imm3 << 8) | imm8;
                if rn == 15 {
                    let base = self.get_reg_align(15);
                    self.set_reg(rd, base.wrapping_sub(imm12));
                } else {
                    self.set_reg(rd, self.get_reg(rn).wrapping_sub(imm12));
                }
                self.regs[15] = pc.wrapping_add(4);
                Some(CpuState::Normal)
            }
            0b01100 => {
                // MOVT (T1): top imm16 into rd[31:16]
                let imm4 = (insn >> 16) & 0xf;
                let imm16 = (imm4 << 12) | (i << 11) | (imm3 << 8) | imm8;
                let cur = self.get_reg(rd) & 0x0000_ffff;
                self.set_reg(rd, cur | (imm16 << 16));
                self.regs[15] = pc.wrapping_add(4);
                Some(CpuState::Normal)
            }
            0b10100 => {
                // SBFX (T1)
                let lsb = ((imm3 << 2) | ((insn >> 6) & 3)) & 0x1f;
                let widthm1 = insn & 0x1f;
                let width = widthm1 + 1;
                let val = self.get_reg(rn);
                let field = (val >> lsb) & ((((1u64 << width) - 1) as u32) | 0);
                let res = Self::sign_extend(field, width);
                self.set_reg(rd, res);
                self.regs[15] = pc.wrapping_add(4);
                Some(CpuState::Normal)
            }
            0b10110 => {
                // BFI / BFC (T1). msb in imm5 field (insn[4:0]); lsb same as above.
                let lsb = ((imm3 << 2) | ((insn >> 6) & 3)) & 0x1f;
                let msb = insn & 0x1f;
                if msb < lsb {
                    self.regs[15] = pc.wrapping_add(4);
                    return Some(CpuState::Normal);
                }
                let width = msb - lsb + 1;
                let mask = if width >= 32 {
                    0xffff_ffffu32
                } else {
                    ((1u32 << width) - 1) << lsb
                };
                let dest = self.get_reg(rd) & !mask;
                let src = if rn == 15 {
                    // BFC: insert zeros
                    0
                } else {
                    (self.get_reg(rn) << lsb) & mask
                };
                self.set_reg(rd, dest | src);
                self.regs[15] = pc.wrapping_add(4);
                Some(CpuState::Normal)
            }
            0b11100 => {
                // UBFX (T1)
                let lsb = ((imm3 << 2) | ((insn >> 6) & 3)) & 0x1f;
                let widthm1 = insn & 0x1f;
                let width = widthm1 + 1;
                let val = self.get_reg(rn);
                let mask = if width >= 32 {
                    0xffff_ffffu32
                } else {
                    (1u32 << width) - 1
                };
                let res = (val >> lsb) & mask;
                self.set_reg(rd, res);
                self.regs[15] = pc.wrapping_add(4);
                Some(CpuState::Normal)
            }
            _ => None,
        }
    }

    /// Data-processing modified immediate, op4 = insn[24:21].
    #[allow(clippy::too_many_arguments)]
    fn t32_dp_modified(
        &mut self,
        op4: u32,
        s: bool,
        rn: usize,
        rd: usize,
        imm32: u32,
        carry: bool,
        pc: u32,
    ) -> Option<CpuState> {
        let n = self.get_reg(rn);
        match op4 {
            0b0000 => {
                // AND / TST (rd==15, s==1)
                let res = n & imm32;
                if rd == 15 {
                    // TST
                    if !s {
                        return None;
                    }
                    self.set_nz(res);
                    self.set_c_flag(carry);
                } else {
                    self.set_reg(rd, res);
                    if s {
                        self.set_nz(res);
                        self.set_c_flag(carry);
                    }
                }
            }
            0b0001 => {
                // BIC
                let res = n & !imm32;
                self.set_reg(rd, res);
                if s {
                    self.set_nz(res);
                    self.set_c_flag(carry);
                }
            }
            0b0010 => {
                // ORR / MOV (rn==15)
                let res = if rn == 15 { imm32 } else { n | imm32 };
                self.set_reg(rd, res);
                if s {
                    self.set_nz(res);
                    self.set_c_flag(carry);
                }
            }
            0b0011 => {
                // ORN / MVN (rn==15)
                let res = if rn == 15 { !imm32 } else { n | !imm32 };
                self.set_reg(rd, res);
                if s {
                    self.set_nz(res);
                    self.set_c_flag(carry);
                }
            }
            0b0100 => {
                // EOR / TEQ (rd==15, s==1)
                let res = n ^ imm32;
                if rd == 15 {
                    if !s {
                        return None;
                    }
                    self.set_nz(res);
                    self.set_c_flag(carry);
                } else {
                    self.set_reg(rd, res);
                    if s {
                        self.set_nz(res);
                        self.set_c_flag(carry);
                    }
                }
            }
            0b1000 => {
                // ADD / CMN (rd==15, s==1)
                let (res, c, v) = Self::add_with_carry(n, imm32, false);
                if rd == 15 {
                    if !s {
                        return None;
                    }
                    self.set_nzcv(res, c, v);
                } else {
                    self.set_reg(rd, res);
                    if s {
                        self.set_nzcv(res, c, v);
                    }
                }
            }
            0b1010 => {
                // ADC
                let (res, c, v) = Self::add_with_carry(n, imm32, self.flag_c());
                self.set_reg(rd, res);
                if s {
                    self.set_nzcv(res, c, v);
                }
            }
            0b1011 => {
                // SBC
                let (res, c, v) = Self::add_with_carry(n, !imm32, self.flag_c());
                self.set_reg(rd, res);
                if s {
                    self.set_nzcv(res, c, v);
                }
            }
            0b1101 => {
                // SUB / CMP (rd==15, s==1)
                let (res, c, v) = Self::add_with_carry(n, !imm32, true);
                if rd == 15 {
                    if !s {
                        return None;
                    }
                    self.set_nzcv(res, c, v);
                } else {
                    self.set_reg(rd, res);
                    if s {
                        self.set_nzcv(res, c, v);
                    }
                }
            }
            0b1110 => {
                // RSB
                let (res, c, v) = Self::add_with_carry(!n, imm32, true);
                self.set_reg(rd, res);
                if s {
                    self.set_nzcv(res, c, v);
                }
            }
            _ => return None,
        }
        self.regs[15] = pc.wrapping_add(4);
        Some(CpuState::Normal)
    }

    // ===================================================================
    //  op1 == 0b01 : Load/store multiple, dual, exclusive, table branch,
    //  and data-processing (shifted register).
    // ===================================================================
    fn t32_ldstm_dp_reg(
        &mut self,
        insn: u32,
        pc: u32,
        mem: &mut impl GuestMem,
    ) -> Option<CpuState> {
        // op1==01 sub-classes (ARM ARM A6.3):
        //   insn[25]==1            -> data-processing (shifted register)
        //   insn[25]==0, bit22==0  -> load/store multiple (LDM/STM, bit22 always 0)
        //   insn[25]==0, bit22==1  -> load/store dual / exclusive / table branch
        //                             (LDRD/STRD/LDREX/STREX{,B,H,D}/TBB/TBH).
        // NB: splitting on insn[24:23] would mis-route LDREXB/H/D and TBB/TBH
        // (insn[24:23]==01, same as LDMIA) into the LDM bucket; bit22 is the
        // real discriminator.
        // op1==01 with insn[26]==1 is the coprocessor / Advanced-SIMD / VFP
        // space (VLDM/VSTM/VFP), NOT an integer load/store. Must be checked
        // BEFORE the LDM/STM split, or e.g. VPOP {d8-d15} (0xecbd8b10) would be
        // mis-decoded as `LDMIA sp!, {…, pc}` and load a garbage PC → derail.
        if (insn >> 26) & 1 != 0 {
            return self.exec_vfp(insn, pc, mem);
        }
        if (insn >> 25) & 1 != 0 {
            return self.t32_dp_shifted_reg(insn, pc);
        }
        if (insn >> 22) & 1 == 0 {
            self.t32_ldstm(insn, pc, mem)
        } else {
            self.t32_ldrd_strd(insn, pc, mem)
        }
    }

    /// LDM/STM (and PUSH.W/POP.W) — T2 encodings.
    fn t32_ldstm(&mut self, insn: u32, pc: u32, mem: &mut impl GuestMem) -> Option<CpuState> {
        let op = (insn >> 23) & 0b11; // insn[24:23]
        let w = (insn >> 21) & 1 != 0;
        let l = (insn >> 20) & 1 != 0;
        let rn = ((insn >> 16) & 0xf) as usize;
        let reglist = insn & 0xffff;

        // op: 01 = IA (LDMIA/STMIA, includes POP.W); 10 = DB (STMDB, includes PUSH.W)
        let (increment, before) = match op {
            0b01 => (true, false), // IA
            0b10 => (false, true), // DB
            _ => return None,
        };
        let n = reglist.count_ones();
        if n == 0 {
            return None;
        }

        let base = self.get_reg(rn);
        let addr_start = if increment {
            base
        } else {
            base.wrapping_sub(4 * n)
        };
        let new_base = if increment {
            base.wrapping_add(4 * n)
        } else {
            base.wrapping_sub(4 * n)
        };

        let _ = before;
        // For IA: addresses go base, base+4, ...; for DB: base-4*n, ..., base-4.
        let mut addr = addr_start;
        // Writeback BEFORE memory if it's STM with wback (the stored value of rn,
        // if in list, is the original). ARM ARM: for STM, if rn is in list and is
        // not the lowest-numbered, the stored value is UNKNOWN — we store the
        // original base for simplicity (matches common cases). Apply wback after.
        for r in 0..16 {
            if reglist & (1 << r) == 0 {
                continue;
            }
            if l {
                // load
                let Some(v) = self.data_r_u32(mem, addr) else {
                    self.regs[15] = pc.wrapping_add(4);
                    return Some(CpuState::Error(CpuError::MemoryError));
                };
                if r == 15 {
                    // LDM with PC in list: interworking branch.
                    if w {
                        self.set_reg(rn, new_base);
                    }
                    self.bx_write_pc(v);
                    return Some(CpuState::Normal);
                }
                self.set_reg(r as usize, v);
            } else {
                // store
                let val = self.get_reg(r as usize);
                if !self.data_w_u32(mem, addr, val) {
                    self.regs[15] = pc.wrapping_add(4);
                    return Some(CpuState::Error(CpuError::MemoryError));
                }
            }
            addr = addr.wrapping_add(4);
        }
        if w {
            self.set_reg(rn, new_base);
        }
        self.regs[15] = pc.wrapping_add(4);
        Some(CpuState::Normal)
    }

    /// Load/store exclusive (LDREX/STREX{,B,H,D}) and table branch (TBB/TBH).
    /// Single-thread exclusive monitor: STREX succeeds iff a prior LDREX to the
    /// same address set the monitor. rt = bits[15:12], rt2 = bits[11:8].
    fn t32_exclusive_or_tb(
        &mut self,
        op5: u32,
        rn: usize,
        rt: usize,
        rt2: usize,
        insn: u32,
        pc: u32,
        mem: &mut impl GuestMem,
    ) -> Option<CpuState> {
        let next = pc.wrapping_add(4);
        let base = self.get_reg(rn);
        match op5 {
            0b00100 => {
                // STREX Rd, Rt, [Rn, #imm8<<2]  (Rd = bits[11:8] = rt2)
                let addr = base.wrapping_add((insn & 0xff) << 2);
                let res = if self.excl_check_clear(addr, 4) {
                    if !self.data_w_u32(mem, addr, self.get_reg(rt)) {
                        self.regs[15] = next;
                        return Some(CpuState::Error(CpuError::MemoryError));
                    }
                    0
                } else {
                    1
                };
                self.set_reg(rt2, res);
                self.regs[15] = next;
                Some(CpuState::Normal)
            }
            0b00101 => {
                // LDREX Rt, [Rn, #imm8<<2]
                let addr = base.wrapping_add((insn & 0xff) << 2);
                let Some(v) = self.data_r_u32(mem, addr) else {
                    self.regs[15] = next;
                    return Some(CpuState::Error(CpuError::MemoryError));
                };
                self.set_reg(rt, v);
                self.excl_set(addr, 4);
                self.regs[15] = next;
                Some(CpuState::Normal)
            }
            0b01100 | 0b01101 => {
                let load = op5 & 1 != 0;
                let op3 = (insn >> 4) & 0xf;
                let rd = (insn & 0xf) as usize;
                if load && op3 <= 1 {
                    // TBB (op3=0) / TBH (op3=1). Rm = bits[3:0].
                    let tbase = if rn == 15 { self.get_reg(15) } else { base };
                    let idx = self.get_reg(rd);
                    let off = if op3 == 1 {
                        let Some(h) = self.data_r_u16(mem, tbase.wrapping_add(idx << 1)) else {
                            self.regs[15] = next;
                            return Some(CpuState::Error(CpuError::MemoryError));
                        };
                        h as u32
                    } else {
                        let Some(b) = self.data_r_u8(mem, tbase.wrapping_add(idx)) else {
                            self.regs[15] = next;
                            return Some(CpuState::Error(CpuError::MemoryError));
                        };
                        b as u32
                    };
                    self.bx_write_pc((self.get_reg(15).wrapping_add(off << 1)) | 1);
                    return Some(CpuState::Normal);
                }
                // LDREX{B,H,D} / STREX{B,H,D}: addr = [Rn], no offset.
                let size = match op3 {
                    4 => 1,
                    5 => 2,
                    7 => 8,
                    _ => return None,
                };
                if base & (size - 1) != 0 {
                    self.regs[15] = next;
                    return Some(CpuState::Error(CpuError::MemoryError));
                }
                if load {
                    match op3 {
                        0b0100 => {
                            let Some(v) = self.data_r_u8(mem, base) else {
                                self.regs[15] = next;
                                return Some(CpuState::Error(CpuError::MemoryError));
                            };
                            self.set_reg(rt, v as u32);
                        }
                        0b0101 => {
                            let Some(v) = self.data_r_u16(mem, base) else {
                                self.regs[15] = next;
                                return Some(CpuState::Error(CpuError::MemoryError));
                            };
                            self.set_reg(rt, v as u32);
                        }
                        0b0111 => {
                            let Some(v0) = self.data_r_u32(mem, base) else {
                                self.regs[15] = next;
                                return Some(CpuState::Error(CpuError::MemoryError));
                            };
                            let Some(v1) = self.data_r_u32(mem, base.wrapping_add(4)) else {
                                self.regs[15] = next;
                                return Some(CpuState::Error(CpuError::MemoryError));
                            };
                            self.set_reg(rt, v0);
                            self.set_reg(rt2, v1);
                        }
                        _ => return None,
                    }
                    self.excl_set(base, size);
                } else {
                    let res = if self.excl_check_clear(base, size) {
                        let ok = match op3 {
                            0b0100 => self.data_w_u8(mem, base, self.get_reg(rt) as u8),
                            0b0101 => self.data_w_u16(mem, base, self.get_reg(rt) as u16),
                            0b0111 => {
                                self.data_w_u32(mem, base, self.get_reg(rt))
                                    && self.data_w_u32(mem, base.wrapping_add(4), self.get_reg(rt2))
                            }
                            _ => return None,
                        };
                        if !ok {
                            self.regs[15] = next;
                            return Some(CpuState::Error(CpuError::MemoryError));
                        }
                        0
                    } else {
                        1
                    };
                    self.set_reg(rd, res);
                }
                self.regs[15] = next;
                Some(CpuState::Normal)
            }
            _ => None,
        }
    }

    /// LDRD/STRD (immediate), T1. Also covers STRD/LDRD literal for LDRD.
    fn t32_ldrd_strd(&mut self, insn: u32, pc: u32, mem: &mut impl GuestMem) -> Option<CpuState> {
        // Must be the dual form: insn[24:20] = 1PU1W0(L) with bit20=L, and
        // op1==01, op2 in {01x}. Exclude exclusive/table-branch (op2==00x here
        // is filtered out by caller; we still guard bit 22).
        let p = (insn >> 24) & 1 != 0;
        let u = (insn >> 23) & 1 != 0;
        let wbit = (insn >> 21) & 1 != 0;
        let l = (insn >> 20) & 1 != 0;
        // bit 22 must be 1 for LDRD/STRD immediate (otherwise exclusive/TB).
        if (insn >> 22) & 1 == 0 {
            return None;
        }
        let rn = ((insn >> 16) & 0xf) as usize;
        let rt = ((insn >> 12) & 0xf) as usize;
        let rt2 = ((insn >> 8) & 0xf) as usize;
        let imm8 = insn & 0xff;
        let imm32 = imm8 << 2;

        if !p && !wbit {
            // Load/store exclusive (LDREX/STREX*) or table branch (TBB/TBH).
            return self.t32_exclusive_or_tb((insn >> 20) & 0x1f, rn, rt, rt2, insn, pc, mem);
        }

        let base = if rn == 15 {
            self.get_reg_align(15)
        } else {
            self.get_reg(rn)
        };
        let offset_addr = if u {
            base.wrapping_add(imm32)
        } else {
            base.wrapping_sub(imm32)
        };
        let addr = if p { offset_addr } else { base };

        if l {
            // LDRD
            let Some(v0) = self.data_r_u32(mem, addr) else {
                self.regs[15] = pc.wrapping_add(4);
                return Some(CpuState::Error(CpuError::MemoryError));
            };
            let Some(v1) = self.data_r_u32(mem, addr.wrapping_add(4)) else {
                self.regs[15] = pc.wrapping_add(4);
                return Some(CpuState::Error(CpuError::MemoryError));
            };
            self.set_reg(rt, v0);
            self.set_reg(rt2, v1);
        } else {
            // STRD
            let v0 = self.get_reg(rt);
            let v1 = self.get_reg(rt2);
            if !self.data_w_u32(mem, addr, v0) {
                self.regs[15] = pc.wrapping_add(4);
                return Some(CpuState::Error(CpuError::MemoryError));
            }
            if !self.data_w_u32(mem, addr.wrapping_add(4), v1) {
                self.regs[15] = pc.wrapping_add(4);
                return Some(CpuState::Error(CpuError::MemoryError));
            }
        }
        if wbit {
            self.set_reg(rn, offset_addr);
        }
        self.regs[15] = pc.wrapping_add(4);
        Some(CpuState::Normal)
    }

    // ===================================================================
    //  Data-processing (shifted register), op1 == 01, bit25 == 1.
    //  Also reached for the shifted-reg DP encodings (A6.3.11).
    // ===================================================================
    fn t32_dp_shifted_reg(&mut self, insn: u32, pc: u32) -> Option<CpuState> {
        let op4 = (insn >> 21) & 0xf;
        let s = (insn >> 20) & 1 != 0;
        let rn = ((insn >> 16) & 0xf) as usize;
        let imm3 = (insn >> 12) & 0x7;
        let rd = ((insn >> 8) & 0xf) as usize;
        let imm2 = (insn >> 6) & 0x3;
        let stype = (insn >> 4) & 0x3;
        let rm = (insn & 0xf) as usize;

        let imm5 = (imm3 << 2) | imm2;
        let (st, amount) = Self::decode_imm_shift(stype, imm5);
        let cin = self.flag_c();
        let (shifted, carry) = self.shift_imm_c(self.get_reg(rm), st, amount, cin);
        let n = self.get_reg(rn);

        match op4 {
            0b0000 => {
                // AND / TST (rd==15 & s)
                let res = n & shifted;
                if rd == 15 {
                    if !s {
                        return None;
                    }
                    self.set_nz(res);
                    self.set_c_flag(carry);
                } else {
                    self.set_reg(rd, res);
                    if s {
                        self.set_nz(res);
                        self.set_c_flag(carry);
                    }
                }
            }
            0b0001 => {
                // BIC
                let res = n & !shifted;
                self.set_reg(rd, res);
                if s {
                    self.set_nz(res);
                    self.set_c_flag(carry);
                }
            }
            0b0010 => {
                // ORR / MOV+shifts (rn==15): MOV/LSL/LSR/ASR/ROR/RRX
                let res = if rn == 15 { shifted } else { n | shifted };
                self.set_reg(rd, res);
                if s {
                    self.set_nz(res);
                    self.set_c_flag(carry);
                }
            }
            0b0011 => {
                // ORN / MVN (rn==15)
                let res = if rn == 15 { !shifted } else { n | !shifted };
                self.set_reg(rd, res);
                if s {
                    self.set_nz(res);
                    self.set_c_flag(carry);
                }
            }
            0b0100 => {
                // EOR / TEQ (rd==15 & s)
                let res = n ^ shifted;
                if rd == 15 {
                    if !s {
                        return None;
                    }
                    self.set_nz(res);
                    self.set_c_flag(carry);
                } else {
                    self.set_reg(rd, res);
                    if s {
                        self.set_nz(res);
                        self.set_c_flag(carry);
                    }
                }
            }
            0b1000 => {
                // ADD / CMN (rd==15 & s)
                let (res, c, v) = Self::add_with_carry(n, shifted, false);
                if rd == 15 {
                    if !s {
                        return None;
                    }
                    self.set_nzcv(res, c, v);
                } else {
                    self.set_reg(rd, res);
                    if s {
                        self.set_nzcv(res, c, v);
                    }
                }
            }
            0b1010 => {
                // ADC
                let (res, c, v) = Self::add_with_carry(n, shifted, self.flag_c());
                self.set_reg(rd, res);
                if s {
                    self.set_nzcv(res, c, v);
                }
            }
            0b1011 => {
                // SBC
                let (res, c, v) = Self::add_with_carry(n, !shifted, self.flag_c());
                self.set_reg(rd, res);
                if s {
                    self.set_nzcv(res, c, v);
                }
            }
            0b1101 => {
                // SUB / CMP (rd==15 & s)
                let (res, c, v) = Self::add_with_carry(n, !shifted, true);
                if rd == 15 {
                    if !s {
                        return None;
                    }
                    self.set_nzcv(res, c, v);
                } else {
                    self.set_reg(rd, res);
                    if s {
                        self.set_nzcv(res, c, v);
                    }
                }
            }
            0b1110 => {
                // RSB
                let (res, c, v) = Self::add_with_carry(!n, shifted, true);
                self.set_reg(rd, res);
                if s {
                    self.set_nzcv(res, c, v);
                }
            }
            _ => return None,
        }
        self.regs[15] = pc.wrapping_add(4);
        Some(CpuState::Normal)
    }

    // ===================================================================
    //  op1 == 0b11 : Load/store single, data-processing (register),
    //  multiply, long multiply, misc.
    // ===================================================================
    fn t32_ldst_single_dp_mul(
        &mut self,
        insn: u32,
        pc: u32,
        mem: &mut impl GuestMem,
    ) -> Option<CpuState> {
        // Coprocessor / Advanced SIMD / VFP space: insn[26]==1.
        if (insn >> 26) & 1 != 0 {
            return self.exec_vfp(insn, pc, mem);
        }

        // Advanced SIMD element/structure load/store (VLD1/VST1/…): Thumb prefix
        // 0xf9 (bits[31:24]==11111001). This shares bits[31:25] with the integer
        // signed loads LDRSB/LDRSH (also 0xf9-prefixed), but those have bit20==1
        // while every NEON element/structure load/store has bit20==0. NB its
        // insn[26]==0, so it does NOT come through the coprocessor check above.
        if insn >> 24 == 0xf9 && (insn >> 20) & 1 == 0 {
            return self.exec_vfp(insn, pc, mem);
        }

        // For op1==11, bit26==0, the sub-class selector is:
        //   insn[25]==0            -> load/store single data item
        //   insn[25]==1, insn[24]==0           -> DP (register) / misc / extends
        //   insn[25]==1, insn[24]==1, insn[23]==0 -> multiply (MUL/MLA/MLS)
        //   insn[25]==1, insn[24]==1, insn[23]==1 -> long multiply / divide
        let bit25 = (insn >> 25) & 1;
        let bit24 = (insn >> 24) & 1;

        if bit25 == 0 {
            return self.t32_ldst_single(insn, pc, mem);
        }
        if bit24 == 0 {
            return self.t32_dp_reg_misc(insn, pc);
        }
        // bit24 == 1: multiply (short) or long-multiply/divide; t32_multiply reads
        // insn[23] (the is_long bit) itself.
        self.t32_multiply(insn, pc)
    }

    /// Load/store single data item (LDR/STR/LDRB/LDRH/LDRSB/LDRSH + .W /
    /// literal / register / imm8-indexed), Thumb-2 T1..T4 encodings.
    ///
    /// Encoding (op1==11, insn[26]==0, insn[25]==0):
    ///   insn[24]    = sign (1 → LDRSB/LDRSH)
    ///   insn[22:21] = size  (00 byte, 01 half, 10 word)
    ///   insn[20]    = L     (1 load, 0 store)
    ///   insn[23]==1 → imm12 positive-offset form (P=1,U=1,W=0); also literal.
    ///   insn[23]==0 → register-offset form (insn[11:6]==000000) or
    ///                 imm8 index/wback form (insn[11]==1, P/U/W = insn[10:8]).
    fn t32_ldst_single(&mut self, insn: u32, pc: u32, mem: &mut impl GuestMem) -> Option<CpuState> {
        let rn = ((insn >> 16) & 0xf) as usize;
        let rt = ((insn >> 12) & 0xf) as usize;
        let l = (insn >> 20) & 1 != 0;
        let is_signed = (insn >> 24) & 1 != 0;
        let size_field = (insn >> 21) & 0b11; // 00 B, 01 H, 10 W

        // Access width in bytes. Signed loads are only byte/half.
        let access_bytes: u32 = match size_field {
            0b00 => 1,
            0b01 => 2,
            0b10 => 4,
            _ => return None, // size 11 not a plain single load/store here
        };
        // A word-sized signed load is meaningless; reject.
        if is_signed && access_bytes == 4 {
            return None;
        }
        // Stores are never signed.
        if is_signed && !l {
            return None;
        }

        let is_imm12 = (insn >> 23) & 1 != 0;

        // ---- address computation ----
        let addr: u32;
        let wback_addr: u32;
        let do_wback: bool;
        if rn == 15 {
            // PC-relative literal: U from insn[23], imm12.
            let base = self.get_reg_align(15);
            let imm12 = insn & 0xfff;
            let u = (insn >> 23) & 1 != 0;
            addr = if u {
                base.wrapping_add(imm12)
            } else {
                base.wrapping_sub(imm12)
            };
            wback_addr = addr;
            do_wback = false;
        } else if is_imm12 {
            // Positive imm12 offset (P=1, U=1, W=0).
            let imm12 = insn & 0xfff;
            addr = self.get_reg(rn).wrapping_add(imm12);
            wback_addr = addr;
            do_wback = false;
        } else if (insn >> 11) & 1 == 0 {
            // Register-offset form: insn[11:6]==000000, imm2=insn[5:4], rm=insn[3:0].
            let rm = (insn & 0xf) as usize;
            let imm2 = (insn >> 4) & 0x3;
            let off = self.get_reg(rm) << imm2;
            addr = self.get_reg(rn).wrapping_add(off);
            wback_addr = addr;
            do_wback = false;
        } else {
            // imm8 index/wback form (P,U,W = insn[10],insn[9],insn[8]).
            let imm8 = insn & 0xff;
            let p = (insn >> 10) & 1 != 0;
            let u = (insn >> 9) & 1 != 0;
            let w = (insn >> 8) & 1 != 0;
            let base = self.get_reg(rn);
            let offset_addr = if u {
                base.wrapping_add(imm8)
            } else {
                base.wrapping_sub(imm8)
            };
            addr = if p { offset_addr } else { base };
            wback_addr = offset_addr;
            do_wback = w || !p; // post-index (P=0) always writes back
        }

        // ---- perform the access ----
        if l {
            let loaded: u32 = match access_bytes {
                1 => {
                    let Some(b) = self.data_r_u8(mem, addr) else {
                        self.regs[15] = pc.wrapping_add(4);
                        return Some(CpuState::Error(CpuError::MemoryError));
                    };
                    if is_signed {
                        Self::sign_extend(b as u32, 8)
                    } else {
                        b as u32
                    }
                }
                2 => {
                    let Some(h) = self.data_r_u16(mem, addr) else {
                        self.regs[15] = pc.wrapping_add(4);
                        return Some(CpuState::Error(CpuError::MemoryError));
                    };
                    if is_signed {
                        Self::sign_extend(h as u32, 16)
                    } else {
                        h as u32
                    }
                }
                _ => {
                    let Some(w) = self.data_r_u32(mem, addr) else {
                        self.regs[15] = pc.wrapping_add(4);
                        return Some(CpuState::Error(CpuError::MemoryError));
                    };
                    w
                }
            };
            if do_wback {
                self.set_reg(rn, wback_addr);
            }
            if rt == 15 {
                self.bx_write_pc(loaded);
                return Some(CpuState::Normal);
            }
            self.set_reg(rt, loaded);
        } else {
            // store
            let val = self.get_reg(rt);
            let ok = match access_bytes {
                1 => self.data_w_u8(mem, addr, val as u8),
                2 => self.data_w_u16(mem, addr, val as u16),
                _ => self.data_w_u32(mem, addr, val),
            };
            if !ok {
                self.regs[15] = pc.wrapping_add(4);
                return Some(CpuState::Error(CpuError::MemoryError));
            }
            if do_wback {
                self.set_reg(rn, wback_addr);
            }
        }
        self.regs[15] = pc.wrapping_add(4);
        Some(CpuState::Normal)
    }

    /// Data-processing (register) + misc (CLZ/RBIT/REV/QADD/extends).
    fn t32_dp_reg_misc(&mut self, insn: u32, pc: u32) -> Option<CpuState> {
        let op1 = (insn >> 20) & 0xf; // insn[23:20]
        let op2 = (insn >> 4) & 0xf; // insn[7:4]
        let rn = ((insn >> 16) & 0xf) as usize;
        let rd = ((insn >> 8) & 0xf) as usize;
        let rm = (insn & 0xf) as usize;

        // Extends (SXTB/UXTB/SXTH/UXTH + the A-accumulate variants) live in
        // insn[23]==0 with op2==10xx (rotate in insn[5:4]). CLZ/REV/RBIT live in
        // insn[23]==1, so gate the extend group on insn[23]==0 to avoid clashing.
        let bit23 = (insn >> 23) & 1;

        // Register-controlled shift: LSL/LSR/ASR/ROR (register), encoding
        // 1111 1010 0 tt S Rn 1111 Rd 0000 Rm. tt=bits[22:21] picks the shift,
        // S=bit20 (explicit, no IT-block gating for 32-bit Thumb). Amount =
        // Rm[7:0].
        if bit23 == 0 && op2 == 0b0000 && (insn >> 12) & 0xf == 0xf {
            let stype = (insn >> 21) & 0b11;
            let s = (insn >> 20) & 1 != 0;
            let amount = self.get_reg(rm) & 0xff;
            let cin = self.flag_c();
            let (res, c) = Self::shift_c(self.get_reg(rn), stype, amount, cin);
            self.set_reg(rd, res);
            if s {
                self.set_nzcv(res, c, self.flag_v());
            }
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }

        if bit23 == 0 && (op2 & 0b1000) != 0 {
            // rotate amount = insn[5:4] * 8
            let rotation = ((insn >> 4) & 0x3) * 8;
            let rotated = self.get_reg(rm).rotate_right(rotation);
            // op selects the extend kind via insn[22:20].
            let kind = (insn >> 20) & 0b111;
            let res = match kind {
                0b000 => {
                    // SXTH (rn==15) / SXTAH
                    let v = Self::sign_extend(rotated & 0xffff, 16);
                    if rn == 15 {
                        v
                    } else {
                        self.get_reg(rn).wrapping_add(v)
                    }
                }
                0b001 => {
                    // UXTH / UXTAH
                    let v = rotated & 0xffff;
                    if rn == 15 {
                        v
                    } else {
                        self.get_reg(rn).wrapping_add(v)
                    }
                }
                0b010 => {
                    // SXTB16 (rn==15) / SXTAB16: 取 rotated 的 byte0、byte2,各符号扩展到
                    // 16 位,分别放进结果的低/高半字;A 变体对每个半字做 16 位加法。
                    let lo = Self::sign_extend(rotated & 0xff, 8);
                    let hi = Self::sign_extend((rotated >> 16) & 0xff, 8);
                    if rn == 15 {
                        ((hi & 0xffff) << 16) | (lo & 0xffff)
                    } else {
                        let n = self.get_reg(rn);
                        let rlo = (n & 0xffff).wrapping_add(lo) & 0xffff;
                        let rhi = ((n >> 16) & 0xffff).wrapping_add(hi) & 0xffff;
                        (rhi << 16) | rlo
                    }
                }
                0b011 => {
                    // UXTB16 (rn==15) / UXTAB16:byte0、byte2 各零扩展到 16 位,放进低/高半字;
                    // A 变体对每个半字做 16 位加法。★摩尔头像/颜色处理用到这条,缺它会 panic/卡死。
                    let lo = rotated & 0xff;
                    let hi = (rotated >> 16) & 0xff;
                    if rn == 15 {
                        (hi << 16) | lo
                    } else {
                        let n = self.get_reg(rn);
                        let rlo = (n & 0xffff).wrapping_add(lo) & 0xffff;
                        let rhi = ((n >> 16) & 0xffff).wrapping_add(hi) & 0xffff;
                        (rhi << 16) | rlo
                    }
                }
                0b100 => {
                    // SXTB / SXTAB
                    let v = Self::sign_extend(rotated & 0xff, 8);
                    if rn == 15 {
                        v
                    } else {
                        self.get_reg(rn).wrapping_add(v)
                    }
                }
                0b101 => {
                    // UXTB / UXTAB
                    let v = rotated & 0xff;
                    if rn == 15 {
                        v
                    } else {
                        self.get_reg(rn).wrapping_add(v)
                    }
                }
                _ => return None,
            };
            self.set_reg(rd, res);
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }

        // CLZ: insn[23:20]==1011, insn[7:4]==1000. rn==rm (both = Rm).
        if op1 == 0b1011 && op2 == 0b1000 {
            let res = self.get_reg(rm).leading_zeros();
            self.set_reg(rd, res);
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }
        // REV / REV16 / RBIT / REVSH: insn[23:20]==1001, op2 in 1000..1011.
        if op1 == 0b1001 {
            let m = self.get_reg(rm);
            let res = match op2 {
                0b1000 => m.swap_bytes(), // REV
                0b1001 => {
                    // REV16: swap bytes within each halfword
                    ((m & 0x00ff_00ff) << 8) | ((m & 0xff00_ff00) >> 8)
                }
                0b1010 => m.reverse_bits(), // RBIT
                0b1011 => {
                    // REVSH: REV16 on low half then sign-extend
                    let lo = ((m & 0xff) << 8) | ((m >> 8) & 0xff);
                    Self::sign_extend(lo, 16)
                }
                _ => return None,
            };
            self.set_reg(rd, res);
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }
        None
    }

    /// Multiply (MUL/MLA/MLS) and long multiply (SMULL/UMULL/SMLAL/UMLAL).
    fn t32_multiply(&mut self, insn: u32, pc: u32) -> Option<CpuState> {
        let op1 = (insn >> 20) & 0b111; // insn[22:20]
        let rn = ((insn >> 16) & 0xf) as usize;
        let ra = ((insn >> 12) & 0xf) as usize; // also RdLo for long
        let rd = ((insn >> 8) & 0xf) as usize; // also RdHi for long
        let op2 = (insn >> 4) & 0xf; // insn[7:4]
        let rm = (insn & 0xf) as usize;

        let is_long = (insn >> 23) & 1 != 0; // bit23 distinguishes (set by caller)

        if !is_long {
            // 32-bit multiply / multiply-accumulate group (ARM ARM A6.3.16),
            // selected by op1=insn[22:20] and op2=insn[7:4].
            // Signed 16-bit half selector: top half if the bit is set.
            let half = |v: u32, top: bool| -> i32 {
                if top {
                    (v >> 16) as i16 as i32
                } else {
                    v as i16 as i32
                }
            };
            let n = self.get_reg(rn);
            let m = self.get_reg(rm);
            let res: u32 = match op1 {
                0b000 => {
                    let prod = n.wrapping_mul(m);
                    match op2 {
                        0b0000 if ra == 15 => prod,                    // MUL
                        0b0000 => self.get_reg(ra).wrapping_add(prod), // MLA
                        0b0001 => self.get_reg(ra).wrapping_sub(prod), // MLS
                        _ => return None,
                    }
                }
                0b001 => {
                    // SMLA<x><y> / SMUL<x><y>: 16×16 → 32. op2 = 00:N:M.
                    if op2 & 0b1100 != 0 {
                        return None;
                    }
                    let prod = half(n, op2 & 0b10 != 0).wrapping_mul(half(m, op2 & 0b01 != 0));
                    if ra == 15 {
                        prod as u32 // SMUL<x><y>
                    } else {
                        (self.get_reg(ra) as i32).wrapping_add(prod) as u32 // SMLA<x><y>
                    }
                }
                0b011 => {
                    // SMLAW<y> / SMULW<y>: 32×16 → top 32. op2 = 000:M.
                    if op2 & 0b1110 != 0 {
                        return None;
                    }
                    let prod = (n as i32 as i64).wrapping_mul(half(m, op2 & 1 != 0) as i64);
                    let top = (prod >> 16) as i32;
                    if ra == 15 {
                        top as u32 // SMULW<y>
                    } else {
                        (self.get_reg(ra) as i32).wrapping_add(top) as u32 // SMLAW<y>
                    }
                }
                0b010 | 0b100 => {
                    // SMUAD/SMLAD (op1=010) and SMUSD/SMLSD (op1=100). op2=000:X
                    // (X swaps Rm halves). dual 16×16.
                    if op2 & 0b1110 != 0 {
                        return None;
                    }
                    let swap = op2 & 1 != 0;
                    let (ml, mh) = if swap {
                        (half(m, true), half(m, false))
                    } else {
                        (half(m, false), half(m, true))
                    };
                    let p1 = half(n, false).wrapping_mul(ml);
                    let p2 = half(n, true).wrapping_mul(mh);
                    let dual = if op1 == 0b010 {
                        (p1 as i64) + (p2 as i64) // SMUAD
                    } else {
                        (p1 as i64) - (p2 as i64) // SMUSD
                    };
                    let acc = if ra == 15 {
                        0
                    } else {
                        self.get_reg(ra) as i32 as i64
                    };
                    (dual + acc) as u32
                }
                0b101 | 0b110 => {
                    // SMMLA/SMMUL (op1=101) and SMMLS (op1=110). op2=000:R (round).
                    if op2 & 0b1110 != 0 {
                        return None;
                    }
                    let round = if op2 & 1 != 0 { 0x8000_0000i64 } else { 0 };
                    let prod = (n as i32 as i64).wrapping_mul(m as i32 as i64);
                    let acc = if ra == 15 {
                        0
                    } else {
                        (self.get_reg(ra) as i32 as i64) << 32
                    };
                    let result = if op1 == 0b101 {
                        acc.wrapping_add(prod).wrapping_add(round) // SMMLA/SMMUL
                    } else {
                        acc.wrapping_sub(prod).wrapping_add(round) // SMMLS
                    };
                    (result >> 32) as u32
                }
                _ => return None,
            };
            self.set_reg(rd, res);
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }

        // Long multiply: insn[22:20] selects.
        //   010 SMULL, 110 SMLAL (op2=0000); 0xA?  -> let's map precisely:
        //   op1: 000 SMULL, 001 (UDIV/SDIV handled elsewhere), 010 UMULL,
        //        100 SMLAL, 110 UMLAL  (per A6.3.16; encodings vary)
        // ARM ARM A6.3.16 Long multiply, op1 = insn[22:20]:
        //   000 -> SMULL  (op2=0000)
        //   010 -> UMULL  (op2=0000)
        //   100 -> SMLAL  (op2=0000)
        //   110 -> UMLAL  (op2=0000)
        //   001 -> SDIV   (op2=1111)
        //   011 -> UDIV   (op2=1111)
        let rdlo = ra;
        let rdhi = rd;
        match op1 {
            0b000 if op2 == 0b0000 => {
                // SMULL
                let res = (self.get_reg(rn) as i32 as i64)
                    .wrapping_mul(self.get_reg(rm) as i32 as i64) as u64;
                self.set_reg(rdlo, res as u32);
                self.set_reg(rdhi, (res >> 32) as u32);
            }
            0b010 if op2 == 0b0000 => {
                // UMULL
                let res = (self.get_reg(rn) as u64).wrapping_mul(self.get_reg(rm) as u64);
                self.set_reg(rdlo, res as u32);
                self.set_reg(rdhi, (res >> 32) as u32);
            }
            0b100 if op2 == 0b0000 => {
                // SMLAL
                let acc = ((self.get_reg(rdhi) as u64) << 32) | (self.get_reg(rdlo) as u64);
                let prod =
                    (self.get_reg(rn) as i32 as i64).wrapping_mul(self.get_reg(rm) as i32 as i64);
                let res = (acc as i64).wrapping_add(prod) as u64;
                self.set_reg(rdlo, res as u32);
                self.set_reg(rdhi, (res >> 32) as u32);
            }
            0b110 if op2 == 0b0000 => {
                // UMLAL
                let acc = ((self.get_reg(rdhi) as u64) << 32) | (self.get_reg(rdlo) as u64);
                let prod = (self.get_reg(rn) as u64).wrapping_mul(self.get_reg(rm) as u64);
                let res = acc.wrapping_add(prod);
                self.set_reg(rdlo, res as u32);
                self.set_reg(rdhi, (res >> 32) as u32);
            }
            0b001 if op2 == 0b1111 => {
                // SDIV (RdHi field is Rd, RdLo unused / =1111)
                let dividend = self.get_reg(rn) as i32;
                let divisor = self.get_reg(rm) as i32;
                let q = if divisor == 0 {
                    0
                } else if dividend == i32::MIN && divisor == -1 {
                    i32::MIN
                } else {
                    dividend.wrapping_div(divisor)
                };
                self.set_reg(rd, q as u32);
            }
            0b011 if op2 == 0b1111 => {
                // UDIV
                let dividend = self.get_reg(rn);
                let divisor = self.get_reg(rm);
                let q = if divisor == 0 { 0 } else { dividend / divisor };
                self.set_reg(rd, q);
            }
            _ => return None,
        }
        self.regs[15] = pc.wrapping_add(4);
        Some(CpuState::Normal)
    }

    // ===================================================================
    //  op1 == 0b10, op == 1 : Branches and miscellaneous control.
    // ===================================================================
    fn t32_system(&mut self, insn: u32, pc: u32) -> Option<CpuState> {
        if insn == 0xf3bf_8f2f {
            // CLREX
            self.excl_clear();
        } else if matches!(insn & 0xffff_fff0, 0xf3bf_8f40 | 0xf3bf_8f50 | 0xf3bf_8f60) {
            // DSB/DMB/ISB: this interpreter executes guest memory sequentially.
        } else if (insn & 0xffff_fff0) == 0xf3af_8000 && (insn & 15) <= 4 {
            // NOP/YIELD/WFE/WFI/SEV are user-mode hints.
        } else if (insn & 0xffff_f0ff) == 0xf3ef_8000 {
            // MRS CPSR
            let destination = ((insn >> 8) & 15) as usize;
            if destination == 15 {
                return None;
            }
            self.regs[destination] = self.cpsr;
        } else if (insn & 0xfff0_f0ff) == 0xf380_8000 {
            // MSR register
            let source = ((insn >> 16) & 15) as usize;
            if source == 15 || !self.write_status((insn >> 8) & 15, self.get_reg(source)) {
                return None;
            }
        } else {
            return None;
        }
        self.regs[15] = pc.wrapping_add(4);
        Some(CpuState::Normal)
    }

    fn t32_branches(&mut self, insn: u32, pc: u32) -> Option<CpuState> {
        // hw1 layout for branches (hw1 = insn[15:0]):
        //   hw1[15] = 1
        //   hw1[14] = J1-position bit that, together with hw1[12], selects form:
        //     hw1[12]==0 -> conditional B (T3) or misc-control/hints
        //     hw1[12]==1, hw1[14]==0 -> B.W unconditional (T4)
        //     hw1[12]==1, hw1[14]==1 -> BL (T1)
        //   J1 = hw1[13], J2 = hw1[11], S = insn[26].
        let hw1_15 = (insn >> 15) & 1;
        let hw1_14 = (insn >> 14) & 1;
        let hw1_12 = (insn >> 12) & 1;
        if hw1_15 == 0 {
            return None;
        }
        let s = (insn >> 26) & 1;
        let j1 = (insn >> 13) & 1;
        let j2 = (insn >> 11) & 1;

        if hw1_12 == 1 {
            // BL (hw1[14]==1) or B.W unconditional (hw1[14]==0). Shared imm form:
            //   imm = S:I1:I2:imm10:imm11:'0' ; I1 = NOT(J1 EOR S), I2 = NOT(J2 EOR S).
            let imm10 = (insn >> 16) & 0x3ff;
            let imm11 = insn & 0x7ff;
            let i1 = 1 ^ (j1 ^ s);
            let i2 = 1 ^ (j2 ^ s);
            let imm = (s << 24) | (i1 << 23) | (i2 << 22) | (imm10 << 12) | (imm11 << 1);
            let offset = Self::sign_extend(imm, 25);
            // Target = read-value of PC (pc+4 in Thumb) + offset. Stays Thumb.
            let target = self.get_reg(15).wrapping_add(offset);
            if hw1_14 == 1 {
                // BL: LR = next-instruction address with Thumb bit set.
                self.set_reg(14, pc.wrapping_add(4) | 1);
            }
            self.bx_write_pc(target | 1);
            return Some(CpuState::Normal);
        }

        // hw1[12] == 0, hw1[14] == 1 : BLX (immediate, T2) — call into ARM state.
        if hw1_14 == 1 {
            let imm10h = (insn >> 16) & 0x3ff;
            let imm10l = (insn >> 1) & 0x3ff; // hw1[10:1]
            let i1 = 1 ^ (j1 ^ s);
            let i2 = 1 ^ (j2 ^ s);
            // imm = S:I1:I2:imm10H:imm10L:'00' (25 bits, word-aligned).
            let imm = (s << 24) | (i1 << 23) | (i2 << 22) | (imm10h << 12) | (imm10l << 2);
            let offset = Self::sign_extend(imm, 25);
            // Target = Align(PC, 4) + offset; switch to ARM (target has bit0 = 0).
            let target = (self.get_reg(15) & !3).wrapping_add(offset);
            self.set_reg(14, pc.wrapping_add(4) | 1); // LR = return | Thumb
            self.bx_write_pc(target);
            return Some(CpuState::Normal);
        }

        // hw1[12] == 0, hw1[14] == 0 : conditional branch (T3) or misc-control / hints.
        let cond = (insn >> 22) & 0xf;
        if cond < 0xe {
            // B<cond> (T3): imm = S:J2:J1:imm6:imm11:'0', sign-extend 21 bits.
            let imm11 = insn & 0x7ff;
            let imm6 = (insn >> 16) & 0x3f;
            let imm = (s << 20) | (j2 << 19) | (j1 << 18) | (imm6 << 12) | (imm11 << 1);
            let offset = Self::sign_extend(imm, 21);
            if self.cond_passed(cond) {
                let target = self.get_reg(15).wrapping_add(offset);
                self.bx_write_pc(target | 1);
            } else {
                self.regs[15] = pc.wrapping_add(4);
            }
            return Some(CpuState::Normal);
        }

        // Unknown system encodings must not silently become NOPs.
        None
    }
}

// =====================================================================
//  Differential tests
// =====================================================================
