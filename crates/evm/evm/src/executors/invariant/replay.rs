use super::{error::FailedInvariantCaseData, shrink_sequence};
use crate::executors::Executor;
use alloy_dyn_abi::JsonAbiExt;
use alloy_primitives::Log;
use eyre::Result;
use foundry_common::{ContractsByAddress, ContractsByArtifact};
use foundry_evm_core::{constants::CALLER, fork::Context};
use foundry_evm_coverage::HitMaps;
use foundry_evm_fuzz::{
    invariant::{BasicTxDetails, InvariantContract},
    BaseCounterExample, CounterExample,
};
use foundry_evm_traces::{load_contracts, TraceKind, Traces};
use parking_lot::RwLock;
use proptest::test_runner::TestError;
use revm::primitives::U256;
use std::sync::Arc;

/// Replays a call sequence for collecting logs and traces.
/// Returns counterexample to be used when the call sequence is a failed scenario.
#[allow(clippy::too_many_arguments)]
pub fn replay_run(
    invariant_contract: &InvariantContract<'_>,
    mut executor: Executor,
    known_contracts: &ContractsByArtifact,
    mut ided_contracts: ContractsByAddress,
    logs: &mut Vec<Log>,
    traces: &mut Traces,
    contexts: &mut Vec<Context>,
    coverage: &mut Option<HitMaps>,
    inputs: Vec<BasicTxDetails>,
) -> Result<Option<CounterExample>> {
    // We want traces for a failed case.
    executor.set_tracing(true);

    let mut counterexample_sequence = vec![];

    // Replay each call from the sequence, collect logs, traces and coverage.
    for (sender, (addr, bytes)) in inputs.iter() {
        let call_result =
            executor.call_raw_committing(*sender, *addr, bytes.clone(), U256::ZERO)?;
        logs.extend(call_result.logs);
        traces.push((TraceKind::Execution, call_result.traces.clone().unwrap()));
        contexts.extend(call_result.contexts);

        if let Some(new_coverage) = call_result.coverage {
            if let Some(old_coverage) = coverage {
                *coverage = Some(std::mem::take(old_coverage).merge(new_coverage));
            } else {
                *coverage = Some(new_coverage);
            }
        }

        // Identify newly generated contracts, if they exist.
        ided_contracts.extend(load_contracts(
            vec![(TraceKind::Execution, call_result.traces.clone().unwrap())],
            known_contracts,
        ));

        // Create counter example to be used in failed case.
        counterexample_sequence.push(BaseCounterExample::create(
            *sender,
            *addr,
            bytes,
            &ided_contracts,
            call_result.traces,
        ));

        // Replay invariant to collect logs and traces.
        let error_call_result = executor.call_raw(
            CALLER,
            invariant_contract.address,
            invariant_contract
                .invariant_function
                .abi_encode_input(&[])
                .expect("invariant should have no inputs")
                .into(),
            U256::ZERO,
        )?;
        traces.push((TraceKind::Execution, error_call_result.traces.clone().unwrap()));
        logs.extend(error_call_result.logs);
        contexts.extend(error_call_result.contexts);
    }

    Ok((!counterexample_sequence.is_empty())
        .then_some(CounterExample::Sequence(counterexample_sequence)))
}

/// Replays the error case, shrinks the failing sequence and collects all necessary traces.
#[allow(clippy::too_many_arguments)]
pub fn replay_error(
    failed_case: &FailedInvariantCaseData,
    invariant_contract: &InvariantContract<'_>,
    mut executor: Executor,
    known_contracts: &ContractsByArtifact,
    ided_contracts: ContractsByAddress,
    logs: &mut Vec<Log>,
    traces: &mut Traces,
    contexts: &mut Vec<Context>,
    coverage: &mut Option<HitMaps>,
) -> Result<Option<CounterExample>> {
    match failed_case.test_error {
        // Don't use at the moment.
        TestError::Abort(_) => Ok(None),
        TestError::Fail(_, ref calls) => {
            // Shrink sequence of failed calls.
            let calls = if failed_case.shrink_sequence {
                shrink_sequence(failed_case, calls, &executor)?
            } else {
                trace!(target: "forge::test", "Shrinking disabled.");
                calls.clone()
            };

            set_up_inner_replay(&mut executor, &failed_case.inner_sequence);
            // Replay calls to get the counterexample and to collect logs, traces and coverage.
            replay_run(
                invariant_contract,
                executor,
                known_contracts,
                ided_contracts,
                logs,
                traces,
                contexts,
                coverage,
                calls,
            )
        }
    }
}

/// Sets up the calls generated by the internal fuzzer, if they exist.
fn set_up_inner_replay(executor: &mut Executor, inner_sequence: &[Option<BasicTxDetails>]) {
    if let Some(fuzzer) = &mut executor.inspector.fuzzer {
        if let Some(call_generator) = &mut fuzzer.call_generator {
            call_generator.last_sequence = Arc::new(RwLock::new(inner_sequence.to_owned()));
            call_generator.set_replay(true);
        }
    }
}
