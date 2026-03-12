#!/usr/bin/env bash
#
# CI Coverage Integration Script
#
# Usage:
#   ./scripts/ci_coverage.sh              # Run tests + generate coverage
#   ./scripts/ci_coverage.sh --check 80   # Fail if coverage < 80%
#   ./scripts/ci_coverage.sh --report     # Generate HTML report only
#   ./scripts/ci_coverage.sh --diff       # Show coverage diff vs last run
#
# Requirements:
#   - cargo-tarpaulin (install: cargo install cargo-tarpaulin)
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
COVERAGE_DIR="$PROJECT_DIR/target/coverage"
REPORT_JSON="$COVERAGE_DIR/coverage.json"
REPORT_HTML="$COVERAGE_DIR/tarpaulin-report.html"
LAST_COVERAGE_FILE="$COVERAGE_DIR/.last_coverage"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log_info()  { echo -e "${BLUE}[INFO]${NC} $*"; }
log_ok()    { echo -e "${GREEN}[OK]${NC} $*"; }
log_warn()  { echo -e "${YELLOW}[WARN]${NC} $*"; }
log_fail()  { echo -e "${RED}[FAIL]${NC} $*"; }

# ── Check prerequisites ───────────────────────────────────────────────
check_tarpaulin() {
    if ! command -v cargo-tarpaulin &>/dev/null; then
        log_warn "cargo-tarpaulin not found. Installing..."
        cargo install cargo-tarpaulin
    fi
}

# ── Run tests ──────────────────────────────────────────────────────────
run_tests() {
    log_info "Running cargo test --lib..."
    cd "$PROJECT_DIR"
    local test_output
    test_output=$(cargo test --lib 2>&1)
    local test_result=$(echo "$test_output" | grep "test result:" | tail -1)

    if echo "$test_result" | grep -q "0 failed"; then
        local passed=$(echo "$test_result" | grep -oP '\d+ passed')
        log_ok "All tests passed ($passed)"
    else
        log_fail "Some tests failed!"
        echo "$test_result"
        exit 1
    fi
}

# ── Generate coverage ─────────────────────────────────────────────────
generate_coverage() {
    log_info "Generating coverage with tarpaulin..."
    mkdir -p "$COVERAGE_DIR"
    cd "$PROJECT_DIR"

    cargo tarpaulin \
        --lib \
        --out Json Html \
        --output-dir "$COVERAGE_DIR" \
        --skip-clean \
        --timeout 300 \
        --exclude-files "src/bin/*" "src/server/*" "src/raft/http_*" "src/raft/node.rs" \
        2>&1 | tail -5

    if [[ -f "$COVERAGE_DIR/tarpaulin-report.json" ]]; then
        cp "$COVERAGE_DIR/tarpaulin-report.json" "$REPORT_JSON"
    fi

    log_ok "Coverage report generated"
}

# ── Parse coverage percentage ──────────────────────────────────────────
get_coverage_pct() {
    if [[ ! -f "$REPORT_JSON" ]]; then
        echo "0"
        return
    fi
    # Extract coverage percentage from tarpaulin JSON
    python3 -c "
import json, sys
with open('$REPORT_JSON') as f:
    data = json.load(f)
if isinstance(data, list) and len(data) > 0:
    files = data[0].get('files', data) if isinstance(data[0], dict) else data
else:
    files = data.get('files', [])

total_lines = 0
covered_lines = 0
for entry in (files if isinstance(files, list) else []):
    traces = entry.get('traces', [])
    for t in traces:
        total_lines += 1
        if t.get('stats', {}).get('Line', 0) > 0:
            covered_lines += 1

if total_lines > 0:
    print(f'{covered_lines * 100.0 / total_lines:.2f}')
else:
    print('0')
" 2>/dev/null || echo "0"
}

# ── Coverage check ─────────────────────────────────────────────────────
check_coverage() {
    local threshold=${1:-80}
    local coverage=$(get_coverage_pct)

    log_info "Coverage: ${coverage}% (threshold: ${threshold}%)"

    if python3 -c "exit(0 if float('$coverage') >= float('$threshold') else 1)" 2>/dev/null; then
        log_ok "Coverage meets threshold"
        return 0
    else
        log_fail "Coverage below threshold! ($coverage% < $threshold%)"
        return 1
    fi
}

# ── Coverage diff ──────────────────────────────────────────────────────
coverage_diff() {
    local current=$(get_coverage_pct)

    if [[ -f "$LAST_COVERAGE_FILE" ]]; then
        local previous=$(cat "$LAST_COVERAGE_FILE")
        local diff=$(python3 -c "print(f'{float(\"$current\") - float(\"$previous\"):+.2f}')" 2>/dev/null || echo "+0")
        log_info "Coverage: ${current}% (previous: ${previous}%, diff: ${diff}%)"

        if python3 -c "exit(0 if float('$current') >= float('$previous') else 1)" 2>/dev/null; then
            log_ok "Coverage maintained or improved"
        else
            log_warn "Coverage decreased!"
        fi
    else
        log_info "Coverage: ${current}% (no previous baseline)"
    fi

    echo "$current" > "$LAST_COVERAGE_FILE"
}

# ── Summary ────────────────────────────────────────────────────────────
print_summary() {
    log_info "=== Coverage Summary ==="
    echo ""
    echo "  Report (HTML): $REPORT_HTML"
    echo "  Report (JSON): $REPORT_JSON"
    echo ""

    if [[ -f "$REPORT_JSON" ]]; then
        local coverage=$(get_coverage_pct)
        echo "  Total coverage: ${coverage}%"
    fi
    echo ""
}

# ── Main ───────────────────────────────────────────────────────────────
main() {
    local mode="${1:-full}"

    case "$mode" in
        --check)
            local threshold="${2:-80}"
            run_tests
            generate_coverage
            check_coverage "$threshold"
            ;;
        --report)
            check_tarpaulin
            generate_coverage
            print_summary
            ;;
        --diff)
            run_tests
            generate_coverage
            coverage_diff
            ;;
        --test-only)
            run_tests
            ;;
        full|*)
            check_tarpaulin
            run_tests
            generate_coverage
            coverage_diff
            print_summary
            ;;
    esac
}

main "$@"
