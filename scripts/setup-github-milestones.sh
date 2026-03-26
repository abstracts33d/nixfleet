#!/usr/bin/env bash
# Setup GitHub milestones for NixFleet development (S0-S8)
# Idempotent: skips milestones that already exist

set -euo pipefail

REPO="abstracts33d/fleet"

declare -A MILESTONES
MILESTONES["S0: Foundation"]="Pre-NixFleet config work (scopes, testing, desktop, secrets)"
MILESTONES["S1: Multi-Org Hosts"]="Generalize for multiple organizations (import paths, org config)"
MILESTONES["S2: Role-Based Config"]="Role system, enterprise scopes (VPN, LDAP, printing, certs)"
MILESTONES["S3: Fleet Agent"]="Rust agent for remote fleet management"
MILESTONES["S4: Control Plane"]="Extend Go dashboard into fleet control plane"
MILESTONES["S5: Binary Cache"]="Attic binary cache integration"
MILESTONES["S6: Air-Gap Deploy"]="Offline/air-gapped deployment support"
MILESTONES["S7: NIS2 Compliance"]="EU NIS2 directive compliance features"
MILESTONES["S8: Open-Core"]="Open-core licensing and commercialization"

# Ordered list of titles (bash associative arrays don't preserve order)
ORDERED=(
  "S0: Foundation"
  "S1: Multi-Org Hosts"
  "S2: Role-Based Config"
  "S3: Fleet Agent"
  "S4: Control Plane"
  "S5: Binary Cache"
  "S6: Air-Gap Deploy"
  "S7: NIS2 Compliance"
  "S8: Open-Core"
)

echo "Fetching existing milestones from ${REPO}..."
EXISTING=$(gh api "repos/${REPO}/milestones?state=all&per_page=100" --jq '.[].title')

created=0
skipped=0

for title in "${ORDERED[@]}"; do
  description="${MILESTONES[$title]}"

  if echo "$EXISTING" | grep -qxF "$title"; then
    echo "  SKIP  $title (already exists)"
    ((skipped++)) || true
  else
    echo "  CREATE $title"
    gh api "repos/${REPO}/milestones" \
      --method POST \
      -f title="$title" \
      -f description="$description" \
      --silent
    ((created++)) || true
  fi
done

echo ""
echo "Done: ${created} created, ${skipped} skipped."
