//! Pano, içe aktarım, gelen kutusu, işlemler ve payeeler.
//! POST sert gider, 303 ile döner; hata kodları URL'den taşınır.

use serde::Deserialize;

use budget_core::parse::{Direction, ParseError, parse_ziraat_html};
use budget_core::store::{TxnFilter, TxnRow};
use topcoat::Result;
use topcoat::context::Cx;
use topcoat::cookie::{Cookie, Cookies, cookies};
use topcoat::router::content::Form;
use topcoat::router::content::multipart::Multipart;
use topcoat::router::error::see_other;
use topcoat::router::header;
use topcoat::router::request::{headers, uri};
use topcoat::router::response::{IntoResponse, Response};
use topcoat::router::{path_param, route};
use topcoat::view::{Child, ViewExt, view};

use crate::i18n::{Key, Lang, category_label, kind_label, lang_of, t, tf};
use crate::layout::{self, Bar, Nav};
use crate::server;

path_param!(payee_id);
path_param!(code);

const IMPORT_LIMIT: usize = 8 * 1024 * 1024;

async fn respond(
    cx: &Cx,
    nav: Nav,
    title: &str,
    stage: impl topcoat::view::View,
) -> Result<Response> {
    let lang = lang_of(cx);
    layout::shell(cx, lang, title, nav, Child::new(stage))
        .await?
        .first()
        .await?
        .into_response(cx)
}

fn current_query(cx: &Cx) -> String {
    uri(cx).query().unwrap_or("").to_string()
}

fn query_value(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| urldecode(v))
    })
}

fn urldecode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = bytes[i + 1];
                let lo = bytes[i + 2];
                if let (Some(h), Some(l)) = (from_hex(hi), from_hex(lo)) {
                    out.push(char::from(h * 16 + l));
                    i += 3;
                } else {
                    out.push('%');
                    i += 1;
                }
            }
            b => {
                out.push(char::from(b));
                i += 1;
            }
        }
    }
    out
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn safe_back(cx: &Cx) -> String {
    let Some(raw) = headers(cx)
        .get(header::REFERER)
        .and_then(|v| v.to_str().ok())
    else {
        return "/".into();
    };
    let path = if let Some(idx) = raw.find("://") {
        let rest = &raw[idx + 3..];
        match rest.find('/') {
            Some(slash) => &rest[slash..],
            None => "/",
        }
    } else if raw.starts_with('/') {
        raw
    } else {
        return "/".into();
    };
    let path = path.split('#').next().unwrap_or("/");
    if !path.starts_with('/') || path.starts_with("//") || path.starts_with("/lang/") {
        return "/".into();
    }
    path.to_string()
}

#[route(GET "/lang/{code}")]
async fn set_lang(cx: &Cx) -> Result<Response> {
    let raw: &str = path_param::<Code>(cx);
    let lang = Lang::from_code(raw);
    cookies(cx).add(
        Cookie::build(("budget_lang", lang.code()))
            .path("/")
            .build(),
    );
    see_other(safe_back(cx)).into_response(cx)
}

#[route(GET "/")]
async fn home(cx: &Cx) -> Result<Response> {
    let lang = lang_of(cx);
    let store = server::store(cx);
    let statements = store.statements().await?;
    if statements.is_empty() {
        let stage = view! {
            cx =>
            <h1 class="page-title">(t(lang, Key::TitleHome))</h1>
            <section class="card empty">
                <p>(t(lang, Key::EmptyNoStatement))</p>
                <p><a class="button" href="/import">(t(lang, Key::FirstImport))</a></p>
            </section>
        };
        return respond(cx, Nav::Home, t(lang, Key::TitleHome), stage).await;
    }

    let query = current_query(cx);
    let picked = match query_value(&query, "d") {
        Some(id) if statements.iter().any(|s| s.id == id) => Some(id),
        _ => statements.first().map(|s| s.id.clone()),
    };
    let dash = store.dashboard(picked.as_deref()).await?;
    let st = dash.statement.as_ref().expect("seçili dönem ekstresi vardır");

    let mut cat_bars: Vec<Bar> = dash
        .spend_by_category
        .iter()
        .filter(|(_, minor)| *minor > 0)
        .map(|(c, minor)| {
            let name = category_label(lang, &c.id, &c.name);
            Bar {
                label: name.clone(),
                minor: *minor,
                text: layout::tr_money(*minor),
                title: format!("{} · {}", name, layout::tr_money(*minor)),
                color: c.color.clone(),
            }
        })
        .collect();
    if dash.unlabeled_debit_minor > 0 {
        let name = t(lang, Key::UnlabeledBar);
        cat_bars.push(Bar {
            label: name.into(),
            minor: dash.unlabeled_debit_minor,
            text: layout::tr_money(dash.unlabeled_debit_minor),
            title: format!("{name} · {}", layout::tr_money(dash.unlabeled_debit_minor)),
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
                "{} · {} {} · {} {}",
                month,
                t(lang, Key::Spend),
                layout::tr_money(*spend),
                t(lang, Key::Payments),
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

    let mut options: Vec<(String, String, bool)> = Vec::with_capacity(statements.len() + 1);
    options.push((
        String::new(),
        t(lang, Key::AllPeriodsHistory).into(),
        picked.is_none(),
    ));
    for s in &statements {
        options.push((
            s.id.clone(),
            format!(
                "{} {} · {}",
                t(lang, Key::Period),
                s.period_end,
                layout::tr_money(s.period_debt_minor)
            ),
            picked.as_deref() == Some(s.id.as_str()),
        ));
    }

    let stage = view! {
        cx =>
        <h1 class="page-title">(t(lang, Key::TitleHome))</h1>
        <form class="toolbar" method="get" action="/">
            <label class="field">
                <span class="field-label">(t(lang, Key::Period))</span>
                <select name="d">
                    for (id, text, sel) in options {
                        <option value=(id) if sel { selected="" }>(text)</option>
                    }
                </select>
            </label>
            <button class="button" type="submit">(t(lang, Key::Show))</button>
        </form>
        <section class="kpis">
            <div class="card kpi">
                <div class="kpi-label">(t(lang, Key::PeriodDebt))</div>
                <div class="kpi-value num">(layout::tr_money(st.period_debt_minor))</div>
            </div>
            <div class="card kpi">
                <div class="kpi-label">(t(lang, Key::Spend))</div>
                <div class="kpi-value num">(layout::tr_money(st.spend_minor))</div>
            </div>
            <div class="card kpi">
                <div class="kpi-label">(t(lang, Key::Payments))</div>
                <div class="kpi-value num">(layout::tr_money(st.payments_minor))</div>
            </div>
            <div class="card kpi">
                <div class="kpi-label">(t(lang, Key::Unlabeled))</div>
                <div class="kpi-value num">(format!("{} {}", dash.unlabeled_count, t(lang, Key::TxnsWord)))</div>
                <div class="kpi-sub num">(layout::tr_money(dash.unlabeled_debit_minor))</div>
            </div>
        </section>
        <div class="grid-2">
            <section class="card">
                <h2>(t(lang, Key::Categories))</h2>
                if cat_svg.is_empty() {
                    <p class="muted">(t(lang, Key::NoSpendPeriod))</p>
                } else {
                    (topcoat::view::Unescaped::new_unchecked(cat_svg))
                }
            </section>
            <section class="card">
                <h2>(t(lang, Key::MonthlySpend))</h2>
                if month_svg.is_empty() {
                    <p class="muted">(t(lang, Key::NoSpend))</p>
                } else {
                    (topcoat::view::Unescaped::new_unchecked(month_svg))
                }
            </section>
            <section class="card">
                <h2>(t(lang, Key::TopPayees))</h2>
                if payee_svg.is_empty() {
                    <p class="muted">(t(lang, Key::NoLabeledSpend))</p>
                } else {
                    (topcoat::view::Unescaped::new_unchecked(payee_svg))
                }
            </section>
        </div>
    };
    respond(cx, Nav::Home, t(lang, Key::TitleHome), stage).await
}

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

fn problem_banner(lang: Lang, problem: &ImportProblem) -> String {
    let head = |body: String| format!("<div class=\"banner banner-problem\">{body}</div>");
    match problem {
        ImportProblem::NoFile => head(t(lang, Key::ErrNoFile).into()),
        ImportProblem::Upload => head(t(lang, Key::ErrUpload).into()),
        ImportProblem::Parse(reason) => {
            head(tf(lang, Key::ErrParse, &layout::esc(reason)))
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
                    "<tr><td>{}</td><td class=\"num\">{}</td></tr>",
                    layout::esc(label),
                    layout::tr_money(minor)
                )
            };
            head(format!(
                "{}<table class=\"recon num\"><tbody>{}{}{}{}{}{}</tbody></table>",
                t(lang, Key::ErrReconcile),
                row(t(lang, Key::ReconPrev), *previous),
                row(t(lang, Key::ReconSpend), *spend),
                row(t(lang, Key::ReconFees), *fees),
                row(t(lang, Key::ReconPay), *credits),
                row(t(lang, Key::ReconComputed), *computed),
                row(t(lang, Key::ReconStated), *stated),
            ))
        }
    }
}

async fn import_page(
    cx: &Cx,
    ok: Option<&str>,
    problem: Option<ImportProblem>,
) -> Result<Response> {
    let lang = lang_of(cx);
    let ok_banner = ok.map(|code| {
        let body = if code == "dupe" {
            t(lang, Key::OkDupe)
        } else {
            t(lang, Key::OkImported)
        };
        format!("<div class=\"banner banner-ok\">{body}</div>")
    });
    let problem_html = problem.as_ref().map(|p| problem_banner(lang, p));
    let stage = view! {
        cx =>
        <h1 class="page-title">(t(lang, Key::TitleImport))</h1>
        if let Some(html) = ok_banner {
            (topcoat::view::Unescaped::new_unchecked(html))
        }
        if let Some(html) = problem_html {
            (topcoat::view::Unescaped::new_unchecked(html))
        }
        <section class="card">
            <form method="post" action="/import" enctype="multipart/form-data">
                <label class="field">
                    <span class="field-label">(t(lang, Key::ImportFileLabel))</span>
                    <span class="file">
                        <input class="file-hidden" type="file" name="file"
                            accept=".html,text/html" required="">
                        <span class="button">(t(lang, Key::ChooseFile))</span>
                        <span class="file-name muted" data-empty=(t(lang, Key::NoFileChosen))>
                            (t(lang, Key::NoFileChosen))
                        </span>
                    </span>
                </label>
                <button class="button" type="submit">(t(lang, Key::Upload))</button>
            </form>
            <p class="muted">(t(lang, Key::ImportHint))</p>
        </section>
    };
    respond(cx, Nav::Import, t(lang, Key::TitleImport), stage).await
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

#[route(GET "/inbox")]
async fn inbox(cx: &Cx) -> Result<Response> {
    let lang = lang_of(cx);
    let store = server::store(cx);
    let groups = store.unlabeled_groups().await?;
    let categories = store.categories().await?;
    let error = query_value(&current_query(cx), "error");
    let error_banner = error.map(|code| {
        let body = if code == "isim" {
            t(lang, Key::InboxErrName)
        } else {
            t(lang, Key::InboxErrStore)
        };
        format!("<div class=\"banner banner-problem\">{body}</div>")
    });
    let cat_opts: Vec<(String, String)> = categories
        .iter()
        .map(|c| (c.id.clone(), category_label(lang, &c.id, &c.name)))
        .collect();

    let stage = view! {
        cx =>
        <h1 class="page-title">(t(lang, Key::TitleInbox))</h1>
        if let Some(html) = error_banner {
            (topcoat::view::Unescaped::new_unchecked(html))
        }
        if groups.is_empty() {
            <section class="card empty">
                <p>(t(lang, Key::InboxEmpty))</p>
            </section>
        }
        for g in groups {
            <section class="card">
                <div class="inbox-head">
                    <strong>(g.sample_raw.clone())</strong>
                    <span class="muted num">(format!("{} {}", g.count, t(lang, Key::TxnsWord)))</span>
                </div>
                <div class="muted num">
                    (format!(
                        "{} {} · {} {}",
                        t(lang, Key::Spend),
                        layout::tr_money(g.debit_minor),
                        t(lang, Key::KindRefund),
                        layout::tr_money(g.credit_minor)
                    ))
                </div>
                <div class="muted">(g.merchant_norm.clone())</div>
                <form class="inline" method="post" action="/inbox/label">
                    <input type="hidden" name="merchant_norm" value=(g.merchant_norm.clone())>
                    <input type="text" name="payee" placeholder=(t(lang, Key::PayeePlaceholder)) required="">
                    <select name="category">
                        for (id, name) in &cat_opts {
                            <option value=(id.clone())>(name.clone())</option>
                        }
                    </select>
                    <button class="button button-small" type="submit">(t(lang, Key::Label))</button>
                </form>
            </section>
        }
    };
    respond(cx, Nav::Inbox, t(lang, Key::TitleInbox), stage).await
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
        match store
            .label(&input.merchant_norm, name, &input.category)
            .await
        {
            Ok(_) => "/inbox",
            Err(_) => "/inbox?error=depo",
        }
    };
    see_other(back).into_response(cx)
}

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
    let lang = lang_of(cx);
    let store = server::store(cx);
    let statements = store.statements().await?;
    let categories = store.categories().await?;

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
                format!("{} {}", t(lang, Key::Period), s.period_end),
                f_statement.as_deref() == Some(s.id.as_str()),
            )
        })
        .collect();
    let cat_opts: Vec<(String, String, bool)> = categories
        .iter()
        .map(|c| {
            (
                c.id.clone(),
                category_label(lang, &c.id, &c.name),
                f_category.as_deref() == Some(c.id.as_str()),
            )
        })
        .collect();

    let stage = view! {
        cx =>
        <h1 class="page-title">(t(lang, Key::TitleTxns))</h1>
        <form class="toolbar" method="get" action="/txns">
            <label class="field">
                <span class="field-label">(t(lang, Key::Period))</span>
                <select name="d">
                    <option value="">(t(lang, Key::AllPeriods))</option>
                    for (id, text, sel) in stmt_opts {
                        <option value=(id) if sel { selected="" }>(text)</option>
                    }
                </select>
            </label>
            <label class="field">
                <span class="field-label">(t(lang, Key::FilterCategory))</span>
                <select name="k">
                    <option value="">(t(lang, Key::AllCategories))</option>
                    for (id, text, sel) in cat_opts {
                        <option value=(id) if sel { selected="" }>(text)</option>
                    }
                </select>
            </label>
            <label class="field">
                <span class="field-label">(t(lang, Key::ColTxn))</span>
                <input type="search" name="q" placeholder=(t(lang, Key::SearchPlaceholder))
                    value=(f_q.clone().unwrap_or_default())>
            </label>
            <label class="check">
                <input type="checkbox" name="u" value="1" if f_unlabeled { checked="" }>
                (t(lang, Key::UnlabeledOnly))
            </label>
            <button class="button" type="submit">(t(lang, Key::Filter))</button>
        </form>
        <p class="muted">(format!("{} {}", rows.len(), t(lang, Key::TxnsWord)))</p>
        <section class="card">
            <table>
                <thead>
                    <tr>
                        <th>(t(lang, Key::ColDate))</th>
                        <th>(t(lang, Key::ColTxn))</th>
                        <th>(t(lang, Key::ColPayee))</th>
                        <th>(t(lang, Key::ColCategory))</th>
                        <th>(t(lang, Key::ColAmount))</th>
                        <th>(t(lang, Key::ColCard))</th>
                        <th>(t(lang, Key::ColKind))</th>
                    </tr>
                </thead>
                <tbody>
                    for row in rows {
                        <tr>
                            <td class="num">(row.date.clone())</td>
                            <td>
                                (row.merchant_raw.clone())
                                if !row.extra.is_empty() {
                                    <span class="muted">(" · ".to_string() + &row.extra)</span>
                                }
                            </td>
                            <td>(row.payee_name.clone().unwrap_or_else(|| "—".into()))</td>
                            <td>(row.category_id.as_deref().map(|id| {
                                category_label(lang, id, row.category_name.as_deref().unwrap_or(""))
                            }).unwrap_or_else(|| "—".into()))</td>
                            <td class=(amount_class(row.direction))>(amount_text(&row))</td>
                            <td class="num">("•• ".to_string() + &row.card_last4)</td>
                            <td class="muted">(kind_label(lang, row.kind))</td>
                        </tr>
                    }
                </tbody>
            </table>
        </section>
    };
    respond(cx, Nav::Txns, t(lang, Key::TitleTxns), stage).await
}

#[route(GET "/payees")]
async fn payees(cx: &Cx) -> Result<Response> {
    let lang = lang_of(cx);
    let store = server::store(cx);
    let payees = store.payees().await?;
    let categories = store.categories().await?;
    let error = query_value(&current_query(cx), "error");
    let cat_opts: Vec<(String, String)> = categories
        .iter()
        .map(|c| (c.id.clone(), category_label(lang, &c.id, &c.name)))
        .collect();

    let stage = view! {
        cx =>
        <h1 class="page-title">(t(lang, Key::TitlePayees))</h1>
        if error.is_some() {
            <div class="banner banner-problem">(t(lang, Key::ErrPayeeSave))</div>
        }
        if payees.is_empty() {
            <section class="card empty">
                <p>(t(lang, Key::PayeesEmpty))</p>
            </section>
        }
        <section class="card">
            <table>
                <thead>
                    <tr>
                        <th>(t(lang, Key::ColPayee))</th>
                        <th>(t(lang, Key::ColCategory))</th>
                    </tr>
                </thead>
                <tbody>
                    for p in payees {
                        <tr>
                            <td>
                                <form id=(format!("p-{}", p.id)) method="post"
                                    action=(format!("/payees/{}", p.id)) data-autosubmit=""></form>
                                <input class="payee-name" type="text" name="name"
                                    form=(format!("p-{}", p.id)) value=(p.name.clone()) required="">
                                if !p.sources.is_empty() {
                                    <div class="field-label">(t(lang, Key::OnStatement))</div>
                                    <ul class="payee-sources">
                                        for s in p.sources.clone() {
                                            <li>
                                                (s.display.clone())
                                                if s.count > 1 {
                                                    <span>(format!(" · {}", s.count))</span>
                                                }
                                                <form class="inline" method="post"
                                                    action=(format!("/payees/{}/source", p.id))>
                                                    <input type="hidden" name="merchant_norm"
                                                        value=(s.merchant_norm.clone())>
                                                    <button class="linkish" type="submit">
                                                        (t(lang, Key::ClearSource))
                                                    </button>
                                                </form>
                                            </li>
                                        }
                                    </ul>
                                }
                            </td>
                            <td>
                                <select name="category" form=(format!("p-{}", p.id))>
                                    for (id, name) in &cat_opts {
                                        <option value=(id.clone()) if *id == p.category_id { selected="" }>
                                            (name.clone())
                                        </option>
                                    }
                                </select>
                                <form class="inline" method="post"
                                    action=(format!("/payees/{}/clear", p.id))>
                                    <button class="linkish" type="submit">(t(lang, Key::Clear))</button>
                                </form>
                            </td>
                        </tr>
                    }
                </tbody>
            </table>
        </section>
    };
    respond(cx, Nav::Payees, t(lang, Key::TitlePayees), stage).await
}

#[derive(Deserialize)]
struct PayeeForm {
    name: String,
    category: String,
}

#[route(POST "/payees/{payee_id}")]
async fn payee_update(cx: &Cx, Form(input): Form<PayeeForm>) -> Result<Response> {
    let store = server::store(cx);
    let id: &str = path_param::<PayeeId>(cx);
    let back = match store
        .update_payee(id, Some(&input.name), Some(&input.category))
        .await
    {
        Ok(()) => "/payees",
        Err(_) => "/payees?error=depo",
    };
    see_other(back).into_response(cx)
}

#[derive(Deserialize)]
struct SourceForm {
    merchant_norm: String,
}

#[route(POST "/payees/{payee_id}/clear")]
async fn payee_clear(cx: &Cx) -> Result<Response> {
    let store = server::store(cx);
    let id: &str = path_param::<PayeeId>(cx);
    let back = match store.clear_payee(id).await {
        Ok(()) => "/payees",
        Err(_) => "/payees?error=depo",
    };
    see_other(back).into_response(cx)
}

#[route(POST "/payees/{payee_id}/source")]
async fn payee_clear_source(cx: &Cx, Form(input): Form<SourceForm>) -> Result<Response> {
    let store = server::store(cx);
    let id: &str = path_param::<PayeeId>(cx);
    let back = match store.clear_source(id, &input.merchant_norm).await {
        Ok(()) => "/payees",
        Err(_) => "/payees?error=depo",
    };
    see_other(back).into_response(cx)
}
