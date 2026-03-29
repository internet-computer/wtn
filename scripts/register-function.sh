#!/usr/bin/env bash
set -euo pipefail

usage() {
    echo "Usage: $0 --name <name> --description <desc> --target-method <method> [--id <function_id>] [--neuron-id <id>]"
    echo ""
    echo "Register a generic nervous system function for the WaterNeuron SNS."
    echo "The validator method is derived automatically as <target-method>_validate."
    echo ""
    echo "Options:"
    echo "  --neuron-id <id>     Proposer neuron id (hex)"
    echo "  --pem-file <path>    PEM file path (default: PEM_FILE env var)"
    echo "  --name               Display name for the function"
    echo "  --description        Description of the function"
    echo "  --target-method      Target method name on the canister"
    echo "  --id                 Function id (default: auto-detect next available)"
    exit 1
}

NEURON_ID="85ff8b442cca2eb2943fe74127085745f16d95b0d539993cd093f682f774dca8"
PEM_FILE="${PEM_FILE:-}"
FUNCTION_ID=""
FUNCTION_NAME=""
DESCRIPTION=""
TARGET_METHOD=""

while [[ $# -gt 0 ]]; do
    case $1 in
        --neuron-id) NEURON_ID="$2"; shift 2 ;;
        --pem-file) PEM_FILE="$2"; shift 2 ;;
        --name) FUNCTION_NAME="$2"; shift 2 ;;
        --description) DESCRIPTION="$2"; shift 2 ;;
        --target-method) TARGET_METHOD="$2"; shift 2 ;;
        --id) FUNCTION_ID="$2"; shift 2 ;;
        --help|-h) usage ;;
        *) echo "Unknown option: $1"; usage ;;
    esac
done

if [ -z "$FUNCTION_NAME" ] || [ -z "$DESCRIPTION" ] || [ -z "$TARGET_METHOD" ]; then
    usage
fi

VALIDATOR_METHOD="${TARGET_METHOD}_validate"

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

TARGET_CANISTER="tsbvt-pyaaa-aaaar-qafva-cai"
GOVERNANCE_CANISTER="jfnic-kaaaa-aaaaq-aadla-cai"

# Validate that both methods exist in the canister source.
MAIN_RS="water_neuron/src/main.rs"
missing=0
for method in "$TARGET_METHOD" "$VALIDATOR_METHOD"; do
    if ! grep -q "fn ${method}(" "$MAIN_RS"; then
        echo "Error: method '${method}' not found in ${MAIN_RS}"
        missing=1
    fi
done
if [ "$missing" -ne 0 ]; then
    exit 1
fi
echo "Both methods found in ${MAIN_RS}."

# Query existing functions from SNS governance.
echo "Querying registered functions from SNS governance..."
FUNCTIONS_OUTPUT="$(dfx canister call "$GOVERNANCE_CANISTER" list_nervous_system_functions '(record {})' --network ic 2>&1)"

echo ""
echo "Registered functions:"
echo "$FUNCTIONS_OUTPUT" | grep -oE 'id = [0-9_]+ : nat64; name = "[^"]*"' | while read -r line; do
    fn_id="$(echo "$line" | grep -oE 'id = [0-9_]+' | grep -oE '[0-9_]+' | tr -d '_')"
    fn_name="$(echo "$line" | sed 's/.*name = "//; s/"//')"
    printf "  %4s: %s\n" "$fn_id" "$fn_name"
done
echo ""

# Auto-detect the next available function ID if not provided.
if [ -z "$FUNCTION_ID" ]; then
    # Collect all used IDs (from functions) and reserved IDs.
    USED_IDS="$(echo "$FUNCTIONS_OUTPUT" | grep -oE 'id = [0-9_]+' | grep -oE '[0-9_]+' | tr -d '_')"
    RESERVED_IDS="$(echo "$FUNCTIONS_OUTPUT" | grep -oE 'reserved_ids = vec \{[^}]*\}' | grep -oE '[0-9_]+' | tr -d '_')"
    ALL_IDS="$(printf "%s\n%s" "$USED_IDS" "$RESERVED_IDS" | sort -n | tail -1)"
    if [ -z "$ALL_IDS" ] || [ "$ALL_IDS" -lt 1000 ]; then
        FUNCTION_ID=1000
    else
        FUNCTION_ID=$((ALL_IDS + 1))
    fi
    echo "Next available function ID: ${FUNCTION_ID}"
fi

# Create a sanitized directory name from the function name.
SAFE_NAME="$(echo "$FUNCTION_NAME" | tr '[:upper:]' '[:lower:]' | tr ' ' '_' | tr -cd 'a-z0-9_')"
DATE="$(date +%Y_%m_%d)"
PROPOSAL_DIR="proposals/${DATE}_register_${SAFE_NAME}"

if [ -d "$PROPOSAL_DIR" ]; then
    i=2
    while [ -d "${PROPOSAL_DIR}_${i}" ]; do
        i=$((i + 1))
    done
    PROPOSAL_DIR="${PROPOSAL_DIR}_${i}"
fi

mkdir -p "$PROPOSAL_DIR"

# Write the summary that SNS voters will see.
cat > "$PROPOSAL_DIR/summary.md" << EOF
# Register generic SNS function: ${FUNCTION_NAME}

${DESCRIPTION}

## Details

| Field | Value |
|-------|-------|
| Function ID | \`${FUNCTION_ID}\` |
| Target canister | \`${TARGET_CANISTER}\` |
| Target method | \`${TARGET_METHOD}\` |
| Validator method | \`${VALIDATOR_METHOD}\` |
EOF

# Write the submission script.
cat > "$PROPOSAL_DIR/submit.sh" << SCRIPT_HEADER
#!/usr/bin/env bash
set -euo pipefail

PEM_FILE="\${PEM_FILE:-${PEM_FILE}}"
if [ -z "\$PEM_FILE" ]; then
    echo "Error: PEM_FILE not set."
    echo "  export PEM_FILE=~/.config/dfx/identity/default/identity.pem"
    exit 1
fi

REPO_ROOT="\$(git rev-parse --show-toplevel)"
cd "\$REPO_ROOT"

SCRIPT_HEADER

cat >> "$PROPOSAL_DIR/submit.sh" << EOF
SNS_CANISTER_IDS="\$(mktemp)"
cat > "\$SNS_CANISTER_IDS" << 'IDS'
{
    "governance_canister_id": "${GOVERNANCE_CANISTER}",
    "root_canister_id": ""
}
IDS

quill sns make-proposal ${NEURON_ID} \\
    --proposal "( record {
        title = \\"Register generic SNS function: ${FUNCTION_NAME}\\";
        url = \\"https://github.com/waterneuron/WaterNeuron\\";
        summary = \\"${DESCRIPTION}\\";
        action = opt variant {
            AddGenericNervousSystemFunction = record {
                id = ${FUNCTION_ID} : nat64;
                name = \\"${FUNCTION_NAME}\\";
                description = opt \\"${DESCRIPTION}\\";
                function_type = opt variant {
                    GenericNervousSystemFunction = record {
                        validator_canister_id = opt principal \\"${TARGET_CANISTER}\\";
                        target_canister_id = opt principal \\"${TARGET_CANISTER}\\";
                        validator_method_name = opt \\"${VALIDATOR_METHOD}\\";
                        target_method_name = opt \\"${TARGET_METHOD}\\";
                    }
                };
            }
        };
    } )" \\
    --canister-ids-file "\$SNS_CANISTER_IDS" \\
    --pem-file "\$PEM_FILE" > msg.json

rm -f "\$SNS_CANISTER_IDS"

echo ""
echo "Proposal message written to msg.json"
echo "To submit: quill send msg.json"
EOF

chmod +x "$PROPOSAL_DIR/submit.sh"

echo ""
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
