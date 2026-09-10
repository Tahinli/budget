//! Her sayfanın üstünden geçtiği belge kabuğu ve tek grafik yardımcısı.
//!
//! Dil çerezi ve `data-autosubmit` dışında çalışma anı çerçevesi yok.
//! Tek stil kaynağı [`STYLE`], tek çubuk grafik yardımcısı [`svg_bars`].

use topcoat::Result;
use topcoat::asset::{Asset, asset};
use topcoat::context::Cx;
use topcoat::view::{Child, Unescaped, View, view};

use crate::i18n::{Key, Lang, t};
use crate::server;

/// `style/main.scss`, `build.rs`'in `assets/main.css`'e derlediği tek stil.
pub(crate) const STYLE: Asset = asset!("assets/main.css");

/// Payee autosave, dosya adı, gelen kutusu etiketi (yenilemeden kart düşer).
const SHELL_JS: &str = r#"
function postForm(form) {
  var body = new URLSearchParams(new FormData(form));
  return fetch(form.getAttribute('action'), { method: 'POST', body: body, redirect: 'follow' });
}
document.addEventListener('change', function (e) {
  var el = e.target;
  if (!el) return;
  if (el.classList && el.classList.contains('file-hidden')) {
    var name = (el.files && el.files[0]) ? el.files[0].name : '';
    var slot = el.parentElement && el.parentElement.querySelector('.file-name');
    if (slot) slot.textContent = name || slot.getAttribute('data-empty') || '';
    return;
  }
  var form = el.form;
  if (form && form.hasAttribute('data-autosubmit')) {
    postForm(form).then(function (res) {
      if (res.url && res.url.indexOf('error=') !== -1) location.href = res.url;
    });
  }
});
document.addEventListener('submit', function (e) {
  var form = e.target;
  if (!form || form.getAttribute('action') !== '/inbox/label') return;
  e.preventDefault();
  var card = form.closest('.card');
  var countEl = card && card.querySelector('.inbox-head .num');
  var n = countEl ? parseInt(countEl.textContent, 10) : 0;
  if (isNaN(n)) n = 0;
  postForm(form).then(function (res) {
    if (res.url && res.url.indexOf('error=') !== -1) { location.href = res.url; return; }
    if (!res.ok) { form.submit(); return; }
    if (card) card.remove();
    var badge = document.querySelector('.topnav .badge');
    if (badge) {
      var left = parseInt(badge.textContent, 10) - n;
      if (isNaN(left) || left <= 0) badge.remove();
      else badge.textContent = String(left);
    }
    if (!document.querySelector('form[action="/inbox/label"]')) location.reload();
  }).catch(function () { form.submit(); });
});
document.addEventListener('input', function (e) {
  var el = e.target;
  if (!el || el.name !== 'q') return;
  var q = el.value.toLowerCase();
  var table = document.querySelector('table');
  if (!table) return;
  var rows = table.querySelectorAll('tbody tr');
  for (var i = 0; i < rows.length; i++) {
    var text = (rows[i].textContent || '').toLowerCase();
    rows[i].hidden = q.length > 0 && text.indexOf(q) === -1;
  }
});
"#;

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
/// işlem sayılarının toplamı — kişinin kendi verisinde. Oturum yoksa
/// rozet de kişinin adı da çerez çıkışı da yoktur; menü yine durur, çünkü
/// her yol girişe döndürür.
pub(crate) async fn shell<'a>(
    cx: &'a Cx,
    lang: Lang,
    title: &'a str,
    active: Nav,
    stage: Child<'a>,
) -> Result<impl View + 'a> {
    let user = server::current_user(cx).await.clone();
    let unlabeled: i64 = match &user {
        Some(user) => server::store(cx)
            .unlabeled_groups(&user.sub)
            .await?
            .iter()
            .map(|g| g.count)
            .sum(),
        None => 0,
    };
    let name = user.as_ref().map(|user| user.name.clone());
    let en = lang == Lang::En;
    Ok(view! {
        cx =>
        <!DOCTYPE html>
        <html lang=(lang.code())>
            <head>
                <meta charset="utf-8">
                <meta name="viewport" content="width=device-width, initial-scale=1">
                <title>(title.to_string())</title>
                <link rel="stylesheet" href=(STYLE)>
            </head>
            <body>
                <nav class="topnav">
                    <a class=(nav_class(Nav::Home, active)) href="/">(t(lang, Key::NavHome))</a>
                    <a class=(nav_class(Nav::Import, active)) href="/import">(t(lang, Key::NavImport))</a>
                    <a class=(nav_class(Nav::Inbox, active)) href="/inbox">
                        (t(lang, Key::NavInbox))
                        if unlabeled > 0 {
                            <span class="badge">(unlabeled.to_string())</span>
                        }
                    </a>
                    <a class=(nav_class(Nav::Txns, active)) href="/txns">(t(lang, Key::NavTxns))</a>
                    <a class=(nav_class(Nav::Payees, active)) href="/payees">(t(lang, Key::NavPayees))</a>
                    <span class="lang">
                        <a class=(en.then_some("active")) href="/lang/en">"EN"</a>
                        <a class=((!en).then_some("active")) href="/lang/tr">"TR"</a>
                    </span>
                    if let Some(name) = name {
                        <span class="muted">(name)</span>
                        <a href="/auth/logout">(t(lang, Key::SignOut))</a>
                    }
                </nav>
                <main class="stage">
                    (stage)
                </main>
                <script>(Unescaped::new_unchecked(SHELL_JS))</script>
            </body>
        </html>
    })
}
