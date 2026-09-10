//! Ziraat Katılım kredi kartı ekstresi ayrıştırıcı.
//!
//! Yalnızca bankanın baskı HTML'ini (`print` çıktısı) anlar: iç içe
//! tablolar, `data:image`/`<img>` süsleri ve 6 hücreli işlem satırları.
//! Dip özeti iki düzende gelir: kutu satırları (etiket + tutar aynı
//! satırda, tek satırda birden çok kutu) ya da banka baskısındaki 9+9
//! hücreli etiket satırı + değer/işleç satırı çifti.
//! Ekstre HTML'i asla saklanmaz; önce img yükleri soyulur, kalan
//! baytların SHA-256'sı `source_sha256` olarak taşınır — mükerrer içe
//! aktarım bu parmak iziyle yakalanır.
//!
//! İki sözleşme burada bıçakla kesilir:
//! - Yön, TL tutarındaki sondaki `+` işaretinden okunur, satır renginden
//!   asla (`kkred` "yabancı" demektir, borç/alçak değil).
//! - Dip özeti (Devreden Bakiye + Harcamalarınız + Ceza − Ödemeleriniz =
//!   Dönem Borcu) hesaplanan toplamlarla **0 kuruş** tutmak zorundadır;
//!   tutmazsa içe aktarım hiç başlamaz.

use scraper::{ElementRef, Html, Selector};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Kart ödemesi açıklamasının normalize edilmiş imzası. İçe aktarımda bu
/// imzayı taşıyan satır otomatik olarak "Kart ödemesi" payee'sine bağlanır.
pub const PAYMENT_NORM: &str = "SUBE-HESAPTAN ODEME";

/// Normalleştirilmiş tüccar adının sonunda soyulacak şehir belirteçleri.
const CITY_TOKENS: &[&str] = &[
    "KAYSERI", "KAYSER", "ISTANBUL", "ANKARA", "IZMIR", "ERZINCAN", "BURSA", "ANTALYA", "SAN",
    "FRANCISCO", "SINGAPORE",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Debit,
    Credit,
}

impl Direction {
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Debit => "debit",
            Direction::Credit => "credit",
        }
    }

    /// `txn.direction` sütunundan geri okuma.
    pub fn from_db(s: &str) -> Option<Direction> {
        match s {
            "debit" => Some(Direction::Debit),
            "credit" => Some(Direction::Credit),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxnKind {
    Pos,
    Installment,
    Payment,
    Refund,
}

impl TxnKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TxnKind::Pos => "pos",
            TxnKind::Installment => "installment",
            TxnKind::Payment => "payment",
            TxnKind::Refund => "refund",
        }
    }

    /// `txn.kind` sütunundan geri okuma.
    pub fn from_db(s: &str) -> Option<TxnKind> {
        match s {
            "pos" => Some(TxnKind::Pos),
            "installment" => Some(TxnKind::Installment),
            "payment" => Some(TxnKind::Payment),
            "refund" => Some(TxnKind::Refund),
            _ => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("dosya bir Ziraat Katılım ekstresi değil")]
    NotZiraat,
    #[error("ekstre yapısı bozuk: {0}")]
    Structure(String),
    #[error("tutar okunamadı: {0:?}")]
    Amount(String),
    #[error(
        "dip özeti tutmuyor: önceki {previous_minor} + harcama {sum_debit_minor} + ceza \
         {fees_minor} − ödeme {sum_credit_minor} = {computed_minor}, ekstre ise dönem borcunu \
         {period_debt_minor} yazıyor"
    )]
    Reconcile {
        previous_minor: i64,
        sum_debit_minor: i64,
        fees_minor: i64,
        sum_credit_minor: i64,
        computed_minor: i64,
        period_debt_minor: i64,
    },
}

#[derive(Debug, Clone)]
pub struct ParseResult {
    pub statement: StatementHead,
    pub txns: Vec<ParsedTxn>,
}

#[derive(Debug, Clone)]
pub struct StatementHead {
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
    pub source_sha256: String,
}

#[derive(Debug, Clone)]
pub struct ParsedTxn {
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
}

pub fn parse_ziraat_html(bytes: &[u8]) -> Result<ParseResult, ParseError> {
    let stripped = strip_images(bytes);
    let source_sha256 = hex_sha256(&stripped);
    let text = String::from_utf8_lossy(&stripped).into_owned();
    let doc = Html::parse_document(&text);

    let title_sel =
        Selector::parse("title").map_err(|_| ParseError::Structure("seçici: title".into()))?;
    let title: String = doc
        .select(&title_sel)
        .map(|t| t.text().collect::<String>())
        .collect::<Vec<_>>()
        .join(" ");
    if !fold_text(&title).contains("ZIRAAT KATILIM") {
        return Err(ParseError::NotZiraat);
    }

    let tr_sel = Selector::parse("tr").map_err(|_| ParseError::Structure("seçici: tr".into()))?;
    let td_sel = Selector::parse("td").map_err(|_| ParseError::Structure("seçici: td".into()))?;

    let mut head = StatementHead {
        period_end: String::new(),
        next_period_end: None,
        due_date: None,
        next_due_date: None,
        previous_balance_minor: 0,
        spend_minor: 0,
        fees_minor: 0,
        payments_minor: 0,
        period_debt_minor: 0,
        period_debt_usd_minor: 0,
        min_pay_minor: 0,
        card_limit_minor: None,
        available_limit_minor: None,
        source_sha256,
    };
    let mut boxes = FooterBoxes::default();
    let mut txns: Vec<ParsedTxn> = Vec::new();
    let mut card_last4 = String::new();
    let mut previous_devir: Option<i64> = None;
    let mut pending_footer: Option<Vec<Option<FooterKey>>> = None;

    for tr in doc.select(&tr_sel) {
        let tr_class = tr.value().attr("class").unwrap_or("").to_string();
        let mut cells: Vec<String> = Vec::new();
        let mut td_classes: Vec<String> = Vec::new();
        for td in tr.select(&td_sel) {
            // İç içe tablo: bir td'nin altında tr varsa o hücre düzen
            // iskeletidir, veri hücresi değil — metni çöpe atılır, iç
            // tablonun kendi satırları belgede ayrıca gezilir.
            if td.select(&tr_sel).next().is_some() {
                continue;
            }
            td_classes.push(td.value().attr("class").unwrap_or("").to_string());
            cells.push(cell_text(td));
        }
        // Satır rengi çoğunlukla tr'de, bazı baskılarda hücrelerde taşınır.
        let row_tokens = format!("{} {}", tr_class, td_classes.join(" "))
            .to_ascii_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let folded = fold_text(&cells.join(" "));

        if folded.contains("KART NO") {
            let digits: String = cells.iter().flat_map(|c| c.chars()).collect::<String>()
                .chars()
                .filter(char::is_ascii_digit)
                .collect();
            let last4: String = digits
                .chars()
                .rev()
                .take(4)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            if last4.len() != 4 {
                return Err(ParseError::Structure(format!(
                    "KART NO satırında son 4 hane okunamadı: {digits:?}"
                )));
            }
            card_last4 = last4;
            continue;
        }

        if folded.contains("ONCEKI AYDAN DEVIR") {
            for cell in &cells {
                if let Ok((minor, _)) = parse_tr_amount(cell) {
                    previous_devir = Some(minor);
                    break;
                }
            }
            continue;
        }

        if let Some(iso) = cells.first().and_then(|c| parse_tr_date(c)) {
            let last4 = card_last4.clone();
            let txn = txn_from_row(cells, &row_tokens, iso, &last4, txns.len())?;
            txns.push(txn);
            continue;
        }

        // Dip özeti iki düzende gelir:
        // (a) kutu satırı — etiket ve tutar aynı satırda; tek satır birden
        //     çok kutu taşıyabilir, tamamı çıkarılır;
        // (b) baskı düzeni — bir yalnız-etiket satırı, ardından değer ve
        //     işleç hücreleri taşıyan satır; kutular aynı hücre dizininde
        //     eşleşir (9+9 hücre).
        let has_colon = cells.iter().any(|c| c == ":");
        if !has_colon {
            let direct = footer_boxes_in_row(&cells);
            if !direct.is_empty() {
                for (key, value) in direct {
                    apply_footer_box(&mut boxes, key, value);
                }
                pending_footer = None;
                continue;
            }
            if let Some(pending) = pending_footer.take() {
                let mut matched = false;
                for (i, key) in pending.into_iter().enumerate() {
                    let Some(key) = key else { continue };
                    let Some(cell) = cells.get(i) else { continue };
                    if let Ok((minor, _)) = parse_tr_amount(cell) {
                        apply_footer_box(&mut boxes, key, minor);
                        matched = true;
                    }
                }
                if matched {
                    continue;
                }
            }
            let row_keys: Vec<Option<FooterKey>> =
                cells.iter().map(|c| footer_key(c)).collect();
            if row_keys.iter().any(|k| k.is_some()) {
                pending_footer = Some(row_keys);
                continue;
            }
        }

        // Kalan satırlar etiket/değer çiftleri taşır: 3 hücre = tek çift
        // (etiket, `:`, değer), 7 hücre = boş `:` hücresiyle iki çift.
        // `:` ve boş hücreler atılıp artıklar sırayla eşleştirilir; her iki
        // düzen de (ve karışımı) bu şekilde çözülür.
        let mut pending: Option<String> = None;
        for cell in &cells {
            let trimmed = cell.trim();
            if trimmed.is_empty() || trimmed == ":" {
                continue;
            }
            match pending.take() {
                None => pending = Some(trimmed.to_string()),
                Some(label) => apply_head_pair(&label, trimmed, &mut head),
            }
        }
    }

    if head.period_end.is_empty() {
        return Err(ParseError::Structure(
            "Hesap Kesim Tarihi bulunamadı".into(),
        ));
    }

    let previous = previous_devir.or(boxes.previous).unwrap_or(0);
    let spend_box = boxes.spend.ok_or_else(|| {
        ParseError::Structure("dip kutusu yok: Harcamalarınız".into())
    })?;
    let fees = boxes.fees.unwrap_or(0);
    let payments_box = boxes.payments.ok_or_else(|| {
        ParseError::Structure("dip kutusu yok: Ödemeleriniz".into())
    })?;
    let debt_box = boxes.debt.ok_or_else(|| {
        ParseError::Structure("dip kutusu yok: Dönem Borcu".into())
    })?;

    let sum_debit: i64 = txns
        .iter()
        .filter(|t| t.direction == Direction::Debit)
        .map(|t| t.amount_minor)
        .sum();
    let sum_credit: i64 = txns
        .iter()
        .filter(|t| t.direction == Direction::Credit)
        .map(|t| t.amount_minor)
        .sum();

    if sum_debit != spend_box || sum_credit != payments_box {
        return Err(reconcile(
            previous,
            sum_debit,
            fees,
            sum_credit,
            debt_box,
        ));
    }
    let computed = previous + sum_debit + fees - sum_credit;
    if computed != debt_box {
        return Err(reconcile(
            previous,
            sum_debit,
            fees,
            sum_credit,
            debt_box,
        ));
    }

    head.previous_balance_minor = previous;
    head.spend_minor = spend_box;
    head.fees_minor = fees;
    head.payments_minor = payments_box;
    head.period_debt_minor = debt_box;

    Ok(ParseResult { statement: head, txns })
}

fn reconcile(
    previous_minor: i64,
    sum_debit_minor: i64,
    fees_minor: i64,
    sum_credit_minor: i64,
    period_debt_minor: i64,
) -> ParseError {
    ParseError::Reconcile {
        previous_minor,
        sum_debit_minor,
        fees_minor,
        sum_credit_minor,
        computed_minor: previous_minor + sum_debit_minor + fees_minor - sum_credit_minor,
        period_debt_minor,
    }
}

#[derive(Default)]
struct FooterBoxes {
    previous: Option<i64>,
    spend: Option<i64>,
    fees: Option<i64>,
    payments: Option<i64>,
    debt: Option<i64>,
    debt_usd: Option<i64>,
}

#[derive(Clone, Copy)]
enum FooterKey {
    Previous,
    Spend,
    Fees,
    Payments,
    Debt,
    DebtUsd,
}

/// Tek hücrenin dip özeti etiketi olup olmadığı: Türkçe katlanmış metinden
/// anahtar. "Dönem Borcu (USD)" başlıklı kutu USD kitabına düşer.
fn footer_key(cell: &str) -> Option<FooterKey> {
    let f = fold_text(cell);
    if f.contains("DEVREDEN BAKIYE") {
        Some(FooterKey::Previous)
    } else if f.contains("HARCAMALARINIZ") {
        Some(FooterKey::Spend)
    } else if f.contains("CEZA") {
        Some(FooterKey::Fees)
    } else if f.contains("ODEMELERINIZ") {
        Some(FooterKey::Payments)
    } else if f.contains("DONEM BORC") {
        if f.contains("USD") {
            Some(FooterKey::DebtUsd)
        } else {
            Some(FooterKey::Debt)
        }
    } else {
        None
    }
}

/// Dip özeti kutusunu ilgili gözeye yaz.
fn apply_footer_box(boxes: &mut FooterBoxes, key: FooterKey, value: i64) {
    match key {
        FooterKey::Previous => boxes.previous = Some(value),
        FooterKey::Spend => boxes.spend = Some(value),
        FooterKey::Fees => boxes.fees = Some(value),
        FooterKey::Payments => boxes.payments = Some(value),
        FooterKey::Debt => boxes.debt = Some(value),
        FooterKey::DebtUsd => boxes.debt_usd = Some(value),
    }
}

/// Satır içi kutu düzeni: her etiket hücresinden, sonraki etiket hücresine
/// kadarki ilk ayrıştırılabilir tutara çift kurulur. Bir satır birden çok
/// kutu taşıyabilir — tamamı çıkarılır; 2 hücreli `etiket, tutar` satırı da
/// bu düzene düşer. `:` ayraçlı başlık satırları çağıranca elenir.
fn footer_boxes_in_row(cells: &[String]) -> Vec<(FooterKey, i64)> {
    let keys: Vec<Option<FooterKey>> = cells.iter().map(|c| footer_key(c)).collect();
    let mut pairs = Vec::new();
    for (i, key) in keys.iter().enumerate() {
        let Some(key) = *key else { continue };
        let limit = keys[i + 1..]
            .iter()
            .position(|k| k.is_some())
            .map_or(cells.len(), |off| i + 1 + off);
        if let Some((minor, _)) = cells[i + 1..limit]
            .iter()
            .find_map(|c| parse_tr_amount(c).ok())
        {
            pairs.push((key, minor));
        }
    }
    pairs
}

/// Başlık tablosundaki bir etiket/değer çiftini ilgili alanına yaz.
fn apply_head_pair(label: &str, value: &str, head: &mut StatementHead) {
    let lf = fold_text(label);
    let amount = |v: &str| parse_tr_amount(v).map(|(m, _)| m).unwrap_or(0);
    if lf.contains("SONRAKI") && lf.contains("HESAP KESIM") {
        if let Some(iso) = parse_tr_date(value) {
            head.next_period_end = Some(iso);
        }
    } else if lf.contains("HESAP KESIM") {
        if let Some(iso) = parse_tr_date(value) {
            head.period_end = iso;
        }
    } else if lf.contains("ASGARI") {
        head.min_pay_minor = amount(value);
    } else if lf.contains("SONRAKI") && lf.contains("ODEME") {
        if let Some(iso) = parse_tr_date(value) {
            head.next_due_date = Some(iso);
        }
    } else if lf.contains("ODEME") && lf.contains("TARIHI") {
        if let Some(iso) = parse_tr_date(value) {
            head.due_date = Some(iso);
        }
    } else if lf.contains("KULLANILABILIR") {
        head.available_limit_minor = Some(amount(value));
    } else if lf.contains("LIMIT") {
        head.card_limit_minor = Some(amount(value));
    } else if lf.contains("USD") && lf.contains("BORC") {
        head.period_debt_usd_minor = amount(value);
    } else if lf.contains("BORC") {
        head.period_debt_minor = amount(value);
    }
}

/// 6 hücreli işlem satırını [`ParsedTxn`]'e çevirir. Yön sondaki `+`'dan,
/// tür açıklamadan (kredi satırları) ya da `Taksit` notundan/satır
/// renginden gelir.
fn txn_from_row(
    cells: Vec<String>,
    row_tokens: &str,
    date: String,
    card_last4: &str,
    row_index: usize,
) -> Result<ParsedTxn, ParseError> {
    if cells.len() < 6 {
        return Err(ParseError::Structure(format!(
            "işlem satırı {row_index} en az 6 hücre bekler, {} buldu",
            cells.len()
        )));
    }
    if card_last4.is_empty() {
        return Err(ParseError::Structure(
            "KART NO satırından önce işlem geldi".into(),
        ));
    }
    let (amount_minor, credit) = parse_tr_amount(&cells[3])?;
    let usd_minor = if cells[4].is_empty() {
        None
    } else {
        Some(parse_tr_amount(&cells[4])?.0)
    };
    let bankkart_lira_minor = if cells[5].is_empty() {
        0
    } else {
        parse_tr_amount(&cells[5])?.0
    };
    let merchant_raw = cells[1].clone();
    let extra = cells[2].clone();
    let desc_norm = merchant_norm(&format!("{merchant_raw} {extra}"));
    let norm = merchant_norm(&merchant_raw);
    let direction = if credit {
        Direction::Credit
    } else {
        Direction::Debit
    };
    let kind = if credit {
        if desc_norm.contains(PAYMENT_NORM) {
            TxnKind::Payment
        } else {
            TxnKind::Refund
        }
    } else if row_tokens.contains("kktaksit") || fold_text(&extra).contains("TAKSIT") {
        TxnKind::Installment
    } else {
        TxnKind::Pos
    };
    Ok(ParsedTxn {
        row_index: row_index as i64,
        date,
        merchant_raw,
        merchant_norm: norm,
        extra,
        amount_minor,
        direction,
        usd_minor,
        bankkart_lira_minor,
        card_last4: card_last4.to_string(),
        kind,
    })
}

/// Ham baytlardan `<img>` etiketlerini ve `data:image` yüklerini soyar.
/// Dönen baytlar tam olarak `source_sha256`'nın kapsadığı baytlardır.
fn strip_images(bytes: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(bytes);
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if starts_with_ci(&chars, i, "<img") {
            // Etiketi, tırnak içindeki '>' bozukluklarını esirgeyerek at.
            let mut quote: Option<char> = None;
            while i < chars.len() {
                let c = chars[i];
                i += 1;
                if let Some(q) = quote {
                    if c == q {
                        quote = None;
                    }
                } else if c == '"' || c == '\'' {
                    quote = Some(c);
                } else if c == '>' {
                    break;
                }
            }
            continue;
        }
        if starts_with_ci(&chars, i, "data:image") {
            // Yükü, CSS `url(...)` ve HTML öznitelik sınırlarını yutmadan at.
            while i < chars.len() {
                let c = chars[i];
                if c == '"' || c == '\'' || c == ')' || c == '<' {
                    break;
                }
                i += 1;
            }
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out.into_bytes()
}

fn starts_with_ci(chars: &[char], at: usize, needle: &str) -> bool {
    let needle: Vec<char> = needle.chars().collect();
    if at + needle.len() > chars.len() {
        return false;
    }
    chars[at..at + needle.len()]
        .iter()
        .zip(needle.iter())
        .all(|(a, b)| a.to_ascii_lowercase() == *b)
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// Hücre metni: HTML metin düğümleri birleştirilip beyaz alan katlanır.
fn cell_text(td: ElementRef<'_>) -> String {
    let raw: String = td.text().collect();
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Eşleştirme anahtarı: Türkçe büyük harf katlaması (İ/ı/i/I → I,
/// diakritikler ASCII çiftlerine iner). Şehir soymaz — o yalnızca
/// [`merchant_norm`]'un işi.
fn fold_text(s: &str) -> String {
    s.chars().map(fold_char).collect()
}

fn fold_char(c: char) -> char {
    match c {
        'İ' | 'ı' | 'i' | 'I' => 'I',
        'ş' | 'Ş' => 'S',
        'ğ' | 'Ğ' => 'G',
        'ü' | 'Ü' => 'U',
        'ö' | 'Ö' => 'O',
        'ç' | 'Ç' => 'C',
        'â' | 'Â' => 'A',
        other => other.to_uppercase().next().unwrap_or(other),
    }
}

/// Tüccar eşleştirme anahtarı: beyaz alan katlanır, Türkçe büyük harfe
/// katlanır, sondaki şehir ve `USD 18.00` kuyruğu soyulur.
/// `merchant_raw` gösterim için dokunulmadan kalır; eşleştirme yalnızca bu
/// anahtarla yapılır.
pub fn merchant_norm(raw: &str) -> String {
    let folded: String = raw
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .map(fold_char)
        .collect();
    let mut tokens: Vec<&str> = folded.split(' ').collect();
    while tokens.len() > 1 && CITY_TOKENS.contains(&tokens.last().copied().unwrap_or_default()) {
        tokens.pop();
    }
    strip_fx_tokens(&mut tokens);
    tokens.join(" ")
}

/// Gösterim: ham POS metninden yalnızca kuyruktaki döviz tutarı düşer,
/// büyük harfe katlanmaz.
pub fn merchant_stem_display(raw: &str) -> String {
    let mut tokens: Vec<&str> = raw.split_whitespace().collect();
    strip_fx_tokens(&mut tokens);
    tokens.join(" ")
}

fn strip_fx_tokens(tokens: &mut Vec<&str>) {
    while tokens.len() > 1 {
        let last = *tokens.last().unwrap_or(&"");
        if !is_fx_amount(last) {
            break;
        }
        if tokens.len() >= 2 && is_fx_ccy(tokens[tokens.len() - 2]) {
            tokens.pop();
            tokens.pop();
            continue;
        }
        break;
    }
}

fn is_fx_ccy(tok: &str) -> bool {
    matches!(
        tok,
        "USD" | "EUR" | "GBP" | "TRY" | "TL" | "CHF" | "usd" | "eur" | "gbp" | "try" | "tl" | "chf"
    )
}

fn is_fx_amount(tok: &str) -> bool {
    let mut saw_digit = false;
    let mut seps = 0u8;
    for c in tok.chars() {
        if c.is_ascii_digit() {
            saw_digit = true;
        } else if c == '.' || c == ',' {
            seps = seps.saturating_add(1);
            if seps > 1 {
                return false;
            }
        } else {
            return false;
        }
    }
    saw_digit
}

/// `1.234,56`, `1.234,56+`, `0,00`, `18.619,06 TL` → (kuruş, kredi mi).
pub fn parse_tr_amount(s: &str) -> Result<(i64, bool), ParseError> {
    let cleaned: String = s
        .trim()
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '\u{00a0}' && *c != '\u{202f}')
        .collect();
    let cleaned = cleaned.strip_suffix("TL").unwrap_or(&cleaned);
    let cleaned = cleaned.strip_suffix('₺').unwrap_or(cleaned);
    if cleaned.contains('-') {
        return Err(ParseError::Amount(s.trim().to_string()));
    }
    let credit = cleaned.ends_with('+');
    let digits = if credit {
        &cleaned[..cleaned.len() - 1]
    } else {
        cleaned
    };
    let (int_part, frac_part) = match digits.split_once(',') {
        Some((i, f)) => (i, f),
        None => (digits, ""),
    };
    let int_digits: String = int_part.chars().filter(char::is_ascii_digit).collect();
    if int_digits.is_empty() {
        return Err(ParseError::Amount(s.trim().to_string()));
    }
    let int_minor: i64 = int_digits
        .parse()
        .map_err(|_| ParseError::Amount(s.trim().to_string()))?;
    let frac_digits: Vec<char> = frac_part.chars().filter(char::is_ascii_digit).take(2).collect();
    let frac_minor = match frac_digits.len() {
        0 => 0,
        1 => (frac_digits[0] as i64 - '0' as i64) * 10,
        _ => {
            (frac_digits[0] as i64 - '0' as i64) * 10
                + (frac_digits[1] as i64 - '0' as i64)
        }
    };
    Ok((int_minor * 100 + frac_minor, credit))
}

/// `dd.mm.yyyy` → ISO `YYYY-MM-DD`; geçersizse None (satır ayıklama
/// basamakları bunu bölüm/alt bilgi satırı ayrımı için kullanır).
fn parse_tr_date(s: &str) -> Option<String> {
    let s = s.trim();
    let mut parts = s.split('.');
    let d: u8 = parts.next()?.trim().parse().ok()?;
    let m: u8 = parts.next()?.trim().parse().ok()?;
    let y: i32 = parts.next()?.trim().parse().ok()?;
    if parts.next().is_some() || !(1..=31).contains(&d) || !(1..=12).contains(&m) {
        return None;
    }
    time::Date::from_calendar_date(y, time::Month::try_from(m).ok()?, d).ok()?;
    Some(format!("{y:04}-{m:02}-{d:02}"))
}
