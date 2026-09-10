//! Pano, içe aktarım, gelen kutusu, işlemler ve payeeler — bütün sayfa
//! işleyicileri. Sayfalar düz HTML formudur: POST sert gider, 303 ile
//! döner; hata kodları URL'den taşınır, cümle asla taşınmaz.

use serde::Deserialize;

use budget_core::parse::{Direction, ParseError, parse_ziraat_html};
use budget_core::store::{TxnFilter, TxnRow};
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::router::content::Form;
use topcoat::router::content::multipart::Multipart;
use topcoat::router::error::see_other;
use topcoat::router::request::uri;
use topcoat::router::response::{IntoResponse, Response};
use topcoat::router::{path_param, route};
use topcoat::view::{Child, ViewExt, view};

use crate::layout::{self, Bar, Nav};
use crate::server;

path_param!(payee_id);

/// `/import` yüklemesi `BodyLimit` ile aynı tavanı paylaşır.
const IMPORT_LIMIT: usize = 8 * 1024 * 1024;

/// Sahneyi kabuğa sarıp yanıtla — her işleyicinin tek çıkış kapısı.
async fn respond(cx: &Cx, nav: Nav, title: &str, stage: impl topcoat::view::View) -> Result<Response> {
    layout::shell(cx, title, nav, Child::new(stage))
        .await?
        .first()
        .await?
        .into_response(cx)
}

/// Sorgu dizesinin tamamı; `uri(cx)`'in tek okunuş burada dursun.
fn current_query(cx: &Cx) -> String {
    uri(cx).query().unwrap_or("").to_string()
}

/// Sorgudan tek anahtarın değeri (`+` boşluk, form stili).
fn query_value(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| urldecode(v))
    })
}

/// Percent-decoder; sorgu değerleri küçüktür, tahsis sorunu değil.
fn urldecode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// GET / — gösterge panosu
// ---------------------------------------------------------------------------

#[route(GET "/")]
async fn home(cx: &Cx) -> Result<Response> {
    let store = server::store(cx);
    let statements = store.statements().await?;
    if statements.is_empty() {
        let stage = view! {
            cx =>
            <h1 class="page-title">"Pano"</h1>
            <section class="card empty">
                <p>"Henüz ekstre yüklenmedi."</p>
                <p><a class="button" href="/import">"İlk ekstreyi içe aktar"</a></p>
            </section>
        };
        return respond(cx, Nav::Home, "Pano", stage).await;
    }

    let query = current_query(cx);
    let picked = match query_value(&query, "d") {
        Some(id) if statements.iter().any(|s| s.id == id) => Some(id),
        // Varsayılan en yeni dönemdir: `statements` period_end'e göre
        // azalan dizili gelir.
        _ => statements.first().map(|s| s.id.clone()),
    };
    let dash = store.dashboard(picked.as_deref()).await?;
    let st = dash.statement.as_ref().expect("seçili dönem ekstresi vardır");

    // Tek SVG yardımcısına giden üç seri: kategoriler (kendi renkleri +
    // etiketsiz kovası), aylar ve en çok harcanan payeeler.
    let mut cat_bars: Vec<Bar> = dash
        .spend_by_category
        .iter()
        .filter(|(_, minor)| *minor > 0)
        .map(|(c, minor)| Bar {
            label: c.name.clone(),
            minor: *minor,
            text: layout::tr_money(*minor),
            title: format!("{} · {}", c.name, layout::tr_money(*minor)),
            color: c.color.clone(),
        })
        .collect();
    if dash.unlabeled_debit_minor > 0 {
        cat_bars.push(Bar {
            label: "Etiketsiz".into(),
            minor: dash.unlabeled_debit_minor,
            text: layout::tr_money(dash.unlabeled_debit_minor),
            title: format!("Etiketsiz · {}", layout::tr_money(dash.unlabeled_debit_minor)),
            color: "#64748b".into(),
        });
    }
    let month_bars: Vec<Bar> = dash
        .monthly
        .iter()
        .map(|(month, spend, credits)| Bar {
            label: month.clone(),
            minor: *spend,
            text: layout::tr_money(*spend),
            title: format!(
                "{month} · harcama {} · ödeme {}",
                layout::tr_money(*spend),
                layout::tr_money(*credits)
            ),
            color: "#5eead4".into(),
        })
        .collect();
    let payee_bars: Vec<Bar> = dash
        .top_payees
        .iter()
        .map(|(name, minor)| Bar {
            label: name.clone(),
            minor: *minor,
            text: layout::tr_money(*minor),
            title: format!("{name} · {}", layout::tr_money(*minor)),
            color: "#38bdf8".into(),
        })
        .collect();

    let cat_svg = layout::svg_bars(&cat_bars, false);
    let month_svg = layout::svg_bars(&month_bars, true);
    let payee_svg = layout::svg_bars(&payee_bars, false);

    // Dönem seçenekleri: boş değer bütün geçmişi ölçer; aksi halde tek
    // ekstreye sıkışır.
    let mut options: Vec<(String, String, bool)> = Vec::with_capacity(statements.len() + 1);
    options.push((
        String::new(),
        "Tüm dönemler (tüm geçmiş)".into(),
        picked.is_none(),
    ));
    for s in &statements {
        options.push((
            s.id.clone(),
            format!("Dönem {} · borç {}", s.period_end, layout::tr_money(s.period_debt_minor)),
            picked.as_deref() == Some(s.id.as_str()),
        ));
    }

    let stage = view! {
        cx =>
        <h1 class="page-title">"Pano"</h1>
        <form class="toolbar" method="get" action="/">
            <label class="field">
                <span class="field-label">"Dönem"</span>
                <select name="d">
                    for (id, text, sel) in options {
                        <option value=(id) if sel { selected="" }>(text)</option>
                    }
                </select>
            </label>
            <button class="button" type="submit">"Göster"</button>
        </form>
        <section class="kpis">
            <div class="card kpi">
                <div class="kpi-label">"Dönem borcu"</div>
                <div class="kpi-value num">(layout::tr_money(st.period_debt_minor))</div>
            </div>
            <div class="card kpi">
                <div class="kpi-label">"Harcamalar"</div>
                <div class="kpi-value num">(layout::tr_money(st.spend_minor))</div>
            </div>
            <div class="card kpi">
                <div class="kpi-label">"Ödemeler"</div>
                <div class="kpi-value num">(layout::tr_money(st.payments_minor))</div>
            </div>
            <div class="card kpi">
                <div class="kpi-label">"Etiketlenmemiş"</div>
                <div class="kpi-value num">(dash.unlabeled_count.to_string())" işlem"</div>
                <div class="kpi-sub num">(layout::tr_money(dash.unlabeled_debit_minor))</div>
            </div>
        </section>
        <div class="grid-2">
            <section class="card">
                <h2>"Kategoriler"</h2>
                if cat_svg.is_empty() {
                    <p class="muted">"Bu dönemde harcama yok."</p>
                } else {
                    (topcoat::view::Unescaped::new_unchecked(cat_svg))
                }
            </section>
            <section class="card">
                <h2>"Aylık harcama"</h2>
                if month_svg.is_empty() {
                    <p class="muted">"Harcama yok."</p>
                } else {
                    (topcoat::view::Unescaped::new_unchecked(month_svg))
                }
            </section>
            <section class="card">
                <h2>"En çok harcanan payeeler"</h2>
                if payee_svg.is_empty() {
                    <p class="muted">"Etiketli harcama yok — önce gelen kutusunu boşaltın."</p>
                } else {
                    (topcoat::view::Unescaped::new_unchecked(payee_svg))
                }
            </section>
        </div>
    };
    respond(cx, Nav::Home, "Pano", stage).await
}

// ---------------------------------------------------------------------------
// GET + POST /import — multipart ekstre yüklemesi
// ---------------------------------------------------------------------------

/// İçe aktarımın geri dönüşünde sayfanın üstünde büyüyen sorun. Uzlaşma
/// hatası dört sayının tamamını taşır: sözleşme gereği.
enum ImportProblem {
    NoFile,
    Upload,
    Parse(String),
    Reconcile {
        previous: i64,
        spend: i64,
        fees: i64,
        credits: i64,
        computed: i64,
        stated: i64,
    },
}
/// Sorun bantları hazır HTML'dir: dört uzlaşma sayısı tablo olarak çıkar,
/// view! grameri sade kalır. Dinamik parçalar `esc` ile kaçışlıdır.
fn problem_banner(problem: &ImportProblem) -> String {
    let head = |body: String| {
        format!("<div class=\"banner banner-problem\">{body}</div>")
    };
    match problem {
        ImportProblem::NoFile => head("Dosya seçilmedi.".into()),
        ImportProblem::Upload => {
            head("Yükleme okunamadı ya da 8 MB sınırını aştı.".into())
        }
        ImportProblem::Parse(reason) => {
            head(format!("Ekstre çözülemedi: {}", layout::esc(reason)))
        }
        ImportProblem::Reconcile {
            previous,
            spend,
            fees,
            credits,
            computed,
            stated,
        } => {
            let row = |label: &str, minor: i64| {
                format!(
                    "<tr><td>{label}</td><td class=\"num\">{}</td></tr>",
                    layout::tr_money(minor)
                )
            };
            head(format!(
                "Uzlaşma tutmuyor — ekstrenin dip özeti işlem satırlarıyla \
                 uyuşmuyor. Hiçbir kayıt yazılmadı.\
                 <table class=\"recon num\"><tbody>{}{}{}{}{}{}</tbody></table>",
                row("Önceki bakiye (ÖNCEKİ AYDAN DEVİR)", *previous),
                row("+ Harcamalarınız", *spend),
                row("+ Ceza ücret ve kesintiler", *fees),
                row("− Ödemeleriniz", *credits),
                row("= Hesaplanan dönem borcu", *computed),
                row("Ekstrenin yazdığı dönem borcu", *stated),
            ))
        }
    }
}

/// Yükleme formu; `ok` ile başarı, `problem` ile hata bandı aynı sahnede.
async fn import_page(
    cx: &Cx,
    ok: Option<&str>,
    problem: Option<ImportProblem>,
) -> Result<Response> {
    let ok_banner = ok.map(|code| {
        if code == "dupe" {
            "<div class=\"banner banner-ok\">Bu ekstre zaten yüklü — aynı \
             özetli dosya hiçbir kayıt yazmaz.</div>"
                .to_string()
        } else {
            "<div class=\"banner banner-ok\">Ekstre yüklendi. Etiketlenmemiş \
             işlemleri <a href=\"/inbox\">gelen kutusunda</a> etiketleyin.</div>"
                .to_string()
        }
    });
    let problem_html = problem.as_ref().map(problem_banner);
    let stage = view! {
        cx =>
        <h1 class="page-title">"İçe aktar"</h1>
        if let Some(html) = ok_banner {
            (topcoat::view::Unescaped::new_unchecked(html))
        }
        if let Some(html) = problem_html {
            (topcoat::view::Unescaped::new_unchecked(html))
        }
        <section class="card">
            <form method="post" action="/import" enctype="multipart/form-data">
                <label class="field">
                    <span class="field-label">"Ziraat Katılım ekstresi (.html)"</span>
                    <input type="file" name="file" accept=".html,text/html" required="">
                </label>
                <button class="button" type="submit">"Yükle"</button>
            </form>
            <p class="muted">
                "Dosyanın kendisi saklanmaz: görselleri soyulmuş baytların "
                "yalnızca SHA-256 özeti tutulur, aynı özetli ikinci yükleme kopya sayılır."
            </p>
        </section>
    };
    respond(cx, Nav::Import, "İçe aktar", stage).await
}

#[route(GET "/import")]
async fn import_get(cx: &Cx) -> Result<Response> {
    let ok = query_value(&current_query(cx), "ok");
    import_page(cx, ok.as_deref(), None).await
}

#[route(POST "/import")]
async fn import_post(cx: &Cx, mut multipart: Multipart) -> Result<Response> {
    let mut file: Option<Vec<u8>> = None;
    loop {
        let mut field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(_) => return import_page(cx, None, Some(ImportProblem::Upload)).await,
        };
        // Yalnızca dosya adı taşıyan alan ekstredir; diğer form alanları
        // (ileride eklenirse) sessizce atlanır.
        if field.file_name().is_none() {
            continue;
        }
        let mut collected = Vec::new();
        loop {
            match field.chunk().await {
                Ok(Some(chunk)) => {
                    if collected.len() + chunk.len() > IMPORT_LIMIT {
                        return import_page(cx, None, Some(ImportProblem::Upload)).await;
                    }
                    collected.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(_) => return import_page(cx, None, Some(ImportProblem::Upload)).await,
            }
        }
        file = Some(collected);
    }
    let Some(bytes) = file else {
        return import_page(cx, None, Some(ImportProblem::NoFile)).await;
    };

    let parsed = match parse_ziraat_html(&bytes) {
        Ok(parsed) => parsed,
        Err(ParseError::Reconcile {
            previous_minor,
            sum_debit_minor,
            fees_minor,
            sum_credit_minor,
            computed_minor,
            period_debt_minor,
        }) => {
            return import_page(
                cx,
                None,
                Some(ImportProblem::Reconcile {
                    previous: previous_minor,
                    spend: sum_debit_minor,
                    fees: fees_minor,
                    credits: sum_credit_minor,
                    computed: computed_minor,
                    stated: period_debt_minor,
                }),
            )
            .await;
        }
        Err(other) => {
            return import_page(cx, None, Some(ImportProblem::Parse(other.to_string()))).await;
        }
    };

    let store = server::store(cx);
    match store.import(parsed).await {
        Ok(report) if report.duplicate => see_other("/import?ok=dupe").into_response(cx),
        Ok(_) => see_other("/import?ok=new").into_response(cx),
        Err(_) => see_other("/import?error=depo").into_response(cx),
    }
}

// ---------------------------------------------------------------------------
// GET + POST /inbox — etiketleme gelen kutusu
// ---------------------------------------------------------------------------

#[route(GET "/inbox")]
async fn inbox(cx: &Cx) -> Result<Response> {
    let store = server::store(cx);
    let groups = store.unlabeled_groups().await?;
    let categories = store.categories().await?;
    let error = query_value(&current_query(cx), "error");
    let error_banner = error.map(|code| {
        if code == "isim" {
            "<div class=\"banner banner-problem\">Payee adı boş olamaz.</div>".to_string()
        } else {
            "<div class=\"banner banner-problem\">Etiket depoya yazılamadı.</div>".to_string()
        }
    });

    let stage = view! {
        cx =>
        <h1 class="page-title">"Etiketle"</h1>
        if let Some(html) = error_banner {
            (topcoat::view::Unescaped::new_unchecked(html))
        }
        if groups.is_empty() {
            <section class="card empty">
                <p>"Gelen kutusu boş — bütün işlemler etiketli."</p>
            </section>
        }
        for g in groups {
            <section class="card">
                <div class="inbox-head">
                    <strong>(g.sample_raw.clone())</strong>
                    <span class="muted num">(g.count.to_string())" işlem"</span>
                </div>
                <div class="muted num">
                    (format!(
                        "harcama {} · iade {}",
                        layout::tr_money(g.debit_minor),
                        layout::tr_money(g.credit_minor)
                    ))
                </div>
                <div class="muted">(g.merchant_norm.clone())</div>
                <form class="inline" method="post" action="/inbox/label">
                    <input type="hidden" name="merchant_norm" value=(g.merchant_norm.clone())>
                    <input type="text" name="payee" placeholder="Payee adı" required="">
                    <select name="category">
                        for c in &categories {
                            <option value=(c.id.clone())>(c.name.clone())</option>
                        }
                    </select>
                    <button class="button button-small" type="submit">"Etiketle"</button>
                </form>
            </section>
        }
    };
    respond(cx, Nav::Inbox, "Etiketle", stage).await
}

#[derive(Deserialize)]
struct LabelForm {
    merchant_norm: String,
    payee: String,
    category: String,
}

#[route(POST "/inbox/label")]
async fn inbox_label(cx: &Cx, Form(input): Form<LabelForm>) -> Result<Response> {
    let store = server::store(cx);
    let name = input.payee.trim();
    let back = if name.is_empty() {
        "/inbox?error=isim"
    } else {
        match store.label(&input.merchant_norm, name, &input.category).await {
            Ok(_) => "/inbox",
            Err(_) => "/inbox?error=depo",
        }
    };
    see_other(back).into_response(cx)
}

// ---------------------------------------------------------------------------
// GET /txns — süzülebilir işlem tablosu
// ---------------------------------------------------------------------------

/// Kredi satırlarının tablodaki imzası: önde `+`, yeşil.
fn amount_text(t: &TxnRow) -> String {
    let base = layout::tr_money(t.amount_minor);
    if t.direction == Direction::Credit {
        format!("+{base}")
    } else {
        base
    }
}

fn amount_class(direction: Direction) -> Option<&'static str> {
    if direction == Direction::Credit {
        Some("num credit")
    } else {
        Some("num")
    }
}

#[route(GET "/txns")]
async fn txns(cx: &Cx) -> Result<Response> {
    let store = server::store(cx);
    let statements = store.statements().await?;
    let categories = store.categories().await?;

    // Süzgeçler sorgudan: boş değer = süzme yok.
    let query = current_query(cx);
    let f_statement = query_value(&query, "d").filter(|id| !id.is_empty());
    let f_category = query_value(&query, "k").filter(|id| !id.is_empty());
    let f_unlabeled = query_value(&query, "u").is_some();
    let f_q = query_value(&query, "q")
        .map(|q| q.trim().to_string())
        .filter(|q| !q.is_empty());

    let rows = store
        .list_txns(TxnFilter {
            statement_id: f_statement.clone(),
            category_id: f_category.clone(),
            unlabeled_only: f_unlabeled,
            q: f_q.clone(),
        })
        .await?;

    let stmt_opts: Vec<(String, String, bool)> = statements
        .iter()
        .map(|s| {
            (
                s.id.clone(),
                format!("Dönem {}", s.period_end),
                f_statement.as_deref() == Some(s.id.as_str()),
            )
        })
        .collect();
    let cat_opts: Vec<(String, String, bool)> = categories
        .iter()
        .map(|c| {
            (
                c.id.clone(),
                c.name.clone(),
                f_category.as_deref() == Some(c.id.as_str()),
            )
        })
        .collect();

    let stage = view! {
        cx =>
        <h1 class="page-title">"İşlemler"</h1>
        <form class="toolbar" method="get" action="/txns">
            <label class="field">
                <span class="field-label">"Dönem"</span>
                <select name="d">
                    <option value="">"Tüm dönemler"</option>
                    for (id, text, sel) in stmt_opts {
                        <option value=(id) if sel { selected="" }>(text)</option>
                    }
                </select>
            </label>
            <label class="field">
                <span class="field-label">"Kategori"</span>
                <select name="k">
                    <option value="">"Tüm kategoriler"</option>
                    for (id, text, sel) in cat_opts {
                        <option value=(id) if sel { selected="" }>(text)</option>
                    }
                </select>
            </label>
            <label class="field">
                <span class="field-label">"Ara"</span>
                <input type="search" name="q" placeholder="tüccar adı"
                    value=(f_q.clone().unwrap_or_default())>
            </label>
            <label class="check">
                <input type="checkbox" name="u" value="1" if f_unlabeled { checked="" }>
                "Etiketsizler"
            </label>
            <button class="button" type="submit">"Süz"</button>
        </form>
        <p class="muted">(format!("{} işlem", rows.len()))</p>
        <section class="card">
            <table>
                <thead>
                    <tr>
                        <th>"Tarih"</th>
                        <th>"İşlem"</th>
                        <th>"Payee"</th>
                        <th>"Kategori"</th>
                        <th>"Tutar"</th>
                        <th>"Kart"</th>
                        <th>"Tür"</th>
                    </tr>
                </thead>
                <tbody>
                    for t in rows {
                        <tr>
                            <td class="num">(t.date.clone())</td>
                            <td>
                                (t.merchant_raw.clone())
                                if !t.extra.is_empty() {
                                    <span class="muted">(" · ".to_string() + &t.extra)</span>
                                }
                            </td>
                            <td>(t.payee_name.clone().unwrap_or_else(|| "—".into()))</td>
                            <td>(t.category_name.clone().unwrap_or_else(|| "—".into()))</td>
                            <td class=(amount_class(t.direction))>(amount_text(&t))</td>
                            <td class="num">("•• ".to_string() + &t.card_last4)</td>
                            <td class="muted">(layout::kind_label(t.kind))</td>
                        </tr>
                    }
                </tbody>
            </table>
        </section>
    };
    respond(cx, Nav::Txns, "İşlemler", stage).await
}

// ---------------------------------------------------------------------------
// GET /payees + POST /payees/{payee_id}/category
// ---------------------------------------------------------------------------

#[route(GET "/payees")]
async fn payees(cx: &Cx) -> Result<Response> {
    let store = server::store(cx);
    let payees = store.payees().await?;
    let categories = store.categories().await?;
    let error = query_value(&current_query(cx), "error");

    let stage = view! {
        cx =>
        <h1 class="page-title">"Payeeler"</h1>
        if error.is_some() {
            <div class="banner banner-problem">"Kategori kaydedilemedi."</div>
        }
        if payees.is_empty() {
            <section class="card empty">
                <p>"Henüz payee yok — işlemleri " <a href="/inbox">"gelen kutusunda"</a> " etiketleyin."</p>
            </section>
        }
        <section class="card">
            <table>
                <thead>
                    <tr>
                        <th>"Payee"</th>
                        <th>"Kategori"</th>
                    </tr>
                </thead>
                <tbody>
                    for p in payees {
                        <tr>
                            <td>(p.name.clone())</td>
                            <td>
                                <form class="inline" method="post" action=(format!("/payees/{}/category", p.id))>
                                    <select name="category">
                                        for c in &categories {
                                            <option value=(c.id.clone()) if c.id == p.category_id { selected="" }>
                                                (c.name.clone())
                                            </option>
                                        }
                                    </select>
                                    <button class="button button-small" type="submit">"Kaydet"</button>
                                </form>
                            </td>
                        </tr>
                    }
                </tbody>
            </table>
        </section>
    };
    respond(cx, Nav::Payees, "Payeeler", stage).await
}

#[derive(Deserialize)]
struct CategoryForm {
    category: String,
}

#[route(POST "/payees/{payee_id}/category")]
async fn payee_category(cx: &Cx, Form(input): Form<CategoryForm>) -> Result<Response> {
    let store = server::store(cx);
    let id: &str = path_param::<PayeeId>(cx);
    let back = match store.set_payee_category(id, &input.category).await {
        Ok(()) => "/payees",
        Err(_) => "/payees?error=depo",
    };
    see_other(back).into_response(cx)
}
