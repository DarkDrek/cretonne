//! Value locations.
//!
//! The register allocator assigns every SSA value to either a register or a stack slot. This
//! assignment is represented by a `ValueLoc` object.

use isa::{RegInfo, RegUnit};
use ir::StackSlot;
use std::fmt;

/// Value location.
#[derive(Copy, Clone, Debug)]
pub enum ValueLoc {
    /// This value has not been assigned to a location yet.
    Unassigned,
    /// Value is assigned to a register.
    Reg(RegUnit),
    /// Value is assigned to a stack slot.
    Stack(StackSlot),
}

impl Default for ValueLoc {
    fn default() -> Self {
        ValueLoc::Unassigned
    }
}

impl ValueLoc {
    /// Get the register unit of this location, or panic.
    pub fn unwrap_reg(self) -> RegUnit {
        match self {
            ValueLoc::Reg(ru) => ru,
            _ => panic!("Expected register: {:?}", self),
        }
    }

    /// Return an object that can display this value location, using the register info from the
    /// target ISA.
    pub fn display<'a, R: Into<Option<&'a RegInfo>>>(self, regs: R) -> DisplayValueLoc<'a> {
        DisplayValueLoc(self, regs.into())
    }
}

/// Displaying a `ValueLoc` correctly requires the associated `RegInfo` from the target ISA.
/// Without the register info, register units are simply show as numbers.
///
/// The `DisplayValueLoc` type can display the contained `ValueLoc`.
pub struct DisplayValueLoc<'a>(ValueLoc, Option<&'a RegInfo>);

impl<'a> fmt::Display for DisplayValueLoc<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.0 {
            ValueLoc::Unassigned => write!(f, "-"),
            ValueLoc::Reg(ru) => {
                match self.1 {
                    Some(regs) => write!(f, "{}", regs.display_regunit(ru)),
                    None => write!(f, "%{}", ru),
                }
            }
            ValueLoc::Stack(ss) => write!(f, "{}", ss),
        }
    }
}

/// Function argument location.
///
/// The ABI specifies how arguments are passed to a function, and where return values appear after
/// the call. Just like a `ValueLoc`, function arguments can be passed in registers or on the
/// stack.
///
/// Function arguments on the stack are accessed differently for the incoming arguments to the
/// current function and the outgoing arguments to a called external function. For this reason,
/// the location of stack arguments is described as an offset into the array of function arguments
/// on the stack.
///
/// An `ArgumentLoc` can be translated to a `ValueLoc` only when we know if we're talking about an
/// incoming argument or an outgoing argument.
///
/// - For stack arguments, different `StackSlot` entities are used to represent incoming and
///   outgoing arguments.
/// - For register arguments, there is usually no difference, but if we ever add support for a
///   register-window ISA like SPARC, register arguments would also need to be translated.
#[derive(Copy, Clone, Debug)]
pub enum ArgumentLoc {
    /// This argument has not been assigned to a location yet.
    Unassigned,
    /// Argument is passed in a register.
    Reg(RegUnit),
    /// Argument is passed on the stack, at the given byte offset into the argument array.
    Stack(u32),
}

impl Default for ArgumentLoc {
    fn default() -> Self {
        ArgumentLoc::Unassigned
    }
}

impl ArgumentLoc {
    /// Is this an assigned location? (That is, not `Unassigned`).
    pub fn is_assigned(&self) -> bool {
        match self {
            &ArgumentLoc::Unassigned => false,
            _ => true,
        }
    }

    /// Return an object that can display this argument location, using the register info from the
    /// target ISA.
    pub fn display<'a, R: Into<Option<&'a RegInfo>>>(self, regs: R) -> DisplayArgumentLoc<'a> {
        DisplayArgumentLoc(self, regs.into())
    }
}

/// Displaying a `ArgumentLoc` correctly requires the associated `RegInfo` from the target ISA.
/// Without the register info, register units are simply show as numbers.
///
/// The `DisplayArgumentLoc` type can display the contained `ArgumentLoc`.
pub struct DisplayArgumentLoc<'a>(ArgumentLoc, Option<&'a RegInfo>);

impl<'a> fmt::Display for DisplayArgumentLoc<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.0 {
            ArgumentLoc::Unassigned => write!(f, "-"),
            ArgumentLoc::Reg(ru) => {
                match self.1 {
                    Some(regs) => write!(f, "{}", regs.display_regunit(ru)),
                    None => write!(f, "%{}", ru),
                }
            }
            ArgumentLoc::Stack(offset) => write!(f, "{}", offset),
        }
    }
}
