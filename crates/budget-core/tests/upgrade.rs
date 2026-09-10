//! Kullanıcı kapsamına yükseltme: `user_id` öncesi şemayla kurulmuş
//! veritabanı `open()` ile açıldığında kiracı tablolar bildirilen tanımla
//! yeniden kurulur, satırlar `local` kiracıya atanır ve dosya parmak izi
//! bildirilen şema ile aynı kalır; `claim_local` satırları oturum
//! kullanıcısına devreder.
//!
//! Eski şema, 0001'in ilk hâlinin tablo tanımlarını birebir taşır;
//! tohum kategorilerden yalnızca FK için gerekli olanı ekilir.

use budget_core::store::schema::{declared_fingerprint, fingerprint};
use budget_core::store::{TursoStore, CAT_ODEME};
use ulid::Ulid;

/// `user_id` öncesi 0001 şeması — yükseltme girdisi.
const LEGACY_SQL: &str = r#"
CREATE TABLE category (
    id    TEXT PRIMARY KEY,
    name  TEXT NOT NULL UNIQUE,
    kind  TEXT NOT NULL,
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
    direction           TEXT NOT NULL,
    usd_minor           INTEGER,
    bankkart_lira_minor INTEGER NOT NULL DEFAULT 0,
    card_last4          TEXT NOT NULL,
    kind                TEXT NOT NULL,
    payee_id            TEXT REFERENCES payee(id),
    UNIQUE(statement_id, row_index)
);
CREATE INDEX txn_norm ON txn(merchant_norm);
CREATE INDEX txn_date ON txn(date);
CREATE INDEX txn_payee ON txn(payee_id);
INSERT OR IGNORE INTO category (id, name, kind, color, sort) VALUES
    ('01CATODEME000000000000000', 'Kart ödemesi', 'transfer', '#cbd5e1', 210);
"#;

const PAYEE_ID: &str = "01PAYTEST0000000000000000";
const STMT_ID: &str = "01STTEST00000000000000000";
const TXN_A: &str = "01TXTEST00000000000000001";
const TXN_B: &str = "01TXTEST00000000000000002";

#[tokio::test]
async fn legacy_db_upgrades_and_claims_local_rows() {
    let root = std::env::temp_dir().join(format!("budget-upgrade-smoke-{}", Ulid::generate()));
    let db_path = root.join("budget.db");
    std::fs::create_dir_all(&root).expect("geçici kök kurulur");
    let storage = root.join("storage");
    let legacy = turso::Builder::new_local(db_path.to_string_lossy().as_ref())
        .build()
        .await
        .expect("eski veritabanı kurulur");
    let conn = legacy.connect().expect("bağlantı");
    conn.execute_batch(LEGACY_SQL).await.expect("eski şema");
    for sql in [
        format!(
            "INSERT INTO payee VALUES ('{PAYEE_ID}', 'Kart ödemesi', '{CAT_ODEME}', 'x')"
        ),
        format!("INSERT INTO payee_alias VALUES ('SUBE-HESAPTAN ODEME', '{PAYEE_ID}', 'x')"),
        format!(
            "INSERT INTO statement VALUES ('{STMT_ID}', 'deadbeef', '2026-08-31', NULL, NULL, \
             NULL, 100000, 6140494, 0, 3322073, 4680327, 0, 0, NULL, NULL, 'x')"
        ),
        format!(
            "INSERT INTO txn VALUES ('{TXN_A}', '{STMT_ID}', 0, '2026-08-10', \
             'MARKET ORNEK V020 KAYSERI', 'MARKET ORNEK V020 KAYSERI', '', 25000, 'debit', \
             NULL, 0, '5578', 'pos', NULL)"
        ),
        format!(
            "INSERT INTO txn VALUES ('{TXN_B}', '{STMT_ID}', 1, '2026-09-01', \
             'ŞUBE-HESAPTAN ÖDEME', 'SUBE-HESAPTAN ODEME', '', 1861906, 'credit', NULL, 0, \
             '5578', 'payment', '{PAYEE_ID}')"
        ),
    ] {
        conn.execute(&sql, ()).await.expect("eski satır");
    }
    drop(conn);
    drop(legacy);

    // Yükseltme + parmak izi kabulü: şema bildirilenle uyuşmazsa open()
    // reddeder; yani başarı eşitliğin kanıtıdır.
    let store = TursoStore::open(&db_path, &storage)
        .await
        .expect("eski veritabanı kullanıcı kapsamına yükseltilir");

    // Satırlar henüz `local` kiracıda: gerçek kullanıcılar boş görür.
    assert!(store.statements("u9").await.unwrap().is_empty());
    assert!(store.payees("u9").await.unwrap().is_empty());

    // Yükseltme sonrası dosya parmak izi bildirilenle aynı.
    drop(store);
    let reopened = turso::Builder::new_local(db_path.to_string_lossy().as_ref())
        .build()
        .await
        .expect("yükseltilmiş veritabanı yeniden açılır");
    let conn = reopened.connect().expect("bağlantı");
    assert_eq!(
        fingerprint(&conn).await.expect("parmak izi"),
        declared_fingerprint().await.expect("bildirilen"),
        "yükseltilmiş şema bildirilenle aynı"
    );
    drop(conn);
    drop(reopened);

    // claim_local satırları oturum kullanıcısına devreder.
    let store = TursoStore::open(&db_path, &storage)
        .await
        .expect("ikinci açılış");
    store.claim_local("u9").await.expect("kiracı devri");

    let stmts = store.statements("u9").await.expect("ekstreler");
    assert_eq!(stmts.len(), 1);
    assert_eq!(stmts[0].id, STMT_ID);

    let payees = store.payees("u9").await.expect("payee'ler");
    assert_eq!(payees.len(), 1);
    assert_eq!(payees[0].id, PAYEE_ID);

    let groups = store.unlabeled_groups("u9").await.expect("gruplar");
    assert_eq!(groups.len(), 1, "yalnızca etiketsiz pazar işlemi");
    assert_eq!(groups[0].count, 1);

    let txns = store
        .list_txns("u9", budget_core::store::TxnFilter::default())
        .await
        .expect("işlemler");
    assert_eq!(txns.len(), 2);

    // Yükseltilen satırlar artık u9'un işlem listesinde.
    std::fs::remove_dir_all(&root).ok();
}
