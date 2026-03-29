#!/usr/bin/env bash
set -euo pipefail

usage() {
    echo "Usage: $0 [OPTIONS]"
    echo ""
    echo "Prepare an SNS upgrade proposal for the WaterNeuron canister."
    echo ""
    echo "Options:"
    echo "  --neuron-id <id>     Proposer neuron id (hex)"
    echo "  --pem-file <path>    Path to PEM file for quill"
    echo "  --title <title>      Proposal title (default: 'Upgrade WaterNeuron Protocol')"
    echo "  --description <desc> Optional description body"
    exit 1
}

TARGET_CANISTER="tsbvt-pyaaa-aaaar-qafva-cai"
NEURON_ID="85ff8b442cca2eb2943fe74127085745f16d95b0d539993cd093f682f774dca8"
PEM_FILE=""
GOVERNANCE_CANISTER="jfnic-kaaaa-aaaaq-aadla-cai"
WASM_PATH="./artifacts/water_neuron.wasm.gz"
DID_PATH="./water_neuron/water_neuron.did"
TITLE="Upgrade WaterNeuron Protocol"
DESCRIPTION=""

while [[ $# -gt 0 ]]; do
    case $1 in
        --neuron-id) NEURON_ID="$2"; shift 2 ;;
        --pem-file) PEM_FILE="$2"; shift 2 ;;
        --title) TITLE="$2"; shift 2 ;;
        --description) DESCRIPTION="$2"; shift 2 ;;
        --help|-h) usage ;;
        *) echo "Unknown option: $1"; usage ;;
    esac
done

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

if ! git diff-index --quiet HEAD --; then
    echo "Error: working tree is dirty. Commit your changes first."
    exit 1
fi

COMMIT_HASH="$(git rev-parse HEAD)"
DATE="$(date +%Y_%m_%d)"
PROPOSAL_DIR="proposals/${DATE}_upgrade_waterneuron"

if [ -d "$PROPOSAL_DIR" ]; then
    i=2
    while [ -d "${PROPOSAL_DIR}_${i}" ]; do
        i=$((i + 1))
    done
    PROPOSAL_DIR="${PROPOSAL_DIR}_${i}"
fi

echo "Building WASM at commit $COMMIT_HASH ..."
./build.sh

echo "Encoding upgrade args ..."
ARG_HASH="$(didc encode -d "$DID_PATH" -t '(LiquidArg)' '(variant{Upgrade})' \
    | xxd -r -p | tee water_neuron_arg.bin | shasum -a 256 | awk '{print $1}')"

WASM_HASH="$(shasum -a 256 "$WASM_PATH" | awk '{print $1}')"

echo ""
echo "Commit:    $COMMIT_HASH"
echo "WASM hash: $WASM_HASH"
echo "Args hash: $ARG_HASH"
echo ""

mkdir -p "$PROPOSAL_DIR"

# Write the summary that SNS voters will see
if [ -n "$DESCRIPTION" ]; then
    DESCRIPTION_BLOCK="
${DESCRIPTION}
"
else
    DESCRIPTION_BLOCK=""
fi

cat > "$PROPOSAL_DIR/summary.md" << EOF
# ${TITLE}
${DESCRIPTION_BLOCK}
## Upgrade args

The args module hash is \`${ARG_HASH}\`.

\`\`\`
git fetch
git checkout ${COMMIT_HASH}
didc encode -d ${DID_PATH} -t '(LiquidArg)' '(variant{Upgrade})' | xxd -r -p > water_neuron_arg.bin
sha256sum water_neuron_arg.bin
\`\`\`

## Wasm Verification

The compressed canister WebAssembly module is built from commit \`${COMMIT_HASH}\`.
The compressed module hash is \`${WASM_HASH}\`.
Target canister: \`${TARGET_CANISTER}\`.

To build the wasm module yourself and verify its hash, run the following commands from the root of the water_neuron repo:
\`\`\`
git fetch
git checkout ${COMMIT_HASH}
./build.sh
\`\`\`
EOF

# Write the submission script
cat > "$PROPOSAL_DIR/submit.sh" << 'SCRIPT_HEADER'
#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

if [ ! -f water_neuron_arg.bin ]; then
    echo "Error: water_neuron_arg.bin not found. Run scripts/prepare-proposal.sh first."
    exit 1
fi

SCRIPT_HEADER

# Determine PEM flag for the generated submit script.
if [ -n "$PEM_FILE" ]; then
    PEM_FLAG="--pem-file \"${PEM_FILE}\""
else
    PEM_FLAG='--pem-file "$PEM_FILE"'
    # Add PEM_FILE check to submit.sh when not baked in.
    cat >> "$PROPOSAL_DIR/submit.sh" << 'PEM_CHECK'
if [ -z "${PEM_FILE:-}" ]; then
    echo "Error: PEM_FILE not set."
    echo "  export PEM_FILE=~/.config/dfx/identity/default/identity.pem"
    exit 1
fi

PEM_CHECK
fi

cat >> "$PROPOSAL_DIR/submit.sh" << EOF
SNS_CANISTER_IDS="\$(mktemp)"
cat > "\$SNS_CANISTER_IDS" << 'IDS'
{
    "governance_canister_id": "${GOVERNANCE_CANISTER}",
    "root_canister_id": ""
}
IDS

PROPOSAL_DIR="${PROPOSAL_DIR}"

quill sns make-upgrade-canister-proposal ${NEURON_ID} \\
    --target-canister-id ${TARGET_CANISTER} \\
    --wasm-path "${WASM_PATH}" \\
    --canister-upgrade-arg-path "./water_neuron_arg.bin" \\
    --mode upgrade \\
    --summary-path "\${REPO_ROOT}/\${PROPOSAL_DIR}/summary.md" \\
    --title "${TITLE}" \\
    --url "https://github.com/internet-computer/wtn" \\
    ${PEM_FLAG} \\
    --canister-ids-file "\$SNS_CANISTER_IDS" > msg.json

rm -f "\$SNS_CANISTER_IDS"

echo ""
echo "Proposal message written to msg.json"
echo "To submit: quill send msg.json"
EOF

chmod +x "$PROPOSAL_DIR/submit.sh"

echo "Proposal prepared in: $PROPOSAL_DIR/"
echo "  summary.md  -- review and edit before submitting"
echo "  submit.sh   -- run to generate msg.json"
echo ""
echo "Next steps:"
echo "  1. Review ${PROPOSAL_DIR}/summary.md"
if [ -z "$PEM_FILE" ]; then
    echo "  2. export PEM_FILE=~/.config/dfx/identity/default/identity.pem"
fi
echo "  3. nix develop --command bash ${PROPOSAL_DIR}/submit.sh"
echo "  4. quill send msg.json"
