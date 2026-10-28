//! Cross-check every open WaterNeuron withdrawal against the NNS neuron behind it.
//!
//!     cargo run --manifest-path scripts/neuron_info/Cargo.toml --bin withdrawals_audit
//!
//! Read-only, anonymous queries against `list_withdrawal_requests` on the WaterNeuron canister and
//! `get_neuron_info` on NNS governance. Two disagreements between the two sides matter:
//!
//! * `WaitingDissolvement` on a neuron the NNS reports as `NotDissolving`. `cancel_withdrawal`
//!   stops the withdrawal neuron before it merges it, the merge is rejected while the main neuron
//!   is dissolving, and the stop is never rolled back. The neuron is parked at a frozen dissolve
//!   delay that does not count down, so the request can never disburse. Merging such a neuron into
//!   the main neuron also raises the main neuron's own delay, because an NNS merge takes
//!   `max(source, target)`.
//! * `WaitingToStartDissolving` on a neuron the NNS already reports as `Dissolving`. The neuron was
//!   split off a dissolving parent, `StartDissolving` came back `RequiresNotDissolving`, and the
//!   request never advanced.

use candid::{CandidType, Decode, Encode, Principal};
use ic_agent::{Agent, identity::AnonymousIdentity};
use serde::Deserialize;

const IC_URL: &str = "https://icp-api.io";
const NNS_GOVERNANCE: &str = "rrkah-fqaaa-aaaaa-aaaaq-cai";
const WATER_NEURON: &str = "tsbvt-pyaaa-aaaar-qafva-cai";
/// `list_withdrawal_requests` clamps the page size to 100.
const PAGE: u64 = 100;
const DAY: u64 = 86_400;
const E8S: u64 = 100_000_000;

#[derive(CandidType, Deserialize, Debug)]
struct Account {
    owner: Principal,
    subaccount: Option<Vec<u8>>,
}

#[derive(CandidType, Deserialize, Debug, Clone, Copy)]
struct NeuronId {
    id: u64,
}

#[derive(CandidType, Deserialize, Debug)]
struct WithdrawalRequest {
    withdrawal_id: u64,
    receiver: Account,
    icp_due: u64,
    neuron_id: Option<NeuronId>,
    timestamp: u64,
}

#[derive(CandidType, Deserialize, Debug)]
enum WithdrawalStatus {
    ConversionDone { transfer_block_height: u64 },
    NotFound,
    Cancelled,
    WaitingToSplitNeuron,
    WaitingDissolvement { neuron_id: NeuronId },
    WaitingToStartDissolving { neuron_id: NeuronId },
}

#[derive(CandidType, Deserialize, Debug)]
struct WithdrawalDetails {
    status: WithdrawalStatus,
    request: WithdrawalRequest,
}

#[derive(CandidType, Deserialize, Debug)]
struct NeuronInfo {
    dissolve_delay_seconds: u64,
    state: i32,
    stake_e8s: u64,
    retrieved_at_timestamp_seconds: u64,
}

#[derive(CandidType, Deserialize, Debug)]
struct GovernanceError {
    error_message: String,
    error_type: i32,
}

#[derive(CandidType, Deserialize, Debug)]
enum NeuronInfoResult {
    Ok(NeuronInfo),
    Err(GovernanceError),
}

fn state_name(state: i32) -> &'static str {
    match state {
        1 => "NotDissolving",
        2 => "Dissolving",
        3 => "Dissolved",
        4 => "Spawning",
        _ => "Unspecified",
    }
}

fn icp(e8s: u64) -> String {
    format!("{}.{:08}", e8s / E8S, e8s % E8S)
}

/// Shortens a principal to its first label, enough to tell owners apart in a table.
fn short(owner: &Principal) -> String {
    let text = owner.to_text();
    match text.split_once('-') {
        Some((head, _)) => format!("{head}-…"),
        None => text,
    }
}

async fn query<T>(agent: &Agent, canister: &str, method: &str, arg: Vec<u8>) -> Result<T, String>
where
    T: CandidType + for<'a> Deserialize<'a>,
{
    let canister = Principal::from_text(canister).expect("hardcoded principal");
    let bytes = agent
        .query(&canister, method)
        .with_arg(arg)
        .call()
        .await
        .map_err(|e| format!("{method} failed: {e}"))?;
    Decode!(&bytes, T).map_err(|e| format!("{method} decode failed: {e}"))
}

/// What the NNS says about a withdrawal neuron, against what WaterNeuron believes.
enum Verdict {
    /// Stopped behind WaterNeuron's back. Never disburses, and raises the main neuron on merge.
    Stranded,
    /// Dissolving, but the request never recorded `StartedToDissolve`.
    UnrecordedDissolve,
    /// Dissolved and waiting on `process_disburse`.
    ReadyToDisburse,
    Healthy,
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let agent = Agent::builder()
        .with_url(IC_URL)
        .with_identity(AnonymousIdentity)
        .build()
        .map_err(|e| format!("agent: {e}"))?;

    let mut all: Vec<WithdrawalDetails> = Vec::new();
    loop {
        let arg = Encode!(&(all.len() as u64), &PAGE).map_err(|e| e.to_string())?;
        let page: Vec<WithdrawalDetails> =
            query(&agent, WATER_NEURON, "list_withdrawal_requests", arg).await?;
        if page.is_empty() {
            break;
        }
        all.extend(page);
    }
    println!("withdrawals   {} total", all.len());

    let open: Vec<&WithdrawalDetails> = all
        .iter()
        .filter(|d| {
            matches!(
                d.status,
                WithdrawalStatus::WaitingToSplitNeuron
                    | WithdrawalStatus::WaitingToStartDissolving { .. }
                    | WithdrawalStatus::WaitingDissolvement { .. }
            )
        })
        .collect();
    println!("open          {}\n", open.len());

    println!(
        "{:<6} {:<26} {:<21} {:<14} {:>7} {:>14}  {}",
        "id", "waterneuron status", "neuron", "nns state", "delay", "icp due", "owner"
    );

    let (mut stranded, mut unrecorded, mut ready) = (Vec::new(), Vec::new(), Vec::new());
    for details in &open {
        let Some(neuron_id) = details.request.neuron_id else {
            println!(
                "{:<6} {:<26} {:<21} {:<14} {:>7} {:>14}  {}",
                details.request.withdrawal_id,
                "WaitingToSplitNeuron",
                "-",
                "-",
                "-",
                icp(details.request.icp_due),
                short(&details.request.receiver.owner)
            );
            continue;
        };

        let arg = Encode!(&neuron_id.id).map_err(|e| e.to_string())?;
        let info = match query::<NeuronInfoResult>(&agent, NNS_GOVERNANCE, "get_neuron_info", arg)
            .await?
        {
            NeuronInfoResult::Ok(info) => info,
            NeuronInfoResult::Err(err) => {
                println!(
                    "{:<6} {:<26} {:<21} governance error {}: {}",
                    details.request.withdrawal_id,
                    format!("{:?}", details.status),
                    neuron_id.id,
                    err.error_type,
                    err.error_message
                );
                continue;
            }
        };

        let verdict = match (&details.status, info.state) {
            (WithdrawalStatus::WaitingDissolvement { .. }, 1) => Verdict::Stranded,
            (WithdrawalStatus::WaitingDissolvement { .. }, 3) => Verdict::ReadyToDisburse,
            (WithdrawalStatus::WaitingToStartDissolving { .. }, 2 | 3) => {
                Verdict::UnrecordedDissolve
            }
            _ => Verdict::Healthy,
        };

        let status = match &details.status {
            WithdrawalStatus::WaitingDissolvement { .. } => "WaitingDissolvement",
            WithdrawalStatus::WaitingToStartDissolving { .. } => "WaitingToStartDissolving",
            other => {
                println!("unexpected status {other:?}");
                continue;
            }
        };
        let flag = match verdict {
            Verdict::Stranded => "  <-- STRANDED, frozen by a failed cancel",
            Verdict::UnrecordedDissolve => "  <-- dissolving, StartedToDissolve never recorded",
            Verdict::ReadyToDisburse => "  <-- dissolved, awaiting disburse",
            Verdict::Healthy => "",
        };
        println!(
            "{:<6} {:<26} {:<21} {:<14} {:>6.1}d {:>14}  {}{}",
            details.request.withdrawal_id,
            status,
            neuron_id.id,
            state_name(info.state),
            info.dissolve_delay_seconds as f64 / DAY as f64,
            icp(details.request.icp_due),
            short(&details.request.receiver.owner),
            flag
        );

        let row = (
            details.request.withdrawal_id,
            neuron_id.id,
            info.dissolve_delay_seconds,
            details.request.icp_due,
            details.request.receiver.owner,
        );
        match verdict {
            Verdict::Stranded => stranded.push(row),
            Verdict::UnrecordedDissolve => unrecorded.push(row),
            Verdict::ReadyToDisburse => ready.push(row),
            Verdict::Healthy => {}
        }
    }

    let total = |rows: &[(u64, u64, u64, u64, Principal)]| -> u64 {
        rows.iter().map(|(_, _, _, icp_due, _)| icp_due).sum()
    };

    println!("\nsummary");
    println!(
        "  stranded by a failed cancel   {:>3} requests, {:>16} ICP",
        stranded.len(),
        icp(total(&stranded))
    );
    println!(
        "  dissolving, never recorded    {:>3} requests, {:>16} ICP",
        unrecorded.len(),
        icp(total(&unrecorded))
    );
    println!(
        "  dissolved, awaiting disburse  {:>3} requests, {:>16} ICP",
        ready.len(),
        icp(total(&ready))
    );

    if let Some((id, neuron, delay, icp_due, owner)) = stranded
        .iter()
        .max_by_key(|(_, _, delay, _, _)| *delay)
        .copied()
    {
        println!(
            "\nhighest stranded delay: withdrawal {id}, neuron {neuron}, {:.1} days, {} ICP, owner {owner}",
            delay as f64 / DAY as f64,
            icp(icp_due),
        );
        println!(
            "cancelling it would raise the main neuron's dissolve delay to {:.1} days, since an NNS\n\
             merge gives the target max(source, target).",
            delay as f64 / DAY as f64
        );
    }

    Ok(())
}
