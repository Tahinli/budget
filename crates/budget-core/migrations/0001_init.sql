-- budget şeması — 0001: kategori, payee, takma ad, ekstre ve işlem.
-- Ekstre HTML'i saklanmaz; yalnızca görselleri soyulmuş baytların
-- SHA-256'sı (`source_sha256`) mükerrer içe aktarım için tutulur.

CREATE TABLE category (
    id    TEXT PRIMARY KEY,
    name  TEXT NOT NULL UNIQUE,
    kind  TEXT NOT NULL,          -- 'spend' | 'transfer' | 'income' | 'ignore'
    color TEXT NOT NULL,
    sort  INTEGER NOT NULL
);

CREATE TABLE payee (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    category_id TEXT NOT NULL REFERENCES category(id),
    created_at  TEXT NOT NULL
);

CREATE TABLE payee_alias (
    merchant_norm TEXT PRIMARY KEY,
    payee_id      TEXT NOT NULL REFERENCES payee(id),
    created_at    TEXT NOT NULL
);

CREATE TABLE statement (
    id                      TEXT PRIMARY KEY,
    source_sha256           TEXT NOT NULL UNIQUE,
    period_end              TEXT NOT NULL,
    next_period_end         TEXT,
    due_date                TEXT,
    next_due_date           TEXT,
    previous_balance_minor  INTEGER NOT NULL,
    spend_minor             INTEGER NOT NULL,
    fees_minor              INTEGER NOT NULL,
    payments_minor          INTEGER NOT NULL,
    period_debt_minor       INTEGER NOT NULL,
    period_debt_usd_minor   INTEGER NOT NULL DEFAULT 0,
    min_pay_minor           INTEGER NOT NULL DEFAULT 0,
    card_limit_minor        INTEGER,
    available_limit_minor   INTEGER,
    imported_at             TEXT NOT NULL
);

CREATE TABLE txn (
    id                  TEXT PRIMARY KEY,
    statement_id        TEXT NOT NULL REFERENCES statement(id),
    row_index           INTEGER NOT NULL,
    date                TEXT NOT NULL,
    merchant_raw        TEXT NOT NULL,
    merchant_norm       TEXT NOT NULL,
    extra               TEXT NOT NULL DEFAULT '',
    amount_minor        INTEGER NOT NULL,
    direction           TEXT NOT NULL,     -- 'debit' | 'credit'
    usd_minor           INTEGER,
    bankkart_lira_minor INTEGER NOT NULL DEFAULT 0,
    card_last4          TEXT NOT NULL,
    kind                TEXT NOT NULL,     -- 'pos' | 'installment' | 'payment' | 'refund'
    payee_id            TEXT REFERENCES payee(id),
    UNIQUE(statement_id, row_index)
);

CREATE INDEX txn_norm ON txn(merchant_norm);
CREATE INDEX txn_date ON txn(date);
CREATE INDEX txn_payee ON txn(payee_id);

-- Tohum kategoriler: sabit ULID'ler — testler ve arayüz bu id'lere çakılıdır.
INSERT OR IGNORE INTO category (id, name, kind, color, sort) VALUES
    ('01CATMARKET000000000000000', 'Market',      'spend',    '#4ade80', 10),
    ('01CATYEME00000000000000000', 'Yeme-içme',   'spend',    '#fb923c', 20),
    ('01CATULASIM000000000000000', 'Ulaşım',      'spend',    '#38bdf8', 30),
    ('01CATFATURA00000000000000',  'Faturalar',   'spend',    '#a78bfa', 40),
    ('01CATSAGLIK00000000000000',  'Sağlık',      'spend',    '#f472b6', 50),
    ('01CATGIYIM000000000000000',  'Giyim',       'spend',    '#facc15', 60),
    ('01CATTEKNO000000000000000',  'Teknoloji',   'spend',    '#22d3ee', 70),
    ('01CATEGITIM00000000000000',  'Eğitim',      'spend',    '#818cf8', 80),
    ('01CATEGLENCE0000000000000',  'Eğlence',     'spend',    '#e879f9', 90),
    ('01CATABONE000000000000000',  'Abonelik',    'spend',    '#2dd4bf', 100),
    ('01CATKONUT000000000000000',  'Konut',       'spend',    '#94a3b8', 110),
    ('01CATDIGER000000000000000',  'Diğer',       'spend',    '#64748b', 120),
    ('01CATIADE0000000000000000',  'İade',        'income',   '#86efac', 200),
    ('01CATODEME000000000000000',  'Kart ödemesi','transfer', '#cbd5e1', 210);
