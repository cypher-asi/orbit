CREATE TABLE repos (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name VARCHAR(128) NOT NULL,
    slug VARCHAR(128) NOT NULL,
    description TEXT,
    visibility VARCHAR(16) NOT NULL DEFAULT 'private',
    default_branch VARCHAR(256) NOT NULL DEFAULT 'main',
    archived BOOLEAN NOT NULL DEFAULT false,
    deleted_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT uq_repos_owner_id_slug UNIQUE (owner_id, slug)
);

CREATE INDEX idx_repos_owner_id ON repos (owner_id);
CREATE INDEX idx_repos_slug_owner_id ON repos (slug, owner_id);
