CREATE TABLE audit_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    actor_id UUID,
    event_type VARCHAR(64) NOT NULL,
    repo_id UUID,
    target_id UUID,
    metadata JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_audit_events_repo_id_created_at ON audit_events (repo_id, created_at);
CREATE INDEX idx_audit_events_actor_id_created_at ON audit_events (actor_id, created_at);
CREATE INDEX idx_audit_events_event_type_created_at ON audit_events (event_type, created_at);
