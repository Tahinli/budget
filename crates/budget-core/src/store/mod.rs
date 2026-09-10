//! turso tabanlı yerel depo.
//!
//! turso süreç-içi ve SQLite uyumlu olduğundan şema düz SQL'dir; kendisi
//! geçiş çalıştırıcısı taşımadığından [`TursoStore::open`] boş veritabanına
//! bildirilen şemayı kurar, dolu olana ise parmak izini sorar: uyuşmayan
//! veritabanı hiç açılmaz.
//!
//! Eşleştirme bütün ürünün omurgasıdır: `label`, `merchant_norm` için bir
//! takma ad yazar ve o anahtarla işaretli tüm işlemleri o payee'ye çeker —
//! sonraki içe aktarımlar takma ada otomatik bağlanır.

pub mod schema;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use thiserror::Error;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use tokio::sync::Mutex;
use turso::params_from_iter;
use turso::{params, Connection, Row, Value};
use ulid::Ulid;

use crate::parse::{
    merchant_norm, merchant_stem_display, Direction, ParseResult, ParsedTxn, StatementHead,
    TxnKind, PAYMENT_NORM,
};

/// Kart ödemesi payee'sinin adı; içe aktarımda tohum takma adı buna bağlanır.
pub const PAYEE_CARD_PAYMENT: &str = "Kart ödemesi";

/// Tohum kategori id'leri — sözleşmedeki sabit ULID'ler.
pub const CAT_MARKET: &str = "01CATMARKET000000000000000";
pub const CAT_IADE: &str = "01CATIADE0000000000000000";
pub const CAT_ODEME: &str = "01CATODEME000000000000000";

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("turso: {0}")]
    Backend(String),
    #[error("veritabanı şeması bildirilenle uyuşmuyor — başlatma reddedildi; fark:\n{0}")]
    SchemaDiff(String),
    #[error("bulunamadı: {0}")]
    NotFound(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T, E = StoreError> = std::result::Result<T, E>;

fn backend<E: std::fmt::Display>(e: E) -> StoreError {
    StoreError::Backend(e.to_string())
}

fn now_text() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .expect("utc stamps are always representable as rfc3339")
}

fn new_id() -> String {
    Ulid::generate().to_string()
}

fn text_val(s: &str) -> Value {
    Value::Text(s.to_string())
}

fn int_val(n: i64) -> Value {
    Value::Integer(n)
}

fn opt_text(v: &Option<String>) -> Value {
    v.as_ref().map(|s| text_val(s)).unwrap_or(Value::Null)
}

fn opt_int(v: &Option<i64>) -> Value {
    v.map(int_val).unwrap_or(Value::Null)
}

fn text(row: &Row, idx: usize) -> Result<String> {
    row.get::<String>(idx).map_err(backend)
}

fn opt_text_of(row: &Row, idx: usize) -> Result<Option<String>> {
    row.get::<Option<String>>(idx).map_err(backend)
}

fn int_of(row: &Row, idx: usize) -> Result<i64> {
    row.get::<i64>(idx).map_err(backend)
}

fn opt_int_of(row: &Row, idx: usize) -> Result<Option<i64>> {
    row.get::<Option<i64>>(idx).map_err(backend)
}

#[derive(Debug, Clone)]
pub struct Category {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub color: String,
    pub sort: i64,
}

#[derive(Debug, Clone)]
pub struct PayeeRow {
    pub id: String,
    pub name: String,
    pub category_id: String,
    pub created_at: String,
    pub sources: Vec<PayeeSource>,
}

#[derive(Debug, Clone)]
pub struct PayeeSource {
    pub display: String,
    pub merchant_norm: String,
    pub count: i64,
}

#[derive(Debug, Clone)]
pub struct StatementRow {
    pub id: String,
    pub source_sha256: String,
    pub period_end: String,
    pub next_period_end: Option<String>,
    pub due_date: Option<String>,
    pub next_due_date: Option<String>,
    pub previous_balance_minor: i64,
    pub spend_minor: i64,
    pub fees_minor: i64,
    pub payments_minor: i64,
    pub period_debt_minor: i64,
    pub period_debt_usd_minor: i64,
    pub min_pay_minor: i64,
    pub card_limit_minor: Option<i64>,
    pub available_limit_minor: Option<i64>,
    pub imported_at: String,
}

#[derive(Debug, Clone)]
pub struct TxnRow {
    pub id: String,
    pub statement_id: String,
    pub row_index: i64,
    pub date: String,
    pub merchant_raw: String,
    pub merchant_norm: String,
    pub extra: String,
    pub amount_minor: i64,
    pub direction: Direction,
    pub usd_minor: Option<i64>,
    pub bankkart_lira_minor: i64,
    pub card_last4: String,
    pub kind: TxnKind,
    pub payee_id: Option<String>,
    pub payee_name: Option<String>,
    pub category_id: Option<String>,
    pub category_name: Option<String>,
    pub category_color: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MerchantHit {
    pub date: String,
    pub amount_minor: i64,
    pub direction: Direction,
    pub extra: String,
}

#[derive(Debug, Clone)]
pub struct MerchantGroup {
    pub merchant_norm: String,
    pub sample_raw: String,
    pub count: i64,
    pub debit_minor: i64,
    pub credit_minor: i64,
    pub hits: Vec<MerchantHit>,
}

#[derive(Debug, Clone)]
pub struct ImportReport {
    pub statement_id: String,
    pub txn_count: usize,
    pub labeled: usize,
    pub unlabeled: usize,
    pub duplicate: bool,
}

#[derive(Debug, Clone)]
pub struct LabelReport {
    pub payee_id: String,
    pub alias_created: bool,
    pub txns_updated: u64,
}

#[derive(Debug, Clone)]
pub struct Dashboard {
    pub statement: Option<StatementRow>,
    /// Debit toplamları; yalnızca `kind = 'spend'` kategoriler. Etiketsiz
    /// harcama kendi kovasında ([`Dashboard::unlabeled_debit_minor`]) taşınır.
    pub spend_by_category: Vec<(Category, i64)>,
    pub unlabeled_debit_minor: i64,
    pub unlabeled_count: i64,
    pub top_payees: Vec<(String, i64)>,
    /// (YYYY-MM, harcama, krediler), ay sıralı.
    pub monthly: Vec<(String, i64, i64)>,
}

#[derive(Debug, Clone, Default)]
pub struct TxnFilter {
    pub statement_id: Option<String>,
    pub category_id: Option<String>,
    pub unlabeled_only: bool,
    pub q: Option<String>,
}

pub struct TursoStore {
    /// Her çağrı tek deyim bu bağlantı üstünden gider; turso bir bağlantı
    /// üzerinde deyimleri sıraya koyar, bu yüzden paylaşımlı kullanım
    /// güvenlidir. İçe aktarım tek işlem istediği için kilidi bütün gövde
    /// boyunca tutar.
    conn: Mutex<Connection>,
    /// Bağlantının üstünü canlı tutan veritabanı tanıtıcısı.
    _db: turso::Database,
}

impl TursoStore {
    /// Veritabanını açar, gereken şemayı kurar ve dosyaları 0600'a çeker.
    ///
    /// `storage` ağacı (ileride yüklenen ekstre görselleri için) açılışta
    /// hazırdır: yeni yol normal ilk açılıştır, 0700 ile kurulur.
    pub async fn open(db_path: &Path, storage: &Path) -> Result<Self> {
        ensure_storage_dir(storage)?;
        let path_str = db_path.to_string_lossy().into_owned();
        let is_memory = path_str == ":memory:";
        let db = turso::Builder::new_local(&path_str)
            .build()
            .await
            .map_err(backend)?;
        let conn = db.connect().map_err(backend)?;
        for pragma in ["PRAGMA foreign_keys = ON", "PRAGMA busy_timeout = 5000"] {
            conn.execute(pragma, ()).await.map_err(backend)?;
        }
        migrate(&conn).await?;
        rekey_merchants(&conn).await?;
        if !is_memory {
            // Turso ana dosyanın yanına WAL/SHM bırakabilir; hepsi aynı
            // kurala bağlanır: varsa 0600.
            for file in [
                db_path.to_path_buf(),
                sibling(db_path, "-wal"),
                sibling(db_path, "-shm"),
            ] {
                restrict_if_present(&file)?;
            }
        }
        Ok(Self {
            conn: Mutex::new(conn),
            _db: db,
        })
    }

    /// Bir ekstreyi depoya yazar.
    ///
    /// 1. Aynı `source_sha256` geçmişte varsa hiçbir şey yazılmaz; mevcut
    ///    sayılarla `duplicate: true` döner.
    /// 2. Yoksa ekstre ve tüm işlemler tek işlemde yazılır.
    /// 3. Her işlem için `payee_alias` eşleşirse `txn.payee_id` set edilir.
    /// 4. "şube-hesaptan ödeme" imzalı tüccarlar tohum takma adıyla otomatik
    ///    olarak "Kart ödemesi" payee'sine bağlanır.
    pub async fn import(&self, parsed: ParseResult) -> Result<ImportReport> {
        let conn = self.conn.lock().await;

        let mut rows = conn
            .query(
                "SELECT id FROM statement WHERE source_sha256 = ?",
                params![parsed.statement.source_sha256.as_str()],
            )
            .await
            .map_err(backend)?;
        if let Some(row) = rows.next().await.map_err(backend)? {
            let statement_id: String = row.get(0).map_err(backend)?;
            drop(rows);
            let (txn_count, labeled) = count_labeled(&conn, &statement_id).await?;
            return Ok(ImportReport {
                statement_id,
                txn_count,
                labeled,
                unlabeled: txn_count - labeled,
                duplicate: true,
            });
        }
        drop(rows);

        let statement_id = new_id();
        conn.execute("BEGIN IMMEDIATE", ()).await.map_err(backend)?;
        if let Err(e) = insert_statement(&conn, &statement_id, &parsed.statement).await {
            let _ = conn.execute("ROLLBACK", ()).await;
            return Err(e);
        }
        for txn in &parsed.txns {
            if let Err(e) = insert_txn(&conn, &statement_id, txn).await {
                let _ = conn.execute("ROLLBACK", ()).await;
                return Err(e);
            }
        }
        // Tohum otomatik etiketi: kart ödemesi imzalı tüccarlar.
        let mut payment_norms: Vec<&str> = parsed
            .txns
            .iter()
            .map(|t| t.merchant_norm.as_str())
            .filter(|norm| *norm == PAYMENT_NORM || norm.contains(PAYMENT_NORM))
            .collect();
        payment_norms.sort_unstable();
        payment_norms.dedup();
        for norm in payment_norms {
            if let Err(e) = link_payment_norm(&conn, norm, &statement_id).await {
                let _ = conn.execute("ROLLBACK", ()).await;
                return Err(e);
            }
        }
        conn.execute("COMMIT", ()).await.map_err(backend)?;

        let (txn_count, labeled) = count_labeled(&conn, &statement_id).await?;
        Ok(ImportReport {
            statement_id,
            txn_count,
            labeled,
            unlabeled: txn_count - labeled,
            duplicate: false,
        })
    }

    /// Etiketsiz işlemleri `merchant_norm` başına gruplar; gelen kutusunun
    /// satırlarıdır. Her grup, tarihe göre yeni→eski işlem listesini taşır.
    pub async fn unlabeled_groups(&self) -> Result<Vec<MerchantGroup>> {
        let conn = self.conn.lock().await;
        let mut rows = conn
            .query(
                "SELECT merchant_norm, merchant_raw, extra, date, amount_minor, direction \
                 FROM txn WHERE payee_id IS NULL \
                 ORDER BY date DESC, row_index DESC",
                (),
            )
            .await
            .map_err(backend)?;
        let mut by_norm: HashMap<String, MerchantGroup> = HashMap::new();
        while let Some(row) = rows.next().await.map_err(backend)? {
            let merchant_norm = text(&row, 0)?;
            let merchant_raw = text(&row, 1)?;
            let extra = text(&row, 2)?;
            let date = text(&row, 3)?;
            let amount_minor = int_of(&row, 4)?;
            let direction = Direction::from_db(&text(&row, 5)?)
                .ok_or_else(|| backend("bilinmeyen yön"))?;
            let group = by_norm.entry(merchant_norm.clone()).or_insert_with(|| {
                MerchantGroup {
                    merchant_norm,
                    sample_raw: merchant_raw.clone(),
                    count: 0,
                    debit_minor: 0,
                    credit_minor: 0,
                    hits: Vec::new(),
                }
            });
            if merchant_raw < group.sample_raw {
                group.sample_raw = merchant_raw;
            }
            match direction {
                Direction::Debit => group.debit_minor += amount_minor,
                Direction::Credit => group.credit_minor += amount_minor,
            }
            group.count += 1;
            group.hits.push(MerchantHit {
                date,
                amount_minor,
                direction,
                extra,
            });
        }
        let mut out: Vec<MerchantGroup> = by_norm.into_values().collect();
        out.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| a.merchant_norm.cmp(&b.merchant_norm))
        });
        Ok(out)
    }

    /// Bir `merchant_norm`'u kalıcı olarak etiketler: payee'yi adıyla bulur
    /// ya da kurar, kategorisini set eder, takma adı yazar (UPSERT) ve o
    /// anahtarlı **tüm** işlemleri — zaten etiketliler dahil — o payee'ye
    /// çeker. Sonraki içe aktarımlar takma ada kendiliğinden bağlanır.
    pub async fn label(
        &self,
        merchant_norm: &str,
        payee_name: &str,
        category_id: &str,
    ) -> Result<LabelReport> {
        let conn = self.conn.lock().await;
        let mut rows = conn
            .query(
                "SELECT 1 FROM category WHERE id = ?",
                params![category_id],
            )
            .await
            .map_err(backend)?;
        let category_exists = rows.next().await.map_err(backend)?.is_some();
        drop(rows);
        if !category_exists {
            return Err(StoreError::NotFound(format!("kategori {category_id}")));
        }

        let mut rows = conn
            .query(
                "SELECT id FROM payee WHERE name = ?",
                params![payee_name],
            )
            .await
            .map_err(backend)?;
        let payee_id = match rows.next().await.map_err(backend)? {
            Some(row) => row.get::<String>(0).map_err(backend)?,
            None => {
                let id = new_id();
                conn.execute(
                    "INSERT INTO payee (id, name, category_id, created_at) VALUES (?, ?, ?, ?)",
                    params![id.as_str(), payee_name, category_id, now_text()],
                )
                .await
                .map_err(backend)?;
                id
            }
        };
        drop(rows);

        conn.execute(
            "UPDATE payee SET category_id = ? WHERE id = ?",
            params![category_id, payee_id.as_str()],
        )
        .await
        .map_err(backend)?;

        let mut rows = conn
            .query(
                "SELECT payee_id FROM payee_alias WHERE merchant_norm = ?",
                params![merchant_norm],
            )
            .await
            .map_err(backend)?;
        let alias_created = rows.next().await.map_err(backend)?.is_none();
        drop(rows);
        if alias_created {
            conn.execute(
                "INSERT INTO payee_alias (merchant_norm, payee_id, created_at) VALUES (?, ?, ?)",
                params![merchant_norm, payee_id.as_str(), now_text()],
            )
            .await
            .map_err(backend)?;
        } else {
            conn.execute(
                "UPDATE payee_alias SET payee_id = ? WHERE merchant_norm = ?",
                params![payee_id.as_str(), merchant_norm],
            )
            .await
            .map_err(backend)?;
        }

        let txns_updated = conn
            .execute(
                "UPDATE txn SET payee_id = ? WHERE merchant_norm = ?",
                params![payee_id.as_str(), merchant_norm],
            )
            .await
            .map_err(backend)?;

        Ok(LabelReport {
            payee_id,
            alias_created,
            txns_updated,
        })
    }

    pub async fn set_payee_category(&self, payee_id: &str, category_id: &str) -> Result<()> {
        self.update_payee(payee_id, None, Some(category_id)).await
    }

    /// Payee adını ve/veya kategorisini yazar. Boş ad reddedilir.
    pub async fn update_payee(
        &self,
        payee_id: &str,
        name: Option<&str>,
        category_id: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        if let Some(name) = name {
            let name = name.trim();
            if name.is_empty() {
                return Err(StoreError::NotFound("payee adı boş".into()));
            }
            let mut rows = conn
                .query(
                    "SELECT id FROM payee WHERE name = ? AND id != ?",
                    params![name, payee_id],
                )
                .await
                .map_err(backend)?;
            let other = match rows.next().await.map_err(backend)? {
                Some(row) => Some(row.get::<String>(0).map_err(backend)?),
                None => None,
            };
            drop(rows);
            if let Some(keeper) = other {
                merge_payee_into(&conn, &keeper, payee_id).await?;
                if let Some(category_id) = category_id {
                    conn.execute(
                        "UPDATE payee SET category_id = ? WHERE id = ?",
                        params![category_id, keeper.as_str()],
                    )
                    .await
                    .map_err(backend)?;
                }
                return Ok(());
            }
            let changed = conn
                .execute(
                    "UPDATE payee SET name = ? WHERE id = ?",
                    params![name, payee_id],
                )
                .await
                .map_err(backend)?;
            if changed == 0 {
                return Err(StoreError::NotFound(format!("payee {payee_id}")));
            }
        }
        if let Some(category_id) = category_id {
            let mut rows = conn
                .query(
                    "SELECT 1 FROM category WHERE id = ?",
                    params![category_id],
                )
                .await
                .map_err(backend)?;
            let exists = rows.next().await.map_err(backend)?.is_some();
            drop(rows);
            if !exists {
                return Err(StoreError::NotFound(format!("kategori {category_id}")));
            }
            let changed = conn
                .execute(
                    "UPDATE payee SET category_id = ? WHERE id = ?",
                    params![category_id, payee_id],
                )
                .await
                .map_err(backend)?;
            if changed == 0 {
                return Err(StoreError::NotFound(format!("payee {payee_id}")));
            }
        }
        Ok(())
    }

    /// Payee'nin bütün etiketini kaldırır: işlemler gelen kutusuna döner.
    pub async fn clear_payee(&self, payee_id: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE txn SET payee_id = NULL WHERE payee_id = ?",
            params![payee_id],
        )
        .await
        .map_err(backend)?;
        conn.execute(
            "DELETE FROM payee_alias WHERE payee_id = ?",
            params![payee_id],
        )
        .await
        .map_err(backend)?;
        let n = conn
            .execute("DELETE FROM payee WHERE id = ?", params![payee_id])
            .await
            .map_err(backend)?;
        if n == 0 {
            return Err(StoreError::NotFound(format!("payee {payee_id}")));
        }
        Ok(())
    }

    /// Tek bir POS gövdesini payee'den koparır.
    pub async fn clear_source(&self, payee_id: &str, merchant_norm: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE txn SET payee_id = NULL WHERE payee_id = ? AND merchant_norm = ?",
            params![payee_id, merchant_norm],
        )
        .await
        .map_err(backend)?;
        conn.execute(
            "DELETE FROM payee_alias WHERE payee_id = ? AND merchant_norm = ?",
            params![payee_id, merchant_norm],
        )
        .await
        .map_err(backend)?;
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM txn WHERE payee_id = ?",
                params![payee_id],
            )
            .await
            .map_err(backend)?;
        let left = match rows.next().await.map_err(backend)? {
            Some(row) => int_of(&row, 0)?,
            None => 0,
        };
        drop(rows);
        if left == 0 {
            conn.execute(
                "DELETE FROM payee_alias WHERE payee_id = ?",
                params![payee_id],
            )
            .await
            .map_err(backend)?;
            conn.execute("DELETE FROM payee WHERE id = ?", params![payee_id])
                .await
                .map_err(backend)?;
        }
        Ok(())
    }


    /// Gösterge panosu. `statement_id` verilmezse bütün geçmiş ölçülür;
    /// verilen ekstre yoksa `NotFound`.
    pub async fn dashboard(&self, statement_id: Option<&str>) -> Result<Dashboard> {
        let conn = self.conn.lock().await;
        let statement = match statement_id {
            Some(id) => Some(statement_by_id(&conn, id).await?),
            None => latest_statement(&conn).await?,
        };
        let (scope_sql, scope_vals) = scope_clause(statement_id);

        let sql = format!(
            "SELECT c.id, c.name, c.kind, c.color, c.sort, SUM(t.amount_minor) \
             FROM txn t \
             JOIN payee p ON t.payee_id = p.id \
             JOIN category c ON p.category_id = c.id \
             WHERE t.direction = 'debit' AND c.kind = 'spend'{scope} \
             GROUP BY c.id ORDER BY SUM(t.amount_minor) DESC",
            scope = scope_sql
        );
        let mut spend_by_category = Vec::new();
        let mut rows = conn
            .query(&sql, params_from_iter(scope_vals.clone()))
            .await
            .map_err(backend)?;
        while let Some(row) = rows.next().await.map_err(backend)? {
            spend_by_category.push((category_from(&row)?, int_of(&row, 5)?));
        }
        drop(rows);

        let sql = format!(
            "SELECT COALESCE(SUM(CASE WHEN t.direction = 'debit' THEN amount_minor ELSE 0 END), 0), \
             COUNT(*) FROM txn t \
             WHERE t.payee_id IS NULL{scope}",
            scope = scope_sql
        );
        let mut unlabeled_debit_minor = 0i64;
        let mut unlabeled_count = 0i64;
        let mut rows = conn
            .query(&sql, params_from_iter(scope_vals.clone()))
            .await
            .map_err(backend)?;
        if let Some(row) = rows.next().await.map_err(backend)? {
            unlabeled_debit_minor = int_of(&row, 0)?;
            unlabeled_count = int_of(&row, 1)?;
        }
        drop(rows);

        let sql = format!(
            "SELECT p.name, SUM(t.amount_minor) FROM txn t \
             JOIN payee p ON t.payee_id = p.id \
             WHERE t.direction = 'debit'{scope} \
             GROUP BY p.id ORDER BY SUM(t.amount_minor) DESC LIMIT 8",
            scope = scope_sql
        );
        let mut top_payees = Vec::new();
        let mut rows = conn
            .query(&sql, params_from_iter(scope_vals.clone()))
            .await
            .map_err(backend)?;
        while let Some(row) = rows.next().await.map_err(backend)? {
            top_payees.push((text(&row, 0)?, int_of(&row, 1)?));
        }
        drop(rows);

        let sql = format!(
            "SELECT substr(date, 1, 7), \
                    SUM(CASE WHEN direction = 'debit' THEN amount_minor ELSE 0 END), \
                    SUM(CASE WHEN direction = 'credit' THEN amount_minor ELSE 0 END) \
             FROM txn t WHERE 1 = 1{scope} GROUP BY 1 ORDER BY 1",
            scope = scope_sql
        );
        let mut monthly = Vec::new();
        let mut rows = conn
            .query(&sql, params_from_iter(scope_vals))
            .await
            .map_err(backend)?;
        while let Some(row) = rows.next().await.map_err(backend)? {
            monthly.push((text(&row, 0)?, int_of(&row, 1)?, int_of(&row, 2)?));
        }
        drop(rows);

        Ok(Dashboard {
            statement,
            spend_by_category,
            unlabeled_debit_minor,
            unlabeled_count,
            top_payees,
            monthly,
        })
    }

    /// İşlem tablosu; filtreler AND'lenir, en yeni tarih başta.
    pub async fn list_txns(&self, f: TxnFilter) -> Result<Vec<TxnRow>> {
        let conn = self.conn.lock().await;

        let mut where_sql = String::new();
        let mut vals: Vec<Value> = Vec::new();
        if let Some(id) = &f.statement_id {
            where_sql.push_str(" AND t.statement_id = ?");
            vals.push(text_val(id));
        }
        if let Some(cat) = &f.category_id {
            where_sql.push_str(
                " AND t.payee_id IN (SELECT id FROM payee WHERE category_id = ?)",
            );
            vals.push(text_val(cat));
        }
        if f.unlabeled_only {
            where_sql.push_str(" AND t.payee_id IS NULL");
        }
        if let Some(q) = &f.q {
            where_sql.push_str(" AND (t.merchant_raw LIKE ? OR t.merchant_norm LIKE ?)");
            let needle = format!("%{q}%");
            vals.push(text_val(&needle));
            vals.push(text_val(&needle));
        }

        let sql = format!(
            "SELECT t.id, t.statement_id, t.row_index, t.date, t.merchant_raw, \
                    t.merchant_norm, t.extra, t.amount_minor, t.direction, t.usd_minor, \
                    t.bankkart_lira_minor, t.card_last4, t.kind, t.payee_id, \
                    p.name, c.id, c.name, c.color \
             FROM txn t \
             LEFT JOIN payee p ON t.payee_id = p.id \
             LEFT JOIN category c ON p.category_id = c.id \
             WHERE 1 = 1{where_sql} ORDER BY t.date DESC, t.row_index DESC",
            where_sql = where_sql
        );
        let mut rows = conn.query(&sql, params_from_iter(vals)).await.map_err(backend)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(backend)? {
            out.push(TxnRow {
                id: text(&row, 0)?,
                statement_id: text(&row, 1)?,
                row_index: int_of(&row, 2)?,
                date: text(&row, 3)?,
                merchant_raw: text(&row, 4)?,
                merchant_norm: text(&row, 5)?,
                extra: text(&row, 6)?,
                amount_minor: int_of(&row, 7)?,
                direction: Direction::from_db(&text(&row, 8)?)
                    .ok_or_else(|| backend("bilinmeyen yön"))?,
                usd_minor: opt_int_of(&row, 9)?,
                bankkart_lira_minor: int_of(&row, 10)?,
                card_last4: text(&row, 11)?,
                kind: TxnKind::from_db(&text(&row, 12)?)
                    .ok_or_else(|| backend("bilinmeyen tür"))?,
                payee_id: opt_text_of(&row, 13)?,
                payee_name: opt_text_of(&row, 14)?,
                category_id: opt_text_of(&row, 15)?,
                category_name: opt_text_of(&row, 16)?,
                category_color: opt_text_of(&row, 17)?,
            });
        }
        drop(rows);
        Ok(out)
    }

    pub async fn categories(&self) -> Result<Vec<Category>> {
        let conn = self.conn.lock().await;
        let mut rows = conn
            .query(
                "SELECT id, name, kind, color, sort FROM category ORDER BY sort",
                (),
            )
            .await
            .map_err(backend)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(backend)? {
            out.push(category_from(&row)?);
        }
        drop(rows);
        Ok(out)
    }

    pub async fn payees(&self) -> Result<Vec<PayeeRow>> {
        let conn = self.conn.lock().await;
        let mut rows = conn
            .query(
                "SELECT id, name, category_id, created_at FROM payee ORDER BY name",
                (),
            )
            .await
            .map_err(backend)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(backend)? {
            out.push(PayeeRow {
                id: text(&row, 0)?,
                name: text(&row, 1)?,
                category_id: text(&row, 2)?,
                created_at: text(&row, 3)?,
                sources: Vec::new(),
            });
        }
        drop(rows);

        let mut src_rows = conn
            .query(
                "SELECT payee_id, merchant_norm, COUNT(*), MIN(merchant_raw) \
                 FROM txn WHERE payee_id IS NOT NULL \
                 GROUP BY payee_id, merchant_norm \
                 ORDER BY COUNT(*) DESC, merchant_norm",
                (),
            )
            .await
            .map_err(backend)?;
        let mut by_payee: HashMap<String, Vec<PayeeSource>> = HashMap::new();
        while let Some(row) = src_rows.next().await.map_err(backend)? {
            let id = text(&row, 0)?;
            let norm = text(&row, 1)?;
            let n = int_of(&row, 2)?;
            let raw = text(&row, 3)?;
            by_payee.entry(id).or_default().push(PayeeSource {
                display: merchant_stem_display(&raw),
                merchant_norm: norm,
                count: n,
            });
        }
        drop(src_rows);
        for p in &mut out {
            if let Some(src) = by_payee.remove(&p.id) {
                p.sources = src;
            }
        }
        Ok(out)
    }

    pub async fn statements(&self) -> Result<Vec<StatementRow>> {
        let conn = self.conn.lock().await;
        let mut rows = conn
            .query(
                "SELECT id, source_sha256, period_end, next_period_end, due_date, \
                        next_due_date, previous_balance_minor, spend_minor, fees_minor, \
                        payments_minor, period_debt_minor, period_debt_usd_minor, \
                        min_pay_minor, card_limit_minor, available_limit_minor, imported_at \
                 FROM statement ORDER BY period_end DESC, imported_at DESC",
                (),
            )
            .await
            .map_err(backend)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.map_err(backend)? {
            out.push(statement_from(&row)?);
        }
        drop(rows);
        Ok(out)
    }

}

/// Boş veritabanına bildirilen şemayı kurar; dolu olana bildirilen şema ile
/// karşılaştırır ve uyuşmazsa reddeder.
async fn migrate(conn: &Connection) -> Result<()> {
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM sqlite_master \
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            (),
        )
        .await
        .map_err(backend)?;
    let empty = match rows.next().await.map_err(backend)? {
        Some(row) => row.get::<i64>(0).map_err(backend)? == 0,
        None => true,
    };
    drop(rows);

    if empty {
        conn.execute("BEGIN IMMEDIATE", ()).await.map_err(backend)?;
        if let Err(e) = conn.execute_batch(&schema::schema_sql()).await {
            let _ = conn.execute("ROLLBACK", ()).await;
            return Err(backend(e));
        }
        return conn
            .execute("COMMIT", ())
            .await
            .map_err(backend)
            .map(|_| ());
    }

    let have = schema::fingerprint(conn).await?;
    let want = schema::declared_fingerprint().await?;
    if have != want {
        return Err(StoreError::SchemaDiff(schema::diff_report(
            &have, &want,
        )));
    }
    Ok(())
}

async fn merge_payee_into(conn: &Connection, keeper: &str, extra: &str) -> Result<()> {
    conn.execute(
        "UPDATE txn SET payee_id = ? WHERE payee_id = ?",
        params![keeper, extra],
    )
    .await
    .map_err(backend)?;
    conn.execute(
        "DELETE FROM payee_alias WHERE payee_id = ?",
        params![extra],
    )
    .await
    .map_err(backend)?;
    conn.execute("DELETE FROM payee WHERE id = ?", params![extra])
        .await
        .map_err(backend)?;
    let now = now_text();
    conn.execute(
        "INSERT OR IGNORE INTO payee_alias (merchant_norm, payee_id, created_at) \
         SELECT DISTINCT merchant_norm, ?, ? FROM txn WHERE payee_id = ?",
        params![keeper, now.as_str(), keeper],
    )
    .await
    .map_err(backend)?;
    Ok(())
}

/// Eski `merchant_norm` değerlerini (USD tutarlı POS) yeniden katlar,
/// aynı gövdeye düşen payee'leri birleştirir.
async fn rekey_merchants(conn: &Connection) -> Result<()> {
    let mut rows = conn
        .query("SELECT DISTINCT merchant_raw FROM txn", ())
        .await
        .map_err(backend)?;
    let mut raws = Vec::new();
    while let Some(row) = rows.next().await.map_err(backend)? {
        raws.push(text(&row, 0)?);
    }
    drop(rows);
    for raw in &raws {
        let norm = merchant_norm(raw);
        conn.execute(
            "UPDATE txn SET merchant_norm = ? WHERE merchant_raw = ?",
            params![norm.as_str(), raw.as_str()],
        )
        .await
        .map_err(backend)?;
    }

    let mut rows = conn
        .query(
            "SELECT merchant_norm, MIN(payee_id) FROM txn \
             WHERE payee_id IS NOT NULL GROUP BY merchant_norm \
             HAVING COUNT(DISTINCT payee_id) > 1",
            (),
        )
        .await
        .map_err(backend)?;
    let mut unify = Vec::new();
    while let Some(row) = rows.next().await.map_err(backend)? {
        unify.push((text(&row, 0)?, text(&row, 1)?));
    }
    drop(rows);
    for (norm, keeper) in unify {
        conn.execute(
            "UPDATE txn SET payee_id = ? WHERE merchant_norm = ? AND payee_id IS NOT NULL",
            params![keeper.as_str(), norm.as_str()],
        )
        .await
        .map_err(backend)?;
    }

    conn.execute("DELETE FROM payee_alias", ())
        .await
        .map_err(backend)?;
    let now = now_text();
    conn.execute(
        "INSERT INTO payee_alias (merchant_norm, payee_id, created_at) \
         SELECT merchant_norm, MIN(payee_id), ? FROM txn \
         WHERE payee_id IS NOT NULL GROUP BY merchant_norm",
        params![now.as_str()],
    )
    .await
    .map_err(backend)?;

    let mut rows = conn
        .query(
            "SELECT name, MIN(id) FROM payee GROUP BY name HAVING COUNT(*) > 1",
            (),
        )
        .await
        .map_err(backend)?;
    let mut dups = Vec::new();
    while let Some(row) = rows.next().await.map_err(backend)? {
        dups.push((text(&row, 0)?, text(&row, 1)?));
    }
    drop(rows);
    for (name, keeper) in dups {
        let mut extras = conn
            .query(
                "SELECT id FROM payee WHERE name = ? AND id != ?",
                params![name.as_str(), keeper.as_str()],
            )
            .await
            .map_err(backend)?;
        let mut extra_ids = Vec::new();
        while let Some(row) = extras.next().await.map_err(backend)? {
            extra_ids.push(text(&row, 0)?);
        }
        drop(extras);
        for extra in extra_ids {
            merge_payee_into(conn, &keeper, &extra).await?;
        }
    }

    conn.execute(
        "DELETE FROM payee WHERE id NOT IN (SELECT DISTINCT payee_id FROM txn WHERE payee_id IS NOT NULL)",
        (),
    )
    .await
    .map_err(backend)?;
    Ok(())
}


async fn insert_statement(
    conn: &Connection,
    statement_id: &str,
    head: &StatementHead,
) -> Result<()> {
    let vals = vec![
        text_val(statement_id),
        text_val(&head.source_sha256),
        text_val(&head.period_end),
        opt_text(&head.next_period_end),
        opt_text(&head.due_date),
        opt_text(&head.next_due_date),
        int_val(head.previous_balance_minor),
        int_val(head.spend_minor),
        int_val(head.fees_minor),
        int_val(head.payments_minor),
        int_val(head.period_debt_minor),
        int_val(head.period_debt_usd_minor),
        int_val(head.min_pay_minor),
        opt_int(&head.card_limit_minor),
        opt_int(&head.available_limit_minor),
        text_val(&now_text()),
    ];
    conn.execute(
        "INSERT INTO statement (id, source_sha256, period_end, next_period_end, due_date, \
                                next_due_date, previous_balance_minor, spend_minor, \
                                fees_minor, payments_minor, period_debt_minor, \
                                period_debt_usd_minor, min_pay_minor, card_limit_minor, \
                                available_limit_minor, imported_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params_from_iter(vals),
    )
    .await
    .map_err(backend)?;
    Ok(())
}

async fn insert_txn(conn: &Connection, statement_id: &str, txn: &ParsedTxn) -> Result<()> {
    // Takma ad eşleşirse işlem doğduğu an etiketli doğar.
    let mut rows = conn
        .query(
            "SELECT payee_id FROM payee_alias WHERE merchant_norm = ?",
            params![txn.merchant_norm.as_str()],
        )
        .await
        .map_err(backend)?;
    let alias_payee: Option<String> = match rows.next().await.map_err(backend)? {
        Some(row) => row.get(0).map_err(backend)?,
        None => None,
    };
    drop(rows);

    let vals = vec![
        text_val(&new_id()),
        text_val(statement_id),
        int_val(txn.row_index),
        text_val(&txn.date),
        text_val(&txn.merchant_raw),
        text_val(&txn.merchant_norm),
        text_val(&txn.extra),
        int_val(txn.amount_minor),
        text_val(txn.direction.as_str()),
        opt_int(&txn.usd_minor),
        int_val(txn.bankkart_lira_minor),
        text_val(&txn.card_last4),
        text_val(txn.kind.as_str()),
        opt_text(&alias_payee),
    ];
    conn.execute(
        "INSERT INTO txn (id, statement_id, row_index, date, merchant_raw, merchant_norm, \
                          extra, amount_minor, direction, usd_minor, bankkart_lira_minor, \
                          card_last4, kind, payee_id) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params_from_iter(vals),
    )
    .await
    .map_err(backend)?;
    Ok(())
}

/// "Kart ödemesi" payee'sini (gerekirse kurar) ve tüccar takma adını yazar,
/// sonra bu normdaki etiketsiz işlemleri ona bağlar.
async fn link_payment_norm(conn: &Connection, norm: &str, statement_id: &str) -> Result<()> {
    let mut rows = conn
        .query(
            "SELECT id FROM payee WHERE name = ?",
            params![PAYEE_CARD_PAYMENT],
        )
        .await
        .map_err(backend)?;
    let payee_id = match rows.next().await.map_err(backend)? {
        Some(row) => row.get::<String>(0).map_err(backend)?,
        None => {
            let id = new_id();
            conn.execute(
                "INSERT INTO payee (id, name, category_id, created_at) VALUES (?, ?, ?, ?)",
                params![id.as_str(), PAYEE_CARD_PAYMENT, CAT_ODEME, now_text()],
            )
            .await
            .map_err(backend)?;
            id
        }
    };
    drop(rows);

    conn.execute(
        "INSERT INTO payee_alias (merchant_norm, payee_id, created_at) VALUES (?, ?, ?) \
         ON CONFLICT(merchant_norm) DO UPDATE SET payee_id = excluded.payee_id",
        params![norm, payee_id.as_str(), now_text()],
    )
    .await
    .map_err(backend)?;

    conn.execute(
        "UPDATE txn SET payee_id = ? WHERE statement_id = ? AND merchant_norm = ? \
         AND payee_id IS NULL",
        params![payee_id.as_str(), statement_id, norm],
    )
    .await
    .map_err(backend)?;
    Ok(())
}

async fn count_labeled(conn: &Connection, statement_id: &str) -> Result<(usize, usize)> {
    let mut rows = conn
        .query(
            "SELECT COUNT(*), \
                    (SELECT COUNT(*) FROM txn WHERE statement_id = ?2 AND payee_id IS NOT NULL) \
             FROM txn WHERE statement_id = ?1",
            params![statement_id, statement_id],
        )
        .await
        .map_err(backend)?;
    let out = match rows.next().await.map_err(backend)? {
        Some(row) => (int_of(&row, 0)? as usize, int_of(&row, 1)? as usize),
        None => (0, 0),
    };
    drop(rows);
    Ok(out)
}

async fn statement_by_id(conn: &Connection, id: &str) -> Result<StatementRow> {
    let mut rows = conn
        .query(
            "SELECT id, source_sha256, period_end, next_period_end, due_date, \
                    next_due_date, previous_balance_minor, spend_minor, fees_minor, \
                    payments_minor, period_debt_minor, period_debt_usd_minor, \
                    min_pay_minor, card_limit_minor, available_limit_minor, imported_at \
             FROM statement WHERE id = ?",
            params![id],
        )
        .await
        .map_err(backend)?;
    let row = rows
        .next()
        .await
        .map_err(backend)?
        .ok_or_else(|| StoreError::NotFound(format!("ekstre {id}")))?;
    statement_from(&row)
}

async fn latest_statement(conn: &Connection) -> Result<Option<StatementRow>> {
    let mut rows = conn
        .query(
            "SELECT id, source_sha256, period_end, next_period_end, due_date, \
                    next_due_date, previous_balance_minor, spend_minor, fees_minor, \
                    payments_minor, period_debt_minor, period_debt_usd_minor, \
                    min_pay_minor, card_limit_minor, available_limit_minor, imported_at \
             FROM statement ORDER BY period_end DESC, imported_at DESC LIMIT 1",
            (),
        )
        .await
        .map_err(backend)?;
    match rows.next().await.map_err(backend)? {
        Some(row) => Ok(Some(statement_from(&row)?)),
        None => Ok(None),
    }
}

fn statement_from(row: &Row) -> Result<StatementRow> {
    Ok(StatementRow {
        id: text(row, 0)?,
        source_sha256: text(row, 1)?,
        period_end: text(row, 2)?,
        next_period_end: opt_text_of(row, 3)?,
        due_date: opt_text_of(row, 4)?,
        next_due_date: opt_text_of(row, 5)?,
        previous_balance_minor: int_of(row, 6)?,
        spend_minor: int_of(row, 7)?,
        fees_minor: int_of(row, 8)?,
        payments_minor: int_of(row, 9)?,
        period_debt_minor: int_of(row, 10)?,
        period_debt_usd_minor: int_of(row, 11)?,
        min_pay_minor: int_of(row, 12)?,
        card_limit_minor: opt_int_of(row, 13)?,
        available_limit_minor: opt_int_of(row, 14)?,
        imported_at: text(row, 15)?,
    })
}

fn category_from(row: &Row) -> Result<Category> {
    Ok(Category {
        id: text(row, 0)?,
        name: text(row, 1)?,
        kind: text(row, 2)?,
        color: text(row, 3)?,
        sort: int_of(row, 4)?,
    })
}

/// `statement_id` odağı için SQL parçası ve bağlı değerleri. `None` odağın
/// "hepsi" anlamına geldiği yerlerde `WHERE 1 = 1` ile bitişir.
fn scope_clause(statement_id: Option<&str>) -> (String, Vec<Value>) {
    match statement_id {
        Some(id) => (" AND t.statement_id = ?".to_string(), vec![text_val(id)]),
        None => (String::new(), Vec::new()),
    }
}

/// `path`in yanına sonek ekler: veritabanının WAL/SHM kardeşleri.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

/// Var olan dosyayı 0600'a çeker; yoksa dokunmaz — chmod ile dosya yaratmak
/// hiç satırı olmayan bir dosya bırakmaktan beterdir.
fn restrict_if_present(path: &Path) -> Result<()> {
    if path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
}

/// Depolama ağacını (kök) eksiksiz ve süreç sahibine özel kurar.
fn ensure_storage_dir(storage: &Path) -> Result<()> {
    if !storage.exists() {
        std::fs::create_dir_all(storage)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(storage, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(())
}
