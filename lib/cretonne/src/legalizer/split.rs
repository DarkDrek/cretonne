//! Value splitting.
//!
//! Some value types are too large to fit in registers, so they need to be split into smaller parts
//! that the ISA can operate on. There's two dimensions of splitting, represented by two
//! complementary instruction pairs:
//!
//! - `isplit` and `iconcat` for splitting integer types into smaller integers.
//! - `vsplit` and `vconcat` for splitting vector types into smaller vector types with the same
//!   lane types.
//!
//! There is no floating point splitting. If an ISA doesn't support `f64` values, they probably
//! have to be bit-cast to `i64` and possibly split into two `i32` values that fit in registers.
//! This breakdown is handled by the ABI lowering.
//!
//! When legalizing a single instruction, it is wrapped in splits and concatenations:
//!
//!```cton
//!     v1 = bxor.i64 v2, v3
//! ```
//!
//! becomes:
//!
//!```cton
//!     v20, v21 = isplit v2
//!     v30, v31 = isplit v3
//!     v10 = bxor.i32 v20, v30
//!     v11 = bxor.i32 v21, v31
//!     v1 = iconcat v10, v11
//! ```
//!
//! This local expansion approach still leaves the original `i64` values in the code as operands on
//! the `split` and `concat` instructions. It also creates a lot of redundant code to clean up as
//! values are constantly split and concatenated.
//!
//! # Optimized splitting
//!
//! We can eliminate a lot of the splitting code quite easily. Whenever we need to split a value,
//! first check if the value is defined by the corresponding concatenation. If so, then just use
//! the two concatenation inputs directly:
//!
//! ```cton
//!     v4 = iadd_imm.i64 v1, 1
//! ```
//!
//! becomes, using the expanded code from above:
//!
//! ```cton
//!     v40, v5 = iadd_imm_cout.i32 v10, 1
//!     v6 = bint.i32
//!     v41 = iadd.i32 v11, v6
//!     v4 = iconcat v40, v41
//! ```
//!
//! This means that the `iconcat` instructions defining `v1` and `v4` end up with no uses, so they
//! can be trivially deleted by a dead code elimination pass.
//!
//! # EBB arguments
//!
//! If all instructions that produce an `i64` value are legalized as above, we will eventually end
//! up with no `i64` values anywhere, except for EBB arguments. We can work around this by
//! iteratively splitting EBB arguments too. That should leave us with no illegal value types
//! anywhere.
//!
//! It is possible to have circular dependencies of EBB arguments that are never used by any real
//! instructions. These loops will remain in the program.

use flowgraph::ControlFlowGraph;
use ir::{DataFlowGraph, Ebb, Inst, Cursor, Value, Type, Opcode, ValueDef, InstructionData,
         InstBuilder};
use std::iter;

/// Split `value` into two values using the `isplit` semantics. Do this by reusing existing values
/// if possible.
pub fn isplit(dfg: &mut DataFlowGraph,
              cfg: &ControlFlowGraph,
              pos: &mut Cursor,
              value: Value)
              -> (Value, Value) {
    split_any(dfg, cfg, pos, value, Opcode::Iconcat)
}

/// Split `value` into halves using the `vsplit` semantics. Do this by reusing existing values if
/// possible.
pub fn vsplit(dfg: &mut DataFlowGraph,
              cfg: &ControlFlowGraph,
              pos: &mut Cursor,
              value: Value)
              -> (Value, Value) {
    split_any(dfg, cfg, pos, value, Opcode::Vconcat)
}

/// After splitting an EBB argument, we need to go back and fix up all of the predecessor
/// instructions. This is potentially a recursive operation, but we don't implement it recursively
/// since that could use up too muck stack.
///
/// Instead, the repairs are deferred and placed on a work list in stack form.
struct Repair {
    concat: Opcode,
    // The argument type after splitting.
    split_type: Type,
    // The destination EBB whose arguments have been split.
    ebb: Ebb,
    // Number of the original EBB argument which has been replaced by the low part.
    num: usize,
    // Number of the new EBB argument which represents the high part after the split.
    hi_num: usize,
}

/// Generic version of `isplit` and `vsplit` controlled by the `concat` opcode.
fn split_any(dfg: &mut DataFlowGraph,
             cfg: &ControlFlowGraph,
             pos: &mut Cursor,
             value: Value,
             concat: Opcode)
             -> (Value, Value) {
    let saved_pos = pos.position();
    let mut repairs = Vec::new();
    let result = split_value(dfg, pos, value, concat, &mut repairs);

    // We have split the value requested, and now we may need to fix some EBB predecessors.
    while let Some(repair) = repairs.pop() {
        for &(_, inst) in cfg.get_predecessors(repair.ebb) {
            let branch_opc = dfg[inst].opcode();
            assert!(branch_opc.is_branch(),
                    "Predecessor not a branch: {}",
                    dfg.display_inst(inst));
            let fixed_args = branch_opc.constraints().fixed_value_arguments();
            let mut args = dfg[inst]
                .take_value_list()
                .expect("Branches must have value lists.");
            let num_args = args.len(&dfg.value_lists);
            // Get the old value passed to the EBB argument we're repairing.
            let old_arg = args.get(fixed_args + repair.num, &dfg.value_lists)
                .expect("Too few branch arguments");

            // It's possible that the CFG's predecessor list has duplicates. Detect them here.
            if dfg.value_type(old_arg) == repair.split_type {
                dfg[inst].put_value_list(args);
                continue;
            }

            // Split the old argument, possibly causing more repairs to be scheduled.
            pos.goto_inst(inst);
            let (lo, hi) = split_value(dfg, pos, old_arg, repair.concat, &mut repairs);

            // The `lo` part replaces the original argument.
            *args.get_mut(fixed_args + repair.num, &mut dfg.value_lists)
                 .unwrap() = lo;

            // The `hi` part goes at the end. Since multiple repairs may have been scheduled to the
            // same EBB, there could be multiple arguments missing.
            if num_args > fixed_args + repair.hi_num {
                *args.get_mut(fixed_args + repair.hi_num, &mut dfg.value_lists)
                     .unwrap() = hi;
            } else {
                // We need to append one or more arguments. If we're adding more than one argument,
                // there must be pending repairs on the stack that will fill in the correct values
                // instead of `hi`.
                args.extend(iter::repeat(hi).take(1 + fixed_args + repair.hi_num - num_args),
                            &mut dfg.value_lists);
            }

            // Put the value list back after manipulating it.
            dfg[inst].put_value_list(args);
        }
    }

    pos.set_position(saved_pos);
    result
}

/// Split a single value using the integer or vector semantics given by the `concat` opcode.
///
/// If the value is defined by a `concat` instruction, just reuse the operand values of that
/// instruction.
///
/// Return the two new values representing the parts of `value`.
fn split_value(dfg: &mut DataFlowGraph,
               pos: &mut Cursor,
               value: Value,
               concat: Opcode,
               repairs: &mut Vec<Repair>)
               -> (Value, Value) {
    let value = dfg.resolve_copies(value);
    let mut reuse = None;

    match dfg.value_def(value) {
        ValueDef::Res(inst, num) => {
            // This is an instruction result. See if the value was created by a `concat`
            // instruction.
            if let InstructionData::Binary { opcode, args, .. } = dfg[inst] {
                assert_eq!(num, 0);
                if opcode == concat {
                    reuse = Some((args[0], args[1]));
                }
            }
        }
        ValueDef::Arg(ebb, num) => {
            // This is an EBB argument. We can split the argument value unless this is the entry
            // block.
            if pos.layout.entry_block() != Some(ebb) {
                // We are going to replace the argument at `num` with two new arguments.
                // Determine the new value types.
                let ty = dfg.value_type(value);
                let split_type = match concat {
                    Opcode::Iconcat => ty.half_width().expect("Invalid type for isplit"),
                    Opcode::Vconcat => ty.half_vector().expect("Invalid type for vsplit"),
                    _ => panic!("Unhandled concat opcode: {}", concat),
                };

                // Since the `repairs` stack potentially contains other argument numbers for `ebb`,
                // avoid shifting and renumbering EBB arguments. It could invalidate other
                // `repairs` entries.
                //
                // Replace the original `value` with the low part, and append the high part at the
                // end of the argument list.
                let lo = dfg.replace_ebb_arg(value, split_type);
                let hi_num = dfg.num_ebb_args(ebb);
                let hi = dfg.append_ebb_arg(ebb, split_type);
                reuse = Some((lo, hi));


                // Now the original value is dangling. Insert a concatenation instruction that can
                // compute it from the two new arguments. This also serves as a record of what we
                // did so a future call to this function doesn't have to redo the work.
                //
                // Note that it is safe to move `pos` here since `reuse` was set above, so we don't
                // need to insert a split instruction before returning.
                pos.goto_top(ebb);
                pos.next_inst();
                let concat_inst = dfg.ins(pos).Binary(concat, ty, lo, hi).0;
                let concat_val = dfg.first_result(concat_inst);
                dfg.change_to_alias(value, concat_val);

                // Finally, splitting the EBB argument is not enough. We also have to repair all
                // of the predecessor instructions that branch here.
                add_repair(concat, split_type, ebb, num, hi_num, repairs);
            }
        }
    }

    // Did the code above succeed in finding values we can reuse?
    if let Some(pair) = reuse {
        pair
    } else {
        // No, we'll just have to insert the requested split instruction at `pos`. Note that `pos`
        // has not been moved by the EBB argument code above when `reuse` is `None`.
        match concat {
            Opcode::Iconcat => dfg.ins(pos).isplit(value),
            Opcode::Vconcat => dfg.ins(pos).vsplit(value),
            _ => panic!("Unhandled concat opcode: {}", concat),
        }
    }
}

// Add a repair entry to the work list.
fn add_repair(concat: Opcode,
              split_type: Type,
              ebb: Ebb,
              num: usize,
              hi_num: usize,
              repairs: &mut Vec<Repair>) {
    repairs.push(Repair {
                     concat: concat,
                     split_type: split_type,
                     ebb: ebb,
                     num: num,
                     hi_num: hi_num,
                 });
}

/// Strip concat-split chains. Return a simpler way of computing the same value.
///
/// Given this input:
///
/// ```cton
///     v10 = iconcat v1, v2
///     v11, v12 = isplit v10
/// ```
///
/// This function resolves `v11` to `v1` and `v12` to `v2`.
fn resolve_splits(dfg: &DataFlowGraph, value: Value) -> Value {
    let value = dfg.resolve_copies(value);

    // Deconstruct a split instruction.
    let split_res;
    let concat_opc;
    let split_arg;
    if let ValueDef::Res(inst, num) = dfg.value_def(value) {
        split_res = num;
        concat_opc = match dfg[inst].opcode() {
            Opcode::Isplit => Opcode::Iconcat,
            Opcode::Vsplit => Opcode::Vconcat,
            _ => return value,
        };
        split_arg = dfg.inst_args(inst)[0];
    } else {
        return value;
    }

    // See if split_arg is defined by a concatenation instruction.
    if let ValueDef::Res(inst, _) = dfg.value_def(split_arg) {
        if dfg[inst].opcode() == concat_opc {
            return dfg.inst_args(inst)[split_res];
        }
    }

    value
}

/// Simplify the arguments to a branch *after* the instructions leading up to the branch have been
/// legalized.
///
/// The branch argument repairs performed by `split_any()` above may be performed on branches that
/// have not yet been legalized. The repaired arguments can be defined by actual split
/// instructions in that case.
///
/// After legalizing the instructions computing the value that was split, it is likely that we can
/// avoid depending on the split instruction. Its input probably comes from a concatenation.
pub fn simplify_branch_arguments(dfg: &mut DataFlowGraph, branch: Inst) {
    let mut new_args = Vec::new();

    for &arg in dfg.inst_args(branch) {
        let new_arg = resolve_splits(dfg, arg);
        new_args.push(new_arg);
    }

    dfg.inst_args_mut(branch).copy_from_slice(&new_args);
}
