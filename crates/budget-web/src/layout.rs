//! Her sayfanın üstünden geçtiği belge kabuğu ve tek grafik yardımcısı.
//!
//! Sayfalar düz HTML formudur: hidrasyon yok, çalışma anı betiği yok. Tek
//! stil kaynağı [`STYLE`], tek çubuk grafik yardımcısı [`svg_bars`] —
//! ikinci bir widget takımı çıkmaz.

use topcoat::Result;
use topcoat::asset::{Asset, asset};
use topcoat::context::Cx;
use topcoat::view::{Child, View, view};

use budget_core::parse::TxnKind;

use crate::server;

/// `style/main.scss`, `build.rs`'in `assets/main.css`'e derlediği tek stil.
pub(crate) const STYLE: Asset = asset!("assets/main.css");

/// Üst menüde hangi sekmenin aktif olduğunu damglamak için.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Nav {
    Home,
    Import,
    Inbox,
    Txns,
    Payees,
}

/// `1.234,56 TL` — kuruşu i64 olan tutarların ekran biçimi.
/// Bankanın kendi birimi; `₺` sistem yazıtipinde tofu basıyor.
pub(crate) fn tr_money(minor: i64) -> String {
    let sign = if minor < 0 { "-" } else { "" };
    let minor = minor.unsigned_abs();
    let digits = (minor / 100).to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push('.');
        }
        grouped.push(c);
    }
    format!("{sign}{grouped},{:02} TL", minor % 100)
}

/// İşlem türünün tablodaki kısa Türkçe etiketi.
pub(crate) fn kind_label(kind: TxnKind) -> &'static str {
    match kind {
        TxnKind::Pos => "pos",
        TxnKind::Installment => "taksit",
        TxnKind::Payment => "ödeme",
        TxnKind::Refund => "iade",
    }
}

/// Tek çubuk: etiket, kuruş değeri, gösterim metni, araç ipucu ve dolgu
/// rengi. Yatay ve dikey çizim aynı yardımcıya gider.
pub(crate) struct Bar {
    pub label: String,
    pub minor: i64,
    pub text: String,
    pub title: String,
    pub color: String,
}

/// Bir dizi çubuğu tek inline SVG'ye döker. Yatayda genişlik en büyüğün
/// yüzdesi, dikeyde yükseklik öyledir; sıfır seri boş metin döner.
pub(crate) fn svg_bars(bars: &[Bar], vertical: bool) -> String {
    if bars.is_empty() {
        return String::new();
    }
    let max = bars.iter().map(|b| b.minor).max().unwrap_or(1).max(1);
    let mut out =
        String::from("<svg class=\"chart\" preserveAspectRatio=\"xMinYMin meet\" role=\"img\">");
    if vertical {
        // Aylık sütunlar: ay başına bir yuva, taban çizgisi üstünde yükselen
        // tek dolgu; ayrıntı araç ipucunda.
        let slot = 56;
        let width = bars.len() as i64 * slot;
        let (height, base, lift) = (170, 140, 120);
        out.push_str(&format!(" viewBox=\"0 0 {width} {height}\">"));
        for (i, bar) in bars.iter().enumerate() {
            let x = i as i64 * slot;
            let bh = (bar.minor * lift / max).max(2);
            let by = base - bh;
            out.push_str(&format!(
                "<g><title>{}</title>\
                 <rect x=\"{}\" y=\"{by}\" width=\"26\" height=\"{bh}\" rx=\"4\" fill=\"{}\"></rect>\
                 <text class=\"chart-value\" x=\"{}\" y=\"{}\" text-anchor=\"middle\">{}</text>\
                 <text class=\"chart-label\" x=\"{}\" y=\"158\" text-anchor=\"middle\">{}</text></g>",
                esc(&bar.title),
                x + 15,
                esc(&bar.color),
                x + 28,
                by - 5,
                esc(&bar.text),
                x + 28,
                esc(&bar.label),
            ));
        }
    } else {
        // Yatay sıralar: sabit etiket kolonu, en büyüğe oranlı çubuk, sağda
        // tutar. Sıra çağıranın gelir — zaten büyükten küçüğe dizilir.
        const WIDTH: i64 = 640;
        const ROW: i64 = 26;
        const BAR_X: i64 = 176;
        const AREA: i64 = 350;
        const VAL_X: i64 = 638;
        let height = bars.len() as i64 * ROW;
        out.push_str(&format!(" viewBox=\"0 0 {WIDTH} {height}\">"));
        for (i, bar) in bars.iter().enumerate() {
            let y = i as i64 * ROW;
            let bw = (bar.minor * AREA / max).max(2);
            out.push_str(&format!(
                "<g><title>{}</title>\
                 <text class=\"chart-label\" x=\"0\" y=\"{}\">{}</text>\
                 <rect x=\"{BAR_X}\" y=\"{}\" width=\"{bw}\" height=\"16\" rx=\"4\" fill=\"{}\"></rect>\
                 <text class=\"chart-value\" x=\"{VAL_X}\" y=\"{}\" text-anchor=\"end\">{}</text></g>",
                esc(&bar.title),
                y + 17,
                esc(&bar.label),
                y + 5,
                esc(&bar.color),
                y + 17,
                esc(&bar.text),
            ));
        }
    }
    out.push_str("</svg>");
    out
}

/// SVG metnine giden her değerin kaçışı; kategori ve payee adları veridir.
/// Sayfa tarafındaki hazır HTML bantları da aynı kaçışı kullanır.
pub(crate) fn esc(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

fn nav_class(mine: Nav, active: Nav) -> Option<&'static str> {
    (mine == active).then_some("active")
}

/// Bütün sayfanın üstünden geçtiği belge: tek koyu tuval, üst menü,
/// etiketlenmemiş işlem sayısı rozeti ve sayfanın kendi sahnesi.
///
/// Rozet her sayfada aynı kuralı izler: gelen kutusunun gruplarındaki
/// işlem sayılarının toplamı.
pub(crate) async fn shell<'a>(
    cx: &'a Cx,
    title: &'a str,
    active: Nav,
    stage: Child<'a>,
) -> Result<impl View + 'a> {
    let unlabeled: i64 = server::store(cx)
        .unlabeled_groups()
        .await?
        .iter()
        .map(|g| g.count)
        .sum();
    Ok(view! {
        cx =>
        <!DOCTYPE html>
        <html lang="tr">
            <head>
                <meta charset="utf-8">
                <meta name="viewport" content="width=device-width, initial-scale=1">
                <title>(title.to_string())</title>
                <link rel="stylesheet" href=(STYLE)>
            </head>
            <body>
                <nav class="topnav">
                    <a class=(nav_class(Nav::Home, active)) href="/">"Anasayfa"</a>
                    <a class=(nav_class(Nav::Import, active)) href="/import">"İçe aktar"</a>
                    <a class=(nav_class(Nav::Inbox, active)) href="/inbox">
                        "Etiketle"
                        if unlabeled > 0 {
                            <span class="badge">(unlabeled.to_string())</span>
                        }
                    </a>
                    <a class=(nav_class(Nav::Txns, active)) href="/txns">"İşlemler"</a>
                    <a class=(nav_class(Nav::Payees, active)) href="/payees">"Payeeler"</a>
                </nav>
                <main class="stage">
                    (stage)
                </main>
            </body>
        </html>
    })
}
