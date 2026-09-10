//! Depo testleri: içe aktarım sayıları, mükerrer SHA koruması, etiketleme
//! kalıcılığı (sonraki içe aktarım takma ada otomatik bağlanır), gösterge
//! panosu, gelen kutusu grupları ve kiracı yalıtımı.

use std::path::Path;

use budget_core::parse::{merchant_norm, parse_ziraat_html, Direction, TxnKind};
use budget_core::store::{
    StoreError, TursoStore, TxnFilter, CAT_IADE, CAT_MARKET, CAT_ODEME,
};
use ulid::Ulid;

const FIXTURE: &str = include_str!("fixtures/ziraat_min.html");

/// Test kiracıları — im OIDC `sub` değerlerini temsil eder.
const U1: &str = "u1";
const U2: &str = "u2";

async fn open_store() -> TursoStore {
    let storage = std::env::temp_dir().join(format!("budget-store-test-{}", Ulid::generate()));
    TursoStore::open(Path::new(":memory:"), &storage)
        .await
        .expect("bellek içi depo açılır")
}

/// İkinci ekstre: aynı tüccarlar, farklı tutarlar — dip kimliği yine tutar,
/// SHA bambaşka olur.
fn fixture_v2() -> Vec<u8> {
    FIXTURE
        .replace("99,00", "105,00")
        .replace("3.895,56", "3.901,56")
        .replace("3.845,56", "3.851,56")
        .into_bytes()
}

#[tokio::test]
async fn imports_statement_with_counts_and_payment_autolabel() {
    let store = open_store().await;
    let report = store
        .import(
            U1,
            parse_ziraat_html(FIXTURE.as_bytes()).expect("fikstür çözümlenir"),
        )
        .await
        .expect("içe aktarım");
    assert!(!report.duplicate);
    assert_eq!(report.txn_count, 10);
    assert_eq!(report.labeled, 1, "yalnızca kart ödemesi otomatik etiketli");
    assert_eq!(report.unlabeled, 9);

    let statements = store.statements(U1).await.expect("ekstreler");
    assert_eq!(statements.len(), 1);
    assert_eq!(statements[0].period_debt_minor, 384_556);
    assert_eq!(statements[0].previous_balance_minor, 100_000);

    let txns = store
        .list_txns(U1, TxnFilter::default())
        .await
        .expect("işlemler");
    assert_eq!(txns.len(), 10);
    let payment = txns
        .iter()
        .find(|t| t.kind == TxnKind::Payment)
        .expect("ödeme işlemi");
    assert_eq!(payment.payee_name.as_deref(), Some("Kart ödemesi"));
    assert_eq!(payment.category_id.as_deref(), Some(CAT_ODEME));
}

#[tokio::test]
async fn duplicate_sha_is_reported_and_not_reinserted() {
    let store = open_store().await;
    let first = store
        .import(
            U1,
            parse_ziraat_html(FIXTURE.as_bytes()).expect("fikstür çözümlenir"),
        )
        .await
        .expect("ilk içe aktarım");
    assert!(!first.duplicate);

    let second = store
        .import(
            U1,
            parse_ziraat_html(FIXTURE.as_bytes()).expect("fikstür çözümlenir"),
        )
        .await
        .expect("ikinci içe aktarım");
    assert!(second.duplicate);
    assert_eq!(second.txn_count, 10, "mevcut işlem sayısı raporlanır");
    assert_eq!(second.statement_id, first.statement_id);

    assert_eq!(store.statements(U1).await.unwrap().len(), 1);
    assert_eq!(
        store
            .list_txns(U1, TxnFilter::default())
            .await
            .unwrap()
            .len(),
        10
    );
}

#[tokio::test]
async fn label_persists_and_relinks_later_imports() {
    let store = open_store().await;
    store
        .import(
            U1,
            parse_ziraat_html(FIXTURE.as_bytes()).expect("fikstür çözümlenir"),
        )
        .await
        .expect("ilk içe aktarım");

    let norm = merchant_norm("KAHVE ORNEK ANKARA");
    let report = store
        .label(U1, &norm, "Kahve Dükkanı", CAT_MARKET)
        .await
        .expect("etiketleme");
    assert!(report.alias_created);
    assert_eq!(report.txns_updated, 1);

    // İkinci, farklı SHA'lı ekstre: aynı tüccar takma ada kendiliğinden
    // bağlanır.
    let second = store
        .import(U1, parse_ziraat_html(&fixture_v2()).expect("v2 çözümlenir"))
        .await
        .expect("ikinci içe aktarım");
    assert!(!second.duplicate);
    assert_eq!(second.labeled, 2, "ödeme takma adı + kahve takma adı");

    let rows = store
        .list_txns(
            U1,
            TxnFilter {
                statement_id: Some(second.statement_id.clone()),
                q: Some("KAHVE".into()),
                ..TxnFilter::default()
            },
        )
        .await
        .expect("filtreli liste");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].payee_id.as_deref(), Some(report.payee_id.as_str()));

    // Yeniden etiketleme: takma ad güncellenir, iki ekstrenin işlemleri de.
    let relabel = store
        .label(U1, &norm, "Kahve Dükkanı", CAT_IADE)
        .await
        .expect("yeniden etiketleme");
    assert!(!relabel.alias_created);
    assert_eq!(relabel.txns_updated, 2);
}

#[tokio::test]
async fn dashboard_unlabeled_groups_and_seed_categories() {
    let store = open_store().await;

    let cats = store.categories().await.expect("tohum kategoriler");
    assert_eq!(cats.len(), 14);
    assert_eq!(cats[0].id, CAT_MARKET, "sort 10 ilk sırada");
    assert!(cats.iter().any(|c| c.id == CAT_IADE));

    store
        .import(
            U1,
            parse_ziraat_html(FIXTURE.as_bytes()).expect("fikstür çözümlenir"),
        )
        .await
        .expect("içe aktarım");

    let groups = store.unlabeled_groups(U1).await.expect("gruplar");
    let market_norm = merchant_norm("MARKET ORNEK V020 KAYSERİ");
    let market = groups
        .iter()
        .find(|g| g.merchant_norm == market_norm)
        .expect("pazar grubu");
    assert_eq!(market.count, 1);
    assert_eq!(market.debit_minor, 25_000);
    assert_eq!(market.credit_minor, 0);
    assert_eq!(market.hits.len(), 1);
    assert_eq!(market.hits[0].date, "2026-08-10");
    assert_eq!(market.hits[0].amount_minor, 25_000);
    assert_eq!(market.hits[0].direction, Direction::Debit);

    let taksit_norm = merchant_norm("ORNEK TAKSIT MAGZA");
    let taksit = groups
        .iter()
        .find(|g| g.merchant_norm == taksit_norm)
        .expect("taksit grubu");
    assert_eq!(taksit.count, 2);
    assert_eq!(
        taksit
            .hits
            .iter()
            .map(|h| (h.date.as_str(), h.amount_minor))
            .collect::<Vec<_>>(),
        vec![("2026-08-22", 75_000), ("2026-08-22", 75_000)]
    );

    let dash = store.dashboard(U1, None).await.expect("pano");
    assert!(dash.statement.is_some());
    assert_eq!(dash.unlabeled_count, 9);
    assert_eq!(dash.unlabeled_debit_minor, 389_556);
    assert!(dash.spend_by_category.is_empty());
    assert_eq!(dash.monthly.len(), 2, "2026-08 ve 2026-09");
    assert_eq!(dash.monthly[0], ("2026-08".to_string(), 359_556, 105_000));
    assert_eq!(dash.monthly[1], ("2026-09".to_string(), 30_000, 0));

    store
        .label(U1, &market_norm, "Ornek Market", CAT_MARKET)
        .await
        .expect("pazar etiketi");
    let dash = store.dashboard(U1, None).await.expect("pano");
    assert_eq!(dash.unlabeled_count, 8);
    assert_eq!(dash.spend_by_category.len(), 1);
    assert_eq!(dash.spend_by_category[0].0.id, CAT_MARKET);
    assert_eq!(dash.spend_by_category[0].1, 25_000);
    assert_eq!(dash.top_payees, vec![("Ornek Market".to_string(), 25_000)]);
}

#[tokio::test]
async fn user_scoping_isolates_tenants() {
    let store = open_store().await;
    let r1 = store
        .import(
            U1,
            parse_ziraat_html(FIXTURE.as_bytes()).expect("fikstür çözümlenir"),
        )
        .await
        .expect("u1 içe aktarım");
    let r2 = store
        .import(
            U2,
            parse_ziraat_html(FIXTURE.as_bytes()).expect("fikstür çözümlenir"),
        )
        .await
        .expect("u2 aynı fikstürü ayrı ekstre olarak alır");
    assert!(!r1.duplicate);
    assert!(!r2.duplicate, "aynı SHA başka kiracıda kopya sayılmaz");
    assert_ne!(r1.statement_id, r2.statement_id);

    // Gelen kutusu kiracıya özgüdür: u1'deki etiket u2'ye sızmaz.
    let norm = merchant_norm("KAHVE ORNEK ANKARA");
    let label = store
        .label(U1, &norm, "Kahve Dükkanı", CAT_MARKET)
        .await
        .expect("u1 etiketi");
    assert!(label.alias_created, "takma adlar kiracıya özgüdür");

    let g1 = store.unlabeled_groups(U1).await.expect("u1 grupları");
    assert!(g1.iter().all(|g| g.merchant_norm != norm));
    let g2 = store.unlabeled_groups(U2).await.expect("u2 grupları");
    assert!(
        g2.iter().any(|g| g.merchant_norm == norm),
        "u2 grubu etiketten etkilenmez"
    );

    // Payee listesi kiracıya özgüdür.
    let p1 = store.payees(U1).await.expect("u1 payee'leri");
    assert!(p1.iter().any(|p| p.name == "Kahve Dükkanı"));
    let p2 = store.payees(U2).await.expect("u2 payee'leri");
    assert!(p2.iter().all(|p| p.name != "Kahve Dükkanı"));

    // Başka kiracının payee'si görünmez: güncelleme bulunamaz döner.
    let foreign = &p2[0];
    let err = store
        .update_payee(U1, &foreign.id, Some("Yeni Ad"), None)
        .await
        .expect_err("başka kiracının payee adı yazılamaz");
    assert!(matches!(err, StoreError::NotFound(_)));
    let err = store
        .set_payee_category(U1, &foreign.id, CAT_MARKET)
        .await
        .expect_err("başka kiracının kategorisi yazılamaz");
    assert!(matches!(err, StoreError::NotFound(_)));
    let p2_after = store.payees(U2).await.expect("u2 payee'leri");
    assert_eq!(p2_after[0].name, p2[0].name, "u2 payee'si değişmedi");

    // Ekstre ve işlem listeleri kiracıya özgüdür.
    let s1 = store.statements(U1).await.expect("u1 ekstreleri");
    let s2 = store.statements(U2).await.expect("u2 ekstreleri");
    assert_eq!(s1.len(), 1);
    assert_eq!(s2.len(), 1);
    assert_ne!(s1[0].id, s2[0].id);
    assert_eq!(
        store
            .list_txns(U1, TxnFilter::default())
            .await
            .unwrap()
            .len(),
        10
    );
    assert_eq!(
        store
            .list_txns(U2, TxnFilter::default())
            .await
            .unwrap()
            .len(),
        10
    );

    // Pano kiracıya özgüdür: u1'in etiketli harcaması u2'de görünmez.
    let d1 = store.dashboard(U1, None).await.expect("u1 panosu");
    assert_eq!(d1.unlabeled_count, 8);
    assert_eq!(d1.spend_by_category.len(), 1);
    assert_eq!(d1.spend_by_category[0].0.id, CAT_MARKET);
    let d2 = store.dashboard(U2, None).await.expect("u2 panosu");
    assert_eq!(d2.unlabeled_count, 9);
    assert!(d2.spend_by_category.is_empty());
}
