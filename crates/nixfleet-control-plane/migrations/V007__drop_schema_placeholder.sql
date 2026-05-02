-- Drop the V1 placeholder table. It carried no application data
-- (id-only schema), was never read by any handler, and only existed
-- because V1 landed before V002's real schema in the original phase
-- ordering. Confirmed unused: no INSERT, SELECT, or UPDATE references
-- anywhere in the codebase. Dropping it cleans up the audit-doc
-- footprint of `\.tables` on operator-side debugging without risk.
DROP TABLE IF EXISTS schema_placeholder;
