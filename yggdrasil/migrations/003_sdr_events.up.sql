-- Sprint 015: SDR-based engram retrieval
-- Add SDR binary column for packed 256-bit sparse distributed representations
ALTER TABLE yggdrasil.engrams ADD COLUMN IF NOT EXISTS sdr_bits BYTEA;

-- Add trigger metadata columns for event-based recall
ALTER TABLE yggdrasil.engrams ADD COLUMN IF NOT EXISTS trigger_type TEXT NOT NULL DEFAULT 'pattern';
ALTER TABLE yggdrasil.engrams ADD COLUMN IF NOT EXISTS trigger_label TEXT NOT NULL DEFAULT '';

-- Drop legacy LSH table (past data expendable per Sprint 015 decision)
DROP TABLE IF EXISTS yggdrasil.lsh_buckets;

-- Drop legacy dense float embedding column (512x storage reduction)
-- cause_embedding was vector(4096) = 16KB per engram
-- sdr_bits is BYTEA(32) = 32 bytes per engram
ALTER TABLE yggdrasil.engrams DROP COLUMN IF EXISTS cause_embedding;
