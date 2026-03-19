CREATE TABLE repo_members (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id UUID NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role VARCHAR(16) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT uq_repo_members_repo_id_user_id UNIQUE (repo_id, user_id)
);

CREATE INDEX idx_repo_members_user_id ON repo_members (user_id);
