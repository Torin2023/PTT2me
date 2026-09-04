#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
cd -- "$REPO_ROOT"

bash tests/model_bundle_variants.sh
bash tests/app_icon_contract.sh
bash tests/pages_artifact_contract.sh
bash tests/release_preflight.sh
bash tests/release_builder_contracts.sh
bash tests/verify_release_artifacts.sh
bash tests/release_ci_contracts.sh
bash tests/release_documentation.sh

echo "PTT2me shell contract gate passed"
