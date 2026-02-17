PRAGMA journal_mode = WAL;

CREATE TABLE IF NOT EXISTS venues (
    venue_id TEXT PRIMARY KEY,
    enabled INTEGER NOT NULL,
    market_types TEXT NOT NULL,
    paper_only INTEGER NOT NULL,
    live_enabled INTEGER NOT NULL,
    message TEXT,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS execution_modes (
    mode_key TEXT PRIMARY KEY,
    venue_id TEXT NOT NULL,
    market_type TEXT NOT NULL,
    mode TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS orders (
    venue_order_id TEXT PRIMARY KEY,
    payload_json TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS fills (
    fill_id TEXT PRIMARY KEY,
    payload_json TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS strategies (
    strategy_id TEXT PRIMARY KEY,
    payload_json TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS risk_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    payload_json TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS events_journal (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_ms INTEGER NOT NULL,
    event_json TEXT NOT NULL
);
