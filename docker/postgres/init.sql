-- Smooth PostgreSQL initialization
-- This runs once when the container is first created

-- Enable extensions
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- Leader memory / knowledge store
CREATE TABLE IF NOT EXISTS memories (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    content TEXT NOT NULL,
    metadata JSONB DEFAULT '{}',
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

-- Worker run records
CREATE TABLE IF NOT EXISTS worker_runs (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    bead_id TEXT NOT NULL,
    worker_id TEXT NOT NULL,
    phase TEXT NOT NULL CHECK (phase IN ('assess', 'plan', 'orchestrate', 'execute', 'finalize', 'review')),
    status TEXT NOT NULL CHECK (status IN ('pending', 'running', 'completed', 'failed', 'timeout')),
    started_at TIMESTAMPTZ DEFAULT NOW(),
    completed_at TIMESTAMPTZ,
    metadata JSONB DEFAULT '{}'
);

-- System configuration
CREATE TABLE IF NOT EXISTS config (
    key TEXT PRIMARY KEY,
    value JSONB NOT NULL,
    updated_at TIMESTAMPTZ DEFAULT NOW()
);

-- LangGraph checkpoint tables (created by @langchain/langgraph-checkpoint-postgres automatically)
-- Better Auth tables (created by Better Auth migrations automatically)

-- Indexes
CREATE INDEX IF NOT EXISTS idx_worker_runs_bead ON worker_runs(bead_id);
CREATE INDEX IF NOT EXISTS idx_worker_runs_status ON worker_runs(status);
CREATE INDEX IF NOT EXISTS idx_memories_created ON memories(created_at DESC);
