#!/usr/bin/env bash
# scripts/gh-issue-helper.sh
# Shared functions for GitHub issue management.
# Source this file from skills or other scripts.
#
# Requires: gh CLI authenticated with project scope
# Project: NixFleet (#1)

REPO="abstracts33d/nixfleet"
PROJECT_NUM="1"

# Cache for project metadata (avoids repeated GraphQL calls — saves rate limit)
_GH_PROJECT_ID=""
_GH_STATUS_FIELD_ID=""
declare -A _GH_STATUS_OPTIONS 2>/dev/null || true # bash associative array

_gh_ensure_cache() {
  if [[ -n $_GH_PROJECT_ID ]]; then return; fi
  # 2 GraphQL calls total to populate the entire cache
  _GH_PROJECT_ID=$(gh project view "$PROJECT_NUM" --owner abstracts33d --format json --jq '.id' 2>/dev/null) || return 1
  # Get field ID + all option name:id pairs in one call
  _GH_STATUS_FIELD_ID=$(gh project field-list "$PROJECT_NUM" --owner abstracts33d --format json --jq '.fields[] | select(.name=="Status") | .id' 2>/dev/null) || return 1
  # Parse options: "name\tid" per line
  while IFS=$'\t' read -r opt_name opt_id; do
    [[ -n $opt_name ]] && _GH_STATUS_OPTIONS["$opt_name"]="$opt_id"
  done < <(gh project field-list "$PROJECT_NUM" --owner abstracts33d --format json --jq '.fields[] | select(.name=="Status") | .options[] | [.name, .id] | @tsv' 2>/dev/null)
}

# Create an issue with labels and optional milestone
# Usage: gh_create_issue "title" "body" "label1,label2" ["milestone_title"]
# Returns: issue number
gh_create_issue() {
  local title="$1"
  local body="$2"
  local labels="$3"
  local milestone="${4:-}"

  local args=(--title "$title" --body "$body" -R "$REPO")

  # Add labels one by one (gh requires separate --label flags for each)
  IFS=',' read -ra label_array <<<"$labels"
  for label in "${label_array[@]}"; do
    args+=(--label "$label")
  done

  if [[ -n $milestone ]]; then
    args+=(--milestone "$milestone")
  fi

  local issue_url
  issue_url=$(gh issue create "${args[@]}")
  local issue_num
  issue_num=$(echo "$issue_url" | grep -o '[0-9]*$')

  # Add to NixFleet project and set to Backlog
  gh project item-add "$PROJECT_NUM" --owner abstracts33d --url "$issue_url" 2>/dev/null || true
  gh_move_issue "$issue_num" "Backlog" 2>/dev/null || true

  echo "$issue_num"
}

# Move an issue to a board column
# Usage: gh_move_issue <issue_number> "In Progress"
gh_move_issue() {
  local issue_num="$1"
  local target_status="$2"

  # Ensure project metadata is cached (2 GraphQL calls on first use, 0 after)
  _gh_ensure_cache || {
    echo "Warning: Could not cache project metadata" >&2
    return 1
  }

  local issue_url="https://github.com/$REPO/issues/$issue_num"
  local item_id
  item_id=$(gh project item-list "$PROJECT_NUM" --owner abstracts33d --format json --jq ".items[] | select(.content.url == \"$issue_url\") | .id" 2>/dev/null) || true

  if [[ -z $item_id ]]; then
    echo "Warning: Issue #$issue_num not found in project" >&2
    return 1
  fi

  local option_id="${_GH_STATUS_OPTIONS[$target_status]}"
  if [[ -z $option_id ]]; then
    echo "Warning: Unknown status '$target_status'" >&2
    return 1
  fi

  # Only 2 GraphQL calls per move: item-list + item-edit (was 4)
  gh project item-edit --project-id "$_GH_PROJECT_ID" --id "$item_id" --field-id "$_GH_STATUS_FIELD_ID" --single-select-option-id "$option_id"
}

# List issues filtered by labels
# Usage: gh_list_issues "scope:core,urgency:now" [extra gh args...]
gh_list_issues() {
  local labels="$1"
  shift
  local args=(-R "$REPO")
  IFS=',' read -ra label_array <<<"$labels"
  for label in "${label_array[@]}"; do
    args+=(--label "$label")
  done
  gh issue list "${args[@]}" "$@"
}

# Post a comment on an issue
# Usage: gh_comment_issue <issue_number> "comment body"
gh_comment_issue() {
  local issue_num="$1"
  local body="$2"
  gh issue comment "$issue_num" -R "$REPO" --body "$body"
}

# Transition an issue through the board based on event
# Usage: gh_transition_issue <issue_number> <event>
# Events: created, planned, started, review, merged
gh_transition_issue() {
  local issue_num="$1"
  local event="$2"
  case "$event" in
  created) gh_move_issue "$issue_num" "Backlog" ;;
  planned) gh_move_issue "$issue_num" "Ready" ;;
  started) gh_move_issue "$issue_num" "In Progress" ;;
  review) gh_move_issue "$issue_num" "In Review" ;;
  merged) gh_close_issue "$issue_num" ;; # gh_close_issue already moves to Done
  esac
}

# Close an issue and move to Done
# Usage: gh_close_issue <issue_number>
gh_close_issue() {
  local issue_num="$1"
  gh issue close "$issue_num" -R "$REPO"
  gh_move_issue "$issue_num" "Done" 2>/dev/null || true
}
