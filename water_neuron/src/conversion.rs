use crate::guards::GuardPrincipal;
use crate::logs::{DEBUG, INFO};
use crate::management::{merge_neuron_into_six_months, start_dissolving, stop_dissolvement};
use crate::nns_types::{NeuronId, time_left_seconds};
use crate::numeric::{ICP, nICP};
use crate::state::audit::process_event;
use crate::state::event::EventType;
use crate::state::{mutate_state, read_state};
use crate::tasks::{TaskType, schedule_now};
use crate::{
    CancelWithdrawalError, ConversionArg, ConversionError, DepositSuccess, ICP_LEDGER_ID,
    ONE_DAY_SECONDS, WithdrawalSuccess, get_full_neuron, timestamp_nanos,
};
use candid::Nat;
use ic_canister_log::log;
use ic_nns_governance_api::{
    manage_neuron_response::Command as CommandResponse, manage_neuron_response::ConfigureResponse,
    manage_neuron_response::MergeResponse,
};
use icrc_ledger_client_cdk::{CdkRuntime, ICRC1Client};
use icrc_ledger_types::icrc1::account::Account;
use icrc_ledger_types::icrc2::transfer_from::TransferFromArgs;

pub const MINIMUM_DEPOSIT_AMOUNT: ICP = ICP::ONE;
pub const MINIMUM_WITHDRAWAL_AMOUNT: ICP = ICP::from_unscaled(10);

/// Seconds until the neuron reaches a zero dissolve delay, whether it is dissolving or stopped.
async fn time_left_of(neuron_id: NeuronId) -> Result<u64, CancelWithdrawalError> {
    let neuron = get_full_neuron(neuron_id.id)
        .await
        .map_err(|message| CancelWithdrawalError::GetFullNeuronError { message })?
        .map_err(CancelWithdrawalError::GovernanceError)?;

    time_left_seconds(&neuron, timestamp_nanos() / crate::SEC_NANOS)
        .ok_or(CancelWithdrawalError::UnknownTimeLeft)
}

pub async fn cancel_withdrawal(
    neuron_id: NeuronId,
) -> Result<MergeResponse, CancelWithdrawalError> {
    let caller = ic_cdk::api::msg_caller();
    let _guard_principal = GuardPrincipal::new(caller)
        .map_err(|guard_error| CancelWithdrawalError::GuardError { guard_error })?;

    let time_left = time_left_of(neuron_id).await?;

    // Buffer against the disbursement timer, scaled to the two-week delay: the old fourteen days
    // covered the whole window.
    if time_left < ONE_DAY_SECONDS {
        return Err(CancelWithdrawalError::TooLate);
    }

    // Ownership is settled before the main neuron is read: the dissolve delay check below reports
    // both neurons' remaining delays, which a caller who does not own this withdrawal has no claim
    // to, and it costs a second governance call that a rejected caller never needs.
    let icp_due = match read_state(|s| {
        s.neuron_id_to_withdrawal_id
            .get(&neuron_id)
            .and_then(|withdrawal_id| s.withdrawal_id_to_request.get(withdrawal_id).cloned())
    }) {
        Some(withdrawal_request) => {
            if withdrawal_request.receiver != caller.into() {
                return Err(CancelWithdrawalError::BadCaller {
                    message: "Caller is not the owner.".to_string(),
                });
            }
            withdrawal_request.icp_due
        }
        None => return Err(CancelWithdrawalError::RequestNotFound),
    };

    // An NNS merge gives the target `max(source, target)` dissolve delay. A withdrawal neuron split
    // off the main neuron before it dissolved further therefore drags the main neuron back up to
    // its own delay, lengthening every other withdrawal. Such a request has to run to disbursement
    // instead of being cancelled.
    let main_neuron_id =
        read_state(|s| s.neuron_id_6m).ok_or_else(|| CancelWithdrawalError::GenericError {
            code: 0,
            message: "main neuron is not set".to_string(),
        })?;
    let main_time_left = time_left_of(main_neuron_id).await?;
    if time_left > main_time_left {
        return Err(CancelWithdrawalError::GenericError {
            code: 1,
            message: format!(
                "cancelling would raise the main neuron dissolve delay from {main_time_left}s to {time_left}s"
            ),
        });
    }

    log!(
        DEBUG,
        "[cancel_withdrawal] Cancelling neuron with id {}, ICP due: {icp_due}",
        neuron_id.id
    );

    // `manage_neuron` reports governance-level failures inside the response rather than as a call
    // error, so the command has to be checked: a `StopDissolving` that governance refused must abort
    // the cancel instead of falling through to a merge the caller never earned.
    let stop_dissolvement_result = match stop_dissolvement(neuron_id).await {
        Ok(response)
            if response.command == Some(CommandResponse::Configure(ConfigureResponse {})) =>
        {
            Ok(())
        }
        Ok(response) => Err(CancelWithdrawalError::StopDissolvementError {
            message: format!("{response:?}"),
        }),
        Err(error_msg) => Err(CancelWithdrawalError::StopDissolvementError { message: error_msg }),
    };

    if stop_dissolvement_result.is_err() {
        log!(
            INFO,
            "[cancel_withdrawal] Unexpected stop_dissolvement result: {stop_dissolvement_result:?}"
        );
    }

    stop_dissolvement_result?;

    let merge_result = merge_withdrawal_neuron(neuron_id).await;

    if merge_result.is_err() {
        // The `StopDissolving` above has already landed. Leaving it in place parks the neuron at a
        // dissolve delay that never counts down, so the withdrawal could never disburse and a later
        // retry would merge a neuron frozen high, dragging the main neuron's own delay up with it
        // (an NNS merge takes `max(source, target)`). Restarting the dissolve restores the absolute
        // timestamp the neuron had, give or take the latency of these two calls.
        if let Err(error) = start_dissolving(neuron_id).await {
            log!(
                INFO,
                "[cancel_withdrawal] Failed to restart the dissolvement of neuron {} after a failed \
                 merge, it is parked and cannot disburse: {error:?}",
                neuron_id.id
            );
        }
    }

    merge_result
}

/// Merges a stopped withdrawal neuron back into the main neuron and credits the caller.
async fn merge_withdrawal_neuron(
    neuron_id: NeuronId,
) -> Result<MergeResponse, CancelWithdrawalError> {
    match merge_neuron_into_six_months(neuron_id)
        .await
        .map_err(|error_msg| CancelWithdrawalError::MergeNeuronError { message: error_msg })?
        .command
        .expect("Command should always be set.")
    {
        CommandResponse::Merge(response) => {
            if response
                .source_neuron
                .as_ref()
                .unwrap()
                .cached_neuron_stake_e8s
                != 0
            {
                log!(
                    INFO,
                    "[cancel_withdrawal] Expected cached_neuron_stake_e8s to be 0 got {response:?}"
                );
                return Err(CancelWithdrawalError::MergeNeuronError {
                    message: format!("Expected cached_neuron_stake_e8s to be 0 got {response:?}"),
                });
            }

            mutate_state(|s| {
                process_event(s, EventType::MergeNeuron { neuron_id });
            });
            schedule_now(TaskType::ProcessPendingTransfers);
            schedule_now(TaskType::RefreshShortTerm);
            Ok(response)
        }
        CommandResponse::Error(e) => Err(CancelWithdrawalError::GovernanceError(e)),
        other => Err(CancelWithdrawalError::BadCommand {
            message: format!("Expected merge command got {other:?}"),
        }),
    }
}

pub async fn nicp_to_icp(arg: ConversionArg) -> Result<WithdrawalSuccess, ConversionError> {
    let caller = ic_cdk::api::msg_caller();
    let _guard_principal = GuardPrincipal::new(caller)
        .map_err(|guard_error| ConversionError::GuardError { guard_error })?;

    let (nicp_amount, maybe_subaccount) = (nICP::from_e8s(arg.amount_e8s), arg.maybe_subaccount);
    let icp_due = read_state(|s| s.convert_nicp_to_icp(nicp_amount));

    if icp_due < MINIMUM_WITHDRAWAL_AMOUNT {
        return Err(ConversionError::AmountTooLow {
            minimum_amount_e8s: MINIMUM_WITHDRAWAL_AMOUNT.0,
        });
    }

    let client = ICRC1Client {
        runtime: CdkRuntime,
        ledger_canister_id: read_state(|s| s.nicp_ledger_id),
    };

    let receiver = Account {
        owner: caller,
        subaccount: maybe_subaccount,
    };

    match client
        .transfer_from(TransferFromArgs {
            spender_subaccount: None,
            from: receiver,
            to: Account {
                owner: ic_cdk::api::canister_self(),
                subaccount: None,
            },
            amount: Nat::from(arg.amount_e8s),
            fee: None,
            memo: None,
            created_at_time: None,
        })
        .await
    {
        Ok(result) => match result {
            Ok(block_index) => {
                let icp_due = read_state(|s| s.convert_nicp_to_icp(nicp_amount));
                log!(
                    INFO,
                    "[nicp_to_icp] Converted {nicp_amount} nICP for {icp_due} ICP by {receiver}",
                );
                schedule_now(TaskType::ProcessLogic);
                let withdrawal_id = mutate_state(|s| {
                    let withdrawal_id = s.withdrawal_id;
                    process_event(
                        s,
                        EventType::NIcpWithdrawal {
                            receiver,
                            nicp_burned: nicp_amount,
                            nicp_burn_index: block_index.clone().0.try_into().unwrap(),
                        },
                    );
                    withdrawal_id
                });
                Ok(WithdrawalSuccess {
                    withdrawal_id,
                    block_index,
                    icp_amount: Some(icp_due),
                })
            }
            Err(transfer_from_error) => {
                Err(ConversionError::TransferFromError(transfer_from_error))
            }
        },
        Err((code, message)) => Err(ConversionError::GenericError { code, message }),
    }
}

pub async fn icp_to_nicp(arg: ConversionArg) -> Result<DepositSuccess, ConversionError> {
    let caller = ic_cdk::api::msg_caller();
    let _guard_principal = GuardPrincipal::new(caller)
        .map_err(|guard_error| ConversionError::GuardError { guard_error })?;

    let (amount, maybe_subaccount) = (ICP::from_e8s(arg.amount_e8s), arg.maybe_subaccount);
    if amount < MINIMUM_DEPOSIT_AMOUNT {
        return Err(ConversionError::AmountTooLow {
            minimum_amount_e8s: MINIMUM_DEPOSIT_AMOUNT.0,
        });
    }

    let client = ICRC1Client {
        runtime: CdkRuntime,
        ledger_canister_id: ICP_LEDGER_ID,
    };

    let receiver = Account {
        owner: caller,
        subaccount: maybe_subaccount,
    };

    match client
        .transfer_from(TransferFromArgs {
            spender_subaccount: None,
            from: receiver,
            to: read_state(|s| s.get_6m_neuron_account()),
            amount: Nat::from(arg.amount_e8s),
            fee: None,
            memo: None,
            created_at_time: None,
        })
        .await
    {
        Ok(result) => match result {
            Ok(block_index) => {
                let nicp_due = read_state(|s| s.convert_icp_to_nicp(amount));
                log!(
                    INFO,
                    "[icp_to_nicp] Converted {amount} ICP for {nicp_due} nICP by {receiver}",
                );
                schedule_now(TaskType::ProcessPendingTransfers);
                schedule_now(TaskType::RefreshShortTerm);
                let transfer_id = read_state(|s| s.transfer_id);
                mutate_state(|s| {
                    process_event(
                        s,
                        EventType::IcpDeposit {
                            receiver,
                            amount,
                            block_index: block_index.clone().0.try_into().unwrap(),
                        },
                    );
                });
                Ok(DepositSuccess {
                    block_index,
                    transfer_id,
                    nicp_amount: Some(nicp_due),
                })
            }
            Err(transfer_from_error) => {
                Err(ConversionError::TransferFromError(transfer_from_error))
            }
        },
        Err((code, message)) => Err(ConversionError::GenericError { code, message }),
    }
}
