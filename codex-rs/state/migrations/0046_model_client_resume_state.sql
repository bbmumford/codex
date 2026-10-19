CREATE TABLE model_client_resume_state (
    thread_id TEXT NOT NULL,
    account_id TEXT NOT NULL,
    turn_state TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (thread_id, account_id)
);

CREATE INDEX model_client_resume_state_updated_at_idx
    ON model_client_resume_state(updated_at);
