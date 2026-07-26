# Critical Red Menu Icon Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show a red warning triangle whenever PTT2me is blocked by a missing permission or a non-recoverable error, while preserving the existing appearance of transient and working states.

**Architecture:** Keep status-to-visual mapping inside `MenuProjection`. Make symbol style an explicit projection field, then let the existing AppKit renderer consume that style without inferring severity from an SF Symbol name.

**Tech Stack:** Rust 2021, `objc2-app-kit`, AppKit `NSImageSymbolConfiguration`, SF Symbols.

## Global Constraints

- `PermissionBlocked(_)` and `Error { recoverable: false, .. }` use `exclamationmark.triangle.fill` with hierarchical system red.
- `Error { recoverable: true, .. }` remains a template `exclamationmark.circle.fill`.
- `Starting`, `Ready`, `Recording`, and `Recognizing` keep their current symbols and behavior.
- Recording remains hierarchical red.
- The immutable four-entry menu is never rebuilt.
- `contentTintColor` is not used.

---

### Task 1: Project blocking severity into the status-bar symbol

**Files:**
- Modify: `src/menu.rs:15-84`
- Modify: `src/menu.rs:256-279`
- Test: `src/menu.rs:324-430`

**Interfaces:**
- Consumes: `AppStatus::{PermissionBlocked, Error}` and the existing `MenuBar::render(&AppStatus)` path.
- Produces: public `SymbolStyle` and `MenuProjection { title, symbol, pulse, style }`; `system_symbol` renders the explicit style.

- [ ] **Step 1: Write the failing behavioral tests**

Add a test that exercises the current production projection and styling:

```rust
#[test]
fn blocking_states_use_red_warning_triangles() {
    let permission =
        MenuProjection::from_status(&AppStatus::PermissionBlocked(PermissionKind::Microphone));
    assert_eq!(permission.symbol, "exclamationmark.triangle.fill");
    assert_eq!(symbol_style(&permission), SymbolStyle::HierarchicalRed);

    let persistent = MenuProjection::from_status(&AppStatus::Error {
        message: "Модель недоступна",
        recoverable: false,
    });
    assert_eq!(persistent.symbol, "exclamationmark.triangle.fill");
    assert_eq!(symbol_style(&persistent), SymbolStyle::HierarchicalRed);
}

#[test]
fn recoverable_error_keeps_template_circle() {
    let transient = MenuProjection::from_status(&AppStatus::Error {
        message: "Ошибка микрофона",
        recoverable: true,
    });
    assert_eq!(transient.symbol, "exclamationmark.circle.fill");
    assert_eq!(symbol_style(&transient), SymbolStyle::Template);
}
```

- [ ] **Step 2: Run the tests and verify RED**

Run:

```bash
cargo test --target aarch64-apple-darwin menu::tests::blocking_states_use_red_warning_triangles
```

Expected: FAIL because permission blocking currently resolves to
`SymbolStyle::Template`, and a persistent error currently uses
`exclamationmark.circle.fill`.

Run:

```bash
cargo test --target aarch64-apple-darwin menu::tests::recoverable_error_keeps_template_circle
```

Expected: PASS, documenting the behavior that must remain unchanged.

- [ ] **Step 3: Make style explicit in `MenuProjection`**

Replace the private, symbol-name-derived style with:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolStyle {
    Template,
    HierarchicalRed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuProjection {
    pub title: String,
    pub symbol: &'static str,
    pub pulse: bool,
    pub style: SymbolStyle,
}

fn symbol_style(projection: &MenuProjection) -> SymbolStyle {
    projection.style
}
```

Project every status explicitly:

```rust
AppStatus::Starting =>
    ("● Запуск…".into(), "hourglass", false, SymbolStyle::Template),
AppStatus::PermissionBlocked(permission) => (
    format!("● Нужен доступ: {}", permission_title(*permission)),
    "exclamationmark.triangle.fill",
    false,
    SymbolStyle::HierarchicalRed,
),
AppStatus::Ready =>
    ("● Готово".into(), "mic", false, SymbolStyle::Template),
AppStatus::Recording => (
    "● Запись…".into(),
    "record.circle.fill",
    false,
    SymbolStyle::HierarchicalRed,
),
AppStatus::Recognizing =>
    ("● Распознавание…".into(), "waveform", true, SymbolStyle::Template),
AppStatus::Error {
    message,
    recoverable: false,
} => (
    format!("● Ошибка: {message}"),
    "exclamationmark.triangle.fill",
    false,
    SymbolStyle::HierarchicalRed,
),
AppStatus::Error {
    message,
    recoverable: true,
} => (
    format!("● Ошибка: {message}"),
    "exclamationmark.circle.fill",
    false,
    SymbolStyle::Template,
),
```

Destructure the fourth tuple member into `style` and store it in the
projection. Update existing exact `MenuProjection` test literals with their
expected `style`.

- [ ] **Step 4: Keep AppKit rendering on the proven hierarchical-red path**

Retain the existing `system_symbol` match:

```rust
match symbol_style(projection) {
    SymbolStyle::Template => {
        unsafe { image.setTemplate(true) };
        Some(image)
    }
    SymbolStyle::HierarchicalRed => {
        let configuration = unsafe {
            NSImageSymbolConfiguration::configurationWithHierarchicalColor(
                &NSColor::systemRedColor(),
            )
        };
        let image =
            unsafe { image.imageWithSymbolConfiguration(&configuration) }.unwrap_or(image);
        unsafe { image.setTemplate(false) };
        Some(image)
    }
}
```

Do not change `MenuBar::render`, menu entries, pulse handling, or
`contentTintColor`.

- [ ] **Step 5: Run focused and full automated verification**

Run:

```bash
cargo fmt --check
cargo test --target aarch64-apple-darwin menu::tests
cargo test --target aarch64-apple-darwin
cargo clippy --all-targets --target aarch64-apple-darwin -- -D warnings
```

Expected: both new tests and the full suite pass with no Clippy warnings.

- [ ] **Step 6: Build and visually verify the blocked state**

Run:

```bash
scripts/build-app.sh
scripts/grant-and-run.sh
pgrep -fl PTT2me
osascript -e 'tell application "System Events" to tell process "PTT2me" to get description of every menu bar item of menu bar 1'
```

Expected: the process remains alive and Accessibility reports the blocking
status. Capture the menu bar and verify the warning triangle is visibly red.
Do not reset TCC during this check.

- [ ] **Step 7: Commit**

```bash
git add src/menu.rs
git commit -m "feat: show critical menu states in red"
```
