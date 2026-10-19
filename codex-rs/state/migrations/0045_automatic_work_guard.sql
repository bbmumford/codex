CREATE TABLE automatic_work_guard (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    used_percent REAL,
    resets_at INTEGER,
    observed_at INTEGER,
    hard_limited INTEGER NOT NULL DEFAULT 0,
    window_started_at INTEGER NOT NULL DEFAULT 0,
    wake_count INTEGER NOT NULL DEFAULT 0
);

INSERT INTO automatic_work_guard (id) VALUES (1);
