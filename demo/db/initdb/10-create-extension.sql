-- First-boot bootstrap for the demo database. CREATE EXTENSION applies the
-- embedded schema.sql (synapse schema, config + audit tables, roles, grants).
CREATE EXTENSION IF NOT EXISTS pg_synapse_pgrx;
