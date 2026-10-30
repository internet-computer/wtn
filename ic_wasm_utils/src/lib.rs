use lazy_static::lazy_static;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU64, Ordering};
use std::{collections::BTreeMap, path::PathBuf};
use thiserror::Error;

/// Paired with the process id, keeps one download's staging file from colliding with another's.
static STAGING_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Error)]
pub enum Error {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),
    #[error("Metadata error: {0}")]
    Metadata(#[from] cargo_metadata::Error),
    #[error("Hash mismatch")]
    HashMismatch,
    #[error("Unknown canister")]
    UnknownCanister,
    #[error("Build failed: {0}")]
    BuildFailed(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Hash, Eq, PartialOrd, Ord, PartialEq)]
pub enum CanisterName {
    Ledger,
    NnsGovernance,
    Icrc1Ledger,
    SnsGovernance,
    SnsSwap,
    Sns,
    SnsRoot,
    Cmc,
    Icrc1IndexNg,
    Local(String),
}

struct WasmBinary {
    hash: &'static str,
    ic_version: &'static str,
    name: &'static str,
}

lazy_static! {
    // The commit NNS governance runs on mainnet, elected by proposal 144200 on 2026-10-28. Its
    // governance-canister.wasm.gz hash is the module hash the canister reports.
    // REF: https://dashboard.internetcomputer.org/proposal/144200
    static ref DFINITY_CANISTERS: BTreeMap<CanisterName, WasmBinary> = {
        let ic_version = "98c898f8de068a1fc0e97ebc805f9fa7f315d862";

        let mut map = BTreeMap::new();
        map.insert(
            CanisterName::Ledger,
            WasmBinary {
                hash: "d16b2d3d8fc3673f14a255f0479ceca1877162127b1df828555eb5e19554dc27",
                ic_version,
                name: "ledger-canister.wasm.gz",
            },
        );
        map.insert(
            CanisterName::NnsGovernance,
            WasmBinary {
                hash: "7f85124ee08c3f59c0050009268807c7aea2b80ee296c23338b05458888f19df",
                ic_version,
                name: "governance-canister.wasm.gz",
            },
        );
        map.insert(
            CanisterName::Cmc,
            WasmBinary {
                hash: "51b2820d47822d7f7f38c9fbadffd39d3af9facd6c2b919ba0fe6963d04d265b",
                ic_version,
                name: "cycles-minting-canister.wasm.gz",
            },
        );
        map.insert(
            CanisterName::SnsGovernance,
            WasmBinary {
                hash: "24ef2004240d2a34bcdaa616134d27bc18d11c1e96b378a46d564feedcc1a8c3",
                ic_version,
                name: "sns-governance-canister.wasm.gz",
            },
        );
        map.insert(
            CanisterName::SnsSwap,
            WasmBinary {
                hash: "f3c9cd2a503f3136c91f1d44c5b4bac2d5f15a7d347731aaff03e5fa80df8c48",
                ic_version,
                name: "sns-swap-canister.wasm.gz",
            },
        );
        map.insert(
            CanisterName::Sns,
            WasmBinary {
                hash: "156eaf0b5d93ed7f4789c8fdd09463b025e25bca9380bf811f5d316fec657192",
                ic_version,
                name: "sns-wasm-canister.wasm.gz",
            },
        );
        map.insert(
            CanisterName::SnsRoot,
            WasmBinary {
                hash: "bd39681c7586e2ecfafbd2857b8ee6d04899c1f69ac59f420a1ccdc2e054ae36",
                ic_version,
                name: "sns-root-canister.wasm.gz",
            },
        );
        map.insert(
            CanisterName::Icrc1Ledger,
            WasmBinary {
                hash: "67f7bad791c19a92c5ac17a65f3f5faa43195e5e568ddff1c90f33134dc25d12",
                ic_version,
                name: "ic-icrc1-ledger.wasm.gz",
            },
        );
        map.insert(
            CanisterName::Icrc1IndexNg,
            WasmBinary {
                hash: "034bb1d09db50d8b8d3df5a03a16dedf90584226062738d85424582c3dbdc993",
                ic_version,
                name: "ic-icrc1-index-ng.wasm.gz",
            },
        );
        map
    };
    static ref WORKSPACE_ROOT: PathBuf = cargo_metadata::MetadataCommand::new()
        .no_deps()
        .exec()
        .expect("Failed to get workspace root")
        .workspace_root
        .into();
    static ref BOOMERANG_WASM: Vec<u8> =
        get_wasm_sync(CanisterName::Local("boomerang".to_string()), false).unwrap();
    static ref WATER_NEURON_WASM: Vec<u8> =
        get_wasm_sync(CanisterName::Local("water_neuron".to_string()), true).unwrap();
}

pub async fn get_wasm_path(name: CanisterName, self_check: bool) -> Result<PathBuf> {
    match name {
        CanisterName::Local(name) => build_local_wasm(&name, self_check),
        remote => fetch_remote_wasm(remote).await,
    }
}

pub fn get_wasm_path_sync(name: CanisterName, self_check: bool) -> Result<PathBuf> {
    match name {
        CanisterName::Local(name) => build_local_wasm(&name, self_check),
        _remote => unreachable!(),
    }
}

async fn get_wasm(name: CanisterName, self_check: bool) -> Result<Vec<u8>> {
    Ok(std::fs::read(get_wasm_path(name, self_check).await?)?)
}

fn get_wasm_sync(name: CanisterName, self_check: bool) -> Result<Vec<u8>> {
    Ok(std::fs::read(get_wasm_path_sync(name, self_check)?)?)
}

fn build_local_wasm(name: &str, self_check: bool) -> Result<PathBuf> {
    std::fs::create_dir_all(WORKSPACE_ROOT.join("artifacts"))?;

    let home_dir = std::env::var("HOME")
        .map_err(|e| Error::Io(std::io::Error::new(std::io::ErrorKind::NotFound, e)))?;
    let cargo_dir = PathBuf::from(home_dir).join(".cargo");

    let rustflags = format!(
        "RUSTFLAGS=\"--remap-path-prefix={}= --remap-path-prefix={}=\"",
        WORKSPACE_ROOT.display(),
        cargo_dir.display()
    );

    let file_name = format!("{0}{1}", name, if self_check { "_self_check" } else { "" });

    let build_steps = [
        format!(
            "{0} cargo canister -p {1} --release --bin {1} --locked {2}",
            rustflags,
            name,
            if self_check {
                "--features=self_check"
            } else {
                ""
            }
        ),
        format!(
            "ic-wasm target/wasm32-unknown-unknown/release/{0}.wasm -o artifacts/{0}.wasm metadata candid:service -f {0}/{0}.did -v public",
            name
        ),
        format!(
            "ic-wasm artifacts/{0}.wasm -o artifacts/{1}.wasm metadata git_commit_id -d $(git rev-parse HEAD) -v public",
            name, file_name
        ),
        format!("ic-wasm artifacts/{0}.wasm shrink", file_name),
        format!(
            "gzip -cnf9 artifacts/{0}.wasm > artifacts/{0}.wasm.gz",
            file_name
        ),
        format!("rm artifacts/{0}.wasm", file_name),
    ];

    for cmd in &build_steps {
        if !std::process::Command::new("sh")
            .current_dir(&*WORKSPACE_ROOT)
            .args(["-c", cmd])
            .status()?
            .success()
        {
            return Err(Error::BuildFailed(cmd.to_string()));
        }
    }

    Ok(WORKSPACE_ROOT.join(format!("artifacts/{}.wasm.gz", file_name)))
}

async fn fetch_remote_wasm(canister: CanisterName) -> Result<PathBuf> {
    let wasm = DFINITY_CANISTERS
        .get(&canister)
        .ok_or(Error::UnknownCanister)?;
    let cache_path = WORKSPACE_ROOT.join("artifacts").join(wasm.name);

    if let Ok(data) = std::fs::read(&cache_path) {
        let mut hasher = Sha256::new();
        hasher.update(&data);
        if format!("{:x}", hasher.finalize()) == wasm.hash {
            return Ok(cache_path);
        }
    }

    std::fs::create_dir_all(cache_path.parent().unwrap())?;
    let url = format!(
        "https://download.dfinity.systems/ic/{}/canisters/{}",
        wasm.ic_version, wasm.name
    );

    let response = reqwest::get(&url).await?;
    let data = response.bytes().await?.to_vec();

    let mut hasher = Sha256::new();
    hasher.update(&data);
    if format!("{:x}", hasher.clone().finalize()) != wasm.hash {
        return Err(Error::HashMismatch);
    }

    // Staged and renamed, so a concurrent fetch never reads a half-written module.
    let staging_path = cache_path.with_extension(format!(
        "{}.{}.partial",
        std::process::id(),
        STAGING_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&staging_path, &data)?;
    std::fs::rename(&staging_path, &cache_path)?;
    Ok(cache_path)
}

pub fn boomerang_wasm() -> Vec<u8> {
    BOOMERANG_WASM.to_vec()
}
pub fn water_neuron_wasm() -> Vec<u8> {
    WATER_NEURON_WASM.to_vec()
}

pub async fn icp_ledger_wasm() -> Vec<u8> {
    get_wasm(CanisterName::Ledger, false).await.unwrap()
}

pub async fn governance_wasm() -> Vec<u8> {
    get_wasm(CanisterName::NnsGovernance, false).await.unwrap()
}

pub async fn ledger_wasm() -> Vec<u8> {
    get_wasm(CanisterName::Icrc1Ledger, false).await.unwrap()
}

pub async fn sns_governance_wasm() -> Vec<u8> {
    get_wasm(CanisterName::SnsGovernance, false).await.unwrap()
}

pub async fn sns_root_wasm() -> Vec<u8> {
    get_wasm(CanisterName::SnsRoot, false).await.unwrap()
}

pub async fn sns_swap_wasm() -> Vec<u8> {
    get_wasm(CanisterName::SnsSwap, false).await.unwrap()
}

pub async fn cmc_wasm() -> Vec<u8> {
    get_wasm(CanisterName::Cmc, false).await.unwrap()
}
