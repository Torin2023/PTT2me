# Стабилизация release-контура PTT2me — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Цель:** добавить fail-closed preflight, независимую read-only проверку готового Full/Update release-набора, shell contract tests, CI gate и ручной P0 checklist без изменения пользовательского поведения PTT2me.

**Архитектура:** `scripts/release-preflight.sh` доказывает полноту среды и входов до production build. `scripts/build-release-artifacts.sh` остаётся единственным builder и вызывает preflight, после чего повторяет критичные проверки. `scripts/verify-release-artifacts.sh` не использует private key или builder workspace: он проверяет закрытый output-набор, signed payload, DMG, извлечённые bundle и source/tag identity.

**Технологии:** Bash 3.2-compatible shell, macOS `plutil`, `hdiutil`, `codesign`, `lipo`, `otool`, `PlistBuddy`, `shasum`, Rust/Cargo 1.94.0, Ed25519 signer/verifier PTT2me, GitHub Actions `macos-15`.

## Global Constraints

- Целевая платформа: Apple Silicon (`aarch64-apple-darwin`), macOS 13+.
- Не менять `src/`, Rust product behavior, TCC, updater, pasteboard или PTT semantics.
- Не менять production model manifest, signed release records, public key или license texts.
- Не загружать и не генерировать production model; private key остаётся вне Git.
- Не публиковать GitHub Release, Pages manifest или stable channel.
- Все ошибки gates имеют стабильную category и не печатают key/model/user contents.
- Невыполненная, skipped или timed-out проверка является failure.

---

### Task 1: Зафиксировать дизайн и устранить version drift fixture

**Files:**

- Create: `docs/superpowers/specs/2026-08-11-application-build-stabilization-design.md`
- Modify: `tests/model_bundle_variants.sh`

**Interfaces:**

- Consumes: package `version = "X.Y.Z"` из первой package-секции `Cargo.toml`.
- Produces: shell fixture `CURRENT_VERSION`, используемый для `CFBundleShortVersionString` без release literal.

- [ ] **Step 1: Подтвердить RED на исходном fixture**

  Run: `bash tests/model_bundle_variants.sh`

  Expected: FAIL до целевой проверки source commit с `CFBundleShortVersionString ... expected '1.1.0'`.

- [ ] **Step 2: Добавить ограниченный package-version parser**

  В начале `tests/model_bundle_variants.sh` после `REPO_ROOT` добавить:

  ```bash
  CURRENT_VERSION="$(awk -F '"' '/^version = "/ { print $2; exit }' "$REPO_ROOT/Cargo.toml")"
  [[ "$CURRENT_VERSION" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] || {
      echo "could not read canonical package version from Cargo.toml" >&2
      exit 1
  }
  ```

  Подставить `"$CURRENT_VERSION"` в `PlistBuddy` command вместо `1.0.5`/`1.1.0`.

- [ ] **Step 3: Проверить GREEN**

  Run: `bash tests/model_bundle_variants.sh`

  Expected: PASS и строка `Model bundle variant checks passed`.

### Task 2: Release preflight — RED contract suite и minimal implementation

**Files:**

- Create: `tests/release_preflight.sh`
- Create: `scripts/release-preflight.sh`

**Interfaces:**

- Consumes: `--version`, `--build`, `--source-commit`, `--model-manifest`, `--model-source`, `--public-key`, `--private-key`, `--published-at`, `--output-dir`.
- Produces: exit 0 plus `PTT2me release preflight passed: VERSION (COMMIT)`; failures use `PTT2me release preflight failed [category]: detail`.

- [ ] **Step 1: Создать fixture repository и failing cases**

  `tests/release_preflight.sh` копирует тестируемый script и generic model checker в temporary Git repository, создаёт synthetic four-file model manifest, private/public key fixtures и fake tool `PATH`. Добавить helpers:

  ```bash
  expect_failure() {
      local category="$1"
      shift
      local output
      if output="$("$@" 2>&1)"; then
          echo "expected failure [$category]: $*" >&2
          exit 1
      fi
      [[ "$output" == *"[$category]"* ]] || exit 1
  }
  ```

  Обязательные RED cases: invalid version/build/commit/timestamp, wrong architecture/old macOS/toolchain, dirty tree, Cargo.toml/Cargo.lock mismatch, symlink output, existing output, insufficient disk, public-key mismatch, key in repo/permissive mode, manifest byte mismatch, missing/extra/symlink/executable/hash-mismatched model, unavailable command, failing bin check и failing AppKit test.

- [ ] **Step 2: Запустить suite и увидеть отсутствие production script**

  Run: `bash tests/release_preflight.sh`

  Expected: FAIL because `scripts/release-preflight.sh` is absent.

- [ ] **Step 3: Реализовать fail-closed preflight в требуемом порядке**

  Реализация должна:

  ```bash
  fail() {
      local category="$1"
      shift
      echo "PTT2me release preflight failed [$category]: $*" >&2
      exit 1
  }
  ```

  Сначала валидировать scalar/path/Git/key/model/output/disk inputs, затем toolchain/binaries, в самом конце выполнять `cargo check --locked --bins` и `cargo test --locked --test pasteboard_main --features test-support -- --test-threads=1`. Model total size читается из четырёх manifest entries; reserve равен `2 * total_model_size + 1073741824` bytes на filesystem output workspace.

- [ ] **Step 4: Проверить GREEN и отсутствие секретов в errors**

  Run: `bash tests/release_preflight.sh`

  Expected: все negative cases отклонены правильной category, synthetic positive case PASS.

### Task 3: Builder вызывает preflight и сохраняет независимые critical checks

**Files:**

- Modify: `scripts/build-release-artifacts.sh`
- Create: `tests/release_builder_contracts.sh`

**Interfaces:**

- Consumes: неизменённый builder CLI.
- Produces: preflight до `mktemp`/Cargo release build; затем существующая повторная проверка version/Git/model/key/output и hard-link publication.

- [ ] **Step 1: Написать failing static/dynamic builder contracts**

  Проверить, что вызов `release-preflight.sh` расположен до `mktemp` и `build-app.sh`, а после него builder всё ещё содержит самостоятельные `git status`, exact manifest/public-key comparison, output non-existence и hard-link publication.

- [ ] **Step 2: Увидеть RED**

  Run: `bash tests/release_builder_contracts.sh`

  Expected: FAIL `builder must run release preflight before creating outputs`.

- [ ] **Step 3: Добавить preflight invocation с теми же девятью inputs**

  ```bash
  "$SCRIPT_DIR/release-preflight.sh" \
      --version "$VERSION" --build "$BUILD" --source-commit "$SOURCE_COMMIT" \
      --model-manifest "$MODEL_MANIFEST" --model-source "$MODEL_SOURCE" \
      --public-key "$PUBLIC_KEY" --private-key "$PRIVATE_KEY" \
      --published-at "$PUBLISHED_AT" --output-dir "$OUTPUT_DIR"
  ```

- [ ] **Step 4: Проверить GREEN**

  Run: `bash tests/release_builder_contracts.sh`

  Expected: PASS.

### Task 4: Independent artifact verifier — signed identity, DMG и cleanup

**Files:**

- Create: `tests/verify_release_artifacts.sh`
- Create: `scripts/verify-release-artifacts.sh`

**Interfaces:**

- Consumes: `--version`, `--source-commit`, optional `--expected-tag`, `--full-dmg`, `--full-checksum`, `--update-dmg`, `--update-checksum`, `--manifest`, `--public-key`, `--model-manifest`.
- Produces: exit 0 plus `PTT2me release artifacts verified: VERSION (COMMIT)`; no writes to supplied outputs.

- [ ] **Step 1: Создать signed synthetic output-set fixture**

  Test derives a temporary Ed25519 public key with `ptt2me-update-signer`, signs a payload whose hashes/sizes match two tiny synthetic DMG files, and places exactly five release outputs in one directory. A fixture copy of verifier uses real `validate-update-manifest.sh` with the built PTT2me verifier binary, real generic model-layout checks, fake `hdiutil` mount boundaries and deterministic fake bundles.

- [ ] **Step 2: Добавить RED negative cases**

  Cases: extra output/symlink/wrong name, malformed checksum, checksum mismatch, signed size/hash/signature mismatch, version/source/tag mismatch, Full without model, Update with model, `hdiutil verify`/attach/detach failure, bundle recheck failure, Mach-O parity failure, smoke failure и cleanup after each mounted failure point.

- [ ] **Step 3: Увидеть RED**

  Run: `bash tests/verify_release_artifacts.sh`

  Expected: FAIL because `scripts/verify-release-artifacts.sh` is absent.

- [ ] **Step 4: Реализовать read-only verifier**

  Verifier должен проверять closed set и `.sha256`, затем вызвать signed manifest verifier, декодировать уже проверенный payload только во временный каталог через `plutil` + `base64`, сравнить exact identity, проверить optional tag, выполнить `hdiutil verify`, read-only mount, `check-bundle.sh`, model layout, three unsigned Mach-O comparisons и bounded Full `--smoke-model`. `trap` отсоединяет оба mountpoints на EXIT/HUP/INT/TERM; явный detach failure блокирует success.

- [ ] **Step 5: Проверить GREEN**

  Run: `bash tests/verify_release_artifacts.sh`

  Expected: positive signed synthetic set PASS; каждый negative case даёт стабильную category и mounted images очищены.

### Task 5: Gate A запускает все shell contract tests

**Files:**

- Create: `scripts/test-shell-contracts.sh`
- Create: `tests/release_ci_contracts.sh`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**

- Consumes: четыре executable shell test files.
- Produces: единая команда Gate A `bash scripts/test-shell-contracts.sh`.

- [ ] **Step 1: Добавить runner/CI contract в tests**

  Проверить, что runner вызывает по одному разу:

  ```text
  tests/model_bundle_variants.sh
  tests/release_preflight.sh
  tests/release_builder_contracts.sh
  tests/verify_release_artifacts.sh
  ```

  Тот же test проверяет, что `.github/workflows/ci.yml` содержит отдельный step
  `bash scripts/test-shell-contracts.sh` после Rust test step.

- [ ] **Step 2: Реализовать runner и CI step**

  Runner использует `set -euo pipefail` и `bash` для каждого теста. В `ci.yml` после Rust tests добавить `Run shell contract tests` с `bash scripts/test-shell-contracts.sh`.

- [ ] **Step 3: Проверить Gate A shell portion**

  Run: `bash scripts/test-shell-contracts.sh`

  Expected: PASS.

### Task 6: Versioned Manual P0 checklist и release documentation

**Files:**

- Create: `docs/release/MANUAL_P0_CHECKLIST.md`
- Create: `tests/release_documentation.sh`
- Modify: `scripts/test-shell-contracts.sh`
- Modify: `README.md`

**Interfaces:**

- Consumes: version/build/source commit, Full DMG SHA-256, verifier result, tester name/date.
- Produces: canonical unfilled checklist; filled copies remain beside local outputs and untracked.

- [ ] **Step 1: Добавить documentation contract**

  `tests/release_documentation.sh` через exact `grep -F` проверяет presence всех P0 sections: identity, install/TCC, 20 Fn short, 20 Fn long, custom key/chords/autorepeat/tap failure, audio/ASR/space, AX/Unicode/pasteboard preservation/clipboard race/focus change, updater Full/Update/failure/open/quit/Finder cycle, final approval. До создания checklist test должен падать.

- [ ] **Step 2: Создать checklist и обновить README**

  README описывает Gate B/C commands, rehearsal без tag, повторный pre-publication verifier с `--expected-tag vX.Y.Z`, внешний key/model boundary и запрет публикации внутри scripts.
  Добавить `tests/release_documentation.sh` в единый shell runner.

- [ ] **Step 3: Проверить docs contract**

  Run: `bash tests/release_documentation.sh`

  Expected: PASS.

### Task 7: Полная локальная верификация и release rehearsal boundary

**Files:**

- Verify: весь checkout; production model/private key/output paths только внешние и untracked.

- [ ] **Step 1: Shell/static gates**

  Run:

  ```bash
  bash -n scripts/*.sh tests/*.sh
  bash scripts/test-shell-contracts.sh
  cargo fmt --all -- --check
  cargo clippy --all-targets --features test-support -- -D warnings
  cargo audit --no-fetch --deny warnings
  git diff --check
  ```

- [ ] **Step 2: Rust/macOS gate**

  Run: `cargo test --all-targets --features test-support -- --test-threads=1`

  Expected on GUI-capable session: PASS including `pasteboard_main`; if the sandbox returns `NSPasteboard pasteboardWithUniqueName` before assertion, record it as blocked AppKit evidence and require GitHub Actions.

- [ ] **Step 3: Production rehearsal when inputs exist**

  Run preflight, builder and verifier with exact clean HEAD, production model, external private key and empty output directory. Do not publish. If any required input is absent, report the exact missing gate instead of substituting or generating it.

- [ ] **Step 4: Independent review and final requirement audit**

  Review the full diff against the design, fix blocking findings, rerun affected checks, and report which acceptance criteria are demonstrated locally, delegated to GitHub Actions, manual, or blocked by external release inputs.
