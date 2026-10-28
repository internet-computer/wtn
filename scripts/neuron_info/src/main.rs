//! Dump an NNS neuron, read from the governance canister with anonymous queries.
//!
//!     cargo run -- <neuron_id>
//!     cargo run -- --nonce <n> [--controller <principal>]

use candid::{CandidType, Decode, Encode, Principal};
use chrono::DateTime;
use ic_agent::{Agent, identity::AnonymousIdentity};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

const IC_URL: &str = "https://icp-api.io";
const NNS_GOVERNANCE: &str = "rrkah-fqaaa-aaaaa-aaaaq-cai";
/// Default controller for --nonce: the WaterNeuron canister, which holds nonces 0 (6m) and 1 (8y).
const DEFAULT_CONTROLLER: &str = "tsbvt-pyaaa-aaaar-qafva-cai";
const DAY: u64 = 86_400;
const E8S: u64 = 100_000_000;
/// MAX_DISSOLVE_DELAY_SECONDS, rs/nns/governance — 2 * ONE_YEAR_SECONDS since Mission 70 cut it
/// from eight years. Not exposed by any governance endpoint, unlike the minimum.
const MAX_DISSOLVE_DELAY_SECONDS: u64 = 63_115_200;

#[derive(CandidType, Deserialize, Debug)]
struct NeuronInfo {
    dissolve_delay_seconds: u64,
    retrieved_at_timestamp_seconds: u64,
    created_timestamp_seconds: u64,
    state: i32,
    stake_e8s: u64,
    age_seconds: u64,
    visibility: Option<i32>,
    voting_power_refreshed_timestamp_seconds: Option<u64>,
    deciding_voting_power: Option<u64>,
    potential_voting_power: Option<u64>,
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

#[derive(CandidType, Deserialize, Debug)]
enum NeuronIdOrSubaccount {
    Subaccount(Vec<u8>),
    NeuronId(NeuronId),
}

#[derive(CandidType, Deserialize, Debug)]
struct NeuronId {
    id: u64,
}

/// Staking subaccount of a neuron: sha256(0x0c, "neuron-stake", controller, nonce).
fn staking_subaccount(controller: Principal, nonce: u64) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update([0x0c]);
    hasher.update(b"neuron-stake");
    hasher.update(controller.as_slice());
    hasher.update(nonce.to_be_bytes());
    hasher.finalize().to_vec()
}

#[derive(CandidType, Deserialize, Debug)]
struct VotingPowerEconomics {
    neuron_minimum_dissolve_delay_to_vote_seconds: Option<u64>,
    start_reducing_voting_power_after_seconds: Option<u64>,
    clear_following_after_seconds: Option<u64>,
}

#[derive(CandidType, Deserialize, Debug)]
struct NetworkEconomics {
    voting_power_economics: Option<VotingPowerEconomics>,
}

fn utc(ts: u64) -> String {
    DateTime::from_timestamp(ts as i64, 0)
        .map(|t| t.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| format!("invalid timestamp {ts}"))
}

fn icp(e8s: u64) -> String {
    format!("{}.{:08} ICP", e8s / E8S, e8s % E8S)
}

async fn query<T>(agent: &Agent, method: &str, arg: Vec<u8>) -> Result<T, String>
where
    T: CandidType + for<'a> Deserialize<'a>,
{
    let governance = Principal::from_text(NNS_GOVERNANCE).expect("hardcoded principal");
    let bytes = agent
        .query(&governance, method)
        .with_arg(arg)
        .call()
        .await
        .map_err(|e| format!("{method} failed: {e}"))?;
    Decode!(&bytes, T).map_err(|e| format!("{method} decode failed: {e}"))
}

const USAGE: &str = "usage: neuron_info <neuron_id> | --nonce <n> [--controller <principal>]";

/// Either a neuron id, or a controller and nonce the staking subaccount is derived from.
/// Returns the query argument and a label for it, since NeuronInfo carries no id of its own.
fn target() -> Result<(NeuronIdOrSubaccount, String), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .map(|i| {
                args.get(i + 1)
                    .cloned()
                    .ok_or(format!("{name} needs a value"))
            })
            .transpose()
    };

    match flag("--nonce")? {
        None => {
            let id: u64 = args
                .first()
                .ok_or(USAGE)?
                .parse()
                .map_err(|e| format!("bad neuron id: {e}"))?;
            Ok((
                NeuronIdOrSubaccount::NeuronId(NeuronId { id }),
                format!("{id}"),
            ))
        }
        Some(nonce) => {
            let nonce: u64 = nonce.parse().map_err(|e| format!("bad nonce: {e}"))?;
            let controller = flag("--controller")?.unwrap_or(DEFAULT_CONTROLLER.to_string());
            let controller =
                Principal::from_text(&controller).map_err(|e| format!("bad controller: {e}"))?;
            Ok((
                NeuronIdOrSubaccount::Subaccount(staking_subaccount(controller, nonce)),
                format!("{controller} nonce {nonce}"),
            ))
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let (target, label) = target()?;

    let agent = Agent::builder()
        .with_url(IC_URL)
        .with_identity(AnonymousIdentity)
        .build()
        .map_err(|e| format!("agent: {e}"))?;

    let arg = Encode!(&target).map_err(|e| e.to_string())?;
    let info = match query::<NeuronInfoResult>(&agent, "get_neuron_info_by_id_or_subaccount", arg)
        .await?
    {
        NeuronInfoResult::Ok(info) => info,
        NeuronInfoResult::Err(err) => {
            return Err(format!(
                "governance error {}: {}",
                err.error_type, err.error_message
            ));
        }
    };
    let economics: NetworkEconomics = query(
        &agent,
        "get_network_economics_parameters",
        Encode!().map_err(|e| e.to_string())?,
    )
    .await?;
    let economics = economics.voting_power_economics;
    let min_delay = economics
        .as_ref()
        .and_then(|e| e.neuron_minimum_dissolve_delay_to_vote_seconds);

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before 1970")
        .as_secs();

    let state = match info.state {
        0 => "Unspecified".to_string(),
        1 => "NotDissolving".to_string(),
        2 => "Dissolving".to_string(),
        3 => "Dissolved".to_string(),
        4 => "Spawning".to_string(),
        other => format!("unknown({other})"),
    };
    let visibility = match info.visibility {
        None => "unset".to_string(),
        Some(1) => "PRIVATE".to_string(),
        Some(2) => "PUBLIC".to_string(),
        Some(other) => format!("unknown({other})"),
    };

    println!("neuron        {label}");
    println!("snapshot      {}", utc(info.retrieved_at_timestamp_seconds));
    println!("created       {}", utc(info.created_timestamp_seconds));
    println!("state         {state}   visibility {visibility}");
    println!("stake         {}", icp(info.stake_e8s));
    println!("age           {} days", info.age_seconds / DAY);
    println!(
        "dissolve dly  {} s ({} days)",
        info.dissolve_delay_seconds,
        info.dissolve_delay_seconds / DAY
    );
    println!(
        "delay bounds  {} .. {MAX_DISSOLVE_DELAY_SECONDS} s ({} days)",
        match min_delay {
            Some(min) => format!("{min} s ({} days)", min / DAY),
            None => "minimum unset in NetworkEconomics".to_string(),
        },
        MAX_DISSOLVE_DELAY_SECONDS / DAY,
    );
    if let Some(min) = min_delay {
        println!(
            "can vote      {}",
            if info.dissolve_delay_seconds >= min {
                "yes".to_string()
            } else {
                format!(
                    "no - {} s below the minimum dissolve delay to vote",
                    min - info.dissolve_delay_seconds
                )
            }
        );
    }

    if let Some(refreshed) = info.voting_power_refreshed_timestamp_seconds {
        println!(
            "vp refreshed  {} ({} days ago)",
            utc(refreshed),
            now.saturating_sub(refreshed) / DAY
        );
        if let Some(e) = &economics
            && let (Some(reduce_after), Some(clear_after)) = (
                e.start_reducing_voting_power_after_seconds,
                e.clear_following_after_seconds,
            )
        {
            println!(
                "vp decays     {}   following cleared {}",
                utc(refreshed + reduce_after),
                utc(refreshed + reduce_after + clear_after)
            );
        }
    }
    if let (Some(deciding), Some(potential)) =
        (info.deciding_voting_power, info.potential_voting_power)
    {
        println!("voting power  deciding {deciding}, potential {potential}");
    }

    if info.state == 2 {
        let when = info.retrieved_at_timestamp_seconds + info.dissolve_delay_seconds;
        println!(
            "dissolves at  {} ({} days)",
            utc(when),
            when.saturating_sub(now) / DAY
        );
    }

    Ok(())
}
