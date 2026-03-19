CREATE TABLE pull_requests (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id UUID NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    author_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    number INTEGER NOT NULL,
    source_branch VARCHAR(256) NOT NULL,
    target_branch VARCHAR(256) NOT NULL,
    title VARCHAR(512) NOT NULL,
    description TEXT,
    status VARCHAR(16) NOT NULL DEFAULT 'open',
    merged_at TIMESTAMPTZ,
    merged_by UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT uq_pull_requests_repo_id_number UNIQUE (repo_id, number)
);

CREATE INDEX idx_pull_requests_repo_id_status ON pull_requests (repo_id, status);
