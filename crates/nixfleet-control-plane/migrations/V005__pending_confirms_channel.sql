-- Persist the channel name explicitly on pending_confirms.
--
-- Pre-#62 the channel was recoverable from rollout_id via the legacy
-- `<channel>@<short-ci-commit>` form. Post-#62 (content-addressed
-- rollout manifests, RFC-0002 §4.4) rolloutIds are sha256(canonical
-- (manifest)) hex strings — no embedded channel. The fallback in
-- `Db::active_rollouts_snapshot` returned the full SHA as the channel
-- name, which then tripped `Action::ChannelUnknown` on every
-- reconcile tick. See #80.
--
-- Adding the column with DEFAULT '' lets existing rows survive the
-- migration. The same statement backfills legacy rollouts (which
-- still encode the channel before the `@`) so post-migration the
-- only rows with empty channel are the new sha256-shaped ones whose
-- producer (the dispatch path) populates the column going forward.

ALTER TABLE pending_confirms ADD COLUMN channel TEXT NOT NULL DEFAULT '';

UPDATE pending_confirms
SET channel = substr(rollout_id, 1, instr(rollout_id, '@') - 1)
WHERE instr(rollout_id, '@') > 0;
