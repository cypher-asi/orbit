-- Drop ALL foreign key constraints referencing users table
ALTER TABLE repos DROP CONSTRAINT IF EXISTS repos_owner_id_fkey;
ALTER TABLE repo_members DROP CONSTRAINT IF EXISTS repo_members_user_id_fkey;
ALTER TABLE pull_requests DROP CONSTRAINT IF EXISTS pull_requests_author_id_fkey;
ALTER TABLE pull_requests DROP CONSTRAINT IF EXISTS pull_requests_merged_by_fkey;

-- Add org_id and project_id to repos (cross-service refs to aura-network)
ALTER TABLE repos ADD COLUMN org_id UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000';
ALTER TABLE repos ADD COLUMN project_id UUID NOT NULL DEFAULT '00000000-0000-0000-0000-000000000000';
ALTER TABLE repos ALTER COLUMN org_id DROP DEFAULT;
ALTER TABLE repos ALTER COLUMN project_id DROP DEFAULT;

-- Update unique constraint: repos scoped by org, not owner
ALTER TABLE repos DROP CONSTRAINT uq_repos_owner_id_slug;
ALTER TABLE repos ADD CONSTRAINT uq_repos_org_id_slug UNIQUE (org_id, slug);

-- Add index on org_id and project_id
CREATE INDEX idx_repos_org_id ON repos (org_id);
CREATE INDEX idx_repos_project_id ON repos (project_id);

-- Drop users and auth_tokens tables
DROP TABLE IF EXISTS auth_tokens;
DROP TABLE IF EXISTS users;
