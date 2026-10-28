use crate::sns_distribution::EXPECTED_INITIAL_BALANCE;
use crate::state::{
    SIX_MONTHS_NEURON_NONCE, SNS_GOVERNANCE_SUBACCOUNT, TransferStatus, WithdrawalStatus,
};
use crate::state_machine::{
    WaterNeuron, init_wtn_withdrawal_setup, nns_claim_or_refresh_neuron,
    nns_governance_make_proposal, nns_increase_dissolve_delay, update,
};
use crate::{
    CancelWithdrawalError, ConversionError, DEFAULT_LEDGER_FEE, DepositSuccess, E8S, ICP,
    LiquidArg, MIN_DISSOLVE_DELAY_FOR_REWARDS, NeuronId, ONE_DAY_SECONDS, ONE_MONTH_SECONDS,
    PendingTransfer, Unit, UpgradeArg, WithdrawalSuccess, compute_neuron_staking_subaccount_bytes,
    nICP,
};
use assert_matches::assert_matches;
use candid::{Encode, Nat, Principal};
use ic_base_types::PrincipalId;
use ic_nns_constants::GOVERNANCE_CANISTER_ID;
use ic_nns_governance_api::{
    MakeProposalRequest, ManageNeuronResponse, Motion, ProposalActionRequest,
    governance_error::ErrorType, manage_neuron_response::Command as CommandResponse, neuron,
};
use ic_sns_governance_api::pb::v1::ListProposals;
use ic_wasm_utils::water_neuron_wasm;
use icrc_ledger_types::icrc1::account::Account;

#[tokio::test]
async fn should_not_cancel_withdrawal_on_conversion_done() {
    let mut water_neuron = WaterNeuron::new().await;
    let caller = PrincipalId::new_user_test_id(212);
    init_wtn_withdrawal_setup(&mut water_neuron).await;

    let nicp_to_unwrap = 10 * E8S;
    match water_neuron
        .nicp_to_icp(caller.0.into(), nicp_to_unwrap)
        .await
    {
        Ok(WithdrawalSuccess { withdrawal_id, .. }) => {
            assert_eq!(withdrawal_id, 0);
        }
        Err(e) => panic!("Expected WithdrawalSuccess, got {e:?}"),
    }

    assert_eq!(
        water_neuron.get_withdrawal_requests(caller.0).await.len(),
        1
    );

    assert_matches!(
        water_neuron
            .get_withdrawal_requests(caller.0)
            .await
            .last()
            .unwrap()
            .status,
        WithdrawalStatus::WaitingToSplitNeuron
    );

    assert_eq!(
        water_neuron
            .balance_of(water_neuron.icp_ledger_id, caller.0)
            .await,
        Nat::from(999_980_000_u64)
    );

    water_neuron
        .advance_time_and_tick(6 * ONE_MONTH_SECONDS)
        .await;

    assert_matches!(
        water_neuron
            .get_withdrawal_requests(caller.0)
            .await
            .last()
            .unwrap()
            .status,
        WithdrawalStatus::WaitingDissolvement { .. }
    );

    water_neuron
        .advance_time_and_tick(6 * ONE_MONTH_SECONDS)
        .await;

    match water_neuron
        .cancel_withdrawal(
            caller.0.into(),
            water_neuron
                .get_withdrawal_requests(caller.0)
                .await
                .last()
                .unwrap()
                .request
                .neuron_id
                .unwrap(),
        )
        .await
    {
        Ok(response) => {
            panic!("Expected CancelWithdrawalError, got response: {response:?}");
        }
        Err(e) => match e {
            CancelWithdrawalError::TooLate => {}
            _ => {
                panic!("Expected TooLate, got {e:?}")
            }
        },
    }

    water_neuron
        .advance_time_and_tick(MIN_DISSOLVE_DELAY_FOR_REWARDS)
        .await;

    assert_eq!(
        water_neuron
            .get_withdrawal_requests(caller.0)
            .await
            .last()
            .unwrap()
            .status,
        WithdrawalStatus::ConversionDone {
            transfer_block_height: 10
        }
    );

    assert_eq!(
        water_neuron
            .balance_of(water_neuron.icp_ledger_id, caller.0)
            .await,
        Nat::from(1_999_960_000_u64)
    );

    match water_neuron
        .cancel_withdrawal(
            caller.0.into(),
            water_neuron
                .get_withdrawal_requests(caller.0)
                .await
                .last()
                .unwrap()
                .request
                .neuron_id
                .unwrap(),
        )
        .await
    {
        Ok(response) => {
            panic!("Expected CancelWithdrawalError, got response: {response:?}");
        }
        Err(e) => match e {
            CancelWithdrawalError::TooLate => {}
            _ => {
                panic!("Expected TooLate, got {e:?}")
            }
        },
    }

    assert_eq!(
        water_neuron
            .balance_of(water_neuron.nicp_ledger_id, caller.0)
            .await,
        Nat::from(8_999_990_000_u64)
    );

    let info = water_neuron.get_info().await;

    assert_eq!(
        water_neuron
            .balance_of(water_neuron.icp_ledger_id, info.neuron_6m_account)
            .await,
        Nat::from(9_100_000_042_u64)
    );
}

#[tokio::test]
async fn should_cancel_withdrawal_while_voting() {
    let mut water_neuron = WaterNeuron::new().await;
    let caller = PrincipalId::new_user_test_id(212);
    init_wtn_withdrawal_setup(&mut water_neuron).await;

    let nicp_to_unwrap = 10 * E8S;
    match water_neuron
        .nicp_to_icp(caller.0.into(), nicp_to_unwrap)
        .await
    {
        Ok(WithdrawalSuccess { withdrawal_id, .. }) => {
            assert_eq!(withdrawal_id, 0);
        }
        Err(e) => panic!("Expected WithdrawalSuccess, got {e:?}"),
    }

    assert_eq!(
        water_neuron.get_withdrawal_requests(caller.0).await.len(),
        1
    );

    water_neuron.advance_time_and_tick(ONE_DAY_SECONDS).await;
    let info = water_neuron.get_info().await;
    assert_eq!(info.exchange_rate, E8S);

    assert_eq!(
        water_neuron
            .balance_of(water_neuron.nicp_ledger_id, caller.0)
            .await,
        Nat::from(8_999_990_000_u64)
    );

    assert_matches!(
        water_neuron
            .get_withdrawal_requests(caller.0)
            .await
            .last()
            .unwrap()
            .status,
        WithdrawalStatus::WaitingDissolvement { .. }
    );

    let proposal = MakeProposalRequest {
        title: Some("Yellah".to_string()),
        summary: "Dummy Proposal".to_string(),
        url: "https://forum.dfinity.org/t/reevaluating-neuron-control-restrictions/28597/215"
            .to_string(),
        action: Some(ProposalActionRequest::Motion(Motion {
            motion_text: "".to_string(),
        })),
    };

    let neuron_id = nns_claim_or_refresh_neuron(&mut water_neuron, caller, 0).await;

    let proposal_id =
        match nns_governance_make_proposal(&mut water_neuron, caller, neuron_id, &proposal)
            .await
            .command
            .unwrap()
        {
            CommandResponse::MakeProposal(response) => response.proposal_id.unwrap(),
            _ => panic!("unexpected response"),
        };

    water_neuron.advance_time_and_tick(30 * 60).await;

    let proposals = water_neuron
        .list_proposals(
            water_neuron.wtn_governance_id,
            ListProposals {
                include_reward_status: vec![],
                before_proposal: None,
                limit: 10,
                exclude_type: vec![],
                include_status: vec![],
                include_topics: None,
            },
        )
        .await;
    assert_eq!(proposals.proposals.len(), 2);

    assert_eq!(
        water_neuron.approve_proposal(proposal_id.id).await,
        Ok(ManageNeuronResponse {
            command: Some(
                ic_nns_governance_api::manage_neuron_response::Command::RegisterVote(
                    ic_nns_governance_api::manage_neuron_response::RegisterVoteResponse {}
                )
            )
        })
    );

    water_neuron.advance_time_and_tick(30 * 60).await;

    match water_neuron
        .cancel_withdrawal(
            caller.0.into(),
            water_neuron
                .get_withdrawal_requests(caller.0)
                .await
                .last()
                .unwrap()
                .request
                .neuron_id
                .unwrap(),
        )
        .await
    {
        Ok(response) => {
            let target_neuron_info = response.target_neuron_info.unwrap().clone();
            let source_neuron_info = response.source_neuron_info.unwrap().clone();
            let target_neuron = response.target_neuron.unwrap().clone();
            assert_eq!(target_neuron.id.unwrap().id, info.neuron_id_6m.unwrap().id);
            assert_eq!(
                target_neuron_info.dissolve_delay_seconds,
                1_209_600 // 2 weeks
            );
            assert_eq!(target_neuron_info.stake_e8s, 10_099_980_042_u64);
            assert_eq!(source_neuron_info.age_seconds, 0);
            assert_eq!(source_neuron_info.stake_e8s, 0);
        }
        Err(e) => {
            panic!("Expected MergeResponse, got error: {e:?}");
        }
    }

    water_neuron.advance_time_and_tick(30 * 60).await;

    let info = water_neuron.get_info().await;
    assert_eq!(info.exchange_rate, 99_950_496_u64);
    assert_eq!(info.stakers_count, 1);
    assert_eq!(info.neuron_6m_stake_e8s, info.tracked_6m_stake);
    let icp_to_wrap = 100 * E8S;

    assert_eq!(info.total_icp_deposited, ICP::from_e8s(icp_to_wrap));
}

#[tokio::test]
async fn should_cancel_withdrawal() {
    let mut water_neuron = WaterNeuron::new().await;
    let caller = PrincipalId::new_user_test_id(212);
    init_wtn_withdrawal_setup(&mut water_neuron).await;

    let nicp_to_unwrap = 10 * E8S;
    match water_neuron
        .nicp_to_icp(caller.0.into(), nicp_to_unwrap)
        .await
    {
        Ok(WithdrawalSuccess { withdrawal_id, .. }) => {
            assert_eq!(withdrawal_id, 0);
        }
        Err(e) => panic!("Expected WithdrawalSuccess, got {e:?}"),
    }

    assert_eq!(
        water_neuron.get_withdrawal_requests(caller.0).await.len(),
        1
    );

    water_neuron.advance_time_and_tick(ONE_DAY_SECONDS).await;
    let mut info = water_neuron.get_info().await;
    assert_eq!(info.exchange_rate, E8S);

    assert_eq!(
        water_neuron
            .balance_of(water_neuron.nicp_ledger_id, caller.0)
            .await,
        Nat::from(8_999_990_000_u64)
    );

    assert_matches!(
        water_neuron.get_withdrawal_requests(caller.0).await[0].status,
        WithdrawalStatus::WaitingDissolvement { .. }
    );

    match water_neuron
        .cancel_withdrawal(
            caller.0.into(),
            water_neuron.get_withdrawal_requests(caller.0).await[0]
                .request
                .neuron_id
                .unwrap(),
        )
        .await
    {
        Ok(response) => {
            let target_neuron_info = response.target_neuron_info.unwrap().clone();
            let source_neuron_info = response.source_neuron_info.unwrap().clone();
            let target_neuron = response.target_neuron.unwrap().clone();
            assert_eq!(target_neuron.id.unwrap().id, info.neuron_id_6m.unwrap().id);
            assert_eq!(
                target_neuron_info.dissolve_delay_seconds,
                1_209_600 // 2 weeks
            );
            assert_eq!(target_neuron_info.stake_e8s, 10_099_980_042);
            assert_eq!(source_neuron_info.age_seconds, 0);
            assert_eq!(source_neuron_info.stake_e8s, 0);
        }
        Err(e) => {
            panic!("Expected MergeResponse, got error: {e:?}");
        }
    }

    water_neuron.advance_time_and_tick(60).await;
    info = water_neuron.get_info().await;
    assert_eq!(
        water_neuron
            .get_full_neuron(info.neuron_id_6m.unwrap().id)
            .await
            .unwrap()
            .unwrap()
            .dissolve_state
            .unwrap(),
        neuron::DissolveState::DissolveDelaySeconds(1_209_600)
    );
    assert_eq!(info.exchange_rate, 99_950_496);
    assert_eq!(info.neuron_6m_stake_e8s, info.tracked_6m_stake);

    assert_eq!(
        water_neuron
            .balance_of(water_neuron.nicp_ledger_id, caller.0)
            .await,
        Nat::from(9_994_970_100_u64)
    );

    water_neuron.advance_time_and_tick(ONE_DAY_SECONDS).await;

    assert_eq!(
        water_neuron
            .get_withdrawal_requests(caller.0)
            .await
            .last()
            .unwrap()
            .status,
        WithdrawalStatus::Cancelled
    );

    info = water_neuron.get_info().await;
    assert_eq!(info.exchange_rate, 99_950_496_u64);
    assert_eq!(info.stakers_count, 1);
    assert_eq!(info.neuron_6m_stake_e8s, info.tracked_6m_stake);
    let icp_to_wrap = 100 * E8S;

    assert_eq!(info.total_icp_deposited, ICP::from_e8s(icp_to_wrap));
}

#[tokio::test]
async fn should_mirror_proposal() {
    let mut water_neuron = WaterNeuron::new().await;
    water_neuron.with_voting_topic().await;

    let water_neuron_principal: Principal = water_neuron.water_neuron_id.into();
    let caller = PrincipalId::new_user_test_id(212);

    water_neuron.advance_time_and_tick(50).await;

    assert_eq!(
        water_neuron
            .transfer(
                water_neuron.minter,
                water_neuron_principal,
                10 * E8S,
                water_neuron.icp_ledger_id
            )
            .await,
        Nat::from(4_u8)
    );
    assert_eq!(
        water_neuron
            .transfer(
                water_neuron.minter,
                caller.0,
                100 * E8S,
                water_neuron.icp_ledger_id
            )
            .await,
        Nat::from(5_u8)
    );

    water_neuron.advance_time_and_tick(60).await;

    assert_eq!(
        water_neuron
            .transfer(
                water_neuron.minter,
                Account {
                    owner: GOVERNANCE_CANISTER_ID.into(),
                    subaccount: Some(compute_neuron_staking_subaccount_bytes(caller.into(), 0))
                },
                11 * E8S,
                water_neuron.icp_ledger_id
            )
            .await,
        Nat::from(6_u8)
    );

    water_neuron
        .approve(
            water_neuron.minter,
            water_neuron.icp_ledger_id,
            water_neuron.water_neuron_id.into(),
        )
        .await;

    assert_eq!(
        water_neuron
            .icp_to_nicp(water_neuron.minter, 1_000 * E8S)
            .await,
        Ok(DepositSuccess {
            block_index: Nat::from(8_u8),
            transfer_id: 0,
            nicp_amount: Some(nICP::from_unscaled(1_000)),
        })
    );

    water_neuron.advance_time_and_tick(70).await;

    let neuron_id = nns_claim_or_refresh_neuron(&mut water_neuron, caller, 0).await;

    let _increase_dissolve_delay_result =
        nns_increase_dissolve_delay(&mut water_neuron, caller, neuron_id, 200 * 24 * 60 * 60).await;

    water_neuron.advance_time_and_tick(70).await;

    let proposal = MakeProposalRequest {
        title: Some("Yellah".to_string()),
        summary: "Dummy Proposal".to_string(),
        url: "https://forum.dfinity.org/t/reevaluating-neuron-control-restrictions/28597/215"
            .to_string(),
        action: Some(ProposalActionRequest::Motion(Motion {
            motion_text: "".to_string(),
        })),
    };

    let proposal_id =
        match nns_governance_make_proposal(&mut water_neuron, caller, neuron_id, &proposal)
            .await
            .command
            .unwrap()
        {
            CommandResponse::MakeProposal(response) => response.proposal_id.unwrap(),
            _ => panic!("unexpected response"),
        };

    let yes_before_water_neuron = water_neuron
        .get_proposal_info(proposal_id.id)
        .await
        .unwrap()
        .latest_tally
        .clone()
        .unwrap()
        .yes;

    water_neuron.advance_time_and_tick(30 * 60).await;

    let proposals = water_neuron
        .list_proposals(
            water_neuron.wtn_governance_id,
            ListProposals {
                include_reward_status: vec![],
                before_proposal: None,
                limit: 10,
                exclude_type: vec![],
                include_status: vec![],
                include_topics: None,
            },
        )
        .await;
    assert_eq!(proposals.proposals.len(), 2);

    assert!(
        water_neuron
            .update::<Result<ManageNeuronResponse, String>>(
                PrincipalId::from(Principal::anonymous()),
                water_neuron.water_neuron_id,
                "approve_proposal",
                proposal_id.id
            )
            .await
            .is_err()
    );

    assert_eq!(
        water_neuron.approve_proposal(proposal_id.id).await,
        Ok(ManageNeuronResponse {
            command: Some(
                ic_nns_governance_api::manage_neuron_response::Command::RegisterVote(
                    ic_nns_governance_api::manage_neuron_response::RegisterVoteResponse {}
                )
            )
        })
    );

    water_neuron
        .advance_time_and_tick(4 * 60 * 60 * 24 - 60 * 60)
        .await;

    let yes_after_water_neuron = water_neuron
        .get_proposal_info(proposal_id.id)
        .await
        .unwrap()
        .latest_tally
        .clone()
        .unwrap()
        .yes;

    assert!(
        yes_after_water_neuron > yes_before_water_neuron,
        "Yes after proposal {yes_after_water_neuron} not greater than before {yes_before_water_neuron}"
    );

    assert_matches!(
        water_neuron
            .upgrade_canister(
                water_neuron.water_neuron_id,
                water_neuron_wasm(),
                Encode!(&LiquidArg::Upgrade(Some(UpgradeArg {
                    governance_fee_share_percent: None,
                })))
                .unwrap(),
            )
            .await,
        Ok(_)
    );

    water_neuron.advance_time_and_tick(60).await;
    let info = water_neuron.get_info().await;
    assert_eq!(info.neuron_6m_stake_e8s, info.tracked_6m_stake);
}

#[tokio::test]
async fn should_distribute_icp_to_sns_neurons() {
    let water_neuron = WaterNeuron::new().await;

    let caller = PrincipalId::new_user_test_id(212);

    // Events: Init, NeuronSixMonths and NeuronEightYears.
    let total_event_count = water_neuron.get_events().await.total_event_count;
    assert_eq!(total_event_count, 3);

    assert_eq!(
        water_neuron
            .transfer(
                water_neuron.minter,
                water_neuron.water_neuron_id,
                10 * E8S,
                water_neuron.icp_ledger_id
            )
            .await,
        Nat::from(4_u8)
    );
    assert_eq!(
        water_neuron
            .transfer(
                water_neuron.minter,
                caller.0,
                100 * E8S,
                water_neuron.icp_ledger_id
            )
            .await,
        Nat::from(5_u8)
    );

    water_neuron.advance_time_and_tick(60).await;

    water_neuron
        .approve(
            caller,
            water_neuron.icp_ledger_id,
            water_neuron.water_neuron_id.into(),
        )
        .await;

    let icp_to_wrap = 10 * E8S;

    assert_eq!(
        water_neuron.icp_to_nicp(caller.0.into(), icp_to_wrap).await,
        Ok(DepositSuccess {
            block_index: Nat::from(7_u8),
            transfer_id: 0,
            nicp_amount: Some(nICP::from_e8s(icp_to_wrap)),
        })
    );

    water_neuron.advance_time_and_tick(70).await;

    assert_eq!(
        water_neuron
            .transfer(
                Principal::anonymous().into(),
                water_neuron.water_neuron_id,
                EXPECTED_INITIAL_BALANCE,
                water_neuron.wtn_ledger_id
            )
            .await,
        Nat::from(0_u8)
    );

    water_neuron.tick().await;

    assert_eq!(
        water_neuron
            .balance_of(water_neuron.wtn_ledger_id, caller.0)
            .await,
        Nat::from(0_u8)
    );

    assert_eq!(
        water_neuron.get_airdrop_allocation(caller.0).await,
        8_000_000_000
    );

    assert_eq!(
        water_neuron.icp_to_nicp(caller.0.into(), icp_to_wrap).await,
        Ok(DepositSuccess {
            block_index: Nat::from(8_u8),
            transfer_id: 1,
            nicp_amount: Some(nICP::from_e8s(icp_to_wrap)),
        })
    );

    water_neuron.tick().await;

    assert_eq!(
        water_neuron.get_airdrop_allocation(caller.0).await,
        16_000_000_000
    );

    assert_eq!(
        water_neuron
            .transfer(
                water_neuron.minter,
                Account {
                    owner: water_neuron.water_neuron_id.into(),
                    subaccount: Some(SNS_GOVERNANCE_SUBACCOUNT)
                },
                100 * E8S,
                water_neuron.icp_ledger_id
            )
            .await,
        Nat::from(9_u8)
    );

    assert_eq!(
        water_neuron
            .balance_of(
                water_neuron.icp_ledger_id,
                Account {
                    owner: water_neuron.water_neuron_id.into(),
                    subaccount: Some(SNS_GOVERNANCE_SUBACCOUNT),
                }
            )
            .await,
        Nat::from(100 * E8S)
    );

    water_neuron.advance_time_and_tick(60 * 60 * 24 * 7).await;
    water_neuron.advance_time_and_tick(60 * 60 * 24).await;

    assert_eq!(
        water_neuron
            .balance_of(
                water_neuron.icp_ledger_id,
                Account {
                    owner: PrincipalId::new_user_test_id(1234).into(),
                    subaccount: None,
                }
            )
            .await,
        Nat::from(100 * E8S) - DEFAULT_LEDGER_FEE
    );

    // Events: Init, NeuronSixMonths and NeuronEightYears.
    // + IcpDeposit, TransferExecuted
    // + IcpDeposit, TransferExecuted
    // + DistributeICPtoSNSv2
    assert_eq!(
        water_neuron.get_events().await.total_event_count,
        total_event_count + 5
    );

    {
        let pic = water_neuron.env.lock().await;
        let water_neuron_id = water_neuron.water_neuron_id;
        assert_eq!(
            update::<Result<u64, ConversionError>>(
                &pic,
                water_neuron_id,
                caller.into(),
                "claim_airdrop",
                caller
            ).await,
            Err(
                format!("Failed to call claim_airdrop of {water_neuron_id} with error: PocketIC returned a rejection error: reject code CanisterError, reject message Error from Canister {water_neuron_id}: Canister called `ic0.trap` with message: 'all rewards must be allocated before being claimable'.\nConsider gracefully handling failures from this canister or altering the canister to handle exceptions. See documentation: https://internetcomputer.org/docs/current/references/execution-errors#trapped-explicitly, error code CanisterCalledTrap")
                .to_string()
            )
        );
    }

    water_neuron
        .approve(
            water_neuron.minter.into(),
            water_neuron.icp_ledger_id,
            water_neuron.water_neuron_id.into(),
        )
        .await;

    assert!(
        water_neuron
            .icp_to_nicp(water_neuron.minter.into(), 21_000_000 * E8S)
            .await
            .is_ok()
    );

    assert_eq!(water_neuron.claim_airdrop(caller.0).await, Ok(1));

    assert_eq!(
        water_neuron
            .balance_of(water_neuron.wtn_ledger_id, caller.0)
            .await,
        Nat::from(15_999_000_000_u64)
    );

    assert_matches!(
        water_neuron
            .upgrade_canister(
                water_neuron.water_neuron_id,
                water_neuron_wasm(),
                Encode!(&LiquidArg::Upgrade(Some(UpgradeArg {
                    governance_fee_share_percent: None
                })))
                .unwrap(),
            )
            .await,
        Ok(_)
    );

    water_neuron.advance_time_and_tick(60).await;
    let info = water_neuron.get_info().await;
    assert_eq!(info.neuron_6m_stake_e8s, info.tracked_6m_stake);
}

#[tokio::test]
async fn transfer_ids_are_as_expected() {
    let water_neuron = WaterNeuron::new().await;
    let caller = PrincipalId::new_user_test_id(212);

    // Events: Init, NeuronSixMonths and NeuronEightYears.
    let total_event_count = water_neuron.get_events().await.total_event_count;
    assert_eq!(total_event_count, 3);

    assert_eq!(
        water_neuron
            .transfer(
                water_neuron.minter,
                water_neuron.water_neuron_id,
                10 * E8S,
                water_neuron.icp_ledger_id
            )
            .await,
        Nat::from(4_u8)
    );
    assert_eq!(
        water_neuron
            .transfer(
                water_neuron.minter,
                caller.0,
                110 * E8S,
                water_neuron.icp_ledger_id
            )
            .await,
        Nat::from(5_u8)
    );

    water_neuron
        .approve(
            caller,
            water_neuron.icp_ledger_id,
            water_neuron.water_neuron_id.into(),
        )
        .await;

    let icp_to_wrap = 100 * E8S;

    water_neuron.advance_time_and_tick(60).await;

    let result = water_neuron.icp_to_nicp(caller.0.into(), icp_to_wrap).await;
    assert_eq!(
        result,
        Ok(DepositSuccess {
            block_index: Nat::from(7_u8),
            transfer_id: 0,
            nicp_amount: Some(nICP::from_e8s(icp_to_wrap)),
        })
    );

    let statuses = water_neuron.get_transfer_statuses(vec![0]).await;
    assert_eq!(
        statuses,
        vec![TransferStatus::Pending(PendingTransfer {
            transfer_id: 0,
            from_subaccount: None,
            memo: Some(7),
            amount: 100 * E8S,
            receiver: caller.0.into(),
            unit: Unit::NICP,
        }),]
    );

    assert_matches!(
        water_neuron
            .upgrade_canister(
                water_neuron.water_neuron_id,
                water_neuron_wasm(),
                Encode!(&LiquidArg::Upgrade(Some(UpgradeArg {
                    governance_fee_share_percent: None
                })))
                .unwrap(),
            )
            .await,
        Ok(_)
    );

    water_neuron.advance_time_and_tick(60).await;
    let info = water_neuron.get_info().await;
    assert_eq!(info.neuron_6m_stake_e8s, info.tracked_6m_stake);
}

#[tokio::test]
async fn should_compute_exchange_rate() {
    let water_neuron = WaterNeuron::new().await;
    let caller = PrincipalId::new_user_test_id(212);

    // Events: Init, NeuronSixMonths and NeuronEightYears.
    let total_event_count = water_neuron.get_events().await.total_event_count;
    assert_eq!(total_event_count, 3);

    let water_neuron_principal: Principal = water_neuron.water_neuron_id.into();

    assert_eq!(
        water_neuron
            .transfer(
                water_neuron.minter,
                water_neuron_principal,
                10 * E8S,
                water_neuron.icp_ledger_id
            )
            .await,
        Nat::from(4_u8)
    );
    assert_eq!(
        water_neuron
            .transfer(
                water_neuron.minter,
                caller.0,
                110 * E8S,
                water_neuron.icp_ledger_id
            )
            .await,
        Nat::from(5_u8)
    );

    water_neuron.advance_time_and_tick(60).await;

    water_neuron
        .approve(
            water_neuron.minter,
            water_neuron.icp_ledger_id,
            water_neuron.water_neuron_id.into(),
        )
        .await;

    assert_matches!(
        water_neuron
            .icp_to_nicp(water_neuron.minter, 1_000 * E8S)
            .await,
        Ok(_)
    );

    water_neuron.advance_time_and_tick(70).await;

    let water_neuron_principal: Principal = water_neuron.water_neuron_id.into();

    assert_eq!(
        water_neuron
            .transfer(
                water_neuron.minter,
                Account {
                    owner: water_neuron_principal,
                    subaccount: Some(crate::NeuronOrigin::NICPSixMonths.to_subaccount())
                },
                100 * E8S,
                water_neuron.icp_ledger_id
            )
            .await,
        Nat::from(8_u8)
    );

    water_neuron.advance_time_and_tick(60 * 60).await;

    assert_matches!(
        water_neuron
            .icp_to_nicp(water_neuron.minter, 1_000 * E8S)
            .await,
        Ok(_)
    );

    water_neuron.advance_time_and_tick(7 * 24 * 60 * 60).await;
    water_neuron.advance_time_and_tick(60 * 60).await;

    let info = water_neuron.get_info().await;
    let previous_rate = info.tracked_6m_stake;
    assert_eq!(info.neuron_6m_stake_e8s, info.tracked_6m_stake);
    assert_eq!(
        water_neuron
            .balance_of(water_neuron.icp_ledger_id, info.neuron_6m_account)
            .await,
        Nat::from(info.tracked_6m_stake.0)
    );
    let prev = info.nicp_supply;

    assert_matches!(
        water_neuron
            .upgrade_canister(
                water_neuron.water_neuron_id,
                water_neuron_wasm(),
                Encode!(&LiquidArg::Upgrade(Some(UpgradeArg {
                    governance_fee_share_percent: None
                })))
                .unwrap(),
            )
            .await,
        Ok(_)
    );
    let info = water_neuron.get_info().await;
    assert_eq!(info.nicp_supply, prev);

    water_neuron.tick().await;

    water_neuron.advance_time_and_tick(60 * 60).await;
    let info = water_neuron.get_info().await;
    assert_eq!(
        water_neuron
            .balance_of(water_neuron.icp_ledger_id, info.neuron_6m_account)
            .await,
        Nat::from(info.tracked_6m_stake.0)
    );
    assert_eq!(info.neuron_6m_stake_e8s, info.tracked_6m_stake);
    assert_eq!(info.tracked_6m_stake, previous_rate);
}

#[tokio::test]
async fn should_mirror_all_proposals() {
    let mut water_neuron = WaterNeuron::new().await;
    water_neuron.with_voting_topic().await;

    let water_neuron_principal: Principal = water_neuron.water_neuron_id.into();
    let caller = PrincipalId::new_user_test_id(212);

    assert_eq!(
        water_neuron
            .transfer(
                water_neuron.minter,
                water_neuron_principal,
                10 * E8S,
                water_neuron.icp_ledger_id
            )
            .await,
        Nat::from(4_u8)
    );
    assert_eq!(
        water_neuron
            .transfer(
                water_neuron.minter,
                caller.0,
                100 * E8S,
                water_neuron.icp_ledger_id
            )
            .await,
        Nat::from(5_u8)
    );

    water_neuron.advance_time_and_tick(60).await;

    assert_eq!(
        water_neuron
            .transfer(
                water_neuron.minter,
                Account {
                    owner: GOVERNANCE_CANISTER_ID.into(),
                    subaccount: Some(compute_neuron_staking_subaccount_bytes(caller.into(), 0))
                },
                11 * E8S,
                water_neuron.icp_ledger_id
            )
            .await,
        Nat::from(6_u8)
    );

    water_neuron
        .approve(
            water_neuron.minter,
            water_neuron.icp_ledger_id,
            water_neuron.water_neuron_id.into(),
        )
        .await;

    assert_eq!(
        water_neuron
            .icp_to_nicp(water_neuron.minter, 1_000 * E8S)
            .await,
        Ok(DepositSuccess {
            block_index: Nat::from(8_u8),
            transfer_id: 0,
            nicp_amount: Some(nICP::from_unscaled(1_000)),
        })
    );

    water_neuron.advance_time_and_tick(70).await;

    let neuron_id = nns_claim_or_refresh_neuron(&mut water_neuron, caller, 0).await;

    let _increase_dissolve_delay_result =
        nns_increase_dissolve_delay(&mut water_neuron, caller, neuron_id, 200 * 24 * 60 * 60).await;

    water_neuron.advance_time_and_tick(70).await;

    assert_matches!(
        water_neuron
            .upgrade_canister(
                water_neuron.water_neuron_id,
                water_neuron_wasm(),
                Encode!(&LiquidArg::Upgrade(Some(UpgradeArg {
                    governance_fee_share_percent: None,
                })))
                .unwrap(),
            )
            .await,
        Ok(_)
    );

    let mut proposals = vec![];

    proposals.push(MakeProposalRequest {
        title: Some("Dummy Motion".to_string()),
        summary: "Dummy Proposal".to_string(),
        url: "https://forum.dfinity.org/t/reevaluating-neuron-control-restrictions/28597/215"
            .to_string(),
        action: Some(ProposalActionRequest::Motion(Motion {
            motion_text: "Some text".to_string(),
        })),
    });

    proposals.push(MakeProposalRequest {
        title: Some("Dummy ManageNetworkEconomics".to_string()),
        summary: "Dummy Proposal".to_string(),
        url: "https://forum.dfinity.org/t/reevaluating-neuron-control-restrictions/28597/215"
            .to_string(),
        action: Some(ProposalActionRequest::ManageNetworkEconomics(
            ic_nns_governance_api::NetworkEconomics {
                reject_cost_e8s: 0,
                neuron_minimum_stake_e8s: 0,
                neuron_management_fee_per_proposal_e8s: 0,
                minimum_icp_xdr_rate: 0,
                neuron_spawn_dissolve_delay_seconds: 0,
                maximum_node_provider_rewards_e8s: 0,
                transaction_fee_e8s: 0,
                max_proposals_to_keep_per_topic: 0,
                voting_power_economics: None,
                neurons_fund_economics: None,
            },
        )),
    });

    proposals.push(MakeProposalRequest {
        title: Some("Dummy ManageNetworkEconomics".to_string()),
        summary: "Dummy Proposal".to_string(),
        url: "https://forum.dfinity.org/t/reevaluating-neuron-control-restrictions/28597/215"
            .to_string(),
        action: Some(ProposalActionRequest::ManageNetworkEconomics(
            ic_nns_governance_api::NetworkEconomics {
                reject_cost_e8s: 0,
                neuron_minimum_stake_e8s: 0,
                neuron_management_fee_per_proposal_e8s: 0,
                minimum_icp_xdr_rate: 0,
                neuron_spawn_dissolve_delay_seconds: 0,
                maximum_node_provider_rewards_e8s: 0,
                transaction_fee_e8s: 0,
                max_proposals_to_keep_per_topic: 0,
                voting_power_economics: None,
                neurons_fund_economics: None,
            },
        )),
    });

    for proposal in proposals {
        let res = nns_governance_make_proposal(&mut water_neuron, caller, neuron_id, &proposal)
            .await
            .command
            .unwrap();
        dbg!(res.clone());
        match res {
            CommandResponse::MakeProposal(_rees) => {}
            _ => panic!("unexpected response"),
        }
    }

    assert_eq!(water_neuron.get_pending_proposals().await.len(), 3);
    dbg!(water_neuron.get_pending_proposals().await);

    water_neuron.advance_time_and_tick(30 * 60).await;

    let proposals = water_neuron
        .list_proposals(
            water_neuron.wtn_governance_id,
            ListProposals {
                include_reward_status: vec![],
                before_proposal: None,
                limit: 10,
                exclude_type: vec![],
                include_status: vec![],
                include_topics: None,
            },
        )
        .await;
    assert_eq!(proposals.proposals.len(), 4);
}

/// Reproduces the mainnet wedge created by proposal 7720: while the main neuron is dissolving, every
/// withdrawal neuron split off it is born Dissolving, StartDissolving on it is rejected with
/// RequiresNotDissolving, and the request never reaches to_disburse. 52 requests holding 41,532 ICP
/// are stuck this way as of 2026-10-15. Fails until process_start_dissolving treats that error as
/// success.
#[tokio::test]
async fn should_disburse_withdrawal_split_from_a_dissolving_main_neuron() {
    let mut water_neuron = WaterNeuron::new().await;
    let caller = PrincipalId::new_user_test_id(212);
    init_wtn_withdrawal_setup(&mut water_neuron).await;

    water_neuron
        .main_neuron_call("start_dissolving_main_neuron", SIX_MONTHS_NEURON_NONCE)
        .await
        .expect("failed to start dissolving the main neuron");

    match water_neuron.nicp_to_icp(caller.0.into(), 10 * E8S).await {
        Ok(WithdrawalSuccess { withdrawal_id, .. }) => assert_eq!(withdrawal_id, 0),
        Err(e) => panic!("Expected WithdrawalSuccess, got {e:?}"),
    }

    water_neuron.advance_time_and_tick(ONE_DAY_SECONDS).await;

    // The split child is dissolving already, so the request has to move on without a successful
    // StartDissolving call.
    assert_matches!(
        water_neuron.get_withdrawal_requests(caller.0).await[0].status,
        WithdrawalStatus::WaitingDissolvement { .. }
    );

    // The child inherited the parent's absolute dissolve timestamp, so it matures with the parent.
    for _ in 0..15 {
        water_neuron.advance_time_and_tick(ONE_DAY_SECONDS).await;
    }

    assert_matches!(
        water_neuron.get_withdrawal_requests(caller.0).await[0].status,
        WithdrawalStatus::ConversionDone { .. }
    );
}

async fn dissolve_state(wn: &WaterNeuron, neuron_id: u64) -> neuron::DissolveState {
    wn.get_full_neuron(neuron_id)
        .await
        .unwrap()
        .unwrap()
        .dissolve_state
        .unwrap()
}

#[tokio::test]
async fn should_restore_min_dissolve_delay_after_stop_dissolving() {
    let water_neuron = WaterNeuron::new().await;
    let neuron_id = water_neuron
        .get_info()
        .await
        .neuron_id_6m
        .expect("6m neuron should exist")
        .id;

    assert_eq!(
        dissolve_state(&water_neuron, neuron_id).await,
        neuron::DissolveState::DissolveDelaySeconds(MIN_DISSOLVE_DELAY_FOR_REWARDS)
    );

    water_neuron
        .main_neuron_call("start_dissolving_main_neuron", SIX_MONTHS_NEURON_NONCE)
        .await
        .expect("failed to start dissolving");
    assert_matches!(
        dissolve_state(&water_neuron, neuron_id).await,
        neuron::DissolveState::WhenDissolvedTimestampSeconds(_)
    );

    // Raising the delay is refused while the neuron is dissolving. That ordering is what forces
    // the transition to be two proposals, 1005 then 1006.
    assert_eq!(
        water_neuron
            .main_neuron_call(
                "set_main_neuron_to_min_dissolve_delay",
                SIX_MONTHS_NEURON_NONCE
            )
            .await,
        Err("neuron is still dissolving; stop dissolving first".to_string())
    );

    // The neuron starts at exactly the target, so any elapsed time drops it below.
    water_neuron.advance_time_and_tick(60 * 60).await;
    water_neuron
        .main_neuron_call("stop_dissolving_main_neuron", SIX_MONTHS_NEURON_NONCE)
        .await
        .expect("failed to stop dissolving");

    let frozen = match dissolve_state(&water_neuron, neuron_id).await {
        neuron::DissolveState::DissolveDelaySeconds(d) => d,
        other => panic!("expected a stopped neuron, got {other:?}"),
    };
    assert!(frozen < MIN_DISSOLVE_DELAY_FOR_REWARDS, "frozen at {frozen}");

    water_neuron
        .main_neuron_call(
            "set_main_neuron_to_min_dissolve_delay",
            SIX_MONTHS_NEURON_NONCE,
        )
        .await
        .expect("failed to set the min dissolve delay");
    assert_eq!(
        dissolve_state(&water_neuron, neuron_id).await,
        neuron::DissolveState::DissolveDelaySeconds(MIN_DISSOLVE_DELAY_FOR_REWARDS)
    );

    // Already at the target, so a second run is refused rather than stacking more delay.
    assert_matches!(
        water_neuron
            .main_neuron_call(
                "set_main_neuron_to_min_dissolve_delay",
                SIX_MONTHS_NEURON_NONCE
            )
            .await,
        Err(e) if e.contains("already >= target")
    );
}

/// The dissolve delay the main neuron carried before #110 lowered it to two weeks.
const PRE_110_MIN_DISSOLVE_DELAY: u64 = 6 * ONE_MONTH_SECONDS + ONE_DAY_SECONDS;

/// Rebuilds the mainnet state that produced the 151-day jump on the main neuron on 2026-10-20.
///
/// Every withdrawal neuron still outstanding was split off a main neuron sitting at
/// `PRE_110_MIN_DISSOLVE_DELAY`, so it inherited roughly six months. SNS proposal 7720 then started
/// the main neuron dissolving, and the NNS refuses `Merge` when either neuron is dissolving
/// (`MergeNeuronsError::TargetNeuronDissolving`). Every `cancel_withdrawal` issued during that
/// window is therefore applied by halves: `StopDissolving` lands on the withdrawal neuron, the
/// merge that was supposed to follow does not.
///
/// Returns the id of the withdrawal neuron left behind in that half-cancelled state.
async fn stall_cancel_withdrawal_on_a_dissolving_main_neuron(
    water_neuron: &mut WaterNeuron,
    caller: PrincipalId,
) -> NeuronId {
    init_wtn_withdrawal_setup(water_neuron).await;

    let water_neuron_principal = PrincipalId::from(water_neuron.water_neuron_id);
    let neuron_id_6m = water_neuron
        .get_info()
        .await
        .neuron_id_6m
        .expect("6m neuron should exist");
    nns_increase_dissolve_delay(
        water_neuron,
        water_neuron_principal,
        neuron_id_6m,
        PRE_110_MIN_DISSOLVE_DELAY - MIN_DISSOLVE_DELAY_FOR_REWARDS,
    )
    .await;

    match water_neuron.nicp_to_icp(caller.0.into(), 10 * E8S).await {
        Ok(WithdrawalSuccess { withdrawal_id, .. }) => assert_eq!(withdrawal_id, 0),
        Err(e) => panic!("Expected WithdrawalSuccess, got {e:?}"),
    }
    water_neuron.advance_time_and_tick(ONE_DAY_SECONDS).await;

    let requests = water_neuron.get_withdrawal_requests(caller.0).await;
    assert_matches!(
        requests[0].status,
        WithdrawalStatus::WaitingDissolvement { .. }
    );
    let withdrawal_neuron_id = requests[0]
        .request
        .neuron_id
        .expect("the withdrawal neuron should have been split");

    // Both neurons carry the same dissolve delay, so without a gap here they would reach zero at
    // the same second and a merge taking `max(source, target)` would be a coin flip. Starting the
    // main neuron three days late keeps it the later of the two throughout.
    water_neuron
        .advance_time_and_tick(3 * ONE_DAY_SECONDS)
        .await;

    // SNS proposal 7720.
    water_neuron
        .main_neuron_call("start_dissolving_main_neuron", SIX_MONTHS_NEURON_NONCE)
        .await
        .expect("failed to start dissolving the main neuron");
    water_neuron.advance_time_and_tick(ONE_DAY_SECONDS).await;

    // `cancel_withdrawal` stops the withdrawal neuron first and only then merges, so this failure
    // arrives after the neuron has already been taken out of Dissolving.
    assert_matches!(
        water_neuron
            .cancel_withdrawal(caller.0.into(), withdrawal_neuron_id)
            .await,
        Err(CancelWithdrawalError::GovernanceError(error))
            if error.error_type == ErrorType::RequiresNotDissolving as i32
    );

    withdrawal_neuron_id
}

/// Cancelling a withdrawal must never raise the main neuron's dissolve delay.
///
/// An NNS merge gives the target `max(source.dissolve_delay, target.dissolve_delay)`
/// (`rs/nns/governance/src/governance/merge_neurons.rs`), so a withdrawal neuron frozen high by a
/// half-applied cancel drags the main neuron up with it when the cancel is retried. That is what
/// moved the main neuron from ~10 days to 151 days on 2026-10-20, which in turn makes
/// `set_main_neuron_to_min_dissolve_delay` (NSF 1006) unable to bring it back down.
#[tokio::test]
async fn should_not_raise_main_neuron_dissolve_delay_on_cancel_withdrawal() {
    let mut water_neuron = WaterNeuron::new().await;
    let caller = PrincipalId::new_user_test_id(212);
    let withdrawal_neuron_id =
        stall_cancel_withdrawal_on_a_dissolving_main_neuron(&mut water_neuron, caller).await;
    let neuron_id_6m = water_neuron
        .get_info()
        .await
        .neuron_id_6m
        .expect("6m neuron should exist")
        .id;

    // SNS proposal 10324: the main neuron is stopped once it is deep into its dissolve. The
    // withdrawal neuron has been counting down alongside it, so neither sits far above the other.
    water_neuron.advance_time_and_tick(5 * ONE_MONTH_SECONDS).await;
    water_neuron
        .main_neuron_call("stop_dissolving_main_neuron", SIX_MONTHS_NEURON_NONCE)
        .await
        .expect("failed to stop dissolving the main neuron");

    let before = match dissolve_state(&water_neuron, neuron_id_6m).await {
        neuron::DissolveState::DissolveDelaySeconds(delay) => delay,
        other => panic!("expected a stopped main neuron, got {other:?}"),
    };

    // The main neuron is no longer dissolving, so the cancel the earlier window refused goes
    // through, and the merge takes the larger of the two delays.
    water_neuron
        .cancel_withdrawal(caller.0.into(), withdrawal_neuron_id)
        .await
        .expect("failed to cancel the withdrawal");

    let after = match dissolve_state(&water_neuron, neuron_id_6m).await {
        neuron::DissolveState::DissolveDelaySeconds(delay) => delay,
        other => panic!("expected a stopped main neuron, got {other:?}"),
    };
    assert!(
        after <= before,
        "cancelling a withdrawal raised the main neuron dissolve delay from {before}s to {after}s"
    );
}

/// A cancel that cannot complete must leave the withdrawal exactly as it found it.
///
/// Without the rollback the `StopDissolving` half survives the failure and the withdrawal neuron is
/// parked at a frozen dissolve delay that never counts down: the request sits in
/// `WaitingDissolvement` forever and the ICP is unreachable. Seven mainnet withdrawals are in that
/// state as of 2026-10-20, holding 3357.08945204 ICP: 1528, 1608, 1614, 1615, 1645, 1648 and 1664.
/// Reproduce the list with
/// `cargo run --manifest-path scripts/neuron_info/Cargo.toml --bin withdrawals_audit`.
#[tokio::test]
async fn should_disburse_withdrawal_after_a_failed_cancel() {
    let mut water_neuron = WaterNeuron::new().await;
    let caller = PrincipalId::new_user_test_id(212);
    let withdrawal_neuron_id =
        stall_cancel_withdrawal_on_a_dissolving_main_neuron(&mut water_neuron, caller).await;

    assert_matches!(
        dissolve_state(&water_neuron, withdrawal_neuron_id.id).await,
        neuron::DissolveState::WhenDissolvedTimestampSeconds(_),
        "a failed cancel should leave the withdrawal neuron dissolving"
    );

    // SNS proposal 10324, so that nothing but the withdrawal neuron's own schedule is in play.
    water_neuron
        .main_neuron_call("stop_dissolving_main_neuron", SIX_MONTHS_NEURON_NONCE)
        .await
        .expect("failed to stop dissolving the main neuron");

    // Comfortably past the withdrawal neuron's six-month horizon.
    for _ in 0..8 {
        water_neuron.advance_time_and_tick(ONE_MONTH_SECONDS).await;
    }

    assert_matches!(
        water_neuron.get_withdrawal_requests(caller.0).await[0].status,
        WithdrawalStatus::ConversionDone { .. }
    );
}
