#!/usr/bin/env bash
# setup-github-labels.sh — Create/update GitHub labels for abstracts33d/fleet
# Idempotent: uses --force to update existing labels, creates missing ones.
# Usage: bash scripts/setup-github-labels.sh

set -euo pipefail

REPO="abstracts33d/fleet"

echo "Setting up GitHub labels for ${REPO}..."

# Helper: create or update a label
upsert_label() {
  local name="$1"
  local color="$2"
  local description="$3"

  # Use exact tab-delimited first-field match to avoid partial matches
  if gh label list -R "$REPO" --limit 200 | awk -F'\t' '{print $1}' | grep -qxF "$name"; then
    gh label edit "$name" -R "$REPO" \
      --color "$color" \
      --description "$description" &&
      echo "  updated: $name"
  else
    gh label create "$name" -R "$REPO" \
      --color "$color" \
      --description "$description" &&
      echo "  created: $name"
  fi
}

# Helper: delete a label if it exists
delete_label() {
  local name="$1"
  if gh label list -R "$REPO" --limit 200 | awk -F'\t' '{print $1}' | grep -qxF "$name"; then
    gh label delete "$name" -R "$REPO" --yes &&
      echo "  deleted: $name"
  else
    echo "  (not found, skip delete): $name"
  fi
}

echo ""
echo "--- Removing unused default labels ---"
delete_label "duplicate"
delete_label "enhancement"
delete_label "good first issue"
delete_label "help wanted"
delete_label "invalid"
delete_label "question"
delete_label "wontfix"
delete_label "documentation"

echo ""
echo "--- Scope labels ---"
upsert_label "scope:core" "1d76db" "Core NixOS/HM modules"
upsert_label "scope:graphical" "0e8a16" "Graphical desktop scope"
upsert_label "scope:dev" "5319e7" "Dev tools scope"
upsert_label "scope:desktop" "006b75" "Desktop compositor scope"
upsert_label "scope:enterprise" "b60205" "Enterprise features scope"
upsert_label "scope:hardware" "d93f0b" "Hardware scope"
upsert_label "scope:wrappers" "c2e0c6" "Portable wrappers scope"
upsert_label "scope:testing" "fef2c0" "Testing infrastructure"
upsert_label "scope:claude" "d4c5f9" "Claude Code / AI tooling"
upsert_label "scope:nixfleet" "ff9f1c" "NixFleet platform"

echo ""
echo "--- Type labels ---"
upsert_label "feature" "a2eeef" "New feature or request"
upsert_label "bug" "d73a4a" "Something isn't working"
upsert_label "refactor" "e6e6e6" "Code refactoring"
upsert_label "docs" "0075ca" "Documentation"
upsert_label "infra" "bfdadc" "Infrastructure / CI / tooling"

echo ""
echo "--- Impact labels ---"
upsert_label "impact:critical" "b60205" "Blocks product/demo"
upsert_label "impact:high" "d93f0b" "Market differentiator"
upsert_label "impact:medium" "fbca04" "Quality/polish"
upsert_label "impact:low" "c2e0c6" "Nice-to-have"

echo ""
echo "--- Urgency labels ---"
upsert_label "urgency:now" "b60205" "This week"
upsert_label "urgency:soon" "fbca04" "This month"
upsert_label "urgency:later" "c2e0c6" "Backlog"

echo ""
echo "--- Phase labels ---"
upsert_label "phase:S0" "e0d8f0" "Pre-NixFleet"
upsert_label "phase:S1" "d4c5f9" "Multi-org hosts"
upsert_label "phase:S2" "c9b2f4" "Role-based config"
upsert_label "phase:S3" "be9fef" "Fleet agent"
upsert_label "phase:S4" "b38cea" "Control plane"
upsert_label "phase:S5" "a879e5" "Binary cache"
upsert_label "phase:S6" "9d66e0" "Air-gap deployment"
upsert_label "phase:S7" "9253db" "NIS2 compliance"
upsert_label "phase:S8" "8740d6" "Open-core licensing"

echo ""
echo "Done. Run 'gh label list -R ${REPO} --limit 50' to verify."
