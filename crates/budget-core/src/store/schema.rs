//! Şema parmak izi: bağlı veritabanının gerçekte taşıdığı şema ile
//! `migrations/` altında bildirilen şema karşılaştırılır.
//!
//! SQLite `CREATE` deyimlerini yorumlarıyla, boşluklarıyla sakladığı için
//! aynı anlamı taşıyan iki şema kozmetik farklarla birbirinden ayrık
//! görünürdü; normalleştirici anlamı değiştirmeyen farkları siler.

use super::{backend, Result};
use turso::Connection;

/// Bildirilen geçişler, sırayla. Boş veritabanı bunlar uygulanarak kurulur.
pub(crate) const MIGRATIONS: &[&str] = &[include_str!("../../migrations/0001_init.sql")];

/// Bildirilen şemanın tamamı — geçişler sırayla uygulanmış — tek SQL yığını.
pub(crate) fn schema_sql() -> String {
    let mut sql = String::new();
    for migration in MIGRATIONS {
        sql.push_str(migration);
        sql.push('\n');
    }
    sql
}

/// Bildirilen şemadan tek tablonun `CREATE TABLE` deyimini soyutlar.
///
/// [`super::migrate`] kullanıcı kapsamına yükseltme sırasında eski
/// tabloyu bildirilen tanımla yeniden kurar; şema metninin tek kaynağı
/// geçiş dosyası kaldığı için deyim buradan soyutlanır, ikilenmez.
pub(crate) fn table_ddl(declared: &str, table: &str) -> Option<String> {
    split_statements(declared).into_iter().find_map(|stmt| {
        let s = stmt.trim();
        let lower = s.to_ascii_lowercase();
        let rest = lower.strip_prefix("create table ")?.trim_start();
        let name: String = rest
            .chars()
            .take_while(|c| *c == '_' || c.is_ascii_alphanumeric())
            .collect();
        name.eq_ignore_ascii_case(table).then(|| s.to_string())
    })
}

/// Bildirilen şemadan `table` üstündeki `CREATE INDEX` deyimlerini soyutlar.
pub(crate) fn index_ddls(declared: &str, table: &str) -> Vec<String> {
    split_statements(declared)
        .into_iter()
        .filter_map(|stmt| {
            let s = stmt.trim();
            let lower = s.to_ascii_lowercase();
            let rest = lower.strip_prefix("create index ")?.trim_start();
            let on = rest.find(" on ")?;
            let after = rest[on + 4..].trim_start();
            let name: String = after
                .chars()
                .take_while(|c| *c == '_' || c.is_ascii_alphanumeric())
                .collect();
            name.eq_ignore_ascii_case(table).then(|| s.to_string())
        })
        .collect()
}

/// SQL yığınını noktalı virgülden deyimlere böler; tek tırnaklı dizgi ve
/// `--` / `/* */` yorum içlerindeki noktalı virgüller bölmez.
fn split_statements(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = sql.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            cur.push(c);
            if c == '\'' {
                if chars.peek() == Some(&'\'') {
                    cur.push(chars.next().expect("peeked char exists"));
                } else {
                    in_string = false;
                }
            }
            continue;
        }
        match c {
            '\'' => {
                in_string = true;
                cur.push(c);
            }
            '-' if chars.peek() == Some(&'-') => {
                chars.next();
                cur.push_str("--");
                for nc in chars.by_ref() {
                    cur.push(nc);
                    if nc == '\n' {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                loop {
                    match chars.next() {
                        Some('*') if chars.peek() == Some(&'/') => {
                            chars.next();
                            break;
                        }
                        Some(_) => {}
                        None => break,
                    }
                }
                cur.push(' ');
            }
            ';' => {
                out.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// Bağlı veritabanının şemasını okuyup normalleştirir.
///
/// Parmak izi `sqlite_master`'dan kurulan deterministik bir metindir:
/// sqlite olmayan her nesne için `tür|ad|normalleştirilmiş_sql`, tür ve ada
/// göre sıralı. Aynı anlamı taşıyan iki şema aynı metni, tablo/sütun/kısıt
/// farklılığı taşıyan ikisi farklı metni üretir.
pub async fn fingerprint(conn: &Connection) -> Result<String> {
    let mut rows = conn
        .query(
            "SELECT type, name, sql FROM sqlite_master \
             WHERE name NOT LIKE 'sqlite_%' \
             ORDER BY type, name",
            (),
        )
        .await
        .map_err(backend)?;

    let mut out = String::new();
    while let Some(row) = rows.next().await.map_err(backend)? {
        let t: String = row.get(0).map_err(backend)?;
        let name: String = row.get(1).map_err(backend)?;
        let sql: Option<String> = row.get(2).map_err(backend)?;
        out.push_str(&t);
        out.push('|');
        out.push_str(&name);
        out.push('|');
        if let Some(sql) = sql {
            out.push_str(&normalize_schema(&sql));
        }
        out.push('\n');
    }
    Ok(out)
}

/// Bildirilen şemadan bellek içi bir veritabanı kurar ve parmak izini alır.
pub async fn declared_fingerprint() -> Result<String> {
    let db = turso::Builder::new_local(":memory:")
        .build()
        .await
        .map_err(backend)?;
    let conn = db.connect().map_err(backend)?;
    conn.execute_batch(&schema_sql())
        .await
        .map_err(backend)?;
    fingerprint(&conn).await
}

/// Fark raporu için tek şema nesnesi.
#[derive(Debug, Clone)]
pub(crate) struct SchemaObject {
    pub(crate) kind: String,
    pub(crate) name: String,
    pub(crate) sql: String,
}

/// Normalleştirilmiş şemayı (tür, ad) anahtarlı nesnelere böler.
pub(crate) fn parse_objects(fingerprint: &str) -> Vec<SchemaObject> {
    fingerprint
        .lines()
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            let mut parts = line.splitn(3, '|');
            let kind = parts.next()?.to_string();
            let name = parts.next()?.to_string();
            let sql = parts.next().unwrap_or("").to_string();
            Some(SchemaObject { kind, name, sql })
        })
        .collect()
}

/// İki parmak izinin nasıl ayrıştığını insan-okur biçimde anlatır.
pub(crate) fn diff_report(old: &str, new: &str) -> String {
    let old_objs = parse_objects(old);
    let new_objs = parse_objects(new);

    let mut missing = Vec::new();
    let mut extra = Vec::new();
    let mut changed = Vec::new();

    for old_obj in &old_objs {
        match new_objs
            .iter()
            .find(|n| n.kind == old_obj.kind && n.name == old_obj.name)
        {
            Some(new_obj) if new_obj.sql != old_obj.sql => changed.push((old_obj, new_obj)),
            Some(_) => {}
            None => missing.push(old_obj),
        }
    }
    for new_obj in &new_objs {
        if !old_objs
            .iter()
            .any(|o| o.kind == new_obj.kind && o.name == new_obj.name)
        {
            extra.push(new_obj);
        }
    }

    let mut lines = Vec::new();
    for obj in missing {
        lines.push(format!("- {} {} (kaldırılmış)", obj.kind, obj.name));
    }
    for obj in extra {
        lines.push(format!("+ {} {} (eklenmiş)", obj.kind, obj.name));
    }
    for (old_obj, new_obj) in changed {
        lines.push(format!("~ {} {} (değişmiş)", old_obj.kind, old_obj.name));
        if old_obj.kind == "table" {
            let old_cols = extract_column_names(&old_obj.sql);
            let new_cols = extract_column_names(&new_obj.sql);
            let added: Vec<&str> = new_cols
                .iter()
                .filter(|c| !old_cols.contains(c))
                .map(String::as_str)
                .collect();
            let removed: Vec<&str> = old_cols
                .iter()
                .filter(|c| !new_cols.contains(c))
                .map(String::as_str)
                .collect();
            if !added.is_empty() {
                lines.push(format!("    eklenen sütunlar: {}", added.join(", ")));
            }
            if !removed.is_empty() {
                lines.push(format!("    silinen sütunlar: {}", removed.join(", ")));
            }
        }
    }

    if lines.is_empty() {
        "şemalar yalnızca kozmetik normalleştirme düzeyinde ayrışıyor".to_string()
    } else {
        lines.join("\n")
    }
}

/// Normalleştirilmiş CREATE TABLE deyiminden sütun adlarını çıkarır.
/// Tablo düzeyindeki kısıtlar sayılmaz; yalnızca bir tanımlayıcıyla başlayan
/// üst düzey tanımlar toplanır.
fn extract_column_names(sql: &str) -> Vec<String> {
    let body = match sql.strip_prefix("CREATE TABLE ") {
        Some(rest) => rest,
        None => return Vec::new(),
    };
    let body = remove_if_not_exists(body);
    let Some(start) = body.find('(') else {
        return Vec::new();
    };
    let Some(end) = body.rfind(')') else {
        return Vec::new();
    };
    let body = &body[start + 1..end];

    let mut cols = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();
    for ch in body.chars() {
        match ch {
            '(' => {
                depth += 1;
                current.push(ch);
            }
            ')' => {
                depth -= 1;
                current.push(ch);
            }
            ',' if depth == 0 => {
                if let Some(col) = first_identifier(&current) {
                    cols.push(col);
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty()
        && let Some(col) = first_identifier(&current)
    {
        cols.push(col);
    }
    cols
}

fn first_identifier(item: &str) -> Option<String> {
    let item = item.trim();
    if item.is_empty() {
        return None;
    }
    let first = item.split_whitespace().next()?;
    let first = first.to_lowercase();
    if first == "constraint"
        || first == "primary"
        || first == "unique"
        || first == "check"
        || first == "foreign"
    {
        return None;
    }
    Some(first)
}

/// SQL metnini kozmetik farklar kaybolacak biçimde normalleştirir.
///
/// Bilinçli olarak yok sayılanlar:
/// - boşluk dizileri tek boşluğa iner;
/// - SQL yorumları (`--` satır sonuna, `/* ... */`) silinir;
/// - `IF NOT EXISTS` (bütünlük belirteci, küçük/büyük harf duyarsız) silinir.
///
/// Bilinçli olarak korunanlar:
/// - tanımlayıcıların büyük/küçük yazımı ve adları;
/// - dizgi içerikleri (içlerindeki boşluk ve yorum benzeri metinler dahil);
/// - sayısal değerler, anahtar sözcük sırası ve kısıt metni.
fn normalize_schema(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    let mut in_string = false;
    let mut prev_ws = false;

    while let Some(ch) = chars.next() {
        if in_string {
            out.push(ch);
            if ch == '\'' {
                if chars.peek() == Some(&'\'') {
                    out.push(chars.next().unwrap());
                } else {
                    in_string = false;
                }
            }
            continue;
        }

        if ch == '-' && chars.peek() == Some(&'-') {
            chars.next();
            for c in chars.by_ref() {
                if c == '\n' {
                    break;
                }
            }
            prev_ws = true;
            continue;
        }

        if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            while let Some(c) = chars.next() {
                if c == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    break;
                }
            }
            prev_ws = true;
            continue;
        }

        if ch == '\'' {
            in_string = true;
            out.push(ch);
            prev_ws = false;
            continue;
        }

        if ch.is_whitespace() {
            prev_ws = true;
            continue;
        }

        if prev_ws && !out.is_empty() {
            out.push(' ');
        }
        out.push(ch);
        prev_ws = false;
    }

    remove_if_not_exists(&out)
}

/// Normalleştirilmiş `IF NOT EXISTS` belirteç dizisini, çevresindeki
/// boşlukları koruyarak siler. `normalize_schema` boşlukları tek boşluğa
/// indirdiği için dizi tek boşlukla ayrılmış belirteçler olarak görünür.
fn remove_if_not_exists(sql: &str) -> String {
    let tokens: Vec<&str> = sql.split(' ').collect();
    let mut out = Vec::with_capacity(tokens.len());
    let mut i = 0;
    while i < tokens.len() {
        if i + 2 < tokens.len()
            && tokens[i].eq_ignore_ascii_case("IF")
            && tokens[i + 1].eq_ignore_ascii_case("NOT")
            && tokens[i + 2].eq_ignore_ascii_case("EXISTS")
        {
            i += 3;
        } else {
            out.push(tokens[i]);
            i += 1;
        }
    }
    out.join(" ")
}
